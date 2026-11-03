// SPDX-License-Identifier: Apache-2.0
//! WP-66 integration tests: `/search` answered from the Windows Search index, with a FAKE index
//! behind the real gate, token authenticator, path resolver and listing filter.
//!
//! The fake feeds the host rows the way the real index would (`file:C:/real/path/below/the/share`)
//! plus hostile ones (`..`, a junction, alternate streams, hidden files, other folders), so what is
//! tested is what the host does with an index it must not trust: nothing that the resolver or the
//! listing filter refuses may come back. The real index is exercised by
//! `host/gander-win/tests/search_index.rs` (`#[ignore]`, needs an indexed folder).

#![cfg(windows)]

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::ControlFlow;
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use gander_core::config::{HostConfig, ShareConfig, ShareGrant};
use gander_core::pairing::b64url;
use gander_core::policy::StaticAssociations;
use gander_core::protocol::files::{Permission, StreamDecision};
use gander_core::protocol::host::TrustAdapterKind;
use gander_core::protocol::pairing::{PairApproved, PairStarted};
use gander_core::protocol::{DeviceId, ShareId};
use gander_core::search::IndexScope;
use gander_host::api::{
    ApiOptions, ApiState, IndexBackend, IndexFailure, IndexRun, IndexStatus, host_routes,
};
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
const NO_GRANT: &str = "shr_nogrant";

// ---------------------------------------------------------------------------------------------
// Fake index

/// A row the fake index returns: below the searched folder, or verbatim.
#[derive(Debug, Clone)]
enum Row {
    /// `<scope url>/<text>`.
    Below(String),
    /// Returned as is (another folder, another scheme).
    Raw(String),
}

fn below(text: &str) -> Row {
    Row::Below(text.to_owned())
}

#[derive(Debug)]
struct FakeIndex {
    rows: Mutex<Vec<Row>>,
    crawled: AtomicBool,
    broken: AtomicBool,
    catching_up: AtomicBool,
    /// Every statement the host sent, and the `skip` of each run.
    sql: Mutex<Vec<String>>,
    skips: Mutex<Vec<u64>>,
    /// The folders the host asked about, as the index would see them.
    scopes: Mutex<Vec<IndexScope>>,
    status_calls: AtomicU64,
}

impl FakeIndex {
    fn new(rows: Vec<Row>) -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(rows),
            crawled: AtomicBool::new(true),
            broken: AtomicBool::new(false),
            catching_up: AtomicBool::new(false),
            sql: Mutex::new(Vec::new()),
            skips: Mutex::new(Vec::new()),
            scopes: Mutex::new(Vec::new()),
            status_calls: AtomicU64::new(0),
        })
    }
}

impl IndexBackend for FakeIndex {
    fn status(&self, _dir: &IndexScope) -> Result<IndexStatus, IndexFailure> {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        if self.broken.load(Ordering::SeqCst) {
            return Err(IndexFailure("service stopped".into()));
        }
        Ok(IndexStatus {
            crawled: self.crawled.load(Ordering::SeqCst),
            catching_up: self.catching_up.load(Ordering::SeqCst),
        })
    }

    fn run(
        &self,
        dir: &IndexScope,
        sql: &str,
        skip: u64,
        visit: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Option<IndexRun>, IndexFailure> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(IndexFailure("service stopped".into()));
        }
        if !self.crawled.load(Ordering::SeqCst) {
            return Ok(None);
        }
        self.sql.lock().unwrap().push(sql.to_owned());
        self.skips.lock().unwrap().push(skip);
        self.scopes.lock().unwrap().push(dir.clone());
        let rows = self.rows.lock().unwrap().clone();
        let mut consumed = 0u64;
        for row in rows.iter().skip(usize::try_from(skip).unwrap()) {
            let url = match row {
                Row::Below(t) => format!("{}/{t}", dir.query_url),
                Row::Raw(u) => u.clone(),
            };
            if visit(&url).is_break() {
                return Ok(Some(IndexRun {
                    catching_up: self.catching_up.load(Ordering::SeqCst),
                    consumed,
                    stopped: true,
                }));
            }
            consumed += 1;
        }
        Ok(Some(IndexRun {
            catching_up: self.catching_up.load(Ordering::SeqCst),
            consumed,
            stopped: false,
        }))
    }
}

