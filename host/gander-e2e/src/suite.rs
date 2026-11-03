// SPDX-License-Identifier: Apache-2.0
//! The cases. The fixture layout and share ids below must match `xtask/src/e2e.rs`, which
//! generates the tree and `config.toml`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use futures_util::{SinkExt as _, StreamExt as _};
use gander_core::error::ErrorCode;
use gander_core::pairing::b64url;
use gander_core::protocol::HostId;
use gander_core::protocol::schema;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use crate::Options;
use crate::client::{Client, Req, Resp, enc};
use crate::report::{Case, Kind, Outcome, Report};

/// Full permissions (`list, preview, fetch, stream`), `stream_default = "allow"`.
pub const SHARE: &str = "shr_e2e";
/// `list` only.
pub const LIMITED: &str = "shr_limited";
/// Configured, but the paired device holds no grant.
pub const NOGRANT: &str = "shr_nogrant";

const PHOTO: &str = "images/photo.bmp";
const PNG: &str = "images/pixel.png";
/// 1400 x 1000: three tile levels (350 x 250, 700 x 500 and 1400 x 1000 px).
const LARGE: &str = "images/large.bmp";
const PDF: &str = "docs/three-pages.pdf";
const UTF16: &str = "docs/utf16.txt";
const UTF16_TEXT: &str = "Gander UTF-16 \u{e9}t\u{e9} \u{65e5}\u{672c} line 1\r\nline 2\r\n";
const NOTES: &str = "docs/notes.txt";
const BINARY: &str = "docs/data.bin";
const TOOL: &str = "docs/tool.exe";
const DEEP: &str = "nested/a/b/c/deep.txt";
const UNICODE_DIR: &str = "\u{dc}n\u{ef}c\u{f6}d\u{e9} \u{65e5}\u{672c}\u{8a9e}";
const EMOJI_FILE: &str = "emoji \u{1f600} file.txt";
const BIG: &str = "big.bin";
const RESUME: &str = "change/resume.bin";
const WATCH_DIR: &str = "watch";
/// Opened by the test host's `e2e-testwin` catalogue entry (approved for `.gtw`).
const STREAM_OK: &str = "stream/e2e.gtw";
/// Same app, an extension it is not approved for: the policy asks.
const STREAM_ASK: &str = "stream/e2e.gtx";
/// The test host's catalogue entry for `gander-testwin` (`--test-stream-app`).
const TEST_APP: &str = "e2e-testwin";
/// How long a launch of the test window, discovery and the engine's offer may take.
const STREAM_WAIT: Duration = Duration::from_secs(40);
const LOCKED_DIR: &str = "locked";

/// Records one case: `$body` is an expression yielding `Result<(), String>` (usually an
/// `async { .. }.await`), evaluated before the report is borrowed.
macro_rules! case {
    ($s:ident, $id:expr, $ep:expr, $kind:expr, $code:expr, $body:expr) => {{
        let started = Instant::now();
        let result: Result<(), String> = $body;
        $s.record($id, $ep, $kind, $code, result, started);
    }};
}

struct Suite<'a> {
    opts: &'a Options,
    doc: Value,
    c: Client,
    report: Report,
    device: Option<String>,
    /// `capabilities.stream.enabled` of the host.
    stream_enabled: bool,
}

