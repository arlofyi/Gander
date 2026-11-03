// SPDX-License-Identifier: Apache-2.0
//! WP-68 integration tests: `GET /v1/zip`, the folder download, through the real gate and token
//! authenticator with a device paired through the real pairing endpoints.
//!
//! The archive that comes out of the stream is unzipped (`zip` crate, which checks every CRC) and
//! compared with the fixture tree on disk; limits, permissions, the skipped-items note, HEAD,
//! `Range` (ignored), the audit event and the client going away mid-stream are covered too.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::HeaderMap;
use futures_util::StreamExt as _;
use gander_core::config::{HostConfig, ShareConfig, ShareGrant, ZipConfig};
use gander_core::pairing::b64url;
use gander_core::protocol::files::{Permission, StreamDecision};
use gander_core::protocol::host::TrustAdapterKind;
use gander_core::protocol::pairing::{PairApproved, PairStarted};
use gander_core::protocol::{DeviceId, ShareId};
use gander_host::api::{ApiOptions, ApiState, host_routes};
use gander_host::audit::AuditLog;
use gander_host::pairing::{DeviceRegistry, PairingService, SharedStore, host_details};
use gander_host::server::gate::{GateState, apply_gate};
use gander_host::server::{
    ConnHandle, ConnInfo, DisconnectSignal, HostAllowList, HostIdentity, PeerNetwork,
};
use serde_json::Value;
use tower::ServiceExt as _;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const MAIN: &str = "shr_main";
const LIST_ONLY: &str = "shr_listonly";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp68-{tag}-{}", ulid::Ulid::new()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // remove_dir_all does not follow junctions.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn data(len: usize, seed: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(31).wrapping_add(seed) % 251) as u8)
        .collect()
}

fn junction(link: &Path, target: &Path) -> bool {
    Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// What the archive of `docs/` is expected to hold, relative to `docs`.
type Tree = BTreeMap<String, Option<Vec<u8>>>;

struct Fixture {
    dir: TempDir,
    junction: bool,
    /// Expected content below `docs` (None = a folder).
    docs: Tree,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new("fixture");
        let share = dir.0.join("share");
        let outside = dir.0.join("outside");
        let docs = share.join("docs");
        for d in [
            docs.join("empty"),
            docs.join("nested/a/b"),
            docs.join("\u{dc}n\u{ef}c\u{f6}d\u{e9} \u{65e5}\u{672c}"),
            share.join(".git"),
            outside.clone(),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(outside.join("secret.txt"), b"outside the share").unwrap();
        fs::write(share.join(".git").join("config"), b"[core]").unwrap();
        let mut want = Tree::new();
        let mut put = |rel: &str, bytes: Option<Vec<u8>>| {
            if let Some(b) = &bytes {
                fs::write(docs.join(rel), b).unwrap();
            }
            want.insert(rel.to_owned(), bytes);
        };
        put("notes.txt", Some(b"some notes".to_vec()));
        put("B-report.pdf", Some(data(1000, 1)));
        // Bigger than the 256 KiB chunk the stream hands out, so it arrives in several frames.
        put("big.bin", Some(data(3 * 1024 * 1024 + 17, 2)));
        put("zero.bin", Some(Vec::new()));
        put("emoji \u{1f600}.md", Some("# hi".as_bytes().to_vec()));
        put("nested/a/b/deep.txt", Some(b"deep".to_vec()));
        put(
            "\u{dc}n\u{ef}c\u{f6}d\u{e9} \u{65e5}\u{672c}/f\u{e9}.txt",
            Some(b"unicode".to_vec()),
        );
        want.insert("empty".to_owned(), None);
        want.insert("nested".to_owned(), None);
        want.insert("nested/a".to_owned(), None);
        want.insert("nested/a/b".to_owned(), None);
        want.insert(
            "\u{dc}n\u{ef}c\u{f6}d\u{e9} \u{65e5}\u{672c}".to_owned(),
            None,
        );
        // Not in the archive: a hidden-attribute file and desktop.ini (listing rules).
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .attributes(0x2)
            .open(docs.join("hidden.txt"))
            .unwrap();
        fs::write(docs.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        let junction = junction(&docs.join("escape"), &outside);
        Self {
            dir,
            junction,
            docs: want,
        }
    }

    fn share(&self) -> PathBuf {
        self.dir.0.join("share")
    }
}

fn store() -> SharedStore {
    Arc::new(gander_win::config_store::WindowsConfigStore)
}

struct Host {
    _dir: TempDir,
    app: Router,
    audit: Arc<AuditLog>,
    token: String,
}

struct Resp {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
    fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }
}

fn conn() -> ConnInfo {
    ConnInfo {
        peer: SocketAddr::new(LOOPBACK, 50_000),
        local: SocketAddr::new(LOOPBACK, 42_637),
        adapter: TrustAdapterKind::Generic,
        network: PeerNetwork::Loopback,
        disconnect: DisconnectSignal::never(),
    }
}

fn request(
    method: &str,
    uri: &str,
    auth: Option<&str>,
    headers: &[(&str, &str)],
) -> axum::http::Request<axum::body::Body> {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("Host", "127.0.0.1");
    if let Some(t) = auth {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let mut req = b.body(axum::body::Body::empty()).unwrap();
    req.extensions_mut().insert(conn());
    req.extensions_mut().insert(ConnHandle::new());
    req
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    headers: &[(&str, &str)],
) -> Resp {
    let resp = app
        .clone()
        .oneshot(request(method, uri, auth, headers))
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), 256 << 20)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    Resp {
        status,
        headers,
        body,
    }
}

