// SPDX-License-Identifier: Apache-2.0
//! WP-14 integration tests: Browse and Fetch against a generated fixture tree (a 10,000-file
//! directory, Unicode names, hidden files, a hidden pattern and a junction to outside the share).
//!
//! Requests go through the real gate and token authenticator (`host_routes` behind
//! `apply_gate`) with a device paired through the real pairing endpoints; one test also runs the
//! TLS listener on 127.0.0.1 port 0. The 10k-entry timing gate (`list_10k_entries_timing`) only
//! asserts in release builds:
//! `cargo test --release -p gander-host --test browse_fetch list_10k -- --nocapture`.

#![cfg(windows)]

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use axum::Router;
use axum::http::HeaderMap;
use gander_core::config::{HostConfig, ShareConfig, ShareGrant};
use gander_core::net::plan::ListenerPlan;
use gander_core::pairing::b64url;
use gander_core::policy::{HandlerInfo, Signature, StaticAssociations};
use gander_core::protocol::files::{Permission, StreamDecision};
use gander_core::protocol::host::{PreviewsUnavailable, TrustAdapterKind};
use gander_core::protocol::pairing::{PairApproved, PairStarted};
use gander_core::protocol::{DeviceId, ShareId};
use gander_host::api::{ApiOptions, ApiState, host_routes};
use gander_host::audit::AuditLog;
use gander_host::pairing::{DeviceRegistry, PairingService, SharedStore, host_details};
use gander_host::server::gate::{GateState, apply_gate};
use gander_host::server::tls::pinned_client_config;
use gander_host::server::{
    ConnHandle, ConnInfo, DisconnectSignal, FixedPlan, HostAllowList, HostIdentity, PeerNetwork,
    Server, ServerOptions,
};
use gander_win::preview::office::OfficeEnv;
use gander_win::preview::service::PreviewService;
use gander_win::render::job::RenderJob;
use gander_win::render::worker::WorkerConfig;
use gander_win::thumbs::cache::ThumbCache;
use gander_win::thumbs::queue::QueueConfig;
use gander_win::thumbs::service::{RenderResult, ThumbRenderer, ThumbService};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tower::ServiceExt as _;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const BIG: usize = 10_000;
const BIG_EXTS: [&str; 5] = ["txt", "pdf", "jpg", "docx", "bin"];
const DATA_LEN: usize = 1024 * 1024;
const MAIN: &str = "shr_main";
const LIST_ONLY: &str = "shr_listonly";
const OTHER: &str = "shr_other";
const STREAM: &str = "shr_stream";

/// Unicode names created in `docs/` (two spellings of "e acute" are different names).
const UNICODE_NAMES: [&str; 6] = [
    "\u{c9}t\u{e9} r\u{e9}sum\u{e9}.txt",
    "\u{65e5}\u{672c}\u{8a9e}.pdf",
    "e\u{301}.txt",
    "\u{e9}.txt",
    "emoji \u{1f600}.md",
    "report+v1.txt",
];

// ---------------------------------------------------------------------------------------------
// Fixture

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp14-{tag}-{}", ulid::Ulid::new()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Fixture {
    share: PathBuf,
    share2: PathBuf,
    junction: bool,
    /// Names (relative paths) of every `needle` match under `tree/`.
    needles: BTreeSet<String>,
}

fn data_byte(i: usize) -> u8 {
    (i.wrapping_mul(31) % 251) as u8
}

/// `mklink /J` (junctions need no privilege).
fn junction(link: &Path, target: &Path) -> bool {
    Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn set_mtime(path: &Path, t: SystemTime) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(t).unwrap();
}

/// The shared read-only fixture, generated once per test process.
fn fixture() -> &'static Fixture {
    static FIX: OnceLock<Fixture> = OnceLock::new();
    FIX.get_or_init(|| {
        let tmp = std::env::temp_dir();
        // Fixtures of earlier runs (the static is never dropped): remove_dir_all does not follow
        // junctions, so this never touches anything outside.
        if let Ok(rd) = fs::read_dir(&tmp) {
            for e in rd.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .starts_with("gander-wp14-fixture-")
                {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
        let base = tmp.join(format!("gander-wp14-fixture-{}", ulid::Ulid::new()));
        let share = base.join("share");
        let share2 = base.join("share2");
        let outside = base.join("outside");
        for d in [&share, &share2, &outside] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(outside.join("secret.txt"), b"outside the share").unwrap();
        fs::write(share2.join("a.txt"), b"a").unwrap();

        let big = share.join("big");
        fs::create_dir_all(&big).unwrap();
        for i in 0..BIG {
            let name = format!("f{i:05}.{}", BIG_EXTS[i % BIG_EXTS.len()]);
            fs::write(big.join(name), i.to_string()).unwrap();
        }

        let docs = share.join("docs");
        fs::create_dir_all(&docs).unwrap();
        for n in UNICODE_NAMES {
            fs::write(docs.join(n), n.as_bytes()).unwrap();
        }
        // FILE_ATTRIBUTE_HIDDEN
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .attributes(0x2)
            .open(docs.join("hidden.txt"))
            .unwrap();
        fs::write(docs.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        fs::create_dir_all(share.join(".git")).unwrap();
        fs::write(share.join(".git").join("config"), b"[core]").unwrap();
        let junction = junction(&docs.join("escape"), &outside);
        if !junction {
            println!("SKIPPED  junction fixture: mklink /J failed");
        }

        let data: Vec<u8> = (0..DATA_LEN).map(data_byte).collect();
        fs::write(share.join("data.bin"), &data).unwrap();

        // Search tree: six nested levels, each with three matches and four side directories.
        let mut needles = BTreeSet::new();
        let mut dir = share.join("tree");
        let mut rel = String::from("tree");
        for level in 1..=6 {
            dir = dir.join(format!("lv{level}"));
            rel = format!("{rel}/lv{level}");
            fs::create_dir_all(&dir).unwrap();
            for k in 0..3 {
                let n = format!("Needle-{level}-{k}.txt");
                fs::write(dir.join(&n), b"n").unwrap();
                needles.insert(format!("{rel}/{n}"));
                fs::write(dir.join(format!("hay-{level}-{k}.txt")), b"h").unwrap();
            }
            for j in 0..4 {
                let side = dir.join(format!("side{j}"));
                fs::create_dir_all(&side).unwrap();
                let n = format!("x-needle-{level}-{j}.txt");
                fs::write(side.join(&n), b"n").unwrap();
                needles.insert(format!("{rel}/side{j}/{n}"));
            }
        }

        // Recent: newest files at level 1 and 2; a newer one at level 4 is outside the walk.
        let now = SystemTime::now();
        fs::write(share.join("recent1.txt"), b"1").unwrap();
        set_mtime(&share.join("recent1.txt"), now + Duration::from_secs(3600));
        fs::write(docs.join("recent2.txt"), b"2").unwrap();
        set_mtime(&docs.join("recent2.txt"), now + Duration::from_secs(7200));
        let deep = share
            .join("tree")
            .join("lv1")
            .join("lv2")
            .join("recent4.txt");
        fs::write(&deep, b"4").unwrap();
        set_mtime(&deep, now + Duration::from_secs(10_800));

        Fixture {
            share,
            share2,
            junction,
            needles,
        }
    })
}

// ---------------------------------------------------------------------------------------------
// Host under test

fn store() -> SharedStore {
    Arc::new(gander_win::config_store::WindowsConfigStore)
}

#[derive(Debug)]
struct FakeRenderer;

impl ThumbRenderer for FakeRenderer {
    fn render(&self, _file: &File, _job: &RenderJob) -> RenderResult {
        RenderResult::Image(b"RIFF\0\0\0\0WEBPfake".to_vec())
    }
}

struct Host {
    _dir: TempDir,
    app: Router,
    svc: Arc<PairingService>,
    api: Arc<ApiState>,
    audit: Arc<AuditLog>,
    identity: HostIdentity,
    token: String,
    device: DeviceId,
}

#[derive(Debug)]
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

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    headers: &[(&str, &str)],
    body: &str,
) -> Resp {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("Host", "127.0.0.1");
    if let Some(t) = auth {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    if !body.is_empty() {
        b = b.header("Content-Type", "application/json");
    }
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let mut req = b.body(axum::body::Body::from(body.to_owned())).unwrap();
    req.extensions_mut().insert(conn());
    req.extensions_mut().insert(ConnHandle::new());
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), 64 << 20)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    Resp {
        status,
        headers,
        body,
    }
}