fn err<E: std::fmt::Display>(e: E) -> String {
    format!("{e:#}")
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ensure(cond: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if cond { Ok(()) } else { Err(what()) }
}

fn is_image(body: &[u8]) -> bool {
    (body.len() > 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP")
        || body.starts_with(&[0xFF, 0xD8])
}

/// Width and height of a WebP file (lossy, lossless or extended), read from its first chunk.
fn webp_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 30 || &b[..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return None;
    }
    match &b[12..16] {
        b"VP8 " if b[23..26] == [0x9d, 0x01, 0x2a] => Some((
            u32::from(u16::from_le_bytes([b[26], b[27]]) & 0x3fff),
            u32::from(u16::from_le_bytes([b[28], b[29]]) & 0x3fff),
        )),
        b"VP8L" if b[20] == 0x2f => {
            let bits = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
        }
        b"VP8X" => Some((
            u32::from_le_bytes([b[24], b[25], b[26], 0]) + 1,
            u32::from_le_bytes([b[27], b[28], b[29], 0]) + 1,
        )),
        _ => None,
    }
}

fn random_nonce() -> String {
    let mut b = [0u8; 16];
    let rng = ring::rand::SystemRandom::new();
    let _ = ring::rand::SecureRandom::fill(&rng, &mut b);
    b64url(&b)
}

fn pair_body(name: &str) -> Value {
    json!({
        "device": { "name": name, "platform": "e2e", "model": "gander-e2e", "app_version": "0.0.0" },
        "nonce": random_nonce(),
    })
}

fn viewport() -> Value {
    json!({ "width": 1080, "height": 2280, "dpr": 2.625 })
}

impl Suite<'_> {
    fn record(
        &mut self,
        id: &str,
        endpoint: &str,
        kind: Kind,
        code: Option<ErrorCode>,
        result: Result<(), String>,
        started: Instant,
    ) {
        self.report.push(Case {
            id: id.to_owned(),
            endpoint: endpoint.to_owned(),
            kind,
            code,
            outcome: match result {
                Ok(()) => Outcome::Pass,
                Err(e) => Outcome::Fail(e),
            },
            ms: started.elapsed().as_millis(),
        });
    }

    fn not_reachable(
        &mut self,
        id: &str,
        endpoint: &str,
        kind: Kind,
        code: Option<ErrorCode>,
        why: &str,
    ) {
        self.report.push(Case {
            id: id.to_owned(),
            endpoint: endpoint.to_owned(),
            kind,
            code,
            outcome: Outcome::NotReachable(why.to_owned()),
            ms: 0,
        });
    }

    fn root(&self) -> &Path {
        &self.opts.share_root
    }

    fn fixture(&self, rel: &str) -> Result<Vec<u8>, String> {
        std::fs::read(self.root().join(rel)).map_err(|e| format!("read fixture {rel}: {e}"))
    }

    // ------------------------------------------------------------------ checks

    fn check_headers(r: &Resp) -> Result<(), String> {
        ensure(r.header("x-gander-protocol") == Some("1"), || {
            format!("X-Gander-Protocol missing or wrong on {}", r.describe())
        })?;
        ensure(
            r.header("x-gander-request").is_some_and(|v| v.len() == 26),
            || format!("X-Gander-Request missing on {}", r.describe()),
        )?;
        ensure(r.header("access-control-allow-origin").is_none(), || {
            "a CORS header was sent".to_owned()
        })
    }

    fn validate(&self, schema_name: &str, value: &Value) -> Result<(), String> {
        schema::validate_named(&self.doc, schema_name, value).map_err(|e| e.join("; "))
    }

    fn json_ok(&self, r: &Resp, status: u16, schema_name: &str) -> Result<Value, String> {
        Self::check_headers(r)?;
        ensure(r.status == status, || {
            format!("expected HTTP {status}, got {}", r.describe())
        })?;
        ensure(
            r.header("content-type")
                .is_some_and(|c| c.starts_with("application/json")),
            || format!("content-type is {:?}", r.header("content-type")),
        )?;
        let v = r.json().ok_or("body is not JSON")?;
        self.validate(schema_name, &v)?;
        Ok(v)
    }

    fn error_ok(&self, r: &Resp, code: ErrorCode) -> Result<(), String> {
        Self::check_headers(r)?;
        let want = code.http_status();
        ensure(
            r.status == want && r.error_code().as_deref() == Some(code.as_str()),
            || {
                format!(
                    "expected HTTP {want} {}, got {}",
                    code.as_str(),
                    r.describe()
                )
            },
        )?;
        let v = r.json().ok_or("error body is not JSON")?;
        self.validate("ErrorBody", &v)?;
        let message = v
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("");
        let root = self.root().to_string_lossy().to_string();
        ensure(!message.contains(&root) && !message.contains(":\\"), || {
            format!("error message leaks a path: {message}")
        })
    }

    async fn send(&self, req: &Req) -> Result<Resp, String> {
        self.c.send(req).await.map_err(err)
    }

    /// Sends `req` until it is not `503 generating` (at most `wait`), returning the final response
    /// and whether a `generating` answer was seen (validated as an error body on the way).
    async fn until_ready(&self, req: &Req, wait: Duration) -> Result<(Resp, bool), String> {
        let deadline = Instant::now() + wait;
        let mut saw = false;
        loop {
            let r = self.send(req).await?;
            if r.status == 503 && r.error_code().as_deref() == Some("generating") {
                self.error_ok(&r, ErrorCode::Generating)?;
                let ms = r
                    .json()
                    .and_then(|v| v.pointer("/error/retry_after_ms").and_then(Value::as_u64))
                    .unwrap_or(0);
                ensure(ms > 0, || "generating without retry_after_ms".to_owned())?;
                saw = true;
                if Instant::now() > deadline {
                    return Err("still generating after the wait".to_owned());
                }
                tokio::time::sleep(Duration::from_millis(ms.clamp(50, 500))).await;
                continue;
            }
            return Ok((r, saw));
        }
    }

    async fn expect_error(&mut self, id: &str, endpoint: &str, code: ErrorCode, req: Req) {
        case!(
            self,
            id,
            endpoint,
            Kind::Error,
            Some(code),
            async {
                let r = self.send(&req).await?;
                self.error_ok(&r, code)
            }
            .await
        );
    }

    // ------------------------------------------------------------------ host, pin, gate

    async fn host_and_gate(&mut self) {
        let ep = "GET /v1/host";
        case!(
            self,
            "host.hello",
            ep,
            Kind::Success,
            None,
            async {
                let r = self.send(&Req::get("/v1/host").anonymous()).await?;
                let v = self.json_ok(&r, 200, "HostHello")?;
                ensure(v["host_id"] == self.c.host_id.as_str(), || {
                    format!("host_id {} differs from the pin", v["host_id"])
                })?;
                ensure(v["pairing_open"] == true, || {
                    "pairing_open is false in test-approve mode".to_owned()
                })
            }
            .await
        );
        case!(
            self,
            "tls.pin_mismatch_refused",
            "TLS",
            Kind::Success,
            None,
            async {
                let other = self
                    .c
                    .with_pin(HostId::from("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"))
                    .map_err(err)?;
                match other.send(&Req::get("/v1/host").anonymous()).await {
                    Ok(r) => Err(format!("a wrongly pinned client got {}", r.describe())),
                    Err(_) => Ok(()),
                }
            }
            .await
        );
        case!(
            self,
            "host.emulator_alias_10.0.2.2",
            ep,
            Kind::Success,
            None,
            async {
                let host = format!("10.0.2.2:{}", self.c.addr.port());
                let r = self
                    .send(&Req::get("/v1/host").anonymous().host(host))
                    .await?;
                self.json_ok(&r, 200, "HostHello").map(drop)
            }
            .await
        );
        let port = self.c.addr.port();
        self.expect_error(
            "gate.bad_host.foreign_name",
            ep,
            ErrorCode::BadHost,
            Req::get("/v1/host").anonymous().host("evil.example"),
        )
        .await;
        self.expect_error(
            "gate.bad_host.wrong_port",
            ep,
            ErrorCode::BadHost,
            Req::get("/v1/host")
                .anonymous()
                .host(format!("127.0.0.1:{}", port.wrapping_add(1))),
        )
        .await;
        self.expect_error(
            "gate.bad_host.alias_wrong_port",
            ep,
            ErrorCode::BadHost,
            Req::get("/v1/host")
                .anonymous()
                .host(format!("10.0.2.2:{}", port.wrapping_add(1))),
        )
        .await;
        for (i, (name, value)) in [
            ("Origin", "https://evil.example"),
            ("Sec-Fetch-Mode", "cors"),
            ("Cookie", "a=b"),
        ]
        .into_iter()
        .enumerate()
        {
            self.expect_error(
                &format!("gate.browser_request.{i}_{}", name.to_ascii_lowercase()),
                ep,
                ErrorCode::BrowserRequest,
                Req::get("/v1/host").anonymous().header(name, value),
            )
            .await;
        }
        self.expect_error(
            "gate.unknown_endpoint_needs_token_first",
            "GET /v1/<unknown>",
            ErrorCode::TokenMissing,
            Req::get("/v1/definitely-not-an-endpoint").anonymous(),
        )
        .await;
    }

    /// Gate cases that need a paired device.
    async fn gate_authenticated(&mut self) {
        self.expect_error(
            "gate.unknown_endpoint",
            "GET /v1/<unknown>",
            ErrorCode::NotFound,
            Req::get("/v1/definitely-not-an-endpoint"),
        )
        .await;
        self.expect_error(
            "gate.method_not_allowed",
            "PUT /v1/me",
            ErrorCode::BadRequest,
            Req::new("PUT", "/v1/me"),
        )
        .await;
    }

    // ------------------------------------------------------------------ pairing

    async fn pairing(&mut self) -> Result<()> {
        let ep_pair = "POST /v1/pair";
        let ep_poll = "GET /v1/pair/{pairing_id}";
        let mut started_id = None;
        case!(
            self,
            "pair.start",
            ep_pair,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(
                        &Req::new("POST", "/v1/pair")
                            .anonymous()
                            .json(&pair_body("e2e device")),
                    )
                    .await?;
                let v = self.json_ok(&r, 200, "PairStarted")?;
                ensure(
                    r.header("cache-control")
                        .is_some_and(|c| c.contains("no-store")),
                    || "PairStarted without Cache-Control: no-store".to_owned(),
                )?;
                started_id = v["pairing_id"].as_str().map(str::to_owned);
                Ok(())
            }
            .await
        );
        let id = started_id.context("pairing could not start; nothing else can run")?;
        self.not_reachable(
            "pair.in_progress",
            ep_pair,
            Kind::Error,
            Some(ErrorCode::PairingInProgress),
            "the test hook approves each request before the next can arrive, so no pairing is ever pending (covered by host/gander-host/tests/pairing.rs)",
        );
        let mut approved = None;
        case!(
            self,
            "pair.poll_approved",
            ep_poll,
            Kind::Success,
            None,
            async {
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    let r = self
                        .send(&Req::get(format!("/v1/pair/{id}")).anonymous())
                        .await?;
                    if r.status == 202 {
                        self.json_ok(&r, 202, "PairPollStatus")?;
                        if Instant::now() > deadline {
                            return Err("still pending after 15 s".to_owned());
                        }
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                    let v = self.json_ok(&r, 200, "PairApproved")?;
                    ensure(v["host"]["id"] == self.c.host_id.as_str(), || {
                        "PairApproved.host.id differs from the pin".to_owned()
                    })?;
                    approved = Some((
                        v["token"].as_str().unwrap_or_default().to_owned(),
                        v["device_id"].as_str().unwrap_or_default().to_owned(),
                    ));
                    return Ok(());
                }
            }
            .await
        );
        let (token, device) = approved.context("pairing was not approved")?;
        self.c.token = Some(token);
        self.device = Some(device);
        self.not_reachable(
            "pair.poll_pending",
            ep_poll,
            Kind::Success,
            None,
            "the test hook approves loopback requests at once; 202 pending is never observable",
        );
        self.not_reachable(
            "pair.poll_denied",
            ep_poll,
            Kind::Success,
            None,
            "403 denied needs the owner's Deny click (tray, WP-18); the test hook only approves",
        );
        self.expect_error(
            "pair.poll_after_collect",
            ep_poll,
            ErrorCode::PairingExpired,
            Req::get(format!("/v1/pair/{id}")).anonymous(),
        )
        .await;
        self.expect_error(
            "pair.poll_unknown",
            ep_poll,
            ErrorCode::NotFound,
            Req::get("/v1/pair/pair_doesnotexist").anonymous(),
        )
        .await;
        // Failures count per source per window; five lead to 429. Keep these last.
        self.expect_error(
            "pair.bad_request.malformed",
            ep_pair,
            ErrorCode::BadRequest,
            Req::new("POST", "/v1/pair")
                .anonymous()
                .raw_body(b"{not json".to_vec()),
        )
        .await;
        let mut wrong_secret = pair_body("wrong secret");
        wrong_secret["secret"] = json!(b64url(&[7u8; 32]));
        self.expect_error(
            "pair.forbidden.bad_secret",
            ep_pair,
            ErrorCode::Forbidden,
            Req::new("POST", "/v1/pair").anonymous().json(&wrong_secret),
        )
        .await;
        let mut bad_nonce = pair_body("bad nonce");
        bad_nonce["nonce"] = json!("short");
        self.expect_error(
            "pair.bad_request.nonce",
            ep_pair,
            ErrorCode::BadRequest,
            Req::new("POST", "/v1/pair").anonymous().json(&bad_nonce),
        )
        .await;
        case!(
            self,
            "pair.rate_limited",
            ep_pair,
            Kind::Error,
            Some(ErrorCode::PairingRateLimited),
            async {
                for _ in 0..8 {
                    let r = self
                        .send(
                            &Req::new("POST", "/v1/pair")
                                .anonymous()
                                .raw_body(b"x".to_vec()),
                        )
                        .await?;
                    if r.status == 429 {
                        return self.error_ok(&r, ErrorCode::PairingRateLimited);
                    }
                }
                Err("no 429 after eight more failed attempts".to_owned())
            }
            .await
        );
        self.not_reachable(
            "pair.closed",
            ep_pair,
            Kind::Error,
            Some(ErrorCode::PairingClosed),
            "the test hook re-opens the pairing window for every loopback request",
        );
        Ok(())
    }

    /// Waits until the `--test-grant` grants were applied (they follow the approval).
    async fn wait_for_grants(&mut self) {
        case!(
            self,
            "shares.list",
            "GET /v1/shares",
            Kind::Success,
            None,
            async {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let r = self.send(&Req::get("/v1/shares")).await?;
                    let v = self.json_ok(&r, 200, "SharesResponse")?;
                    let ids: BTreeSet<&str> = v["shares"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|s| s["id"].as_str()).collect())
                        .unwrap_or_default();
                    if ids.contains(SHARE) && ids.contains(LIMITED) {
                        ensure(!ids.contains(NOGRANT), || {
                            "a share without a grant is listed".to_owned()
                        })?;
                        return Ok(());
                    }
                    if Instant::now() > deadline {
                        return Err(format!("test grants not applied after 10 s: {ids:?}"));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            .await
        );
    }

    // ------------------------------------------------------------------ auth, me, rotate

    async fn auth_me_rotate(&mut self) {
        let ep = "GET /v1/me";
        case!(
            self,
            "me.ok",
            ep,
            Kind::Success,
            None,
            async {
                let r = self.send(&Req::get("/v1/me")).await?;
                let v = self.json_ok(&r, 200, "MeResponse")?;
                ensure(v["capabilities"]["events"] == true, || {
                    "capabilities.events is not true".to_owned()
                })?;
                ensure(v["network"]["relayed"].is_null(), || {
                    "network.relayed is not null on loopback".to_owned()
                })?;
                ensure(v["device"]["id"].as_str() == self.device.as_deref(), || {
                    "device.id is not the paired device".to_owned()
                })
            }
            .await
        );
        self.expect_error(
            "auth.token_missing",
            ep,
            ErrorCode::TokenMissing,
            Req::get("/v1/me").anonymous(),
        )
        .await;
        self.expect_error(
            "auth.token_invalid",
            ep,
            ErrorCode::TokenInvalid,
            Req::get("/v1/me")
                .anonymous()
                .header("Authorization", format!("Bearer {}", b64url(&[9u8; 32]))),
        )
        .await;
        self.expect_error(
            "auth.token_malformed",
            ep,
            ErrorCode::TokenInvalid,
            Req::get("/v1/me")
                .anonymous()
                .header("Authorization", "Basic abc"),
        )
        .await;
        self.not_reachable(
            "auth.network_identity_mismatch",
            ep,
            Kind::Error,
            Some(ErrorCode::NetworkIdentityMismatch),
            "needs a Tailscale WhoIs identity; loopback peers are never looked up (covered by \
             host/gander-host/tests/pairing.rs with a fake WhoIs)",
        );

        let ep = "POST /v1/token/rotate";
        let old = self.c.token.clone();
        let mut new = None;
        case!(
            self,
            "token.rotate",
            ep,
            Kind::Success,
            None,
            async {
                let r = self.send(&Req::new("POST", "/v1/token/rotate")).await?;
                let v = self.json_ok(&r, 200, "TokenRotateResponse")?;
                new = v["token"].as_str().map(str::to_owned);
                ensure(new.is_some() && new != old, || "no new token".to_owned())
            }
            .await
        );
        if let Some(t) = new {
            self.c.token = Some(t);
            let stale = self.c.with_token(old);
            case!(
                self,
                "token.old_refused",
                ep,
                Kind::Error,
                Some(ErrorCode::TokenInvalid),
                async {
                    let r = stale.send(&Req::get("/v1/me")).await.map_err(err)?;
                    self.error_ok(&r, ErrorCode::TokenInvalid)
                }
                .await
            );
            case!(
                self,
                "token.new_accepted",
                ep,
                Kind::Success,
                None,
                async {
                    let r = self.send(&Req::get("/v1/me")).await?;
                    self.json_ok(&r, 200, "MeResponse").map(drop)
                }
                .await
            );
        }
        self.expect_error(
            "token.rotate_without_token",
            ep,
            ErrorCode::TokenMissing,
            Req::new("POST", "/v1/token/rotate").anonymous(),
        )
        .await;
    }

    // ------------------------------------------------------------------ browse

    async fn browse(&mut self) {
        let ep = "GET /v1/shares/{share}/list";
        case!(
            self,
            "list.root",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!("/v1/shares/{SHARE}/list?path=")))
                    .await?;
                let v = self.json_ok(&r, 200, "ListResponse")?;
                let names: BTreeSet<&str> = v["entries"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|e| e["name"].as_str()).collect())
                    .unwrap_or_default();
                for want in ["images", "docs", "nested", UNICODE_DIR, BIG] {
                    ensure(names.contains(want), || {
                        format!("{want} missing from {names:?}")
                    })?;
                }
                for hidden in [".hidden.txt", "private"] {
                    ensure(!names.contains(hidden), || format!("{hidden} is listed"))?;
                }
                ensure(r.header("etag") == v["etag"].as_str(), || {
                    "ETag header differs from body".to_owned()
                })
            }
            .await
        );
        case!(
            self,
            "list.unicode_dir",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/list?path={}",
                        enc(UNICODE_DIR)
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "ListResponse")?;
                ensure(v["path"] == UNICODE_DIR, || {
                    format!("path is {}", v["path"])
                })?;
                let names: BTreeSet<&str> = v["entries"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|e| e["name"].as_str()).collect())
                    .unwrap_or_default();
                ensure(
                    names.contains(EMOJI_FILE) && names.contains("e\u{301}.txt"),
                    || format!("unicode names missing: {names:?}"),
                )
            }
            .await
        );
        case!(
            self,
            "list.paging_and_304",
            ep,
            Kind::Success,
            None,
            async {
                let base = format!("/v1/shares/{SHARE}/list?path=docs&limit=2&sort=name");
                let r1 = self.send(&Req::get(base.clone())).await?;
                let v1 = self.json_ok(&r1, 200, "ListResponse")?;
                let cursor = v1["next_cursor"]
                    .as_str()
                    .ok_or("no next_cursor with limit=2")?;
                let r2 = self
                    .send(&Req::get(format!("{base}&cursor={}", enc(cursor))))
                    .await?;
                let v2 = self.json_ok(&r2, 200, "ListResponse")?;
                let first: Vec<&Value> = v1["entries"]
                    .as_array()
                    .map(|a| a.iter().collect())
                    .unwrap_or_default();
                let second: Vec<&Value> = v2["entries"]
                    .as_array()
                    .map(|a| a.iter().collect())
                    .unwrap_or_default();
                ensure(first.len() == 2 && !second.is_empty(), || {
                    "pages have wrong sizes".to_owned()
                })?;
                ensure(
                    first
                        .iter()
                        .all(|e| !second.iter().any(|f| f["name"] == e["name"])),
                    || "pages overlap".to_owned(),
                )?;
                let etag = v1["etag"].as_str().unwrap_or_default();
                let r3 = self
                    .send(&Req::get(base).header("If-None-Match", etag))
                    .await?;
                Self::check_headers(&r3)?;
                ensure(r3.status == 304 && r3.body.is_empty(), || {
                    format!("If-None-Match gave {}", r3.describe())
                })
            }
            .await
        );
        let list = |share: &str, q: &str| Req::get(format!("/v1/shares/{share}/list?{q}"));
        self.expect_error(
            "list.share_not_found.unknown",
            ep,
            ErrorCode::ShareNotFound,
            list("shr_nope", "path="),
        )
        .await;
        self.expect_error(
            "list.share_not_found.no_grant",
            ep,
            ErrorCode::ShareNotFound,
            list(NOGRANT, "path="),
        )
        .await;
        self.expect_error(
            "list.bad_path.dotdot",
            ep,
            ErrorCode::BadPath,
            list(SHARE, "path=docs/../.."),
        )
        .await;
        self.expect_error(
            "list.bad_path.ads",
            ep,
            ErrorCode::BadPath,
            list(SHARE, "path=docs%3Astream"),
        )
        .await;
        self.expect_error(
            "list.not_found.missing",
            ep,
            ErrorCode::NotFound,
            list(SHARE, "path=no-such-dir"),
        )
        .await;
        self.expect_error(
            "list.not_found.hidden_pattern",
            ep,
            ErrorCode::NotFound,
            list(SHARE, "path=private"),
        )
        .await;
        self.expect_error(
            "list.bad_request.cursor",
            ep,
            ErrorCode::BadRequest,
            list(SHARE, "path=docs&cursor=garbage"),
        )
        .await;
        self.expect_error(
            "list.bad_request.sort",
            ep,
            ErrorCode::BadRequest,
            list(SHARE, "path=&sort=colour"),
        )
        .await;
        if self.opts.locked {
            self.expect_error(
                "list.forbidden.acl",
                ep,
                ErrorCode::Forbidden,
                list(SHARE, &format!("path={LOCKED_DIR}")),
            )
            .await;
        } else {
            self.not_reachable(
                "list.forbidden.acl",
                ep,
                Kind::Error,
                Some(ErrorCode::Forbidden),
                "the fixture's deny ACE could not be set (icacls failed)",
            );
        }

        let ep = "GET /v1/shares/{share}/stat";
        case!(
            self,
            "stat.image_meta",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/stat?path={}&meta=1",
                        enc(PHOTO)
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "Entry")?;
                ensure(v["name"] == "photo.bmp" && v["kind"] == "file", || {
                    format!("entry {v}")
                })
            }
            .await
        );
        case!(
            self,
            "stat.pdf_meta",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/stat?path={}&meta=1",
                        enc(PDF)
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "Entry")?;
                match v.pointer("/meta/pdf/pages") {
                    Some(p) => ensure(*p == 3, || format!("pdf pages {p}")),
                    None => Ok(()),
                }
            }
            .await
        );
        self.expect_error(
            "stat.not_found",
            ep,
            ErrorCode::NotFound,
            Req::get(format!("/v1/shares/{SHARE}/stat?path=nope.txt")),
        )
        .await;
        self.expect_error(
            "stat.share_not_found",
            ep,
            ErrorCode::ShareNotFound,
            Req::get(format!("/v1/shares/{NOGRANT}/stat?path=a.txt")),
        )
        .await;
        self.expect_error(
            "stat.bad_path.trailing_dot",
            ep,
            ErrorCode::BadPath,
            Req::get(format!(
                "/v1/shares/{SHARE}/stat?path={}",
                enc("docs/notes.txt.")
            )),
        )
        .await;
        self.expect_error(
            "stat.bad_path.device_name",
            ep,
            ErrorCode::BadPath,
            Req::get(format!("/v1/shares/{SHARE}/stat?path=docs/CON")),
        )
        .await;

        let ep = "GET /v1/shares/{share}/search";
        case!(
            self,
            "search.hits",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!("/v1/shares/{SHARE}/search?q=DEEP&path=")))
                    .await?;
                let v = self.json_ok(&r, 200, "SearchResponse")?;
                let found = v["entries"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|e| e["path"] == DEEP));
                ensure(found, || format!("{DEEP} not found case-insensitively"))?;
                ensure(matches!(v["via"].as_str(), Some("index" | "walk")), || {
                    format!("via is {} (index or walk)", v["via"])
                })
            }
            .await
        );
        // WP-66: the share says how it searches, `scope=content` is accepted everywhere (a share
        // searched by the walk answers it as a name search and says so), and a share that is not
        // indexed never claims `via: index`. The fixture lives in a temporary folder, which the
        // Windows Search service normally does not crawl, but either answer is valid here.
        case!(
            self,
            "search.scope_and_via",
            ep,
            Kind::Success,
            None,
            async {
                let r = self.send(&Req::get("/v1/shares")).await?;
                let shares = self.json_ok(&r, 200, "SharesResponse")?;
                let mine = shares["shares"]
                    .as_array()
                    .and_then(|a| a.iter().find(|s| s["id"] == SHARE))
                    .cloned()
                    .ok_or_else(|| format!("{SHARE} not in /shares"))?;
                let indexed = mine["search"] == "indexed";
                ensure(indexed || mine["search"] == true, || {
                    format!("share.search is {} (true or \"indexed\")", mine["search"])
                })?;
                let r = self.send(&Req::get("/v1/me")).await?;
                let me = self.json_ok(&r, 200, "MeResponse")?;
                if indexed {
                    ensure(me["capabilities"]["search"] == "indexed", || {
                        "a share is indexed but capabilities.search is not".to_owned()
                    })?;
                }
                for scope in ["name", "content"] {
                    let r = self
                        .send(&Req::get(format!(
                            "/v1/shares/{SHARE}/search?q=DEEP&scope={scope}"
                        )))
                        .await?;
                    let v = self.json_ok(&r, 200, "SearchResponse")?;
                    let via = v["via"].as_str().unwrap_or("");
                    ensure(indexed || via == "walk", || {
                        format!("scope={scope}: via {via} on a share that is not indexed")
                    })?;
                    if scope == "name" {
                        ensure(
                            v["entries"]
                                .as_array()
                                .is_some_and(|a| a.iter().any(|e| e["path"] == DEEP)),
                            || format!("{DEEP} not found with scope=name"),
                        )?;
                    }
                }
                Ok(())
            }
            .await
        );
        self.expect_error(
            "search.bad_request.scope",
            ep,
            ErrorCode::BadRequest,
            Req::get(format!("/v1/shares/{SHARE}/search?q=a&scope=everything")),
        )
        .await;
        self.expect_error(
            "search.bad_request.cursor",
            ep,
            ErrorCode::BadRequest,
            Req::get(format!(
                "/v1/shares/{SHARE}/search?q=a&cursor=i1.0123456789abcdef.zz"
            )),
        )
        .await;
        case!(
            self,
            "search.unicode",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/search?q={}",
                        enc("\u{1f600}")
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "SearchResponse")?;
                ensure(
                    v["entries"].as_array().is_some_and(|a| !a.is_empty()),
                    || "emoji search found nothing".to_owned(),
                )
            }
            .await
        );
        self.expect_error(
            "search.bad_request.no_q",
            ep,
            ErrorCode::BadRequest,
            Req::get(format!("/v1/shares/{SHARE}/search?path=")),
        )
        .await;
        self.expect_error(
            "search.share_not_found",
            ep,
            ErrorCode::ShareNotFound,
            Req::get("/v1/shares/shr_nope/search?q=a"),
        )
        .await;

        let ep = "GET /v1/recent";
        case!(
            self,
            "recent.files",
            ep,
            Kind::Success,
            None,
            async {
                let r = self.send(&Req::get("/v1/recent?limit=50")).await?;
                let v = self.json_ok(&r, 200, "RecentResponse")?;
                ensure(
                    v["entries"].as_array().is_some_and(|a| !a.is_empty()),
                    || "no recent files".to_owned(),
                )
            }
            .await
        );
        self.expect_error(
            "recent.bad_request.since",
            ep,
            ErrorCode::BadRequest,
            Req::get("/v1/recent?since=yesterday"),
        )
        .await;
        self.expect_error(
            "recent.token_missing",
            ep,
            ErrorCode::TokenMissing,
            Req::get("/v1/recent").anonymous(),
        )
        .await;
    }

    // ------------------------------------------------------------------ previews

    async fn previews(&mut self) {
        let wait = Duration::from_secs(60);
        let ep = "GET /v1/shares/{share}/thumb";
        for (id, path) in [("thumb.bmp", PHOTO), ("thumb.png", PNG), ("thumb.pdf", PDF)] {
            let mut generating = false;
            case!(
                self,
                id,
                ep,
                Kind::Success,
                None,
                async {
                    let req = Req::get(format!(
                        "/v1/shares/{SHARE}/thumb?path={}&size=128",
                        enc(path)
                    ));
                    let (r, saw) = self.until_ready(&req, wait).await?;
                    generating = saw;
                    Self::check_headers(&r)?;
                    ensure(r.status == 200 && is_image(&r.body), || {
                        format!("thumbnail: {}", r.describe())
                    })?;
                    ensure(r.header("etag").is_some_and(|e| e.starts_with('"')), || {
                        "no strong ETag".to_owned()
                    })
                }
                .await
            );
            if generating {
                self.record(
                    &format!("{id}.generating"),
                    ep,
                    Kind::Error,
                    Some(ErrorCode::Generating),
                    Ok(()),
                    Instant::now(),
                );
            }
        }
        let thumb = |share: &str, path: &str, extra: &str| {
            Req::get(format!(
                "/v1/shares/{share}/thumb?path={}{extra}",
                enc(path)
            ))
        };
        self.expect_error(
            "thumb.unsupported_type",
            ep,
            ErrorCode::UnsupportedType,
            thumb(SHARE, BINARY, ""),
        )
        .await;
        self.expect_error(
            "thumb.unsupported_type.folder",
            ep,
            ErrorCode::UnsupportedType,
            thumb(SHARE, "docs", ""),
        )
        .await;
        self.expect_error(
            "thumb.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            thumb(LIMITED, "readme.txt", ""),
        )
        .await;
        self.expect_error(
            "thumb.bad_request.priority",
            ep,
            ErrorCode::BadRequest,
            thumb(SHARE, PHOTO, "&priority=now"),
        )
        .await;
        self.expect_error(
            "thumb.not_found",
            ep,
            ErrorCode::NotFound,
            thumb(SHARE, "images/none.bmp", ""),
        )
        .await;

        let ep = "GET /v1/shares/{share}/preview/info";
        case!(
            self,
            "preview_info.pdf",
            ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!("/v1/shares/{SHARE}/preview/info?path={}", enc(PDF)));
                let (r, _) = self.until_ready(&req, wait).await?;
                let v = self.json_ok(&r, 200, "PreviewInfo")?;
                ensure(v["kind"] == "pdf" && v["pages"] == 3, || {
                    format!("info {v}")
                })
            }
            .await
        );
        case!(
            self,
            "preview_info.text",
            ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!(
                    "/v1/shares/{SHARE}/preview/info?path={}",
                    enc(UTF16)
                ));
                let (r, _) = self.until_ready(&req, wait).await?;
                let v = self.json_ok(&r, 200, "PreviewInfo")?;
                ensure(v["kind"] == "text", || format!("info {v}"))
            }
            .await
        );
        case!(
            self,
            "preview_info.none",
            ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!(
                    "/v1/shares/{SHARE}/preview/info?path={}",
                    enc(BINARY)
                ));
                let r = self.send(&req).await?;
                let v = self.json_ok(&r, 200, "PreviewInfo")?;
                ensure(v["kind"] == "none", || format!("info {v}"))
            }
            .await
        );
        self.expect_error(
            "preview_info.unsupported_type.folder",
            ep,
            ErrorCode::UnsupportedType,
            Req::get(format!("/v1/shares/{SHARE}/preview/info?path=docs")),
        )
        .await;
        self.expect_error(
            "preview_info.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            Req::get(format!("/v1/shares/{LIMITED}/preview/info?path=readme.txt")),
        )
        .await;

        let ep = "GET /v1/shares/{share}/preview";
        let mut generating = false;
        case!(
            self,
            "preview.pdf_page_2",
            ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!(
                    "/v1/shares/{SHARE}/preview?path={}&page=2&width=600&format=jpeg",
                    enc(PDF)
                ));
                let (r, saw) = self.until_ready(&req, wait).await?;
                generating = saw;
                Self::check_headers(&r)?;
                ensure(r.status == 200 && r.body.starts_with(&[0xFF, 0xD8]), || {
                    format!("page: {}", r.describe())
                })
            }
            .await
        );
        if generating {
            self.record(
                "preview.generating",
                ep,
                Kind::Error,
                Some(ErrorCode::Generating),
                Ok(()),
                Instant::now(),
            );
        }
        case!(
            self,
            "preview.image",
            ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!("/v1/shares/{SHARE}/preview?path={}", enc(PHOTO)));
                let (r, _) = self.until_ready(&req, wait).await?;
                Self::check_headers(&r)?;
                ensure(r.status == 200 && is_image(&r.body), || {
                    format!("image preview: {}", r.describe())
                })
            }
            .await
        );
        let page = |q: &str| Req::get(format!("/v1/shares/{SHARE}/preview?{q}"));
        self.expect_error(
            "preview.unsupported_type",
            ep,
            ErrorCode::UnsupportedType,
            page(&format!("path={}", enc(BINARY))),
        )
        .await;
        self.expect_error(
            "preview.bad_request.format",
            ep,
            ErrorCode::BadRequest,
            page(&format!("path={}&format=gif", enc(PDF))),
        )
        .await;
        self.expect_error(
            "preview.bad_request.page",
            ep,
            ErrorCode::BadRequest,
            page(&format!("path={}&page=abc", enc(PDF))),
        )
        .await;
        // The renderer spec clamps widths over 2048; the protocol's error table lists too_large
        // for "requested preview dimension over limit". The host follows the renderer spec.
        case!(
            self,
            "preview.width_clamped",
            ep,
            Kind::Success,
            None,
            async {
                let req = page(&format!("path={}&width=100000&format=jpeg", enc(PDF)));
                let (r, _) = self.until_ready(&req, wait).await?;
                Self::check_headers(&r)?;
                ensure(r.status == 200 && r.body.starts_with(&[0xFF, 0xD8]), || {
                    format!("clamped page: {}", r.describe())
                })
            }
            .await
        );
        self.expect_error(
            "preview.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            Req::get(format!("/v1/shares/{LIMITED}/preview?path=readme.txt")),
        )
        .await;

        let ep = "GET /v1/shares/{share}/preview/text";
        case!(
            self,
            "preview_text.utf16",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/preview/text?path={}",
                        enc(UTF16)
                    )))
                    .await?;
                Self::check_headers(&r)?;
                ensure(r.status == 200, || format!("text: {}", r.describe()))?;
                ensure(
                    r.header("content-type")
                        .is_some_and(|c| c.starts_with("text/plain")),
                    || "not text/plain".to_owned(),
                )?;
                let enc_h = r
                    .header("x-gander-text-encoding")
                    .unwrap_or("")
                    .to_ascii_lowercase();
                ensure(enc_h.contains("16"), || {
                    format!("X-Gander-Text-Encoding is {enc_h:?}")
                })?;
                ensure(r.header("x-gander-text-total").is_some(), || {
                    "no X-Gander-Text-Total".to_owned()
                })?;
                ensure(String::from_utf8_lossy(&r.body) == UTF16_TEXT, || {
                    format!(
                        "decoded text differs: {:?}",
                        String::from_utf8_lossy(&r.body)
                    )
                })
            }
            .await
        );
        case!(
            self,
            "preview_text.offset_limit",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/preview/text?path={}&offset=0&limit=4",
                        enc(NOTES)
                    )))
                    .await?;
                Self::check_headers(&r)?;
                ensure(
                    r.status == 200 && r.body.len() <= 4 && !r.body.is_empty(),
                    || format!("text window: {}", r.describe()),
                )
            }
            .await
        );
        self.expect_error(
            "preview_text.unsupported_type.binary",
            ep,
            ErrorCode::UnsupportedType,
            Req::get(format!(
                "/v1/shares/{SHARE}/preview/text?path={}",
                enc(BINARY)
            )),
        )
        .await;
        self.expect_error(
            "preview_text.bad_request.limit",
            ep,
            ErrorCode::BadRequest,
            Req::get(format!(
                "/v1/shares/{SHARE}/preview/text?path={}&limit=0",
                enc(NOTES)
            )),
        )
        .await;
        self.expect_error(
            "preview_text.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            Req::get(format!("/v1/shares/{LIMITED}/preview/text?path=readme.txt")),
        )
        .await;

        self.preview_tiles(wait).await;
    }

    /// WP-67: the tile fields of `preview/info` and `GET /preview/tile`.
    async fn preview_tiles(&mut self, wait: Duration) {
        let info_ep = "GET /v1/shares/{share}/preview/info";
        case!(
            self,
            "preview_info.tiles.image",
            info_ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!(
                    "/v1/shares/{SHARE}/preview/info?path={}",
                    enc(LARGE)
                ));
                let (r, _) = self.until_ready(&req, wait).await?;
                let v = self.json_ok(&r, 200, "PreviewInfo")?;
                ensure(
                    v["kind"] == "image"
                        && v["width"] == 1400
                        && v["height"] == 1000
                        && v["tile_size"] == 512
                        && v["tile_levels"] == 3
                        && v["tile_pages"]
                            == serde_json::json!([{"width": 1400, "height": 1000, "levels": 3}]),
                    || format!("info {v}"),
                )
            }
            .await
        );
        case!(
            self,
            "preview_info.tiles.pdf",
            info_ep,
            Kind::Success,
            None,
            async {
                let req = Req::get(format!("/v1/shares/{SHARE}/preview/info?path={}", enc(PDF)));
                let (r, _) = self.until_ready(&req, wait).await?;
                let v = self.json_ok(&r, 200, "PreviewInfo")?;
                // Three A4 pages (595 x 842 pt) at 300 dpi are about 2479 x 3508 px: four levels each.
                let pages = v["tile_pages"].as_array().cloned().unwrap_or_default();
                ensure(
                    v["tile_size"] == 512
                        && pages.len() == 3
                        && pages.iter().all(|p| {
                            p["levels"] == 4
                                && p["width"].as_u64().is_some_and(|w| w.abs_diff(2479) <= 1)
                                && p["height"].as_u64().is_some_and(|h| h.abs_diff(3508) <= 1)
                        }),
                    || format!("info {v}"),
                )
            }
            .await
        );
        case!(
            self,
            "preview_info.tiles.absent_for_small_images",
            info_ep,
            Kind::Success,
            None,
            async {
                // 64 x 48 px fits in one tile: the base preview is already the whole picture.
                let req = Req::get(format!(
                    "/v1/shares/{SHARE}/preview/info?path={}",
                    enc(PHOTO)
                ));
                let (r, _) = self.until_ready(&req, wait).await?;
                let v = self.json_ok(&r, 200, "PreviewInfo")?;
                ensure(
                    v.get("tile_size").is_none()
                        && v.get("tile_levels").is_none()
                        && v.get("tile_pages").is_none(),
                    || format!("info {v}"),
                )
            }
            .await
        );

        let ep = "GET /v1/shares/{share}/preview/tile";
        let tile = |path: &str, q: &str| {
            Req::get(format!(
                "/v1/shares/{SHARE}/preview/tile?path={}&{q}",
                enc(path)
            ))
        };
        let mut generating = false;
        // (case id, path, query, expected tile size)
        for (id, path, q, size) in [
            (
                "preview_tile.image.whole",
                LARGE,
                "level=0&x=0&y=0",
                (350, 250),
            ),
            (
                "preview_tile.image.full",
                LARGE,
                "level=2&x=0&y=0",
                (512, 512),
            ),
            (
                "preview_tile.image.right_edge",
                LARGE,
                "level=2&x=2&y=0",
                (376, 512),
            ),
            (
                "preview_tile.image.corner",
                LARGE,
                "level=2&x=2&y=1",
                (376, 488),
            ),
            (
                "preview_tile.image.middle_level",
                LARGE,
                "level=1&x=1&y=0",
                (188, 500),
            ),
            (
                "preview_tile.pdf.page_2",
                PDF,
                "page=2&level=2&x=0&y=0",
                (512, 512),
            ),
        ] {
            case!(
                self,
                id,
                ep,
                Kind::Success,
                None,
                async {
                    let (r, saw) = self.until_ready(&tile(path, q), wait).await?;
                    generating |= saw;
                    Self::check_headers(&r)?;
                    ensure(r.status == 200, || format!("tile: {}", r.describe()))?;
                    ensure(r.header("content-type") == Some("image/webp"), || {
                        format!("content-type {:?}", r.header("content-type"))
                    })?;
                    ensure(
                        r.header("cache-control") == Some("private, max-age=31536000, immutable"),
                        || format!("cache-control {:?}", r.header("cache-control")),
                    )?;
                    ensure(r.header("etag").is_some_and(|e| e.starts_with('"')), || {
                        "tile without a strong ETag".to_owned()
                    })?;
                    let got = webp_size(&r.body);
                    ensure(got == Some(size), || {
                        format!("tile size {got:?}, want {size:?}")
                    })
                }
                .await
            );
        }
        if generating {
            self.record(
                "preview_tile.generating",
                ep,
                Kind::Error,
                Some(ErrorCode::Generating),
                Ok(()),
                Instant::now(),
            );
        }
        case!(
            self,
            "preview_tile.etag_and_not_modified",
            ep,
            Kind::Success,
            None,
            async {
                let req = tile(LARGE, "level=2&x=1&y=1");
                let (r, _) = self.until_ready(&req, wait).await?;
                let etag = r.header("etag").ok_or("no ETag")?.to_owned();
                let again = self
                    .send(&req.clone().header("If-None-Match", etag.clone()))
                    .await?;
                ensure(
                    again.status == 304 && again.header("etag") == Some(etag.as_str()),
                    || format!("revalidation: {}", again.describe()),
                )?;
                // Another tile has another ETag.
                let (other, _) = self
                    .until_ready(&tile(LARGE, "level=2&x=0&y=1"), wait)
                    .await?;
                ensure(other.header("etag") != Some(etag.as_str()), || {
                    "two tiles share an ETag".to_owned()
                })
            }
            .await
        );
        for (id, path, q) in [
            ("preview_tile.bad_request.missing_level", LARGE, "x=0&y=0"),
            ("preview_tile.bad_request.missing_x", LARGE, "level=0&y=0"),
            (
                "preview_tile.bad_request.not_a_number",
                LARGE,
                "level=a&x=0&y=0",
            ),
            (
                "preview_tile.bad_request.level_out_of_range",
                LARGE,
                "level=3&x=0&y=0",
            ),
            (
                "preview_tile.bad_request.column_out_of_range",
                LARGE,
                "level=2&x=3&y=0",
            ),
            (
                "preview_tile.bad_request.row_out_of_range",
                LARGE,
                "level=0&x=0&y=1",
            ),
            (
                "preview_tile.bad_request.page_out_of_range",
                PDF,
                "page=4&level=0&x=0&y=0",
            ),
            (
                "preview_tile.bad_request.image_has_one_page",
                LARGE,
                "page=2&level=0&x=0&y=0",
            ),
        ] {
            self.expect_error(id, ep, ErrorCode::BadRequest, tile(path, q))
                .await;
        }
        self.expect_error(
            "preview_tile.unsupported_type.text",
            ep,
            ErrorCode::UnsupportedType,
            tile(NOTES, "level=0&x=0&y=0"),
        )
        .await;
        self.expect_error(
            "preview_tile.unsupported_type.folder",
            ep,
            ErrorCode::UnsupportedType,
            Req::get(format!(
                "/v1/shares/{SHARE}/preview/tile?path=docs&level=0&x=0&y=0"
            )),
        )
        .await;
        self.expect_error(
            "preview_tile.not_found",
            ep,
            ErrorCode::NotFound,
            tile("images/none.bmp", "level=0&x=0&y=0"),
        )
        .await;
        self.expect_error(
            "preview_tile.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            Req::get(format!(
                "/v1/shares/{LIMITED}/preview/tile?path=readme.txt&level=0&x=0&y=0"
            )),
        )
        .await;
    }

    // ------------------------------------------------------------------ fetch

    async fn fetch(&mut self) {
        let ep = "GET /v1/shares/{share}/file";
        let file = |share: &str, path: &str| {
            Req::get(format!("/v1/shares/{share}/file?path={}", enc(path)))
        };
        case!(
            self,
            "file.full",
            ep,
            Kind::Success,
            None,
            async {
                let want = self.fixture(PDF)?;
                let r = self.send(&file(SHARE, PDF)).await?;
                Self::check_headers(&r)?;
                ensure(r.status == 200 && r.body == want, || {
                    format!("file: {}", r.describe())
                })?;
                ensure(r.header("accept-ranges") == Some("bytes"), || {
                    "no Accept-Ranges".to_owned()
                })?;
                ensure(r.header("etag").is_some_and(|e| e.starts_with('"')), || {
                    "no strong ETag".to_owned()
                })?;
                ensure(r.header("content-type") == Some("application/pdf"), || {
                    format!("content-type {:?}", r.header("content-type"))
                })?;
                ensure(
                    r.header("content-disposition")
                        .is_some_and(|d| d.contains("filename*=UTF-8''")),
                    || "no RFC 5987 Content-Disposition".to_owned(),
                )
            }
            .await
        );
        case!(
            self,
            "file.unicode_name",
            ep,
            Kind::Success,
            None,
            async {
                let rel = format!("{UNICODE_DIR}/{EMOJI_FILE}");
                let want = self.fixture(&rel)?;
                let r = self.send(&file(SHARE, &rel)).await?;
                ensure(r.status == 200 && r.body == want, || {
                    format!("unicode file: {}", r.describe())
                })
            }
            .await
        );
        case!(
            self,
            "file.head",
            "HEAD /v1/shares/{share}/file",
            Kind::Success,
            None,
            async {
                let len = self.fixture(PDF)?.len();
                let mut req = file(SHARE, PDF);
                req.method = "HEAD".to_owned();
                let r = self.send(&req).await?;
                Self::check_headers(&r)?;
                ensure(r.status == 200 && r.body.is_empty(), || {
                    format!("HEAD: {}", r.describe())
                })?;
                ensure(
                    r.header("content-length") == Some(len.to_string().as_str()),
                    || {
                        format!(
                            "Content-Length {:?}, file has {len}",
                            r.header("content-length")
                        )
                    },
                )
            }
            .await
        );
        case!(
            self,
            "file.range_206",
            ep,
            Kind::Success,
            None,
            async {
                let want = self.fixture(PDF)?;
                let r = self
                    .send(&file(SHARE, PDF).header("Range", "bytes=5-14"))
                    .await?;
                Self::check_headers(&r)?;
                ensure(r.status == 206 && r.body == want[5..15], || {
                    format!("range: {}", r.describe())
                })?;
                ensure(
                    r.header("content-range")
                        == Some(format!("bytes 5-14/{}", want.len()).as_str()),
                    || format!("Content-Range {:?}", r.header("content-range")),
                )
            }
            .await
        );
        case!(
            self,
            "file.resume_if_range",
            ep,
            Kind::Success,
            None,
            async {
                let want = self.fixture(RESUME)?;
                let head = self
                    .send(&file(SHARE, RESUME).header("Range", "bytes=0-99"))
                    .await?;
                let etag = head.header("etag").ok_or("no ETag")?.to_owned();
                let r = self
                    .send(
                        &file(SHARE, RESUME)
                            .header("Range", "bytes=100-")
                            .header("If-Range", etag),
                    )
                    .await?;
                ensure(r.status == 206 && r.body == want[100..], || {
                    format!("resume: {}", r.describe())
                })
            }
            .await
        );
        case!(
            self,
            "file.file_changed",
            ep,
            Kind::Error,
            Some(ErrorCode::FileChanged),
            async {
                let first = self
                    .send(&file(SHARE, RESUME).header("Range", "bytes=0-99"))
                    .await?;
                let etag = first.header("etag").ok_or("no ETag")?.to_owned();
                let path = self.root().join(RESUME);
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .map_err(err)?;
                f.write_all(b"changed").map_err(err)?;
                drop(f);
                let r = self
                    .send(
                        &file(SHARE, RESUME)
                            .header("Range", "bytes=100-")
                            .header("If-Range", etag),
                    )
                    .await?;
                self.error_ok(&r, ErrorCode::FileChanged)
            }
            .await
        );
        self.expect_error(
            "file.file_changed.if_match",
            ep,
            ErrorCode::FileChanged,
            file(SHARE, PDF).header("If-Match", "\"stale\""),
        )
        .await;
        self.expect_error(
            "file.range_not_satisfiable.past_end",
            ep,
            ErrorCode::RangeNotSatisfiable,
            file(SHARE, PDF).header("Range", "bytes=999999999-"),
        )
        .await;
        self.expect_error(
            "file.range_not_satisfiable.multipart",
            ep,
            ErrorCode::RangeNotSatisfiable,
            file(SHARE, PDF).header("Range", "bytes=0-1,4-5"),
        )
        .await;
        self.expect_error(
            "file.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            file(LIMITED, "readme.txt"),
        )
        .await;
        self.expect_error(
            "file.bad_path.folder",
            ep,
            ErrorCode::BadPath,
            file(SHARE, "docs"),
        )
        .await;
        self.expect_error(
            "file.bad_path.backslash",
            ep,
            ErrorCode::BadPath,
            file(SHARE, "docs\\notes.txt"),
        )
        .await;
        // Path-resolution spec: hidden/system entries are only omitted from listings; only
        // `hidden_patterns` make an entry unresolvable.
        case!(
            self,
            "file.hidden_attribute_resolvable",
            ep,
            Kind::Success,
            None,
            async {
                let r = self.send(&file(SHARE, ".hidden.txt")).await?;
                ensure(r.status == 200, || {
                    format!("hidden-attribute file: {}", r.describe())
                })
            }
            .await
        );
        self.expect_error(
            "file.not_found.hidden_pattern",
            ep,
            ErrorCode::NotFound,
            file(SHARE, "private/secret.txt"),
        )
        .await;
        self.expect_error(
            "file.token_invalid",
            ep,
            ErrorCode::TokenInvalid,
            file(SHARE, PDF)
                .anonymous()
                .header("Authorization", "Bearer x"),
        )
        .await;

        let ep = "GET /v1/shares/{share}/hash";
        case!(
            self,
            "hash.sha256",
            ep,
            Kind::Success,
            None,
            async {
                let want = sha256_hex(&self.fixture(NOTES)?);
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/hash?path={}&algo=sha256",
                        enc(NOTES)
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "HashResponse")?;
                ensure(v["hex"] == want.as_str(), || {
                    format!("sha256 {} != {want}", v["hex"])
                })
            }
            .await
        );
        case!(
            self,
            "hash.blake3",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/hash?path={}&algo=blake3",
                        enc(PDF)
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "HashResponse")?;
                ensure(v["hex"].as_str().is_some_and(|h| h.len() == 64), || {
                    format!("blake3 {}", v["hex"])
                })
            }
            .await
        );
        case!(
            self,
            "hash.generating_then_ready",
            ep,
            Kind::Error,
            Some(ErrorCode::Generating),
            async {
                let want = sha256_hex(&self.fixture(BIG)?);
                let req = Req::get(format!("/v1/shares/{SHARE}/hash?path={BIG}&algo=sha256"));
                let (r, saw) = self.until_ready(&req, Duration::from_secs(60)).await?;
                ensure(saw, || {
                    "a file over 16 MiB was hashed inline (no 503 generating)".to_owned()
                })?;
                let v = self.json_ok(&r, 200, "HashResponse")?;
                ensure(v["hex"] == want.as_str(), || {
                    "big file sha256 differs".to_owned()
                })
            }
            .await
        );
        self.expect_error(
            "hash.bad_request.algo",
            ep,
            ErrorCode::BadRequest,
            Req::get(format!(
                "/v1/shares/{SHARE}/hash?path={}&algo=md5",
                enc(NOTES)
            )),
        )
        .await;
        self.expect_error(
            "hash.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            Req::get(format!(
                "/v1/shares/{LIMITED}/hash?path=readme.txt&algo=sha256"
            )),
        )
        .await;
    }

    // ------------------------------------------------------------------ zip

    /// What `unzip` of `bytes` holds (folders map to `None`), every CRC checked by the reader.
    fn unzip(bytes: &[u8]) -> Result<std::collections::BTreeMap<String, Option<Vec<u8>>>, String> {
        use std::io::Read as _;
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|e| format!("not a zip: {e}"))?;
        let mut out = std::collections::BTreeMap::new();
        for i in 0..ar.len() {
            let mut f = ar.by_index(i).map_err(err)?;
            let name = f.name().map_err(err)?.into_owned();
            if f.is_dir() {
                out.insert(name.trim_end_matches('/').to_owned(), None);
            } else {
                let mut v = Vec::new();
                f.read_to_end(&mut v).map_err(err)?;
                out.insert(name, Some(v));
            }
        }
        Ok(out)
    }

    /// The tree of `dir` on disk as `top/rel` -> content (`top` itself included).
    fn disk_tree(
        dir: &Path,
        top: &str,
    ) -> Result<std::collections::BTreeMap<String, Option<Vec<u8>>>, String> {
        fn walk(
            dir: &Path,
            prefix: &str,
            out: &mut std::collections::BTreeMap<String, Option<Vec<u8>>>,
        ) -> Result<(), String> {
            for e in std::fs::read_dir(dir).map_err(err)? {
                let e = e.map_err(err)?;
                let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
                if e.file_type().map_err(err)?.is_dir() {
                    out.insert(name.clone(), None);
                    walk(&e.path(), &name, out)?;
                } else {
                    out.insert(name, Some(std::fs::read(e.path()).map_err(err)?));
                }
            }
            Ok(())
        }
        let mut out = std::collections::BTreeMap::new();
        out.insert(top.to_owned(), None);
        walk(dir, top, &mut out)?;
        Ok(out)
    }

    async fn zip(&mut self) {
        let ep = "GET /v1/zip";
        let zip =
            |share: &str, path: &str| Req::get(format!("/v1/zip?share={share}&path={}", enc(path)));
        // A folder: the archive unzips to exactly the tree on disk.
        for (id, dir, top) in [
            ("zip.docs", "docs", "docs"),
            ("zip.nested", "nested", "nested"),
            ("zip.unicode_names", UNICODE_DIR, UNICODE_DIR),
        ] {
            case!(
                self,
                id,
                ep,
                Kind::Success,
                None,
                async {
                    let r = self.send(&zip(SHARE, dir)).await?;
                    Self::check_headers(&r)?;
                    ensure(r.status == 200, || format!("zip: {}", r.describe()))?;
                    ensure(r.header("content-type") == Some("application/zip"), || {
                        format!("content-type {:?}", r.header("content-type"))
                    })?;
                    ensure(r.header("accept-ranges") == Some("none"), || {
                        "a zip must not advertise ranges".to_owned()
                    })?;
                    ensure(
                        r.header("content-disposition")
                            .is_some_and(|d| d.contains("filename*=UTF-8''")),
                        || "no RFC 5987 Content-Disposition".to_owned(),
                    )?;
                    let want = Self::disk_tree(&self.root().join(dir), top)?;
                    let got = Self::unzip(&r.body)?;
                    ensure(got.keys().eq(want.keys()), || {
                        format!("entries differ: {:?} vs {:?}", got.keys(), want.keys())
                    })?;
                    ensure(got == want, || {
                        "file contents differ from the tree".to_owned()
                    })?;
                    let bytes: u64 = want.values().flatten().map(|v| v.len() as u64).sum();
                    let entries = want.len() as u64 - 1;
                    ensure(
                        r.header("x-gander-zip-entries") == Some(entries.to_string().as_str())
                            && r.header("x-gander-zip-bytes") == Some(bytes.to_string().as_str()),
                        || format!("totals headers vs {entries} entries, {bytes} bytes"),
                    )
                }
                .await
            );
        }
        case!(
            self,
            "zip.dry_run",
            ep,
            Kind::Success,
            None,
            async {
                let want = Self::disk_tree(&self.root().join("docs"), "docs")?;
                let bytes: u64 = want.values().flatten().map(|v| v.len() as u64).sum();
                let entries = want.len() as u64 - 1;
                let r = self
                    .send(&Req::get(format!(
                        "/v1/zip?share={SHARE}&path=docs&dry_run=1"
                    )))
                    .await?;
                let v = self.json_ok(&r, 200, "ZipPlan")?;
                ensure(v["entries"] == entries && v["bytes"] == bytes, || {
                    format!("dry run {v} vs {entries} entries, {bytes} bytes")
                })
            }
            .await
        );
        case!(
            self,
            "zip.head",
            "HEAD /v1/zip",
            Kind::Success,
            None,
            async {
                let mut req = zip(SHARE, "docs");
                req.method = "HEAD".to_owned();
                let r = self.send(&req).await?;
                Self::check_headers(&r)?;
                ensure(
                    r.status == 200
                        && r.body.is_empty()
                        && r.header("content-type") == Some("application/zip")
                        && r.header("x-gander-zip-bytes").is_some(),
                    || format!("HEAD: {}", r.describe()),
                )
            }
            .await
        );
        case!(
            self,
            "zip.range_ignored",
            ep,
            Kind::Success,
            None,
            async {
                let r = self
                    .send(&zip(SHARE, "docs").header("Range", "bytes=10-20"))
                    .await?;
                ensure(
                    r.status == 200
                        && r.header("content-range").is_none()
                        && r.body.starts_with(b"PK"),
                    || format!("a Range must be ignored, got {}", r.describe()),
                )?;
                Self::unzip(&r.body).map(|_| ())
            }
            .await
        );
        // Over the limit (config [zip] max_mib = 16; big.bin is 24 MiB): 413 before any streaming.
        self.expect_error("zip.too_large", ep, ErrorCode::TooLarge, zip(SHARE, ""))
            .await;
        self.expect_error(
            "zip.too_large.dry_run",
            ep,
            ErrorCode::TooLarge,
            Req::get(format!("/v1/zip?share={SHARE}&dry_run=1")),
        )
        .await;
        case!(
            self,
            "zip.capabilities",
            "GET /v1/me",
            Kind::Success,
            None,
            async {
                let r = self.send(&Req::get("/v1/me")).await?;
                let v = self.json_ok(&r, 200, "MeResponse")?;
                let z = &v["capabilities"]["fetch"]["zip"];
                ensure(
                    z["max_bytes"] == 16u64 * 1024 * 1024 && z["max_entries"] == 20_000,
                    || format!("capabilities.fetch.zip is {z}"),
                )
            }
            .await
        );
        self.expect_error(
            "zip.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            zip(LIMITED, ""),
        )
        .await;
        self.expect_error(
            "zip.share_not_found",
            ep,
            ErrorCode::ShareNotFound,
            zip(NOGRANT, ""),
        )
        .await;
        self.expect_error(
            "zip.bad_request.no_share",
            ep,
            ErrorCode::BadRequest,
            Req::get("/v1/zip"),
        )
        .await;
        self.expect_error(
            "zip.bad_path.file",
            ep,
            ErrorCode::BadPath,
            zip(SHARE, NOTES),
        )
        .await;
        self.expect_error(
            "zip.bad_path.dot_dot",
            ep,
            ErrorCode::BadPath,
            zip(SHARE, "docs/.."),
        )
        .await;
        self.expect_error(
            "zip.not_found.hidden_pattern",
            ep,
            ErrorCode::NotFound,
            zip(SHARE, "private"),
        )
        .await;
        self.expect_error(
            "zip.not_found",
            ep,
            ErrorCode::NotFound,
            zip(SHARE, "docs/missing"),
        )
        .await;
        self.expect_error(
            "zip.token_invalid",
            ep,
            ErrorCode::TokenInvalid,
            zip(SHARE, "docs")
                .anonymous()
                .header("Authorization", "Bearer x"),
        )
        .await;
    }

    // ------------------------------------------------------------------ stream

    async fn stream(&mut self) {
        let ep = "POST /v1/stream";
        let body = |share: &str, path: &str, app: Value| {
            Req::new("POST", "/v1/stream")
                .json(&json!({ "share": share, "path": path, "app": app, "viewport": viewport() }))
        };
        self.stream_enabled = match self.send(&Req::get("/v1/me")).await {
            Ok(r) => r
                .json()
                .is_some_and(|v| v.pointer("/capabilities/stream/enabled") == Some(&json!(true))),
            Err(_) => false,
        };
        self.expect_error(
            "stream.stream_refused.exe",
            ep,
            ErrorCode::StreamRefused,
            body(SHARE, TOOL, Value::Null),
        )
        .await;
        self.expect_error(
            "stream.app_unavailable.unknown_app",
            ep,
            ErrorCode::AppUnavailable,
            body(SHARE, PDF, json!("no-such-app")),
        )
        .await;
        self.expect_error(
            "stream.permission_denied",
            ep,
            ErrorCode::PermissionDenied,
            body(LIMITED, "readme.txt", Value::Null),
        )
        .await;
        self.expect_error(
            "stream.share_not_found",
            ep,
            ErrorCode::ShareNotFound,
            body("shr_nope", "a.pdf", Value::Null),
        )
        .await;
        self.expect_error(
            "stream.bad_path.folder",
            ep,
            ErrorCode::BadPath,
            body(SHARE, "docs", Value::Null),
        )
        .await;
        self.expect_error(
            "stream.not_found",
            ep,
            ErrorCode::NotFound,
            body(SHARE, "docs/none.pdf", Value::Null),
        )
        .await;
        self.expect_error(
            "stream.bad_request.body",
            ep,
            ErrorCode::BadRequest,
            Req::new("POST", "/v1/stream").raw_body(b"{}".to_vec()),
        )
        .await;
        self.expect_error(
            "stream.too_large.body",
            ep,
            ErrorCode::TooLarge,
            Req::new("POST", "/v1/stream")
                .header("Content-Type", "application/json")
                .raw_body(vec![b' '; 70 * 1024]),
        )
        .await;
        if !self.stream_enabled {
            self.stream_phase1(ep, body).await;
            return;
        }
        self.not_reachable(
            "stream.stream_unavailable",
            ep,
            Kind::Error,
            Some(ErrorCode::StreamUnavailable),
            "the test host runs Stream mode (capabilities.stream.enabled = true)",
        );
        self.expect_error(
            "stream.resume.unknown",
            ep,
            ErrorCode::NotFound,
            Req::new("POST", "/v1/stream").json(&json!({
                "share": SHARE, "path": STREAM_OK, "app": TEST_APP, "viewport": viewport(),
                "resume": "ses_none"
            })),
        )
        .await;
        let mut ws = match self.ws_connect().await {
            Ok(ws) => Some(ws),
            Err(e) => {
                eprintln!("stream: no events socket ({e}); session cases will fail");
                None
            }
        };
        // Real-lock check: a computer that is really locked refuses every session (the simulated
        // lock can only add a lock, never hide the real one).
        let mut locked_for_real = false;
        if let Some(ws) = ws.as_mut()
            && let Ok(v) = wait_for(ws, |v| v["type"] == "host.state", Duration::from_secs(5)).await
        {
            locked_for_real = v["locked"] == true;
        }
        if locked_for_real {
            eprintln!("stream: this computer is locked; session cases are not reachable");
            self.expect_error(
                "stream.host_locked",
                ep,
                ErrorCode::HostLocked,
                body(SHARE, STREAM_OK, json!(TEST_APP)),
            )
            .await;
            self.not_reachable(
                "stream.host_unattended",
                ep,
                Kind::Error,
                Some(ErrorCode::HostUnattended),
                "the computer is locked, which is checked first (host_locked)",
            );
        } else {
            // The policy asks (testwin is approved for .gtw only) and the test host reports
            // nobody at the computer: nothing launches.
            self.expect_error(
                "stream.host_unattended",
                ep,
                ErrorCode::HostUnattended,
                body(SHARE, STREAM_ASK, json!(TEST_APP)),
            )
            .await;
        }
        let delete_ep = "DELETE /v1/stream/{session}";
        if locked_for_real {
            for (id, endpoint, kind, code) in [
                ("stream.created", ep, Kind::Success, None),
                (
                    "stream.stream_busy",
                    ep,
                    Kind::Error,
                    Some(ErrorCode::StreamBusy),
                ),
                ("stream.delete", delete_ep, Kind::Success, None),
                (
                    "stream.lock_ends_session",
                    "GET /v1/events",
                    Kind::Event,
                    Some(ErrorCode::HostLocked),
                ),
            ] {
                self.not_reachable(
                    id,
                    endpoint,
                    kind,
                    code,
                    "the computer is locked: Stream refuses every session (fail closed)",
                );
            }
        } else {
            let mut session = None;
            case!(
                self,
                "stream.created",
                ep,
                Kind::Success,
                None,
                async {
                    let ws = ws.as_mut().ok_or("no events socket")?;
                    let r = self.send(&body(SHARE, STREAM_OK, json!(TEST_APP))).await?;
                    let v = self.json_ok(&r, 201, "StreamCreated")?;
                    let id = v["session"].as_str().ok_or("no session id")?.to_owned();
                    ensure(
                        v["state"] == "requested" && v["needs_approval"] == false,
                        || format!("201 body {v}"),
                    )?;
                    session = Some(id.clone());
                    // window_found (validated), then the engine's offer.
                    let found = wait_for(
                        ws,
                        |e| {
                            e["type"] == "stream.state"
                                && e["session"] == id.as_str()
                                && (e["state"] == "window_found" || e["state"] == "ended")
                        },
                        STREAM_WAIT,
                    )
                    .await?;
                    self.validate("ServerEvent", &found)?;
                    ensure(found["state"] == "window_found", || {
                        format!("ended early: {found}")
                    })?;
                    let offer = wait_for(
                        ws,
                        |e| e["type"] == "stream.offer" && e["session"] == id.as_str(),
                        STREAM_WAIT,
                    )
                    .await?;
                    self.validate("ServerEvent", &offer)?;
                    ensure(
                        offer["sdp"].as_str().is_some_and(|s| s.contains("H264")),
                        || "offer without H.264".to_owned(),
                    )
                }
                .await
            );
            case!(
                self,
                "stream.stream_busy",
                ep,
                Kind::Error,
                Some(ErrorCode::StreamBusy),
                async {
                    ensure(session.is_some(), || {
                        "no session to be busy with".to_owned()
                    })?;
                    let r = self.send(&body(SHARE, STREAM_OK, json!(TEST_APP))).await?;
                    self.error_ok(&r, ErrorCode::StreamBusy)
                }
                .await
            );
            case!(
                self,
                "stream.delete",
                delete_ep,
                Kind::Success,
                None,
                async {
                    let ws = ws.as_mut().ok_or("no events socket")?;
                    let id = session.clone().ok_or("no session to delete")?;
                    let r = self
                        .send(&Req::new(
                            "DELETE",
                            format!("/v1/stream/{id}?close_app=true"),
                        ))
                        .await?;
                    Self::check_headers(&r)?;
                    ensure(r.status == 204, || {
                        format!("expected 204, got {}", r.describe())
                    })?;
                    let end = wait_for(
                        ws,
                        |e| {
                            e["type"] == "stream.state"
                                && e["session"] == id.as_str()
                                && e["state"] == "ended"
                        },
                        Duration::from_secs(10),
                    )
                    .await?;
                    self.validate("ServerEvent", &end)?;
                    ensure(end["reason"] == "client_closed", || format!("ended {end}"))?;
                    // The slot is free again: the same request is not busy.
                    let r = self.send(&body(SHARE, STREAM_ASK, json!(TEST_APP))).await?;
                    self.error_ok(&r, ErrorCode::HostUnattended)
                }
                .await
            );
            self.stream_lock(ep, &mut ws).await;
        }
        self.expect_error(
            "stream.delete.unknown",
            delete_ep,
            ErrorCode::NotFound,
            Req::new("DELETE", "/v1/stream/ses_none"),
        )
        .await;
    }

    /// Writes one command to the test host (the suite's stdout is the host's stdin).
    fn host_command(line: &str) -> Result<(), String> {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(err)
    }

    /// `host_locked` through the test host's simulated lock: refused before a session exists,
    /// and a running session ends with `error host_locked` + `stream.state ended`.
    async fn stream_lock(&mut self, ep: &str, ws: &mut Option<Ws>) {
        let body = |path: &str| {
            Req::new("POST", "/v1/stream").json(
                &json!({ "share": SHARE, "path": path, "app": TEST_APP, "viewport": viewport() }),
            )
        };
        if !self.opts.host_commands {
            for (id, endpoint, kind) in [
                ("stream.host_locked", ep, Kind::Error),
                ("stream.lock_ends_session", "GET /v1/events", Kind::Event),
            ] {
                self.not_reachable(
                    id,
                    endpoint,
                    kind,
                    Some(ErrorCode::HostLocked),
                    "the simulated lock needs the test host's stdin (--host-commands)",
                );
            }
            return;
        }
        case!(
            self,
            "stream.host_locked",
            ep,
            Kind::Error,
            Some(ErrorCode::HostLocked),
            async {
                let ws = ws.as_mut().ok_or("no events socket")?;
                Self::host_command("lock")?;
                wait_for(
                    ws,
                    |v| v["type"] == "host.state" && v["locked"] == true,
                    Duration::from_secs(10),
                )
                .await?;
                let r = self.send(&body(STREAM_OK)).await?;
                let result = self.error_ok(&r, ErrorCode::HostLocked);
                Self::host_command("unlock")?;
                wait_for(
                    ws,
                    |v| v["type"] == "host.state" && v["locked"] == false,
                    Duration::from_secs(10),
                )
                .await?;
                result
            }
            .await
        );
        case!(
            self,
            "stream.lock_ends_session",
            "GET /v1/events",
            Kind::Event,
            Some(ErrorCode::HostLocked),
            async {
                let ws = ws.as_mut().ok_or("no events socket")?;
                let r = self.send(&body(STREAM_OK)).await?;
                let v = self.json_ok(&r, 201, "StreamCreated")?;
                let id = v["session"].as_str().ok_or("no session id")?.to_owned();
                wait_for(
                    ws,
                    |e| e["type"] == "stream.offer" && e["session"] == id.as_str(),
                    STREAM_WAIT,
                )
                .await?;
                Self::host_command("lock")?;
                let e = wait_for(
                    ws,
                    |e| e["type"] == "error" && e["session"] == id.as_str(),
                    Duration::from_secs(10),
                )
                .await;
                let end = wait_for(
                    ws,
                    |e| {
                        e["type"] == "stream.state"
                            && e["session"] == id.as_str()
                            && e["state"] == "ended"
                    },
                    Duration::from_secs(10),
                )
                .await;
                Self::host_command("unlock")?;
                let e = e?;
                self.validate("ServerEvent", &e)?;
                ensure(e["code"] == "host_locked", || format!("error event {e}"))?;
                let end = end?;
                self.validate("ServerEvent", &end)?;
                ensure(end["reason"] == "error", || format!("ended {end}"))?;
                wait_for(
                    ws,
                    |v| v["type"] == "host.state" && v["locked"] == false,
                    Duration::from_secs(10),
                )
                .await?;
                Ok(())
            }
            .await
        );
    }

    /// The Phase 1 answers (a host without Stream mode).
    async fn stream_phase1(&mut self, ep: &str, body: impl Fn(&str, &str, Value) -> Req) {
        self.not_reachable(
            "stream.created",
            ep,
            Kind::Success,
            None,
            "this host has no Stream mode (capabilities.stream.enabled = false)",
        );
        // Which file types pass the policy depends on the machine's associations: try a few.
        case!(
            self,
            "stream.stream_unavailable",
            ep,
            Kind::Error,
            Some(ErrorCode::StreamUnavailable),
            async {
                let mut seen = Vec::new();
                for path in [PDF, PHOTO, PNG, NOTES, UTF16] {
                    let r = self.send(&body(SHARE, path, Value::Null)).await?;
                    if r.status == 503 {
                        let v = self.json_ok(&r, 503, "ErrorBody")?;
                        ensure(v["error"]["code"] == "stream_unavailable", || {
                            format!("503 {v}")
                        })?;
                        return Ok(());
                    }
                    seen.push(format!("{path}: {}", r.describe()));
                }
                Err(format!(
                    "no fixture type passed the launch policy: {seen:?}"
                ))
            }
            .await
        );
        for (id, code) in [
            ("stream.stream_busy", ErrorCode::StreamBusy),
            ("stream.host_locked", ErrorCode::HostLocked),
            ("stream.host_unattended", ErrorCode::HostUnattended),
        ] {
            self.not_reachable(
                id,
                ep,
                Kind::Error,
                Some(code),
                "this host has no Stream mode",
            );
        }
        self.not_reachable(
            "stream.delete",
            "DELETE /v1/stream/{session}",
            Kind::Success,
            None,
            "this host has no Stream mode, so no session can exist",
        );
        self.expect_error(
            "stream.delete.unknown",
            "DELETE /v1/stream/{session}",
            ErrorCode::NotFound,
            Req::new("DELETE", "/v1/stream/ses_none"),
        )
        .await;
    }

    // ------------------------------------------------------------------ gate limits

    async fn concurrency(&mut self) {
        let ep = "(gate) any endpoint";
        case!(
            self,
            "gate.too_many_requests",
            ep,
            Kind::Error,
            Some(ErrorCode::TooManyRequests),
            async {
                let mut held = Vec::new();
                for _ in 0..gander_core::protocol::PER_DEVICE_CONCURRENCY {
                    let h = self
                        .c
                        .send_held(&Req::get(format!("/v1/shares/{SHARE}/file?path={BIG}")))
                        .await
                        .map_err(err)?;
                    ensure(h.status == 200, || {
                        format!("held download got {}", h.status)
                    })?;
                    held.push(h);
                }
                let r = self.send(&Req::get("/v1/me")).await?;
                drop(held);
                self.error_ok(&r, ErrorCode::TooManyRequests)?;
                ensure(r.header("retry-after").is_some(), || {
                    "429 without Retry-After".to_owned()
                })?;
                // The permits come back once the host notices the dropped connections.
                let deadline = Instant::now() + Duration::from_secs(15);
                loop {
                    let r = self.send(&Req::get("/v1/me")).await?;
                    if r.status == 200 {
                        return Ok(());
                    }
                    if Instant::now() > deadline {
                        return Err(format!("permits not released: {}", r.describe()));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            .await
        );
        self.not_reachable(
            "internal",
            "(any)",
            Kind::Error,
            Some(ErrorCode::Internal),
            "500 internal is an unexpected host failure; there is no way to cause one on purpose",
        );
    }

    // ------------------------------------------------------------------ events

    async fn ws_connect(
        &self,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
        String,
    > {
        let stream = self.c.connect().await.map_err(err)?;
        let mut request = format!("ws://127.0.0.1:{}/v1/events", self.c.addr.port())
            .into_client_request()
            .map_err(err)?;
        let token = self.c.token.clone().unwrap_or_default();
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {token}").parse().map_err(err)?,
        );
        let (ws, resp) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(err)?;
        ensure(resp.status() == 101, || {
            format!("upgrade got {}", resp.status())
        })?;
        Ok(ws)
    }

    async fn events(
        &mut self,
    ) -> Option<
        tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
    > {
        let ep = "GET /v1/events";
        self.expect_error(
            "events.token_missing",
            ep,
            ErrorCode::TokenMissing,
            Req::get("/v1/events").anonymous(),
        )
        .await;
        self.expect_error(
            "events.bad_request.no_upgrade",
            ep,
            ErrorCode::BadRequest,
            Req::get("/v1/events"),
        )
        .await;
        let mut socket = None;
        case!(
            self,
            "events.hello_then_host_state",
            ep,
            Kind::Event,
            None,
            async {
                let mut ws = self.ws_connect().await?;
                let first = next_event(&mut ws, Duration::from_secs(5)).await?;
                self.validate("ServerEvent", &first)?;
                ensure(first["type"] == "hello", || {
                    format!("first message {first}")
                })?;
                let second = next_event(&mut ws, Duration::from_secs(5)).await?;
                self.validate("ServerEvent", &second)?;
                ensure(second["type"] == "host.state", || {
                    format!("second message {second}")
                })?;
                socket = Some(ws);
                Ok(())
            }
            .await
        );
        let mut ws = socket?;
        case!(
            self,
            "events.ping_pong",
            ep,
            Kind::Event,
            None,
            async {
                ws.send(Message::Text(r#"{"type":"ping"}"#.into()))
                    .await
                    .map_err(err)?;
                let v = wait_for(&mut ws, |v| v["type"] == "pong", Duration::from_secs(5)).await?;
                self.validate("ServerEvent", &v)
            }
            .await
        );
        // Without Stream mode every stream message is refused with stream_unavailable; with it,
        // a message for a session that does not exist is `not_found`.
        let (refused_id, refused_code) = if self.stream_enabled {
            ("events.stream_message_unknown_session", ErrorCode::NotFound)
        } else {
            (
                "events.stream_message_refused",
                ErrorCode::StreamUnavailable,
            )
        };
        case!(
            self,
            refused_id,
            ep,
            Kind::Event,
            Some(refused_code),
            async {
                ws.send(Message::Text(
                    r#"{"type":"stream.pause","session":"ses_e2e"}"#.into(),
                ))
                .await
                .map_err(err)?;
                let v = wait_for(&mut ws, |v| v["type"] == "error", Duration::from_secs(5)).await?;
                self.validate("ServerEvent", &v)?;
                ensure(
                    v["code"] == refused_code.as_str() && v["session"] == "ses_e2e",
                    || format!("error event {v}"),
                )
            }
            .await
        );
        case!(
            self,
            "events.share_changed",
            ep,
            Kind::Event,
            None,
            async {
                // Arm the watch by listing the directory, then change it.
                let r = self
                    .send(&Req::get(format!(
                        "/v1/shares/{SHARE}/list?path={WATCH_DIR}"
                    )))
                    .await?;
                ensure(r.status == 200, || {
                    format!("list watch dir: {}", r.describe())
                })?;
                let name = format!("new-{}.txt", std::process::id());
                std::fs::write(self.root().join(WATCH_DIR).join(name), b"touched").map_err(err)?;
                let v = wait_for(
                    &mut ws,
                    |v| v["type"] == "share.changed" && v["path"] == WATCH_DIR,
                    Duration::from_secs(10),
                )
                .await?;
                self.validate("ServerEvent", &v)?;
                ensure(v["share"] == SHARE, || format!("share.changed {v}"))
            }
            .await
        );
        Some(ws)
    }

    // ------------------------------------------------------------------ revocation

    async fn revocation(
        &mut self,
        ws: Option<
            tokio_tungstenite::WebSocketStream<
                tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) {
        let ep = "GET /v1/me";
        let Some(device) = self.device.clone().filter(|_| self.opts.host_commands) else {
            self.not_reachable(
                "auth.token_revoked",
                ep,
                Kind::Error,
                Some(ErrorCode::TokenRevoked),
                "revocation needs the test host's stdin (--host-commands)",
            );
            return;
        };
        // stdout is the host's stdin.
        let sent = {
            let mut out = std::io::stdout().lock();
            writeln!(out, "revoke {device}").and_then(|()| out.flush())
        };
        let mut ws = ws;
        case!(
            self,
            "events.device_revoked",
            "GET /v1/events",
            Kind::Event,
            None,
            async {
                sent.as_ref().map_err(err)?;
                let ws = ws.as_mut().ok_or("no events socket")?;
                let v = wait_for(
                    ws,
                    |v| v["type"] == "device.revoked",
                    Duration::from_secs(10),
                )
                .await?;
                self.validate("ServerEvent", &v)?;
                match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                    Ok(None | Some(Ok(Message::Close(_)) | Err(_))) => Ok(()),
                    Ok(Some(Ok(m))) => {
                        Err(format!("socket still open after device.revoked: {m:?}"))
                    }
                    Err(_) => Err("socket not closed within 5 s of device.revoked".to_owned()),
                }
            }
            .await
        );
        case!(
            self,
            "auth.token_revoked",
            ep,
            Kind::Error,
            Some(ErrorCode::TokenRevoked),
            async {
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let r = self.send(&Req::get("/v1/me")).await?;
                    if r.status == 401 {
                        return self.error_ok(&r, ErrorCode::TokenRevoked);
                    }
                    if Instant::now() > deadline {
                        return Err(format!("still accepted after revoke: {}", r.describe()));
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            .await
        );
        self.expect_error(
            "token.rotate_revoked",
            "POST /v1/token/rotate",
            ErrorCode::TokenRevoked,
            Req::new("POST", "/v1/token/rotate"),
        )
        .await;
    }

    // ------------------------------------------------------------------ coverage

    /// Every `paths` entry of the document has at least one case.
    fn endpoint_coverage(&mut self) {
        let mut missing = Vec::new();
        if let Some(paths) = self.doc.get("paths").and_then(Value::as_object) {
            for (path, item) in paths {
                for method in item
                    .as_object()
                    .map(|m| m.keys().collect::<Vec<_>>())
                    .unwrap_or_default()
                {
                    let ep = format!("{} /v1{path}", method.to_ascii_uppercase());
                    if !self.report.cases.iter().any(|c| c.endpoint == ep) {
                        missing.push(ep);
                    }
                }
            }
        }
        let result = ensure(missing.is_empty(), || {
            format!("endpoints without cases: {missing:?}")
        });
        self.record(
            "openapi.every_endpoint_has_cases",
            "protocol/openapi.yaml",
            Kind::Success,
            None,
            result,
            Instant::now(),
        );
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

async fn next_event(ws: &mut Ws, wait: Duration) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_| "no event in time".to_owned())?
            .ok_or("socket closed")?
            .map_err(err)?;
        match msg {
            Message::Text(t) => return serde_json::from_str(t.as_str()).map_err(err),
            Message::Close(c) => return Err(format!("socket closed: {c:?}")),
            _ => {}
        }
    }
}

async fn wait_for(
    ws: &mut Ws,
    want: impl Fn(&Value) -> bool,
    wait: Duration,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let v = next_event(ws, left).await?;
        if want(&v) {
            return Ok(v);
        }
    }
}

/// Runs the whole suite and writes the report. `Ok(true)` when everything passed.
pub async fn run(opts: &Options) -> Result<bool> {
    let text = std::fs::read_to_string(&opts.openapi)
        .with_context(|| format!("read {}", opts.openapi.display()))?;
    let doc: Value = serde_norway::from_str(&text).context("parse openapi.yaml")?;
    let c = Client::new(opts.addr, HostId::from(opts.host_id.as_str()))?;
    let mut s = Suite {
        opts,
        doc,
        c,
        report: Report::default(),
        device: None,
        stream_enabled: false,
    };
    let started = time::OffsetDateTime::now_utc();
    let t0 = Instant::now();
    s.host_and_gate().await;
    let paired = s.pairing().await;
    if let Err(e) = &paired {
        eprintln!("pairing failed, stopping: {e:#}");
    } else {
        s.wait_for_grants().await;
        s.gate_authenticated().await;
        s.auth_me_rotate().await;
        s.browse().await;
        s.previews().await;
        s.fetch().await;
        s.zip().await;
        s.stream().await;
        let ws = s.events().await;
        s.concurrency().await;
        s.revocation(ws).await;
    }
    s.endpoint_coverage();

    let meta = json!({
        "suite": "cargo xtask e2e-host (WP-1B)",
        "started_at": started.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
        "duration_ms": t0.elapsed().as_millis(),
        "host": opts.addr.to_string(),
        "host_id": opts.host_id,
        "openapi": opts.openapi.display().to_string().replace('\\', "/"),
        "os": std::env::consts::OS,
    });
    let value = s.report.to_json(meta);
    if let Some(dir) = opts.report.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&opts.report, serde_json::to_vec_pretty(&value)?)
        .with_context(|| format!("write {}", opts.report.display()))?;
    eprintln!("summary: {}", value["summary"]);
    Ok(paired.is_ok() && value["summary"]["ok"] == true)
}