// ---------------------------------------------------------------------------------------------
// Fixture

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp66-{tag}-{}", ulid::Ulid::new()));
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

fn junction(link: &Path, target: &Path) -> bool {
    Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct Fixture {
    dir: TempDir,
    junction: bool,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new("fixture");
        let share = dir.0.join("share");
        let outside = dir.0.join("outside");
        let docs = share.join("docs");
        let lv1 = share.join("tree").join("lv1");
        for d in [
            docs.clone(),
            lv1.join("side0"),
            share.join(".git"),
            outside.clone(),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(outside.join("secret.txt"), b"outside the share").unwrap();
        fs::write(share.join(".git").join("config"), b"[core]").unwrap();
        fs::write(share.join("data.bin"), b"0123456789").unwrap();
        fs::write(docs.join("recent2.txt"), b"22").unwrap();
        fs::write(docs.join("\u{c9}t\u{e9} r\u{e9}sum\u{e9}.txt"), b"unicode").unwrap();
        fs::write(docs.join("desktop.ini"), b"[.ShellClassInfo]").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .attributes(0x2) // FILE_ATTRIBUTE_HIDDEN
            .open(docs.join("hidden.txt"))
            .unwrap();
        fs::write(lv1.join("Needle-1-0.txt"), b"nnn").unwrap();
        fs::write(lv1.join("hay-1-0.txt"), b"h").unwrap();
        fs::write(lv1.join("side0").join("x-needle-1-0.txt"), b"n").unwrap();
        for i in 0..5 {
            fs::write(docs.join(format!("page{i}.txt")), format!("{i}")).unwrap();
        }
        // A hidden folder: everything below it is hidden too.
        let hidden_dir = share.join("hiddendir");
        fs::create_dir_all(&hidden_dir).unwrap();
        fs::write(hidden_dir.join("inside.txt"), b"x").unwrap();
        // FILE_ATTRIBUTE_HIDDEN on a directory: `attrib +h`.
        let _ = Command::new("attrib").arg("+h").arg(&hidden_dir).output();
        let escape = junction(&docs.join("escape"), &outside);
        // A junction to a folder INSIDE the share: still refused (links are never followed).
        let _ = junction(&docs.join("alias"), &lv1);
        Self {
            dir,
            junction: escape,
        }
    }

    fn share(&self) -> PathBuf {
        self.dir.0.join("share")
    }
}

// ---------------------------------------------------------------------------------------------
// Host under test

fn store() -> SharedStore {
    Arc::new(gander_win::config_store::WindowsConfigStore)
}

struct Host {
    _dir: TempDir,
    app: Router,
    api: Arc<ApiState>,
    token: String,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
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

async fn send(app: &Router, method: &str, uri: &str, auth: Option<&str>, body: &str) -> Resp {
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
    let mut req = b.body(axum::body::Body::from(body.to_owned())).unwrap();
    req.extensions_mut().insert(conn());
    req.extensions_mut().insert(ConnHandle::new());
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = axum::body::to_bytes(resp.into_body(), 16 << 20)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    Resp { status, body }
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

async fn start(fix: &Fixture, index: Option<Arc<FakeIndex>>, options: ApiOptions) -> Host {
    let backend: Option<Arc<dyn IndexBackend>> = index.map(|i| i as Arc<dyn IndexBackend>);
    start_at(&fix.share(), backend, options).await
}

/// The host over a share rooted at `root`, searching through `backend`.
async fn start_at(
    root: &Path,
    backend: Option<Arc<dyn IndexBackend>>,
    options: ApiOptions,
) -> Host {
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
        .with_options(options)
        .with_associations(Arc::new(StaticAssociations::new()))
        .with_search_index(backend),
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
    let r = send(&app, "POST", "/v1/pair", None, &body).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let started: PairStarted = serde_json::from_slice(&r.body).unwrap();
    svc.approve(&started.pairing_id).unwrap();
    let r = send(
        &app,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        None,
        "",
    )
    .await;
    assert_eq!(r.status, 200);
    let approved: PairApproved = serde_json::from_slice(&r.body).unwrap();
    let device = approved.device_id.clone();

    use Permission::*;
    api.set_config(HostConfig {
        shares: vec![
            share_cfg(MAIN, root, &device, &[List]),
            share_cfg(NO_GRANT, root, &device, &[]),
        ],
        ..Default::default()
    });
    Host {
        _dir: dir,
        app,
        api,
        token: approved.token.expose().to_owned(),
    }
}

impl Host {
    async fn get(&self, uri: &str) -> Resp {
        send(&self.app, "GET", uri, Some(&self.token), "").await
    }

    /// `/search` on the main share; `extra` is appended to the query string.
    async fn search(&self, extra: &str) -> Value {
        let r = self.get(&format!("/v1/shares/{MAIN}/search?{extra}")).await;
        assert_eq!(
            r.status,
            200,
            "{extra}: {}",
            String::from_utf8_lossy(&r.body)
        );
        r.json()
    }
}

fn paths(v: &Value) -> Vec<String> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Tests

#[tokio::test(flavor = "multi_thread")]
async fn shares_and_me_advertise_the_index() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;

    let shares = host.get("/v1/shares").await.json();
    let list = shares["shares"].as_array().unwrap();
    assert_eq!(
        list.len(),
        1,
        "only the share the device holds a permission on"
    );
    assert_eq!(list[0]["id"], MAIN);
    assert_eq!(list[0]["search"], "indexed");
    let me = host.get("/v1/me").await.json();
    assert_eq!(me["capabilities"]["search"], "indexed");

    // The folder leaves the crawl scope: after the cache time the share says so. The cache is
    // dropped by a config change, which is what the tray does.
    index.crawled.store(false, Ordering::SeqCst);
    let again = host.get("/v1/shares").await.json();
    assert_eq!(
        again["shares"][0]["search"], "indexed",
        "cached for a short while"
    );
    host.api
        .set_config(host.api.current_config().as_ref().clone());
    let shares = host.get("/v1/shares").await.json();
    assert_eq!(shares["shares"][0]["search"], true);
    let me = host.get("/v1/me").await.json();
    assert_eq!(me["capabilities"]["search"], true);

    // A failing service is the walk, not an error.
    index.broken.store(true, Ordering::SeqCst);
    host.api
        .set_config(host.api.current_config().as_ref().clone());
    let shares = host.get("/v1/shares").await.json();
    assert_eq!(shares["shares"][0]["search"], true);
    assert_eq!(
        host.get("/v1/me").await.json()["capabilities"]["search"],
        true
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_index_means_the_walk_everywhere() {
    let fix = Fixture::new();
    let host = start(&fix, None, ApiOptions::default()).await;
    let shares = host.get("/v1/shares").await.json();
    assert_eq!(shares["shares"][0]["search"], true);
    assert_eq!(
        host.get("/v1/me").await.json()["capabilities"]["search"],
        true
    );
    let v = host.search("q=needle").await;
    assert_eq!(v["via"], "walk");
    assert_eq!(paths(&v).len(), 2, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn index_rows_are_mapped_through_the_resolver_and_the_listing_rules() {
    let fix = Fixture::new();
    let mut rows = vec![
        below("tree/lv1/Needle-1-0.txt"),
        below("docs/recent2.txt"),
        below("docs/\u{c9}t\u{e9} r\u{e9}sum\u{e9}.txt"),
        below("tree/lv1/side0"),
    ];
    // What the index must never be able to smuggle in.
    let hostile = [
        // Path tricks.
        "../outside/secret.txt",
        "docs/../data.bin",
        "docs/../../outside/secret.txt",
        "./docs/recent2.txt",
        "docs//recent2.txt",
        "docs/recent2.txt/",
        "/docs/recent2.txt",
        "docs\\recent2.txt",
        "docs/recent2.txt/child",
        "docs/recent2.txt:stream",
        "docs/recent2.txt:$DATA",
        "docs/recent2.txt::$DATA",
        "docs/recent2.txt.",
        "docs/recent2.txt ",
        "docs/con.txt",
        "docs/nul",
        "docs/RECENT~1.TXT",
        "docs/RECENT~1.TXT/",
        "docs/a*b",
        "docs/a?b",
        "docs/a|b",
        "docs/nonexistent.txt",
        "",
        "docs/",
        // Listing rules.
        "docs/hidden.txt",
        "docs/desktop.ini",
        "docs/DESKTOP.INI",
        ".git/config",
        ".GIT/config",
        "hiddendir/inside.txt",
        "hiddendir",
        // Links.
        "docs/escape/secret.txt",
        "docs/escape",
        "docs/alias/Needle-1-0.txt",
        "docs/alias",
    ];
    rows.extend(hostile.iter().map(|t| below(t)));
    rows.extend([
        Row::Raw("file:C:/Windows/win.ini".into()),
        Row::Raw("file:C:/".into()),
        Row::Raw("file:".into()),
        Row::Raw("".into()),
        Row::Raw("C:/Windows/win.ini".into()),
        Row::Raw("http://example.com/docs/recent2.txt".into()),
        Row::Raw("file:///C:/Windows/win.ini".into()),
    ]);
    // A row that names a folder of the share the folder being searched is NOT below.
    let index = FakeIndex::new(rows);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;

    // Content scope: the index decides what matches, so only the host's own rules filter.
    let v = host.search("q=anything&scope=content&limit=500").await;
    assert_eq!(v["via"], "index");
    let got = paths(&v);
    let mut want = vec![
        "tree/lv1/Needle-1-0.txt".to_owned(),
        "docs/recent2.txt".to_owned(),
        "docs/\u{c9}t\u{e9} r\u{e9}sum\u{e9}.txt".to_owned(),
        "tree/lv1/side0".to_owned(),
    ];
    // The junction rows are refused; without mklink they simply do not exist.
    println!("junction fixture: {}", fix.junction);
    want.sort();
    let mut got_sorted = got.clone();
    got_sorted.sort();
    assert_eq!(got_sorted, want, "{v}");
    assert_eq!(v["next_cursor"], Value::Null);
    assert_eq!(v["partial"], false);

    // The entries come from the files, not from the index.
    let e = &v["entries"][0];
    assert_eq!(e["path"], "tree/lv1/Needle-1-0.txt");
    assert_eq!(e["name"], "Needle-1-0.txt");
    assert_eq!(e["kind"], "file");
    assert_eq!(e["size"], 3);
    assert_eq!(e["ext"], "txt");
    assert!(e["mtime"].as_str().unwrap().ends_with('Z'));
    let dir = v["entries"][3].clone();
    assert_eq!(dir["kind"], "dir");
    assert_eq!(dir["size"], 0);
    // Nothing the host does not list leaks, whatever the spelling.
    for p in &got {
        let lower = p.to_lowercase();
        for banned in [
            "secret",
            "hidden",
            "desktop.ini",
            ".git",
            "escape",
            "alias",
            "win.ini",
            "data.bin",
            "outside",
            "..",
            ":",
            "~",
            "\\",
        ] {
            assert!(!lower.contains(banned), "{p} contains {banned}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_case_variant_resolves_to_the_stored_spelling() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![below("DOCS/RECENT2.TXT"), below("Docs/Page0.TXT")]);
    let host = start(&fix, Some(index), ApiOptions::default()).await;
    let v = host.search("q=anything&scope=content").await;
    assert_eq!(paths(&v), ["docs/recent2.txt", "docs/page0.txt"]);
    assert_eq!(v["entries"][0]["name"], "recent2.txt");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_name_search_checks_the_stored_name() {
    let fix = Fixture::new();
    // `LIKE` over-matches (and the index may hold an old name): only real name matches stay.
    let index = FakeIndex::new(vec![
        below("tree/lv1/Needle-1-0.txt"),
        below("tree/lv1/hay-1-0.txt"),
        below("tree/lv1/side0/x-needle-1-0.txt"),
        below("docs/recent2.txt"),
    ]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    let v = host.search("q=NeEdLe").await;
    assert_eq!(v["via"], "index");
    assert_eq!(
        paths(&v),
        ["tree/lv1/Needle-1-0.txt", "tree/lv1/side0/x-needle-1-0.txt"]
    );
    // The statement is a name query over the folder's REAL path, with the lower-cased text.
    let sql = index.sql.lock().unwrap()[0].clone();
    assert!(sql.contains("System.FileName LIKE '%needle%'"), "{sql}");
    assert!(!sql.contains("CONTAINS"), "{sql}");
    let scope = index.scopes.lock().unwrap()[0].clone();
    let real = fs::canonicalize(fix.share()).unwrap();
    let real = real
        .to_str()
        .unwrap()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    assert_eq!(scope.query_url, format!("file:{real}"));
    assert!(sql.contains(&format!("SCOPE='file:{real}'")), "{sql}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_content_search_sends_the_full_text_condition() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![below("docs/recent2.txt")]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    let v = host.search("q=Budget%20plan&scope=content").await;
    assert_eq!(v["via"], "index");
    assert_eq!(paths(&v), ["docs/recent2.txt"]);
    let sql = index.sql.lock().unwrap()[0].clone();
    assert!(
        sql.contains("CONTAINS(System.Search.Contents, '\"budget*\" AND \"plan*\"')"),
        "{sql}"
    );
    assert!(sql.contains("ORDER BY System.Search.Rank DESC"), "{sql}");
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_search_text_never_reaches_the_sql_unescaped() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    for q in [
        "x%27%20OR%201%3D1%20--",
        "%27%3B%20DROP%20TABLE%20SystemIndex%3B%20--",
        "%22%20OR%20%22%22%3D%22",
        "a%25b_c%5Bd",
    ] {
        for scope in ["name", "content"] {
            let _ = host.search(&format!("q={q}&scope={scope}")).await;
        }
    }
    let statements = index.sql.lock().unwrap().clone();
    assert_eq!(statements.len(), 8);
    for sql in statements {
        // Remove every complete literal; what is left is the fixed statement.
        let mut skeleton = String::new();
        let mut it = sql.chars().peekable();
        while let Some(c) = it.next() {
            if c != '\'' {
                skeleton.push(c);
                continue;
            }
            loop {
                match it.next() {
                    Some('\'') if it.peek() == Some(&'\'') => {
                        it.next();
                    }
                    Some('\'') => break,
                    Some(_) => {}
                    None => panic!("unterminated literal: {sql}"),
                }
            }
            skeleton.push_str("<>");
        }
        assert!(
            skeleton.starts_with("SELECT TOP ")
                && skeleton.contains(" System.ItemUrl FROM SystemIndex WHERE SCOPE=<> AND ")
                && skeleton.matches("System.FileName LIKE <>").count() == 1
                && !skeleton.contains("DROP")
                && !skeleton.contains("1=1"),
            "{sql} -> {skeleton}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn control_characters_and_long_text_are_answered_by_the_walk() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![below("docs/recent2.txt")]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    let v = host.search("q=a%00b").await;
    assert_eq!(v["via"], "walk");
    let v = host.search("q=a%0Ab").await;
    assert_eq!(v["via"], "walk");
    let v = host.search(&format!("q={}", "a".repeat(200))).await;
    assert_eq!(v["via"], "walk");
    assert!(index.sql.lock().unwrap().is_empty(), "nothing was sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn paging_counts_index_rows_and_binds_the_cursor_to_the_query() {
    let fix = Fixture::new();
    // Rejected rows sit between the good ones: they are consumed, never returned.
    let index = FakeIndex::new(vec![
        below("docs/nonexistent-a.txt"),
        below("docs/page0.txt"),
        below("docs/hidden.txt"),
        below("docs/page1.txt"),
        below("docs/page2.txt"),
        below("docs/escape/secret.txt"),
        below("docs/page3.txt"),
        below("docs/page4.txt"),
    ]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    let base = "q=anything&scope=content&limit=2";

    let p1 = host.search(base).await;
    assert_eq!(paths(&p1), ["docs/page0.txt", "docs/page1.txt"]);
    let c1 = p1["next_cursor"].as_str().unwrap().to_owned();
    assert!(c1.starts_with("i1."), "{c1}");
    assert!(
        c1.ends_with(".4"),
        "rows 0..4 consumed, the next one only peeked: {c1}"
    );
    assert_eq!(p1["partial"], false);
    assert_eq!(p1["via"], "index");

    let p2 = host.search(&format!("{base}&cursor={c1}")).await;
    assert_eq!(paths(&p2), ["docs/page2.txt", "docs/page3.txt"]);
    let c2 = p2["next_cursor"].as_str().unwrap().to_owned();

    let p3 = host.search(&format!("{base}&cursor={c2}")).await;
    assert_eq!(paths(&p3), ["docs/page4.txt"]);
    assert_eq!(p3["next_cursor"], Value::Null, "the last row ends the list");
    assert_eq!(*index.skips.lock().unwrap(), [0, 4, 7]);

    // A cursor of another query (other text, scope, folder or limit-independent parts) is refused.
    for other in [
        format!("q=other&scope=content&limit=2&cursor={c1}"),
        format!("q=anything&scope=name&limit=2&cursor={c1}"),
        format!("q=anything&scope=content&limit=2&path=docs&cursor={c1}"),
    ] {
        let r = host.get(&format!("/v1/shares/{MAIN}/search?{other}")).await;
        assert_eq!(r.status, 400, "{other}");
        assert_eq!(r.code(), "bad_request");
    }
    for bad in [
        "i1.",
        "i1.zzzz.1",
        "i1.0123456789abcdef.x",
        "i1.0123456789abcdef.-1",
    ] {
        let r = host
            .get(&format!("/v1/shares/{MAIN}/search?q=a&cursor={bad}"))
            .await;
        assert_eq!(r.status, 400, "{bad}");
    }
    // A limit change between pages is fine: the offset is in index rows.
    let p = host
        .search(&format!("q=anything&scope=content&limit=100&cursor={c1}"))
        .await;
    assert_eq!(paths(&p).len(), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_folder_search_maps_below_that_folder() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![
        below("Needle-1-0.txt"),
        below("side0/x-needle-1-0.txt"),
        // Not below the folder that is searched: a sibling's path under the share root.
        Row::Raw("file:C:/elsewhere/Needle-1-0.txt".into()),
    ]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    let v = host.search("q=needle&path=tree/lv1").await;
    assert_eq!(v["via"], "index");
    assert_eq!(
        paths(&v),
        ["tree/lv1/Needle-1-0.txt", "tree/lv1/side0/x-needle-1-0.txt"]
    );
    let scope = index.scopes.lock().unwrap()[0].clone();
    assert!(
        scope.query_url.ends_with("/share/tree/lv1"),
        "{}",
        scope.query_url
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_walk_answers_when_the_index_cannot() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![below("docs/recent2.txt")]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;

    // Not crawled: the walk, and `scope=content` is answered as a name search.
    index.crawled.store(false, Ordering::SeqCst);
    for extra in ["q=needle", "q=needle&scope=content"] {
        let v = host.search(extra).await;
        assert_eq!(v["via"], "walk", "{extra}");
        assert_eq!(paths(&v).len(), 2, "{v}");
    }
    // The service fails: the same.
    index.crawled.store(true, Ordering::SeqCst);
    index.broken.store(true, Ordering::SeqCst);
    let v = host.search("q=needle").await;
    assert_eq!(v["via"], "walk");
    assert_eq!(paths(&v).len(), 2);
    index.broken.store(false, Ordering::SeqCst);
    let v = host.search("q=needle").await;
    assert_eq!(v["via"], "index", "it works again");

    // A bad scope is the client's mistake, whatever the index says.
    let r = host
        .get(&format!("/v1/shares/{MAIN}/search?q=a&scope=everything"))
        .await;
    assert_eq!((r.status, r.code().as_str()), (400, "bad_request"));
    // An empty q too.
    let r = host.get(&format!("/v1/shares/{MAIN}/search?q=%20")).await;
    assert_eq!(r.status, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn cursors_keep_their_kind() {
    let fix = Fixture::new();
    let index = FakeIndex::new(
        (0..5)
            .map(|i| below(&format!("docs/page{i}.txt")))
            .collect(),
    );
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;

    // A walk in progress stays a walk, even when the index would now answer.
    index.crawled.store(false, Ordering::SeqCst);
    let w1 = host.search("q=page&limit=3").await;
    assert_eq!(w1["via"], "walk");
    let wc = w1["next_cursor"].as_str().unwrap().to_owned();
    assert!(wc.starts_with("s1."));
    index.crawled.store(true, Ordering::SeqCst);
    let w2 = host.search(&format!("q=page&limit=3&cursor={wc}")).await;
    assert_eq!(w2["via"], "walk");
    assert_eq!(paths(&w2).len(), 2);
    assert_eq!(index.sql.lock().unwrap().len(), 0);

    // An index cursor whose index is gone starts the walk over instead of failing.
    let i1 = host.search("q=page&limit=2").await;
    assert_eq!(i1["via"], "index");
    let ic = i1["next_cursor"].as_str().unwrap().to_owned();
    index.broken.store(true, Ordering::SeqCst);
    let again = host.search(&format!("q=page&limit=2&cursor={ic}")).await;
    assert_eq!(again["via"], "walk");
    assert_eq!(paths(&again).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_when_the_indexer_is_behind_or_the_page_ran_out_of_time() {
    let fix = Fixture::new();
    let index = FakeIndex::new(
        (0..5)
            .map(|i| below(&format!("docs/page{i}.txt")))
            .collect(),
    );
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    let v = host.search("q=page").await;
    assert_eq!(v["partial"], false);
    index.catching_up.store(true, Ordering::SeqCst);
    let v = host.search("q=page").await;
    assert_eq!(v["partial"], true, "the indexer still has items to crawl");
    assert_eq!(paths(&v).len(), 5);
    assert_eq!(v["next_cursor"], Value::Null);

    // No time at all: one row is still taken so a cursor always makes progress.
    let options = ApiOptions {
        search_budget: Duration::ZERO,
        ..ApiOptions::default()
    };
    let host = start(&fix, Some(index.clone()), options).await;
    index.catching_up.store(false, Ordering::SeqCst);
    let v = host.search("q=page").await;
    assert_eq!(v["partial"], true);
    assert_eq!(paths(&v).len(), 1);
    let cursor = v["next_cursor"].as_str().unwrap().to_owned();
    assert!(cursor.ends_with(".1"), "{cursor}");
    let v = host.search(&format!("q=page&cursor={cursor}")).await;
    assert_eq!(paths(&v).len(), 1, "it moves on");
}

#[tokio::test(flavor = "multi_thread")]
async fn permissions_are_the_walks() {
    let fix = Fixture::new();
    let index = FakeIndex::new(vec![below("docs/recent2.txt")]);
    let host = start(&fix, Some(index.clone()), ApiOptions::default()).await;
    // A share the device holds nothing on does not exist for it, indexed or not.
    let r = host
        .get(&format!("/v1/shares/{NO_GRANT}/search?q=recent"))
        .await;
    assert_eq!((r.status, r.code().as_str()), (404, "share_not_found"));
    assert!(index.sql.lock().unwrap().is_empty());
    // Without the token nothing is answered.
    let r = send(
        &host.app,
        "GET",
        &format!("/v1/shares/{MAIN}/search?q=a"),
        None,
        "",
    )
    .await;
    assert_eq!(r.status, 401);
    // A folder that does not exist, and a file as the folder.
    let r = host
        .get(&format!("/v1/shares/{MAIN}/search?q=a&path=nope"))
        .await;
    assert_eq!(r.status, 404);
    let r = host
        .get(&format!("/v1/shares/{MAIN}/search?q=a&path=data.bin"))
        .await;
    assert_eq!((r.status, r.code().as_str()), (400, "bad_path"));
    assert!(index.sql.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------
// The real index (read-only, `#[ignore]`)

/// The first folder (`GANDER_INDEX_TEST_DIR`, then the signed-in user's Documents, Downloads,
/// Pictures, Desktop) that the indexer crawls and that has at least one indexed file.
fn live_indexed_folder() -> Option<PathBuf> {
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Pictures,
        SHGetKnownFolderPath,
    };
    let session = gander_win::search::IndexSession::open().ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("GANDER_INDEX_TEST_DIR") {
        candidates.push(PathBuf::from(p));
    }
    for id in [
        &FOLDERID_Documents,
        &FOLDERID_Downloads,
        &FOLDERID_Pictures,
        &FOLDERID_Desktop,
    ] {
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: a valid known-folder id, no token (the current user) and a valid out-pointer;
        // the returned buffer is read once and leaked (a test process, a few bytes).
        let hr = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw) };
        if hr == 0 && !raw.is_null() {
            let mut len = 0;
            // SAFETY: the buffer is NUL-terminated.
            while unsafe { *raw.add(len) } != 0 {
                len += 1;
            }
            // SAFETY: `len` units were just read from the buffer.
            let wide = unsafe { std::slice::from_raw_parts(raw, len) };
            candidates.push(PathBuf::from(String::from_utf16_lossy(wide)));
        }
    }
    for dir in candidates {
        let Some(text) = dir.to_str() else { continue };
        let Ok(scope) = IndexScope::for_dir(text) else {
            continue;
        };
        if !session.is_in_scope(&scope).unwrap_or(false) {
            continue;
        }
        let sql =
            gander_core::search::build_sql(&scope, "a", gander_core::search::SearchScope::Name, 50)
                .ok()?;
        let mut any = false;
        let _ = session.query(&sql, 0, &mut |_| {
            any = true;
            ControlFlow::Break(())
        });
        if any {
            return Some(dir);
        }
    }
    None
}

/// The host's whole path against the machine's real Windows Search index: `/shares` and `/me`
/// advertise it, a name search and a content search answer `via: index`, and every returned path
/// is a real, visible entry (`/stat` agrees with the entry, no hidden or system flag, the name
/// really contains the text). Read-only: nothing is written, no crawl scope or setting is touched.
///
/// ```text
/// cargo test -p gander-host --test search_index -- --ignored --nocapture live
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the Windows Search service and an indexed folder with files"]
async fn live_index_end_to_end() {
    let Some(root) = live_indexed_folder() else {
        println!("SKIP: the Windows Search service is off or no known folder is indexed");
        return;
    };
    println!("indexed folder found (name withheld)");
    let host = start_at(
        &root,
        Some(Arc::new(gander_host::api::WindowsSearch)),
        ApiOptions::default(),
    )
    .await;

    let shares = host.get("/v1/shares").await.json();
    assert_eq!(shares["shares"][0]["search"], "indexed", "{shares}");
    let me = host.get("/v1/me").await.json();
    assert_eq!(me["capabilities"]["search"], "indexed");

    // Names: every hit has the text in its stored name, is visible and agrees with /stat.
    let started = std::time::Instant::now();
    let v = host.search("q=a&limit=50").await;
    println!("name search: {:?}", started.elapsed());
    assert_eq!(v["via"], "index");
    let entries = v["entries"].as_array().unwrap();
    assert!(
        !entries.is_empty(),
        "the indexed folder has an `a` in some name"
    );
    for e in entries {
        let name = e["name"].as_str().unwrap();
        assert!(
            name.to_lowercase().contains('a'),
            "name hit without the text"
        );
        let flags: Vec<&str> = e["flags"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            !flags.contains(&"hidden") && !flags.contains(&"system") && !flags.contains(&"link"),
            "a hidden, system or link entry came back: {flags:?}"
        );
        let path = e["path"].as_str().unwrap();
        assert!(path.ends_with(name), "the path ends with the stored name");
        let r = host
            .get(&format!("/v1/shares/{MAIN}/stat?path={}", enc(path)))
            .await;
        assert_eq!(r.status, 200, "an index hit that does not resolve");
        let s = r.json();
        assert_eq!(s["size"], e["size"]);
        assert_eq!(s["kind"], e["kind"]);
        assert_eq!(s["name"], e["name"]);
    }
    // A second page continues where the first stopped (when there is one).
    if let Some(cursor) = v["next_cursor"].as_str() {
        let p2 = host.search(&format!("q=a&limit=50&cursor={cursor}")).await;
        assert_eq!(p2["via"], "index");
        let first: Vec<String> = paths(&v);
        for p in paths(&p2) {
            assert!(!first.contains(&p), "the second page repeats a hit");
        }
    }

    // Content: it answers through the index too (how many hits depends on the folder).
    let started = std::time::Instant::now();
    let v = host.search("q=the&scope=content&limit=20").await;
    println!(
        "content search: {:?}, {} hits",
        started.elapsed(),
        v["entries"].as_array().unwrap().len()
    );
    assert_eq!(v["via"], "index");
    for e in v["entries"].as_array().unwrap() {
        let path = e["path"].as_str().unwrap();
        let r = host
            .get(&format!("/v1/shares/{MAIN}/stat?path={}", enc(path)))
            .await;
        assert_eq!(r.status, 200, "a content hit that does not resolve");
    }

    // Text that cannot be in any name: nothing from a name search.
    let v = host.search("q=zzqx-no-such-name-wp66-0f3a").await;
    assert_eq!(v["via"], "index");
    assert!(v["entries"].as_array().unwrap().is_empty());
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