fn share(id: &str, root: &Path, device: &DeviceId, perms: &[Permission]) -> ShareConfig {
    ShareConfig {
        id: ShareId::from(id),
        name: id.trim_start_matches("shr_").to_owned(),
        root: root.to_path_buf(),
        show_hidden: false,
        show_path: id == MAIN,
        hidden_patterns: vec!["**/.git".to_owned()],
        stream_default: StreamDecision::Ask,
        grants: if perms.is_empty() {
            Vec::new()
        } else {
            vec![ShareGrant {
                device: device.clone(),
                permissions: perms.to_vec(),
            }]
        },
    }
}

struct Setup {
    options: ApiOptions,
    real_associations: bool,
    fake_thumbs: bool,
    /// No render worker: no thumbnails or previews, `previews_unavailable` set as `main` does.
    no_worker: bool,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            options: ApiOptions::default(),
            real_associations: false,
            fake_thumbs: true,
            no_worker: false,
        }
    }
}

/// A notepad-like handler for `.txt`, unknown to the catalogue (policy outcome `ask`).
fn associations() -> StaticAssociations {
    StaticAssociations::new().with(
        "txt",
        HandlerInfo::new(
            "C:\\Program Files\\Editor\\editor.exe",
            Signature::Signed {
                publisher: "Example Corp".into(),
            },
        ),
    )
}

async fn start(setup: Setup) -> Host {
    let fix = fixture();
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
    let mut api = ApiState::new(
        HostConfig::default(),
        Arc::clone(svc.registry()),
        audit.clone(),
        details,
    )
    .with_options(setup.options);
    if !setup.real_associations {
        api = api.with_associations(Arc::new(associations()));
    }
    let worker = WorkerConfig::new(dir.0.join("no-worker.exe"));
    if setup.no_worker {
        api = api.with_previews_unavailable(PreviewsUnavailable::RenderWorkerMissing);
    } else if setup.fake_thumbs {
        let cache = Arc::new(ThumbCache::open(&dir.0.join("cache"), 64 << 20).unwrap());
        api = api.with_thumbs(Arc::new(ThumbService::new(
            cache,
            FakeRenderer,
            QueueConfig::default(),
            Box::new(|_| false),
        )));
    } else {
        let cache = Arc::new(ThumbCache::open(&dir.0.join("cache"), 64 << 20).unwrap());
        api = api.with_thumbs(Arc::new(ThumbService::with_worker(cache, worker.clone())));
    }
    if !setup.no_worker {
        let pcache =
            Arc::new(ThumbCache::open_in(&dir.0.join("cache"), "previews", 64 << 20).unwrap());
        api = api.with_previews(Arc::new(PreviewService::with_worker(
            pcache,
            worker,
            OfficeEnv::none(),
        )));
    }
    let api = Arc::new(api);
    let routes = host_routes(identity.host_id().clone(), &svc, &api);
    let hosts = Arc::new(HostAllowList::default());
    hosts.set_addresses([LOOPBACK]);
    let app = apply_gate(routes, GateState::new(hosts, svc.authenticator()));

    // Pair through the real endpoints (flow B, owner approval).
    svc.open_window();
    let nonce = gander_host::pairing::random_bytes::<16>();
    let body = serde_json::json!({
        "device": {"name": "Pixel 9", "platform": "android", "model": "Pixel 9", "app_version": "0.1.0"},
        "nonce": b64url(&nonce),
    })
    .to_string();
    let r = send(&app, "POST", "/v1/pair", None, &[], &body).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let started: PairStarted = serde_json::from_slice(&r.body).unwrap();
    svc.approve(&started.pairing_id).unwrap();
    let r = send(
        &app,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        None,
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, 200);
    let approved: PairApproved = serde_json::from_slice(&r.body).unwrap();
    let device = approved.device_id.clone();

    use Permission::*;
    let config = HostConfig {
        shares: vec![
            share(MAIN, &fix.share, &device, &[List, Preview, Fetch]),
            share(LIST_ONLY, &fix.share2, &device, &[List]),
            share(OTHER, &fix.share2, &device, &[]),
            share(STREAM, &fix.share2, &device, &[Stream]),
        ],
        ..Default::default()
    };
    api.set_config(config);
    Host {
        _dir: dir,
        app,
        svc,
        api,
        audit,
        identity,
        token: approved.token.expose().to_owned(),
        device,
    }
}