fn share_cfg(id: &str, root: &Path, device: &DeviceId, perms: &[Permission]) -> ShareConfig {
    ShareConfig {
        id: ShareId::from(id),
        name: id.trim_start_matches("shr_").to_owned(),
        root: root.to_path_buf(),
        show_hidden: false,
        show_path: false,
        hidden_patterns: vec!["**/.git".to_owned()],
        stream_default: StreamDecision::Ask,
        grants: vec![ShareGrant {
            device: device.clone(),
            permissions: perms.to_vec(),
        }],
    }
}

async fn start(fix: &Fixture, options: ApiOptions, zip: ZipConfig) -> Host {
    let dir = TempDir::new("host");
    let identity = HostIdentity::generate().unwrap();
    let audit = Arc::new(AuditLog::memory());
    let registry = Arc::new(
        DeviceRegistry::load(
            store(),
            &dir.0.join("cfg"),
            Default::default(),
            audit.clone(),
        )
        .unwrap(),
    );
    let details = host_details(identity.host_id().clone(), vec![LOOPBACK], None);
    let svc = PairingService::new(&identity, registry, audit.clone(), details.clone(), 42_637);
    let api = Arc::new(
        ApiState::new(
            HostConfig::default(),
            Arc::clone(svc.registry()),
            audit.clone(),
            details,
        )
        .with_options(options),
    );
    let routes = host_routes(identity.host_id().clone(), &svc, &api);
    let hosts = Arc::new(HostAllowList::default());
    hosts.set_addresses([LOOPBACK]);
    let app = apply_gate(routes, GateState::new(hosts, svc.authenticator()));

    svc.open_window();
    let nonce = gander_host::pairing::random_bytes::<16>();
    let body = serde_json::json!({
        "device": {"name": "Pixel 9", "platform": "android", "model": "Pixel 9", "app_version": "0.1.0"},
        "nonce": b64url(&nonce),
    })
    .to_string();
    let mut req = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/pair")
        .header("Host", "127.0.0.1")
        .header("Content-Type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    req.extensions_mut().insert(conn());
    req.extensions_mut().insert(ConnHandle::new());
    let r = app.clone().oneshot(req).await.unwrap();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    let started: PairStarted = serde_json::from_slice(&bytes).unwrap();
    svc.approve(&started.pairing_id).unwrap();
    let r = send(
        &app,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, 200);
    let approved: PairApproved = serde_json::from_slice(&r.body).unwrap();
    let device = approved.device_id.clone();

    use Permission::*;
    api.set_config(HostConfig {
        shares: vec![
            share_cfg(MAIN, &fix.share(), &device, &[List, Preview, Fetch]),
            share_cfg(LIST_ONLY, &fix.share(), &device, &[List]),
        ],
        zip,
        ..Default::default()
    });
    Host {
        _dir: dir,
        app,
        audit,
        token: approved.token.expose().to_owned(),
    }
}

impl Host {
    async fn get(&self, uri: &str) -> Resp {
        self.req("GET", uri, &[]).await
    }

    async fn req(&self, method: &str, uri: &str, headers: &[(&str, &str)]) -> Resp {
        send(&self.app, method, uri, Some(&self.token), headers).await
    }

    /// The `access` audit events whose `detail.kind` is `zip`.
    fn zip_events(&self) -> Vec<gander_host::audit::AuditEvent> {
        self.audit
            .recent(500)
            .into_iter()
            .filter(|e| {
                e.event == "access" && e.detail.get("kind").and_then(Value::as_str) == Some("zip")
            })
            .collect()
    }
}

fn enc(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn zip_uri(share: &str, path: &str) -> String {
    format!("/v1/zip?share={share}&path={}", enc(path))
}

/// Unzips and checks every CRC. Folder entries map to `None`.
fn unzip(bytes: Vec<u8>) -> Tree {
    let mut ar = zip::ZipArchive::new(Cursor::new(bytes)).expect("a valid zip");
    let mut out = Tree::new();
    for i in 0..ar.len() {
        let mut f = ar.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        if f.is_dir() {
            out.insert(name.trim_end_matches('/').to_owned(), None);
        } else {
            let mut v = Vec::new();
            f.read_to_end(&mut v).unwrap();
            out.insert(name, Some(v));
        }
    }
    out
}

/// The archive of `docs`, below its top-level folder.
fn below(tree: &Tree, top: &str) -> Tree {
    let prefix = format!("{top}/");
    tree.iter()
        .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|r| (r.to_owned(), v.clone())))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_of_a_folder_matches_the_fixture_tree() {
    let fix = Fixture::new();
    let host = start(&fix, ApiOptions::default(), ZipConfig::default()).await;
    let r = host.get(&zip_uri(MAIN, "docs")).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.header("x-gander-protocol"), Some("1"));
    assert_eq!(r.header("content-type"), Some("application/zip"));
    assert_eq!(r.header("accept-ranges"), Some("none"));
    assert!(r.header("content-length").is_none(), "no length up front");
    assert!(
        r.header("content-disposition")
            .is_some_and(|d| d.contains("filename*=UTF-8''docs.zip")),
        "{:?}",
        r.header("content-disposition")
    );
    let want_entries = fix.docs.len();
    let want_bytes: usize = fix.docs.values().flatten().map(Vec::len).sum();
    assert_eq!(
        r.header("x-gander-zip-entries"),
        Some(want_entries.to_string().as_str())
    );
    assert_eq!(
        r.header("x-gander-zip-bytes"),
        Some(want_bytes.to_string().as_str())
    );

    let mut got = unzip(r.body);
    assert_eq!(
        got.remove("docs"),
        Some(None),
        "the folder itself comes first"
    );
    let note = got.remove("docs/_gander-skipped.txt");
    let got = below(&got, "docs");
    assert_eq!(
        got.keys().collect::<Vec<_>>(),
        fix.docs.keys().collect::<Vec<_>>()
    );
    for (name, content) in &fix.docs {
        assert_eq!(&got[name], content, "{name}");
    }
    // Nothing from outside the share, nothing hidden.
    assert!(!got.contains_key("hidden.txt"));
    assert!(!got.contains_key("desktop.ini"));
    assert!(
        !got.keys()
            .any(|k| k.contains("secret") || k.starts_with("escape/"))
    );
    if fix.junction {
        let note = String::from_utf8(note.expect("a note for the junction").unwrap()).unwrap();
        assert!(note.contains("escape\t"), "{note}");
    } else {
        println!("SKIPPED  junction note: mklink /J failed");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn zip_of_the_share_root_is_named_after_the_share() {
    let fix = Fixture::new();
    let host = start(&fix, ApiOptions::default(), ZipConfig::default()).await;
    let r = host.get(&format!("/v1/zip?share={MAIN}")).await;
    assert_eq!(r.status, 200);
    assert!(
        r.header("content-disposition")
            .is_some_and(|d| d.contains("filename=\"main.zip\"")),
        "{:?}",
        r.header("content-disposition")
    );
    let got = unzip(r.body);
    assert!(got.contains_key("main"));
    assert_eq!(got["main/docs/notes.txt"], Some(b"some notes".to_vec()));
    // The hidden-pattern folder is not there.
    assert!(!got.keys().any(|k| k.contains(".git")));
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_head_and_range() {
    let fix = Fixture::new();
    let host = start(&fix, ApiOptions::default(), ZipConfig::default()).await;
    let want_bytes: usize = fix.docs.values().flatten().map(Vec::len).sum();

    let r = host
        .get(&format!("{}&dry_run=1", zip_uri(MAIN, "docs")))
        .await;
    assert_eq!(r.status, 200);
    assert!(
        r.header("content-type")
            .is_some_and(|c| c.starts_with("application/json"))
    );
    let v = r.json();
    assert_eq!(v["entries"], fix.docs.len());
    assert_eq!(v["bytes"], want_bytes);
    assert_eq!(v["skipped"], u64::from(fix.junction));
    assert_eq!(v["cloud_only"], 0);

    // HEAD: the same headers, no body, nothing streamed (and no audit event).
    let r = host.req("HEAD", &zip_uri(MAIN, "docs"), &[]).await;
    assert_eq!(r.status, 200);
    assert!(r.body.is_empty());
    assert_eq!(r.header("content-type"), Some("application/zip"));
    assert_eq!(
        r.header("x-gander-zip-bytes"),
        Some(want_bytes.to_string().as_str())
    );

    // Range and If-Range are ignored: always the whole archive, from the start, 200.
    let r = host
        .req(
            "GET",
            &zip_uri(MAIN, "docs/nested"),
            &[("Range", "bytes=10-20"), ("If-Range", "\"x\"")],
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(r.header("content-range").is_none());
    assert_eq!(&r.body[..4], b"PK\x03\x04");
    let got = unzip(r.body);
    assert_eq!(got["nested/a/b/deep.txt"], Some(b"deep".to_vec()));

    // Measuring is not audited, the one download is.
    assert_eq!(host.zip_events().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn over_the_limit_is_413_before_anything_is_streamed() {
    let fix = Fixture::new();
    // docs holds 3 MiB+ in a file and 12 entries.
    let tiny_bytes = ZipConfig {
        max_mib: 1,
        max_entries: 1000,
    };
    let host = start(&fix, ApiOptions::default(), tiny_bytes).await;
    for uri in [
        zip_uri(MAIN, "docs"),
        format!("{}&dry_run=1", zip_uri(MAIN, "docs")),
    ] {
        let r = host.get(&uri).await;
        assert_eq!(r.status, 413, "{uri}");
        assert_eq!(r.code(), "too_large");
        assert!(
            r.header("content-type")
                .is_some_and(|c| c.starts_with("application/json"))
        );
        let msg = r.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(msg.contains("1 MiB") && !msg.contains("docs"), "{msg}");
    }
    // A smaller folder under the same limit is fine.
    assert_eq!(host.get(&zip_uri(MAIN, "docs/nested")).await.status, 200);

    let few = ZipConfig {
        max_mib: 4096,
        max_entries: 2,
    };
    let host = start(&fix, ApiOptions::default(), few).await;
    assert_eq!(host.get(&zip_uri(MAIN, "docs")).await.status, 413);
    // nested holds a, a/b and a/b/deep.txt: 3 entries; the folder asked for is not counted.
    assert_eq!(host.get(&zip_uri(MAIN, "docs/nested")).await.status, 413);
    assert_eq!(host.get(&zip_uri(MAIN, "docs/nested/a")).await.status, 200); // b, deep.txt
    assert_eq!(host.get(&zip_uri(MAIN, "docs/empty")).await.status, 200);
    // The refusals are audited as refusals, never as a download.
    assert!(
        host.zip_events()
            .iter()
            .all(|e| e.outcome == gander_host::audit::Outcome::Ok)
    );
    let refused = host
        .audit
        .recent(500)
        .into_iter()
        .filter(|e| e.outcome == gander_host::audit::Outcome::Refused)
        .count();
    assert!(refused >= 2);

    // The walk's time budget is a limit too.
    let host = start(
        &fix,
        ApiOptions {
            zip_walk_budget: Duration::ZERO,
            ..ApiOptions::default()
        },
        ZipConfig::default(),
    )
    .await;
    let r = host.get(&zip_uri(MAIN, "docs")).await;
    assert_eq!((r.status, r.code().as_str()), (413, "too_large"));
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_and_errors() {
    let fix = Fixture::new();
    let host = start(&fix, ApiOptions::default(), ZipConfig::default()).await;
    let check = |r: Resp, status: u16, code: &str| {
        assert_eq!(
            (r.status, r.code()),
            (status, code.to_owned()),
            "{:?}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.header("x-gander-protocol"), Some("1"));
        let msg = r.json()["error"]["message"].as_str().unwrap().to_owned();
        assert!(
            !msg.contains(":\\") && !msg.contains("gander-wp68"),
            "{msg}"
        );
    };
    // `list` only: no `fetch` permission.
    check(
        host.get(&zip_uri(LIST_ONLY, "docs")).await,
        403,
        "permission_denied",
    );
    check(
        host.get(&zip_uri("shr_unknown", "docs")).await,
        404,
        "share_not_found",
    );
    check(host.get("/v1/zip").await, 400, "bad_request");
    check(
        host.get(&zip_uri(MAIN, "docs/notes.txt")).await,
        400,
        "bad_path",
    );
    check(host.get(&zip_uri(MAIN, "docs/..")).await, 400, "bad_path");
    check(
        host.get(&zip_uri(MAIN, "docs/missing")).await,
        404,
        "not_found",
    );
    check(host.get(&zip_uri(MAIN, ".git")).await, 404, "not_found");
    if fix.junction {
        // A junction is not a folder the host will zip.
        check(
            host.get(&zip_uri(MAIN, "docs/escape")).await,
            404,
            "not_found",
        );
    }
    // No token.
    let r = send(&host.app, "GET", &zip_uri(MAIN, "docs"), None, &[]).await;
    assert_eq!(r.status, 401);
    // The permission refusal is audited (the same way a refused file fetch is).
    assert!(host.audit.recent(500).iter().any(|e| {
        e.outcome == gander_host::audit::Outcome::Refused
            && e.detail.get("action").and_then(Value::as_str) == Some("fetch")
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn one_audit_event_with_counts_and_no_file_names() {
    let fix = Fixture::new();
    let host = start(&fix, ApiOptions::default(), ZipConfig::default()).await;
    let r = host.get(&zip_uri(MAIN, "docs")).await;
    assert_eq!(r.status, 200);
    let events = host.zip_events();
    assert_eq!(events.len(), 1, "{events:?}");
    let e = &events[0];
    let want_bytes: u64 = fix.docs.values().flatten().map(|v| v.len() as u64).sum();
    assert_eq!(e.detail["action"], "fetch");
    assert_eq!(e.detail["path"], "docs");
    assert_eq!(e.detail["share"], MAIN);
    assert_eq!(e.detail["entries"], fix.docs.len());
    assert_eq!(e.detail["bytes"], want_bytes);
    assert_eq!(e.detail["result"], "complete");
    let line = serde_json::to_string(e).unwrap();
    for name in [
        "notes.txt",
        "B-report",
        "big.bin",
        "deep.txt",
        "escape",
        "hidden",
    ] {
        assert!(
            !line.contains(name),
            "{name} leaked into the audit line: {line}"
        );
    }
    // Capabilities say what the limits are.
    let me = host.get("/v1/me").await.json();
    assert_eq!(me["capabilities"]["fetch"]["zip"]["max_bytes"], 4u64 << 30);
    assert_eq!(me["capabilities"]["fetch"]["zip"]["max_entries"], 20_000);
}

/// Waits until `f` holds (at most 10 s).
async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_stream_stops_when_the_client_goes_away_and_the_slot_is_freed() {
    let fix = Fixture::new();
    // A folder much bigger than what the channel and the socket can hold.
    let big = fix.share().join("big");
    fs::create_dir_all(&big).unwrap();
    for i in 0..6 {
        fs::write(big.join(format!("f{i}.bin")), data(4 * 1024 * 1024, i)).unwrap();
    }
    let options = ApiOptions {
        zip_concurrency: 1,
        ..ApiOptions::default()
    };
    let host = start(&fix, options, ZipConfig::default()).await;

    let resp = host
        .app
        .clone()
        .oneshot(request(
            "GET",
            &zip_uri(MAIN, "big"),
            Some(&host.token),
            &[],
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let mut body = resp.into_body().into_data_stream();
    let first = body.next().await.expect("a first chunk").unwrap();
    assert_eq!(&first[..4], b"PK\x03\x04");

    // The only slot is busy: another download is refused, a measurement is not.
    let r = host.get(&zip_uri(MAIN, "docs/nested")).await;
    assert_eq!((r.status, r.code().as_str()), (429, "too_many_requests"));
    assert!(r.header("retry-after").is_some());
    assert_eq!(
        host.get(&format!("{}&dry_run=1", zip_uri(MAIN, "docs/nested")))
            .await
            .status,
        200
    );
    assert!(host.zip_events().is_empty(), "still streaming");

    // The client disconnects: the host stops reading and records what happened.
    drop(body);
    eventually("the cancelled download to be audited", || {
        host.zip_events()
            .iter()
            .any(|e| e.detail["result"] == "cancelled")
    })
    .await;
    let e = &host.zip_events()[0];
    assert!(
        e.detail["bytes"].as_u64().unwrap() < 6 * 4 * 1024 * 1024,
        "it did not read everything: {e:?}"
    );
    // The slot is free again (the audit line is written just before the task lets go of it).
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = host.get(&zip_uri(MAIN, "docs/nested")).await;
        if r.status == 200 {
            break;
        }
        assert_eq!(r.status, 429, "{}", String::from_utf8_lossy(&r.body));
        assert!(Instant::now() < deadline, "the slot was never released");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