impl Host {
    async fn get(&self, uri: &str) -> Resp {
        self.req("GET", uri, &[]).await
    }

    async fn req(&self, method: &str, uri: &str, headers: &[(&str, &str)]) -> Resp {
        send(&self.app, method, uri, Some(&self.token), headers, "").await
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

fn names(v: &Value) -> Vec<String> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Tests

#[tokio::test(flavor = "multi_thread")]
async fn me_and_shares() {
    let host = start(Setup::default()).await;
    let r = host.get("/v1/me").await;
    assert_eq!(r.status, 200);
    let me = r.json();
    assert_eq!(me["device"]["id"], host.device.as_str());
    assert_eq!(me["device"]["name"], "Pixel 9");
    let os_version = me["host"]["os_version"].as_str().unwrap();
    assert_eq!(os_version.split('.').count(), 3, "{os_version}");
    assert_eq!(me["host"]["os"], "windows");
    assert_eq!(me["network"]["adapter"], "generic");
    assert_eq!(me["capabilities"]["search"], true);
    assert_eq!(me["capabilities"]["fetch"]["range"], true);
    assert_eq!(
        me["capabilities"]["fetch"]["hash"],
        serde_json::json!(["blake3", "sha256"])
    );
    assert_eq!(
        me["capabilities"]["thumb_sizes"],
        serde_json::json!([128, 256, 512])
    );
    assert_eq!(me["capabilities"]["stream"]["enabled"], false);
    assert_eq!(me["capabilities"]["preview_kinds"]["pdf"], true);
    assert!(me["capabilities"].get("previews_unavailable").is_none());
    assert_eq!(me["limits"]["list_max"], 1000);

    let r = host.get("/v1/shares").await;
    assert_eq!(r.status, 200);
    let shares = r.json();
    let ids: Vec<&str> = shares["shares"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [MAIN, LIST_ONLY, STREAM], "no grant -> not listed");
    let main = &shares["shares"][0];
    assert_eq!(
        main["permissions"],
        serde_json::json!(["list", "preview", "fetch"])
    );
    assert!(main["path_display"].as_str().unwrap().ends_with("share"));
    assert!(shares["shares"][1].get("path_display").is_none());

    // Without a token the gate refuses before any route runs.
    let r = send(&host.app, "GET", "/v1/shares", None, &[], "").await;
    assert_eq!((r.status, r.code().as_str()), (401, "token_missing"));
    // Token rotation (WP-13) is part of the assembled routes; the old token stops working.
    let r = send(
        &host.app,
        "POST",
        "/v1/token/rotate",
        Some(&host.token),
        &[],
        "",
    )
    .await;
    assert_eq!(r.status, 200);
    let new_token = r.json()["token"].as_str().unwrap().to_owned();
    let r = host.get("/v1/me").await;
    assert_eq!(r.status, 401);
    let r = send(&host.app, "GET", "/v1/me", Some(&new_token), &[], "").await;
    assert_eq!(r.status, 200);
    // `/v1/host` stays anonymous.
    let r = send(&host.app, "GET", "/v1/host", None, &[], "").await;
    assert_eq!(r.status, 200);
}

/// A host without `gander-render.exe` says so in `/me` and never offers a preview it cannot make
/// (found on the owner's machine: `cargo run -p gander-host` builds no worker).
#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_the_render_worker_says_previews_are_unavailable() {
    let host = start(Setup {
        no_worker: true,
        ..Setup::default()
    })
    .await;
    let me = host.get("/v1/me").await.json();
    assert_eq!(
        me["capabilities"]["previews_unavailable"],
        "render_worker_missing"
    );
    for kind in ["image", "pdf", "text", "video_poster", "raw"] {
        assert_eq!(me["capabilities"]["preview_kinds"][kind], false, "{kind}");
    }
    let listing = host
        .get(&format!("/v1/shares/{MAIN}/list?path=&limit=1000"))
        .await
        .json();
    for e in listing["entries"].as_array().unwrap() {
        assert_eq!(e["preview"], "none", "{e}");
        assert_eq!(e["thumb"], false, "{e}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_10k_paging_etag_and_filters() {
    let host = start(Setup::default()).await;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut etag: Option<String> = None;
    loop {
        let mut uri = format!("/v1/shares/{MAIN}/list?path=big&limit=1000&sort=name");
        if let Some(c) = &cursor {
            uri.push_str(&format!("&cursor={}", enc(c)));
        }
        let r = host.get(&uri).await;
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let v = r.json();
        assert_eq!(v["path"], "big");
        assert_eq!(v["truncated"], false);
        assert_eq!(v["unlistable"], 0);
        let header_etag = r.header("etag").unwrap().to_owned();
        assert_eq!(v["etag"], header_etag.as_str());
        assert!(header_etag.starts_with("W/\""));
        // The ETag covers the whole listing: identical on every page.
        assert_eq!(*etag.get_or_insert(header_etag.clone()), header_etag);
        seen.extend(names(&v));
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen.len(), BIG);
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, seen, "pages in order, no duplicates");

    // Decoration: mime, thumb prediction, preview kind, stream outcome.
    let r = host
        .get(&format!("/v1/shares/{MAIN}/list?path=big&limit=5"))
        .await;
    let v = r.json();
    let e0 = &v["entries"][0]; // f00000.txt
    assert_eq!(e0["name"], "f00000.txt");
    assert_eq!(e0["mime"], "text/plain");
    assert_eq!(e0["thumb"], false);
    assert_eq!(e0["preview"], "text");
    assert_eq!(
        e0["stream"], "refused",
        "no stream permission on this share"
    );
    let e2 = &v["entries"][2]; // f00002.jpg
    assert_eq!(
        (e2["thumb"].as_bool(), e2["preview"].as_str()),
        (Some(true), Some("image"))
    );

    // 304 for the same listing, with a different page size and with a list of tags.
    let et = etag.unwrap();
    let r = host
        .req(
            "GET",
            &format!("/v1/shares/{MAIN}/list?path=big&limit=10"),
            &[("If-None-Match", &et)],
        )
        .await;
    assert_eq!(r.status, 304);
    assert!(r.body.is_empty());
    assert_eq!(r.header("etag"), Some(et.as_str()));
    let r = host
        .req(
            "GET",
            &format!("/v1/shares/{MAIN}/list?path=big&sort=size"),
            &[("If-None-Match", &et)],
        )
        .await;
    assert_eq!(r.status, 200, "another sort is another listing");

    // Filters.
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/list?path=big&kinds=file&ext=PDF&limit=1000"
        ))
        .await;
    let v = r.json();
    assert_eq!(v["entries"].as_array().unwrap().len(), 1000);
    assert!(names(&v).iter().all(|n| n.ends_with(".pdf")));
    let r = host.get(&format!("/v1/shares/{MAIN}/list?kinds=dir")).await;
    assert_eq!(
        names(&r.json()),
        ["big", "docs", "tree"],
        ".git hidden by pattern"
    );
    let r = host
        .get(&format!("/v1/shares/{MAIN}/list?sort=mtime&order=desc"))
        .await;
    let v = r.json();
    // Directories first regardless of order, then the newest file.
    assert_eq!(v["entries"][0]["kind"], "dir");
    let first_file = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "file")
        .unwrap();
    assert_eq!(first_file["name"], "recent1.txt");

    // Bad input.
    for (q, code) in [
        ("sort=bogus", "bad_request"),
        ("order=up", "bad_request"),
        ("kinds=files", "bad_request"),
        ("limit=x", "bad_request"),
        ("cursor=!!", "bad_request"),
        ("path=data.bin", "bad_path"),
        ("path=big%2F..%2Fdocs", "bad_path"),
        ("path=nope", "not_found"),
    ] {
        let r = host.get(&format!("/v1/shares/{MAIN}/list?{q}")).await;
        assert_eq!(r.code(), code, "{q}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn list_etag_changes_with_the_directory() {
    let host = start(Setup::default()).await;
    // A private copy of a small directory inside a new share root.
    let dir = TempDir::new("etag");
    fs::write(dir.0.join("a.txt"), b"a").unwrap();
    let mut config = HostConfig::default();
    config
        .shares
        .push(share("shr_tmp", &dir.0, &host.device, &[Permission::List]));
    host.api.set_config(config);
    let uri = "/v1/shares/shr_tmp/list";
    let e1 = host.get(uri).await.header("etag").unwrap().to_owned();
    assert_eq!(
        host.req("GET", uri, &[("If-None-Match", &e1)]).await.status,
        304
    );
    fs::write(dir.0.join("b.txt"), b"b").unwrap();
    let r = host.req("GET", uri, &[("If-None-Match", &e1)]).await;
    assert_eq!(r.status, 200, "a new entry changes the ETag");
    let e2 = r.header("etag").unwrap().to_owned();
    set_mtime(
        &dir.0.join("a.txt"),
        SystemTime::now() + Duration::from_secs(60),
    );
    let e3 = host.get(uri).await.header("etag").unwrap().to_owned();
    assert_ne!(e2, e3, "an mtime change changes the ETag");
}

/// The non-functional requirement: listing 10,000 entries in under 300 ms (release builds).
/// Measured through the gate with the production association lookup and thumbnail prediction.
#[tokio::test(flavor = "multi_thread")]
async fn list_10k_entries_timing() {
    let host = start(Setup {
        real_associations: true,
        fake_thumbs: false,
        ..Default::default()
    })
    .await;
    let uri = format!("/v1/shares/{MAIN}/list?path=big&limit=1000");
    let t = Instant::now();
    let r = host.get(&uri).await;
    let cold = t.elapsed();
    assert_eq!(r.status, 200);
    let mut warm = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let r = host.get(&uri).await;
        warm.push(t.elapsed());
        assert_eq!(r.status, 200);
        assert_eq!(r.json()["entries"].as_array().unwrap().len(), 1000);
    }
    warm.sort();
    let median = warm[warm.len() / 2];
    println!(
        "TIMING   /list of {BIG} entries (page 1000): first {} ms, warm median {} ms, max {} ms ({})",
        cold.as_millis(),
        median.as_millis(),
        warm.last().unwrap().as_millis(),
        if cfg!(debug_assertions) {
            "debug, not asserted"
        } else {
            "release"
        }
    );
    if !cfg!(debug_assertions) {
        assert!(
            median < Duration::from_millis(300),
            "warm median {median:?}"
        );
        assert!(cold < Duration::from_millis(300), "first request {cold:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn unicode_hidden_and_junction() {
    let fix = fixture();
    let host = start(Setup::default()).await;
    let r = host.get(&format!("/v1/shares/{MAIN}/list?path=docs")).await;
    assert_eq!(r.status, 200);
    let v = r.json();
    let listed: BTreeSet<String> = names(&v).into_iter().collect();
    for n in UNICODE_NAMES {
        assert!(listed.contains(n), "{n:?} returned byte-identical");
    }
    assert!(!listed.contains("hidden.txt"), "hidden attribute");
    assert!(!listed.contains("desktop.ini"), "always omitted");
    if fix.junction {
        let j = v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "escape")
            .expect("junction listed greyed");
        assert_eq!(j["flags"], serde_json::json!(["link"]));
        assert_eq!(j["stream"], "refused");
        // The resolver refuses the junction itself and anything through it (not_found, as
        // for every denied reparse point). Windows RedirectionGuard may also refuse to follow a
        // non-admin junction (error 448); the resolver never gets that far.
        for p in ["docs/escape", "docs/escape/secret.txt"] {
            for route in ["list", "stat", "file"] {
                let r = host
                    .get(&format!("/v1/shares/{MAIN}/{route}?path={}", enc(p)))
                    .await;
                assert_eq!(
                    (r.status, r.code().as_str()),
                    (404, "not_found"),
                    "{route} {p}"
                );
            }
        }
        let refusals = host
            .audit
            .recent(500)
            .into_iter()
            .filter(|e| e.event == "access" && format!("{:?}", e.detail).contains("reparse_point"))
            .count();
        assert!(refusals >= 1, "junction refusals are audited");
    } else {
        println!("SKIPPED  junction assertions: no junction fixture");
    }
    // Hidden pattern: unresolvable, not only unlisted.
    let r = host
        .get(&format!("/v1/shares/{MAIN}/stat?path=.git%2Fconfig"))
        .await;
    assert_eq!(r.code(), "not_found");
    // Unicode and '+' paths resolve when percent-encoded once.
    for n in UNICODE_NAMES {
        let r = host
            .get(&format!(
                "/v1/shares/{MAIN}/stat?path={}",
                enc(&format!("docs/{n}"))
            ))
            .await;
        assert_eq!(r.status, 200, "{n:?}");
        assert_eq!(r.json()["name"], n);
    }
    let r = host
        .get(&format!("/v1/shares/{MAIN}/stat?path=docs/report+v1.txt"))
        .await;
    assert_eq!(r.status, 200, "a literal '+' is a plus, not a space");
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/file?path={}",
            enc("docs/\u{e9}.txt")
        ))
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "\u{e9}.txt".as_bytes());
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"_.txt\"; filename*=UTF-8''%C3%A9.txt")
    );
    // Double-encoded traversal stays a literal name; invalid UTF-8 is bad_path.
    let r = host
        .get(&format!("/v1/shares/{MAIN}/stat?path=%252e%252e"))
        .await;
    assert_eq!(r.code(), "not_found");
    let r = host.get(&format!("/v1/shares/{MAIN}/stat?path=%FF")).await;
    assert_eq!(r.code(), "bad_path");
    let r = host
        .get(&format!("/v1/shares/{MAIN}/stat?path=docs%2F..%2F.."))
        .await;
    assert_eq!(r.code(), "bad_path");
}

#[tokio::test(flavor = "multi_thread")]
async fn stat_with_meta() {
    let host = start(Setup::default()).await;
    let dir = TempDir::new("meta");
    // A 3x2 PNG header (only the header is read).
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&3u32.to_be_bytes());
    png.extend_from_slice(&2u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    fs::write(dir.0.join("pic.png"), &png).unwrap();
    let mut config = HostConfig::default();
    config
        .shares
        .push(share("shr_meta", &dir.0, &host.device, &[Permission::List]));
    host.api.set_config(config);
    let r = host
        .get("/v1/shares/shr_meta/stat?path=pic.png&meta=1")
        .await;
    assert_eq!(r.status, 200);
    let v = r.json();
    assert_eq!(v["meta"]["image"]["width"], 3);
    assert_eq!(v["meta"]["image"]["height"], 2);
    assert_eq!(v["mime"], "image/png");
    let r = host.get("/v1/shares/shr_meta/stat?path=pic.png").await;
    assert!(r.json().get("meta").is_none());
    let r = host.get("/v1/shares/shr_meta/stat?path=").await;
    assert_eq!(
        (r.json()["kind"].as_str(), r.json()["name"].as_str()),
        (Some("dir"), Some(""))
    );
    let r = host
        .get("/v1/shares/shr_meta/stat?path=pic.png&meta=2")
        .await;
    assert_eq!(r.code(), "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn range_requests_and_validators() {
    let host = start(Setup::default()).await;
    let data: Vec<u8> = (0..DATA_LEN).map(data_byte).collect();
    let uri = format!("/v1/shares/{MAIN}/file?path=data.bin");

    // Full.
    let r = host.get(&uri).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body, data);
    assert_eq!(
        r.header("content-length"),
        Some(DATA_LEN.to_string().as_str())
    );
    assert_eq!(r.header("accept-ranges"), Some("bytes"));
    assert_eq!(r.header("content-type"), Some("application/octet-stream"));
    assert!(r.header("last-modified").unwrap().ends_with(" GMT"));
    assert_eq!(
        r.header("content-disposition"),
        Some("attachment; filename=\"data.bin\"; filename*=UTF-8''data.bin")
    );
    let etag = r.header("etag").unwrap().to_owned();
    assert!(etag.starts_with('"'), "strong ETag: {etag}");

    // Partial, open-ended, suffix, clamp.
    for (range, start, end) in [
        ("bytes=10-19", 10, 19),
        ("bytes=1048000-", 1_048_000, DATA_LEN - 1),
        ("bytes=-100", DATA_LEN - 100, DATA_LEN - 1),
        ("bytes=1048570-9999999", 1_048_570, DATA_LEN - 1),
        ("bytes=0-0", 0, 0),
    ] {
        let r = host.req("GET", &uri, &[("Range", range)]).await;
        assert_eq!(r.status, 206, "{range}");
        assert_eq!(r.body, &data[start..=end], "{range}");
        assert_eq!(
            r.header("content-range"),
            Some(format!("bytes {start}-{end}/{DATA_LEN}").as_str())
        );
        assert_eq!(
            r.header("content-length"),
            Some((end - start + 1).to_string().as_str())
        );
    }
    // Multipart and unsatisfiable -> 416 with the size.
    for range in [
        "bytes=0-1,5-6",
        "bytes=1048576-",
        "bytes=-0",
        "bytes=9-1",
        "bytes=abc",
    ] {
        let r = host.req("GET", &uri, &[("Range", range)]).await;
        assert_eq!(r.status, 416, "{range}");
        assert_eq!(r.code(), "range_not_satisfiable");
        assert_eq!(
            r.header("content-range"),
            Some(format!("bytes */{DATA_LEN}").as_str())
        );
    }
    // Another unit is ignored.
    let r = host.req("GET", &uri, &[("Range", "items=0-1")]).await;
    assert_eq!((r.status, r.body.len()), (200, DATA_LEN));
    // HEAD: same headers, no body.
    let r = host.req("HEAD", &uri, &[("Range", "bytes=0-9")]).await;
    assert_eq!(r.status, 206);
    assert!(r.body.is_empty());
    assert_eq!(r.header("content-length"), Some("10"));
    // If-Range with the current ETag resumes.
    let r = host
        .req("GET", &uri, &[("Range", "bytes=100-"), ("If-Range", &etag)])
        .await;
    assert_eq!(r.status, 206);
    // A folder is not a file.
    let r = host.get(&format!("/v1/shares/{MAIN}/file?path=docs")).await;
    assert_eq!(r.code(), "bad_path");
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_file_gives_412() {
    let host = start(Setup::default()).await;
    let dir = TempDir::new("changed");
    fs::write(dir.0.join("doc.bin"), vec![7u8; 4096]).unwrap();
    let mut config = HostConfig::default();
    config
        .shares
        .push(share("shr_chg", &dir.0, &host.device, &[Permission::Fetch]));
    host.api.set_config(config);
    let uri = "/v1/shares/shr_chg/file?path=doc.bin";
    let r = host.req("GET", uri, &[("Range", "bytes=0-1023")]).await;
    assert_eq!(r.status, 206);
    let etag = r.header("etag").unwrap().to_owned();
    let last_modified = r.header("last-modified").unwrap().to_owned();
    // The file changes on the host between two parts of a resumed download.
    fs::write(dir.0.join("doc.bin"), vec![8u8; 5000]).unwrap();
    let r = host
        .req("GET", uri, &[("Range", "bytes=1024-"), ("If-Range", &etag)])
        .await;
    assert_eq!((r.status, r.code().as_str()), (412, "file_changed"));
    let r = host.req("GET", uri, &[("If-Match", &etag)]).await;
    assert_eq!((r.status, r.code().as_str()), (412, "file_changed"));
    let r = host
        .req(
            "GET",
            uri,
            &[("Range", "bytes=1024-"), ("If-Range", "W/\"weak\"")],
        )
        .await;
    assert_eq!(r.status, 412, "a weak validator never matches If-Range");
    let _ = last_modified;
    let r = host.get(uri).await;
    assert_eq!((r.status, r.body.len()), (200, 5000));
    assert_ne!(r.header("etag"), Some(etag.as_str()));
    let fetches = host
        .audit
        .recent(500)
        .into_iter()
        .filter(|e| e.event == "access" && format!("{:?}", e.detail).contains("fetch"))
        .count();
    assert!(fetches >= 3, "fetches and refusals are audited");
}

#[tokio::test(flavor = "multi_thread")]
async fn hash_inline_and_in_the_background() {
    let host = start(Setup::default()).await;
    let data: Vec<u8> = (0..DATA_LEN).map(data_byte).collect();
    let uri = format!("/v1/shares/{MAIN}/hash?path=data.bin&algo=blake3");
    let r = host.get(&uri).await;
    assert_eq!(r.status, 200);
    let v = r.json();
    assert_eq!(v["hex"], blake3::hash(&data).to_hex().as_str());
    assert_eq!(v["size"], DATA_LEN);
    assert_eq!(v["algo"], "blake3");
    let file_etag = host
        .req(
            "HEAD",
            &format!("/v1/shares/{MAIN}/file?path=data.bin"),
            &[],
        )
        .await
        .header("etag")
        .unwrap()
        .to_owned();
    assert_eq!(v["etag"], file_etag.as_str());
    let r = host
        .get(&format!("/v1/shares/{MAIN}/hash?path=data.bin&algo=sha256"))
        .await;
    let sha = ring::digest::digest(&ring::digest::SHA256, &data);
    let hex: String = sha.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(r.json()["hex"], hex.as_str());
    for q in ["algo=md5", ""] {
        let r = host
            .get(&format!("/v1/shares/{MAIN}/hash?path=data.bin&{q}"))
            .await;
        assert_eq!(r.code(), "bad_request", "{q}");
    }
    let r = host
        .get(&format!("/v1/shares/{MAIN}/hash?path=docs&algo=blake3"))
        .await;
    assert_eq!(r.code(), "bad_path", "a folder has no hash");

    // Everything in the background: 503 generating with Retry-After, then the digest.
    let bg = start(Setup {
        options: ApiOptions {
            hash_inline_max: 0,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let r = bg.get(&uri).await;
    assert_eq!((r.status, r.code().as_str()), (503, "generating"));
    assert!(r.json()["error"]["retry_after_ms"].as_u64().unwrap() > 0);
    assert!(r.header("retry-after").is_some());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let r = bg.get(&uri).await;
        if r.status == 200 {
            assert_eq!(r.json()["hex"], blake3::hash(&data).to_hex().as_str());
            break;
        }
        assert_eq!(r.status, 503);
        assert!(Instant::now() < deadline, "hash never finished");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn search_finds_everything_and_reports_partial() {
    let fix = fixture();
    let host = start(Setup::default()).await;
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/search?q=NEEDLE&path=tree&limit=500"
        ))
        .await;
    assert_eq!(r.status, 200);
    let v = r.json();
    assert_eq!(v["partial"], false);
    assert!(v["next_cursor"].is_null());
    let found: BTreeSet<String> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        found, fix.needles,
        "case-insensitive substring, whole subtree"
    );
    // Breadth first: the shallowest match comes first.
    assert!(
        v["entries"][0]["path"]
            .as_str()
            .unwrap()
            .starts_with("tree/lv1/Needle")
    );

    // Limit paging with a cursor.
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut uri = format!("/v1/shares/{MAIN}/search?q=needle&path=tree&limit=7");
        if let Some(c) = &cursor {
            uri.push_str(&format!("&cursor={c}"));
        }
        let v = host.get(&uri).await.json();
        assert_eq!(v["partial"], false);
        all.extend(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["path"].as_str().unwrap().to_owned()),
        );
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(all.len(), fix.needles.len(), "no duplicates");
    assert_eq!(all.into_iter().collect::<BTreeSet<_>>(), fix.needles);

    // Bad input.
    for q in ["q=", "q=a&cursor=bogus", "q=a&path=data.bin"] {
        let r = host.get(&format!("/v1/shares/{MAIN}/search?{q}")).await;
        assert!(r.status == 400, "{q}: {}", r.status);
    }

    // A zero time budget: every request stops after one directory and says `partial`; following
    // the cursor still finds every match exactly once.
    let slow = start(Setup {
        options: ApiOptions {
            search_budget: Duration::ZERO,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    let mut partial_seen = false;
    let mut requests = 0;
    loop {
        requests += 1;
        assert!(requests < 1000);
        let mut uri = format!("/v1/shares/{MAIN}/search?q=needle&path=tree&limit=500");
        if let Some(c) = &cursor {
            uri.push_str(&format!("&cursor={c}"));
        }
        let v = slow.get(&uri).await.json();
        partial_seen |= v["partial"] == true;
        all.extend(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["path"].as_str().unwrap().to_owned()),
        );
        match v["next_cursor"].as_str() {
            Some(c) => {
                assert_eq!(v["partial"], true, "stopped by the time bound");
                cursor = Some(c.to_owned());
            }
            None => break,
        }
    }
    assert!(partial_seen);
    assert!(requests > 2);
    assert_eq!(all.len(), fix.needles.len());
    assert_eq!(all.into_iter().collect::<BTreeSet<_>>(), fix.needles);

    // The 10k directory within the default 2 s budget.
    let r = host
        .get(&format!("/v1/shares/{MAIN}/search?q=f0999&path=big"))
        .await;
    let v = r.json();
    assert_eq!(v["partial"], false);
    assert_eq!(v["entries"].as_array().unwrap().len(), 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn search_cursor_survives_session_expiry() {
    let fix = fixture();
    let host = start(Setup {
        options: ApiOptions {
            search_session_ttl: Duration::ZERO,
            ..Default::default()
        },
        ..Default::default()
    })
    .await;
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut uri = format!("/v1/shares/{MAIN}/search?q=needle&path=tree&limit=10");
        if let Some(c) = &cursor {
            uri.push_str(&format!("&cursor={c}"));
        }
        // Another search in between evicts the expired session.
        let _ = host
            .get(&format!("/v1/shares/{MAIN}/search?q=hay&path=tree&limit=1"))
            .await;
        let v = host.get(&uri).await.json();
        all.extend(
            v["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["path"].as_str().unwrap().to_owned()),
        );
        match v["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(all.len(), fix.needles.len(), "re-walk skips what was sent");
    assert_eq!(all.into_iter().collect::<BTreeSet<_>>(), fix.needles);
}

#[tokio::test(flavor = "multi_thread")]
async fn recent_is_bounded_newest_first() {
    let host = start(Setup::default()).await;
    let r = host.get("/v1/recent?limit=2").await;
    assert_eq!(r.status, 200);
    let v = r.json();
    let paths: Vec<(&str, &str)> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["share"].as_str().unwrap(), e["path"].as_str().unwrap()))
        .collect();
    // recent4.txt (level 4) is newer but outside the top three levels.
    assert_eq!(paths, [(MAIN, "docs/recent2.txt"), (MAIN, "recent1.txt")]);
    assert_eq!(v["partial"], true, "deeper levels were not walked");
    let since = (time::OffsetDateTime::now_utc() + time::Duration::minutes(90))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    let r = host.get(&format!("/v1/recent?since={}", enc(&since))).await;
    let v = r.json();
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    assert_eq!(v["entries"][0]["name"], "recent2.txt");
    let r = host.get("/v1/recent?since=yesterday").await;
    assert_eq!(r.code(), "bad_request");
    // Cached: a new file does not show up until the cache expires.
    let r = host.get("/v1/recent?limit=500").await;
    assert_eq!(r.json()["entries"].as_array().unwrap().len(), 500);
}

#[tokio::test(flavor = "multi_thread")]
async fn permissions_and_unknown_shares() {
    let host = start(Setup::default()).await;
    // A share without a grant does not exist for this device.
    let r = host.get(&format!("/v1/shares/{OTHER}/list")).await;
    assert_eq!((r.status, r.code().as_str()), (404, "share_not_found"));
    let r = host.get("/v1/shares/shr_nope/list").await;
    assert_eq!(r.code(), "share_not_found");
    // List only: fetch, hash, thumb and preview are refused, naming the permission.
    let r = host.get(&format!("/v1/shares/{LIST_ONLY}/list")).await;
    assert_eq!(names(&r.json()), ["a.txt"]);
    for (route, perm) in [
        ("file", "fetch"),
        ("hash", "fetch"),
        ("thumb", "preview"),
        ("preview/info", "preview"),
    ] {
        let r = host
            .get(&format!(
                "/v1/shares/{LIST_ONLY}/{route}?path=a.txt&algo=blake3"
            ))
            .await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (403, "permission_denied"),
            "{route}"
        );
        assert!(
            r.json()["error"]["message"]
                .as_str()
                .unwrap()
                .contains(perm)
        );
    }
    // Stream-only share: no listing.
    let r = host.get(&format!("/v1/shares/{STREAM}/list")).await;
    assert_eq!(r.code(), "permission_denied");
    let refused = host
        .audit
        .recent(500)
        .into_iter()
        .filter(|e| e.event == "access" && format!("{:?}", e.detail).contains("permission_denied"))
        .count();
    assert!(refused >= 5);
}

#[tokio::test(flavor = "multi_thread")]
async fn thumbnails_and_previews() {
    let host = start(Setup::default()).await;
    let uri = format!("/v1/shares/{MAIN}/thumb?path=big%2Ff00002.jpg&size=128&priority=visible");
    let deadline = Instant::now() + Duration::from_secs(10);
    let r = loop {
        let r = host.get(&uri).await;
        if r.status == 200 {
            break r;
        }
        assert_eq!((r.status, r.code().as_str()), (503, "generating"));
        assert!(r.header("retry-after").is_some());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(r.header("content-type"), Some("image/webp"));
    assert_eq!(
        r.header("cache-control"),
        Some("private, max-age=31536000, immutable")
    );
    let etag = r.header("etag").unwrap().to_owned();
    let r = host.req("GET", &uri, &[("If-None-Match", &etag)]).await;
    assert_eq!(r.status, 304);
    for (q, code) in [
        ("path=big%2Ff00002.jpg&size=100", "bad_request"),
        ("path=big%2Ff00002.jpg&priority=now", "bad_request"),
        ("path=big%2Ff00000.txt", "unsupported_type"),
        ("path=big", "unsupported_type"),
    ] {
        let r = host.get(&format!("/v1/shares/{MAIN}/thumb?{q}")).await;
        assert_eq!(r.code(), code, "{q}");
    }

    // Text previews run in-process (no worker needed).
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/preview/text?path={}",
            enc("docs/report+v1.txt")
        ))
        .await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.body, b"report+v1.txt");
    assert_eq!(r.header("content-type"), Some("text/plain; charset=utf-8"));
    assert_eq!(r.header("x-gander-text-encoding"), Some("utf-8"));
    assert_eq!(r.header("x-gander-text-total"), Some("13"));
    assert_eq!(r.header("x-gander-text-next"), Some("13"));
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/preview/info?path=big%2Ff00000.txt"
        ))
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["kind"], "text");
    let r = host
        .get(&format!("/v1/shares/{MAIN}/preview/info?path=data.bin"))
        .await;
    assert_eq!((r.status, r.json()["kind"].as_str()), (200, Some("none")));
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/preview?path=big%2Ff00000.txt&format=gif"
        ))
        .await;
    assert_eq!(r.code(), "bad_request");
    let r = host
        .get(&format!("/v1/shares/{MAIN}/preview/text?path=data.bin"))
        .await;
    assert_eq!(r.code(), "unsupported_type", "binary sniff");
}

/// Deep-zoom tiles (WP-67): what the route itself decides. Rendering a real tile is covered by
/// the `gander-render` golden tests and the `e2e-host` suite.
#[tokio::test(flavor = "multi_thread")]
async fn preview_tile_validates_its_request() {
    let host = start(Setup::default()).await;
    let tile = |q: &str| format!("/v1/shares/{MAIN}/preview/tile?{q}");
    for (q, code) in [
        // level, x and y are required numbers.
        ("path=big%2Ff00002.jpg&x=0&y=0", "bad_request"),
        ("path=big%2Ff00002.jpg&level=0&y=0", "bad_request"),
        ("path=big%2Ff00002.jpg&level=0&x=0", "bad_request"),
        ("path=big%2Ff00002.jpg&level=abc&x=0&y=0", "bad_request"),
        ("path=big%2Ff00002.jpg&level=-1&x=0&y=0", "bad_request"),
        // Nothing has more than 7 levels, a page is numbered from 1.
        ("path=big%2Ff00002.jpg&level=9&x=0&y=0", "bad_request"),
        (
            "path=big%2Ff00002.jpg&level=0&x=0&y=0&page=0",
            "bad_request",
        ),
        (
            "path=big%2Ff00002.jpg&level=0&x=0&y=0&page=2",
            "bad_request",
        ),
        // No tiles for text and folders.
        ("path=big%2Ff00000.txt&level=0&x=0&y=0", "unsupported_type"),
        ("path=big&level=0&x=0&y=0", "unsupported_type"),
        ("path=data.bin&level=0&x=0&y=0", "unsupported_type"),
        ("path=nope.png&level=0&x=0&y=0", "not_found"),
    ] {
        let r = host.get(&tile(q)).await;
        assert_eq!(r.code(), code, "{q}: {}", String::from_utf8_lossy(&r.body));
    }
    // The message for a tile that does not exist names no path.
    let r = host
        .get(&tile("path=big%2Ff00002.jpg&level=9&x=0&y=0"))
        .await;
    assert_eq!(
        r.json()["error"]["message"].as_str(),
        Some("tile out of range")
    );
    // A list-only share has no preview permission, so no tiles either.
    let r = host
        .get(&format!(
            "/v1/shares/{LIST_ONLY}/preview/tile?path=a.txt&level=0&x=0&y=0"
        ))
        .await;
    assert_eq!((r.status, r.code().as_str()), (403, "permission_denied"));
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_tile_without_a_render_worker_is_unsupported() {
    let host = start(Setup {
        no_worker: true,
        ..Setup::default()
    })
    .await;
    let r = host
        .get(&format!(
            "/v1/shares/{MAIN}/preview/tile?path=big%2Ff00002.jpg&level=0&x=0&y=0"
        ))
        .await;
    assert_eq!((r.status, r.code().as_str()), (415, "unsupported_type"));
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_requests_run_the_policy() {
    let host = start(Setup::default()).await;
    let body = |share: &str, path: &str| {
        serde_json::json!({
            "share": share, "path": path, "app": null,
            "viewport": {"width": 1080, "height": 2280, "dpr": 2.625}
        })
        .to_string()
    };
    let post = |b: String| {
        let app = host.app.clone();
        let token = host.token.clone();
        async move { send(&app, "POST", "/v1/stream", Some(&token), &[], &b).await }
    };
    // Stream permission, handler known to the OS but not approved: policy says ask, Phase 1
    // answers stream_unavailable.
    let r = post(body(STREAM, "a.txt")).await;
    assert_eq!((r.status, r.code().as_str()), (503, "stream_unavailable"));
    // No stream permission on the share.
    let r = post(body(MAIN, "docs/report+v1.txt")).await;
    assert_eq!((r.status, r.code().as_str()), (403, "permission_denied"));
    // Unknown share, bad path, bad body.
    assert_eq!(post(body(OTHER, "a.txt")).await.code(), "share_not_found");
    assert_eq!(post(body(STREAM, "../a.txt")).await.code(), "bad_path");
    assert_eq!(post(body(STREAM, "missing.txt")).await.code(), "not_found");
    assert_eq!(post("{}".to_owned()).await.code(), "bad_request");
}

/// The assembled routes over the real TLS listener (127.0.0.1, port 0): a ranged download is
/// streamed through the connection, the limiter and the gate.
#[tokio::test(flavor = "multi_thread")]
async fn ranged_fetch_over_tls() {
    let host = start(Setup::default()).await;
    let plan = ListenerPlan::new(TrustAdapterKind::Generic, 0, vec![LOOPBACK], vec![]).unwrap();
    let mut options = ServerOptions::new(host.identity.clone(), Arc::new(FixedPlan(plan)));
    options.watch_address_changes = false;
    options.authenticator = host.svc.authenticator();
    let routes = host_routes(host.identity.host_id().clone(), &host.svc, &host.api);
    let server = Server::start(options, routes).await.unwrap();
    let addr = server.bound_addrs()[0];
    assert_eq!(addr.ip(), LOOPBACK);

    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let connector = tokio_rustls::TlsConnector::from(
        pinned_client_config(host.identity.host_id().clone()).unwrap(),
    );
    let mut tls = connector
        .connect(
            rustls::pki_types::ServerName::IpAddress(addr.ip().into()),
            tcp,
        )
        .await
        .unwrap();
    let req = format!(
        "GET /v1/shares/{MAIN}/file?path=data.bin HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nRange: bytes=-65536\r\nConnection: close\r\n\r\n",
        host.token
    );
    tls.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tls.read_to_end(&mut buf).await;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let body = &buf[split + 4..];
    assert!(head.starts_with("HTTP/1.1 206"), "{head}");
    assert!(head.to_ascii_lowercase().contains("x-gander-protocol: 1"));
    let data: Vec<u8> = (0..DATA_LEN).map(data_byte).collect();
    assert_eq!(body, &data[DATA_LEN - 65_536..]);
    server.shutdown().await;
}
