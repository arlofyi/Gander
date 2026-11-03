// SPDX-License-Identifier: Apache-2.0
//! WP-17 integration tests: the `/v1/events` WebSocket over a real loopback TLS listener, with
//! real pairing and bearer tokens.
//!
//! Covered: `hello` and `host.state`, ping/pong, ignored and refused client messages
//! (`stream_unavailable`), host-state pushes, `shares.updated`, `share.changed` (permission
//! filter with a fake backend; on Windows a real `ReadDirectoryChangesW` watch that must deliver
//! within 2 s, coalesce bursts and skip hidden names), revocation, the kill switch, heartbeat
//! drops and the per-device socket cap.
//!
//! Every socket is bound on 127.0.0.1 port 0. Tokens are generated per run, never written to
//! fixtures.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use gander_core::config::{HostConfig, SecureFileStore, ShareConfig, ShareGrant};
use gander_core::error::ErrorCode;
use gander_core::net::plan::ListenerPlan;
use gander_core::pairing::b64url;
use gander_core::protocol::events::{HostStateEvent, ServerEvent};
use gander_core::protocol::files::{Permission, StreamDecision};
use gander_core::protocol::host::TrustAdapterKind;
use gander_core::protocol::pairing::{PairApproved, PairStarted};
use gander_core::protocol::{DeviceId, HostId, SessionId, ShareId};
use gander_host::audit::AuditLog;
use gander_host::events::{
    ChangeNotifier, ConfigShares, Events, EventsConfig, EventsOptions, HostStateConfig,
    HostStateMonitor, ManualProbe, WatchBackend, WatchConfig, WatchEvent, WatchHandle,
};
use gander_host::pairing::{DeviceRegistry, PairingService, SharedStore, host_details};
use gander_host::server::tls::pinned_client_config;
use gander_host::server::{FixedPlan, HostIdentity, Server, ServerOptions, hello};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const SHARE: &str = "shr_01WP17TESTSHARE00000000000";

// ---------- harness ----------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp17-{tag}-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(windows)]
fn store() -> SharedStore {
    Arc::new(gander_win::config_store::WindowsConfigStore)
}

#[cfg(not(windows))]
fn store() -> SharedStore {
    #[derive(Debug)]
    struct Plain;
    impl SecureFileStore for Plain {
        fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
            match std::fs::read(path) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn write_owner_only(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, contents)
        }
    }
    Arc::new(Plain)
}

#[allow(dead_code)]
fn _store_trait_in_scope(_: &dyn SecureFileStore) {}

/// A backend with no filesystem: the test fires changes by hand.
#[derive(Default)]
struct FakeBackend {
    notifiers: Mutex<HashMap<(String, String), ChangeNotifier>>,
}

impl std::fmt::Debug for FakeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FakeBackend")
    }
}

impl FakeBackend {
    fn fire(&self, share: &str, dir: &str) {
        let n = self
            .notifiers
            .lock()
            .unwrap()
            .get(&(share.to_owned(), dir.to_owned()))
            .cloned()
            .expect("directory not watched");
        n(WatchEvent::Changed);
    }
}

impl WatchBackend for FakeBackend {
    fn watch(
        &self,
        share: &ShareId,
        dir: &str,
        changed: ChangeNotifier,
    ) -> io::Result<WatchHandle> {
        self.notifiers
            .lock()
            .unwrap()
            .insert((share.to_string(), dir.to_owned()), changed);
        Ok(Box::new(()))
    }
}

struct Host {
    _dir: TempDir,
    server: Server,
    svc: Arc<PairingService>,
    identity: HostIdentity,
    addr: SocketAddr,
    events: Events,
    probe: Arc<ManualProbe>,
    shares: Arc<ConfigShares>,
}

fn fast_events() -> EventsConfig {
    EventsConfig {
        watch: WatchConfig {
            debounce: Duration::from_millis(100),
            sweep_interval: Duration::from_millis(50),
            ..WatchConfig::default()
        },
        ..EventsConfig::default()
    }
}

async fn start_host(
    shares: Arc<ConfigShares>,
    backend: Arc<dyn WatchBackend>,
    events_config: EventsConfig,
) -> Host {
    let dir = TempDir::new("host");
    let path = dir.0.clone();
    let identity = HostIdentity::generate().unwrap();
    let audit = Arc::new(AuditLog::open(&path.join("logs")).unwrap());
    let registry = Arc::new(
        DeviceRegistry::load(
            store(),
            &path.join("cfg"),
            Default::default(),
            audit.clone(),
        )
        .unwrap(),
    );
    let svc = PairingService::new(
        &identity,
        registry.clone(),
        audit,
        host_details(identity.host_id().clone(), vec![LOOPBACK], None),
        0,
    );
    let probe = Arc::new(ManualProbe::default());
    probe.set_idle(Some(Duration::from_secs(5)));
    let monitor = HostStateMonitor::spawn(
        probe.clone(),
        Arc::default(),
        HostStateConfig {
            poll_interval: Duration::from_millis(50),
            ..HostStateConfig::default()
        },
    );
    let events = Events::new(EventsOptions {
        registry,
        shares: shares.clone(),
        host: monitor,
        backend,
        config: events_config,
    });
    let routes = axum::Router::new()
        .merge(hello::router(
            identity.host_id().clone(),
            svc.pairing_open_flag(),
        ))
        .merge(svc.router())
        .merge(gander_host::events::router(&events));
    let plan = ListenerPlan::new(TrustAdapterKind::Generic, 0, vec![LOOPBACK], vec![]).unwrap();
    let mut options = ServerOptions::new(identity.clone(), Arc::new(FixedPlan(plan)));
    options.watch_address_changes = false;
    options.authenticator = svc.authenticator();
    let server = Server::start(options, routes).await.unwrap();
    let addr = server.bound_addrs()[0];
    assert_eq!(addr.ip(), LOOPBACK);
    svc.set_host_details(
        host_details(identity.host_id().clone(), vec![LOOPBACK], None),
        addr.port(),
    );
    Host {
        _dir: dir,
        server,
        svc,
        identity,
        addr,
        events,
        probe,
        shares,
    }
}

async fn tls(addr: SocketAddr, id: &HostId) -> tokio_rustls::client::TlsStream<TcpStream> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    TlsConnector::from(pinned_client_config(id.clone()).unwrap())
        .connect(ServerName::IpAddress(addr.ip().into()), tcp)
        .await
        .unwrap()
}

/// One HTTP/1.1 request (`Connection: close`); returns (status, body).
async fn call(
    host: &Host,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = tls(host.addr, host.identity.host_id()).await;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n");
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    let body = body.unwrap_or("");
    if !body.is_empty() || method == "POST" {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, body.to_owned())
}

/// Pairs a device (flow B, owner approves) and returns its token and id.
async fn pair_device(host: &Host) -> (String, DeviceId) {
    host.svc.open_window();
    let body = serde_json::json!({
        "device": {"name": "Pixel 9", "platform": "android", "model": "Pixel 9", "app_version": "0.1.0"},
        "nonce": b64url(&gander_host::pairing::random_bytes::<16>()),
    })
    .to_string();
    let (status, resp) = call(host, "POST", "/v1/pair", &[], Some(&body)).await;
    assert_eq!(status, 200, "{resp}");
    let started: PairStarted = serde_json::from_str(&resp).unwrap();
    host.svc.approve(&started.pairing_id).unwrap();
    let (status, resp) = call(
        host,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200, "{resp}");
    let approved: PairApproved = serde_json::from_str(&resp).unwrap();
    (approved.token.expose().to_owned(), approved.device_id)
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

async fn connect_ws(host: &Host, token: &str) -> Ws {
    let stream = tls(host.addr, host.identity.host_id()).await;
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            "ws://127.0.0.1/v1/events",
        )
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, resp) = tokio_tungstenite::client_async(request, stream)
        .await
        .expect("websocket upgrade");
    assert_eq!(resp.status(), 101);
    ws
}

/// The next server event, within `within`; WebSocket control frames are skipped.
async fn next_within(ws: &mut Ws, within: Duration) -> Option<ServerEvent> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return None,
            Ok(None) | Ok(Some(Err(_))) => panic!("socket closed while waiting for an event"),
            Ok(Some(Ok(WsMessage::Text(t)))) => {
                return Some(serde_json::from_str(&t).expect("a ServerEvent"));
            }
            Ok(Some(Ok(WsMessage::Close(f)))) => panic!("socket closed while waiting: {f:?}"),
            Ok(Some(Ok(_))) => {}
        }
    }
}

async fn next_event(ws: &mut Ws) -> ServerEvent {
    next_within(ws, Duration::from_secs(3))
        .await
        .expect("no event within 3 s")
}

/// Reads hello and the initial host.state, returning the latter.
async fn handshake(ws: &mut Ws) -> HostStateEvent {
    let ServerEvent::Hello {
        protocol,
        host_time,
    } = next_event(ws).await
    else {
        panic!("first message must be hello");
    };
    assert_eq!(protocol, 1);
    let skew = (time::OffsetDateTime::now_utc() - host_time).abs();
    assert!(skew < time::Duration::seconds(5), "host_time off by {skew}");
    match next_event(ws).await {
        ServerEvent::HostState(s) => s,
        other => panic!("second message must be host.state, got {other:?}"),
    }
}

async fn send_text(ws: &mut Ws, text: &str) {
    ws.send(WsMessage::Text(text.into())).await.unwrap();
}

/// Round trip that proves nothing else was queued before: ping, then the next event is pong.
async fn assert_quiet(ws: &mut Ws) {
    tokio::time::sleep(Duration::from_millis(250)).await;
    send_text(ws, r#"{"type":"ping"}"#).await;
    assert_eq!(next_event(ws).await, ServerEvent::Pong {});
}

/// Waits until the socket is closed by the host (close frame, error or EOF).
async fn assert_closes(ws: &mut Ws, within: Duration) {
    let closed = tokio::time::timeout(within, async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(WsMessage::Close(_)) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "socket still open after {within:?}");
}

fn config_with(root: &Path, grants: &[&DeviceId], hidden: &[&str]) -> HostConfig {
    let mut config = HostConfig::default();
    config.shares.push(ShareConfig {
        id: ShareId::from(SHARE),
        name: "Docs".into(),
        root: root.to_path_buf(),
        show_hidden: false,
        show_path: false,
        hidden_patterns: hidden.iter().map(|s| (*s).to_owned()).collect(),
        stream_default: StreamDecision::Ask,
        grants: grants
            .iter()
            .map(|d| ShareGrant {
                device: (*d).clone(),
                permissions: vec![Permission::List],
            })
            .collect(),
    });
    config
}

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < Duration::from_secs(3), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn fake_host_parts() -> (Arc<ConfigShares>, Arc<FakeBackend>) {
    (ConfigShares::new(HostConfig::default()), Arc::default())
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_needs_a_token_and_a_websocket_request() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, _) = pair_device(&host).await;

    let (status, body) = call(&host, "GET", "/v1/events", &[], None).await;
    assert_eq!(status, 401);
    assert!(body.contains("token_missing"), "{body}");

    let (status, body) = call(
        &host,
        "GET",
        "/v1/events",
        &[("Authorization", "Bearer not-a-real-token")],
        None,
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("token_invalid"), "{body}");

    // A valid token on a plain GET (no Upgrade headers) is a client error, in the protocol's
    // error format.
    let (status, body) = call(
        &host,
        "GET",
        "/v1/events",
        &[("Authorization", &format!("Bearer {token}"))],
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert!(body.contains("bad_request"), "{body}");
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hello_then_host_state_then_ping_pong() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, _) = pair_device(&host).await;
    let mut ws = connect_ws(&host, &token).await;

    let state = handshake(&mut ws).await;
    assert_eq!(
        state,
        HostStateEvent {
            locked: false,
            attended: true,
            on_battery: false,
            stream_available: false,
        },
        "Phase 1 has no stream engine"
    );

    send_text(&mut ws, r#"{"type":"ping"}"#).await;
    assert_eq!(next_event(&mut ws).await, ServerEvent::Pong {});
    // Unknown and malformed messages are ignored, not fatal.
    send_text(&mut ws, r#"{"type":"future.thing","x":1}"#).await;
    send_text(&mut ws, "not json at all").await;
    send_text(&mut ws, r#"{"type":"ping"}"#).await;
    assert_eq!(next_event(&mut ws).await, ServerEvent::Pong {});
    assert_eq!(host.events.open_sockets(), 1);

    drop(ws);
    wait_until("socket count back to zero", || {
        host.events.open_sockets() == 0
    })
    .await;
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_signalling_is_refused_with_stream_unavailable() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, _) = pair_device(&host).await;
    let mut ws = connect_ws(&host, &token).await;
    handshake(&mut ws).await;

    for msg in [
        r#"{"type":"stream.answer","session":"ses_1","sdp":"v=0"}"#,
        r#"{"type":"stream.ice","session":"ses_1","candidate":"candidate:1"}"#,
        r#"{"type":"stream.resize","session":"ses_1","width":1080,"height":2280}"#,
        r#"{"type":"stream.pause","session":"ses_1"}"#,
        r#"{"type":"stream.resume","session":"ses_1"}"#,
    ] {
        send_text(&mut ws, msg).await;
        assert_eq!(
            next_event(&mut ws).await,
            ServerEvent::Error {
                code: ErrorCode::StreamUnavailable,
                message: "Stream mode is not available on this computer yet.".into(),
                session: Some(SessionId::from("ses_1")),
            },
            "for {msg}"
        );
    }
    // The socket survives the refusals.
    send_text(&mut ws, r#"{"type":"ping"}"#).await;
    assert_eq!(next_event(&mut ws).await, ServerEvent::Pong {});
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn host_state_changes_are_pushed() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, _) = pair_device(&host).await;
    let mut ws = connect_ws(&host, &token).await;
    handshake(&mut ws).await;

    // The session locks: locked, and nobody is attending a locked machine.
    host.probe.set_locked(true);
    host.events.host_monitor().refresh_now();
    let ServerEvent::HostState(s) = next_event(&mut ws).await else {
        panic!("expected host.state");
    };
    assert!(s.locked && !s.attended && !s.stream_available, "{s:?}");

    host.probe.set_locked(false);
    host.probe.set_on_battery(true);
    host.probe.set_idle(Some(Duration::from_secs(900)));
    let ServerEvent::HostState(s) = next_event(&mut ws).await else {
        panic!("expected host.state");
    };
    // The poll may deliver the three changes in one or two messages; the last one is complete.
    let s = if s.on_battery && !s.attended && !s.locked {
        s
    } else {
        let ServerEvent::HostState(s) = next_event(&mut ws).await else {
            panic!("expected host.state");
        };
        s
    };
    assert_eq!(
        s,
        HostStateEvent {
            locked: false,
            attended: false,
            on_battery: true,
            stream_available: false
        }
    );
    // A socket that connects later starts from the current state.
    let mut late = connect_ws(&host, &token).await;
    assert_eq!(handshake(&mut late).await, s);
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn shares_updated_is_pushed_to_every_socket() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, _) = pair_device(&host).await;
    let mut a = connect_ws(&host, &token).await;
    let mut b = connect_ws(&host, &token).await;
    handshake(&mut a).await;
    handshake(&mut b).await;
    host.events.shares_updated();
    assert_eq!(next_event(&mut a).await, ServerEvent::SharesUpdated {});
    assert_eq!(next_event(&mut b).await, ServerEvent::SharesUpdated {});
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn share_changed_goes_only_to_devices_that_may_list_the_share() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend.clone(), fast_events()).await;
    let (token_a, device_a) = pair_device(&host).await;
    let (token_b, _device_b) = pair_device(&host).await;
    // Only A holds a grant.
    host.shares.replace(config_with(
        Path::new("C:/unused-by-the-fake"),
        &[&device_a],
        &[],
    ));
    let mut a = connect_ws(&host, &token_a).await;
    let mut b = connect_ws(&host, &token_b).await;
    handshake(&mut a).await;
    handshake(&mut b).await;

    let share = ShareId::from(SHARE);
    // Nothing is watched before a listing touches the directory.
    host.events.touch_armed(&share, "Clients/Acme").await;
    backend.fire(SHARE, "Clients/Acme");
    assert_eq!(
        next_event(&mut a).await,
        ServerEvent::ShareChanged {
            share: share.clone(),
            path: "Clients/Acme".into()
        }
    );
    assert_quiet(&mut b).await;

    // After revoking A's grant it stops hearing about the share, without reconnecting.
    host.shares
        .replace(config_with(Path::new("C:/unused-by-the-fake"), &[], &[]));
    for _ in 0..3 {
        backend.fire(SHARE, "Clients/Acme");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert_quiet(&mut a).await;
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_sends_device_revoked_and_closes_the_socket() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, device) = pair_device(&host).await;
    let mut ws = connect_ws(&host, &token).await;
    handshake(&mut ws).await;
    let mut other = {
        let (t2, _) = pair_device(&host).await;
        let mut ws2 = connect_ws(&host, &t2).await;
        handshake(&mut ws2).await;
        ws2
    };

    let t0 = Instant::now();
    assert!(host.svc.registry().revoke(&device).unwrap());
    assert_eq!(next_event(&mut ws).await, ServerEvent::DeviceRevoked {});
    assert_closes(&mut ws, Duration::from_secs(1)).await;
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "revocation took {:?}",
        t0.elapsed()
    );
    wait_until("revoked socket released", || {
        host.events.open_sockets() == 1
    })
    .await;

    // The revoked token no longer opens a socket; other devices are unaffected.
    let (status, body) = call(
        &host,
        "GET",
        "/v1/events",
        &[("Authorization", &format!("Bearer {token}"))],
        None,
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("token_revoked"), "{body}");
    assert_quiet(&mut other).await;
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_switch_closes_every_socket() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token, _) = pair_device(&host).await;
    let mut a = connect_ws(&host, &token).await;
    let mut b = connect_ws(&host, &token).await;
    handshake(&mut a).await;
    handshake(&mut b).await;
    host.server.disconnect_all();
    assert_closes(&mut a, Duration::from_secs(1)).await;
    assert_closes(&mut b, Duration::from_secs(1)).await;
    wait_until("sockets released", || host.events.open_sockets() == 0).await;
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_two_heartbeats_drops_the_socket_but_a_live_client_stays() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(
        shares,
        backend,
        EventsConfig {
            heartbeat_interval: Duration::from_millis(80),
            ..fast_events()
        },
    )
    .await;
    let (token, _) = pair_device(&host).await;

    // This client does not read, so it never answers the server's pings.
    let mut deaf = connect_ws(&host, &token).await;
    // This one reads continuously, which answers pings automatically.
    let mut live = connect_ws(&host, &token).await;
    handshake(&mut live).await;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(700) {
        assert!(
            next_within(&mut live, Duration::from_millis(100))
                .await
                .is_none(),
            "no events expected"
        );
    }
    assert_eq!(
        host.events.open_sockets(),
        1,
        "only the live client remains"
    );
    send_text(&mut live, r#"{"type":"ping"}"#).await;
    assert_eq!(next_event(&mut live).await, ServerEvent::Pong {});
    // The deaf client's socket is gone.
    assert_closes(&mut deaf, Duration::from_secs(1)).await;
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_keeps_at_most_the_configured_number_of_sockets() {
    let (shares, backend) = fake_host_parts();
    let host = start_host(
        shares,
        backend,
        EventsConfig {
            max_sockets_per_device: 2,
            ..fast_events()
        },
    )
    .await;
    let (token, _) = pair_device(&host).await;
    let mut first = connect_ws(&host, &token).await;
    handshake(&mut first).await;
    let mut second = connect_ws(&host, &token).await;
    handshake(&mut second).await;
    let mut third = connect_ws(&host, &token).await;
    handshake(&mut third).await;
    // The oldest was pushed out (a phone that changed network leaves such a socket behind).
    assert_closes(&mut first, Duration::from_secs(1)).await;
    wait_until("two sockets left", || host.events.open_sockets() == 2).await;
    assert_quiet(&mut second).await;
    assert_quiet(&mut third).await;
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn phase_two_can_push_to_one_device_and_install_a_signalling_handler() {
    use gander_core::protocol::events::{ClientEvent, StreamStateEvent};
    use gander_core::protocol::stream::{StreamReason, StreamState};
    use gander_host::events::StreamSignalling;

    #[derive(Debug)]
    struct Echo;
    impl StreamSignalling for Echo {
        fn on_client_event(&self, _: &DeviceId, event: ClientEvent) -> Option<ServerEvent> {
            match event {
                ClientEvent::StreamPause { session } => {
                    Some(ServerEvent::StreamState(StreamStateEvent {
                        session,
                        state: StreamState::Paused,
                        reason: Some(StreamReason::PhoneBackground),
                        window: None,
                        candidates: Vec::new(),
                    }))
                }
                _ => None,
            }
        }
    }

    let (shares, backend) = fake_host_parts();
    let host = start_host(shares, backend, fast_events()).await;
    let (token_a, device_a) = pair_device(&host).await;
    let (token_b, _) = pair_device(&host).await;
    let mut a = connect_ws(&host, &token_a).await;
    let mut b = connect_ws(&host, &token_b).await;
    handshake(&mut a).await;
    handshake(&mut b).await;

    host.events.send_to(
        &device_a,
        ServerEvent::StreamOffer {
            session: SessionId::from("ses_9"),
            sdp: "v=0".into(),
        },
    );
    assert_eq!(
        next_event(&mut a).await,
        ServerEvent::StreamOffer {
            session: SessionId::from("ses_9"),
            sdp: "v=0".into()
        }
    );
    assert_quiet(&mut b).await;

    host.events.set_stream_signalling(Arc::new(Echo));
    send_text(&mut a, r#"{"type":"stream.pause","session":"ses_9"}"#).await;
    let ServerEvent::StreamState(s) = next_event(&mut a).await else {
        panic!("expected stream.state");
    };
    assert_eq!(s.state, StreamState::Paused);
    host.server.shutdown().await;
}

// ---------- the real filesystem (Windows) ----------

#[cfg(windows)]
mod real_filesystem {
    use super::*;
    use gander_host::events::system::WindowsWatch;
    use std::fs;

    async fn start_with_real_watch(root: &Path, hidden: &[&str]) -> (Host, String, ShareId) {
        let shares = ConfigShares::new(HostConfig::default());
        let backend = Arc::new(WindowsWatch::new(shares.clone()).unwrap());
        // Default timing: the 500 ms debounce of the spec.
        let host = start_host(shares, backend, EventsConfig::default()).await;
        let (token, device) = pair_device(&host).await;
        host.shares.replace(config_with(root, &[&device], hidden));
        (host, token, ShareId::from(SHARE))
    }

    /// The verify step of WP-17: `share.changed` within 2 s of touching a file.
    #[tokio::test(flavor = "multi_thread")]
    async fn share_changed_arrives_within_two_seconds_of_touching_a_file() {
        let root = TempDir::new("share");
        fs::create_dir_all(root.0.join("docs")).unwrap();
        fs::create_dir_all(root.0.join("other")).unwrap();
        let (host, token, share) = start_with_real_watch(&root.0, &[]).await;
        let mut ws = connect_ws(&host, &token).await;
        handshake(&mut ws).await;

        // /list arms the watch, then lists.
        host.events.touch_armed(&share, "docs").await;
        assert_eq!(host.events.watch_registry().watching(), 1);

        let t0 = Instant::now();
        fs::write(root.0.join("docs").join("report.txt"), b"hello").unwrap();
        let ev = next_within(&mut ws, Duration::from_secs(2))
            .await
            .expect("no share.changed within 2 s of touching a file");
        let elapsed = t0.elapsed();
        assert_eq!(
            ev,
            ServerEvent::ShareChanged {
                share: share.clone(),
                path: "docs".into()
            }
        );
        eprintln!("WP-17 share.changed arrived {elapsed:?} after the file was written");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");

        // A directory nobody listed is not watched: no event for it.
        fs::write(root.0.join("other").join("x.txt"), b"x").unwrap();
        assert_quiet(&mut ws).await;
        host.server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bursts_coalesce_and_hidden_names_stay_quiet() {
        let root = TempDir::new("share");
        fs::create_dir_all(root.0.join("docs")).unwrap();
        let (host, token, share) = start_with_real_watch(&root.0, &["**/*.tmp"]).await;
        let mut ws = connect_ws(&host, &token).await;
        handshake(&mut ws).await;
        host.events.touch_armed(&share, "docs").await;

        // A change to a hidden name is not something the listing shows.
        fs::write(root.0.join("docs").join("scratch.tmp"), b"x").unwrap();
        fs::write(root.0.join("docs").join("desktop.ini"), b"x").unwrap();
        let got = next_within(&mut ws, Duration::from_millis(1200)).await;
        assert!(
            got.is_none(),
            "hidden and always-omitted names must not raise share.changed: {got:?}"
        );

        // 30 files in a burst: one event for the window, not 30.
        for i in 0..30 {
            fs::write(root.0.join("docs").join(format!("f{i}.txt")), b"x").unwrap();
        }
        let ev = next_within(&mut ws, Duration::from_secs(2))
            .await
            .expect("event");
        assert_eq!(
            ev,
            ServerEvent::ShareChanged {
                share: share.clone(),
                path: "docs".into()
            }
        );
        assert!(
            next_within(&mut ws, Duration::from_millis(900))
                .await
                .is_none(),
            "a burst inside one window must yield one event"
        );

        // The next change after the window is a new event.
        fs::rename(
            root.0.join("docs").join("f0.txt"),
            root.0.join("docs").join("renamed.txt"),
        )
        .unwrap();
        assert!(matches!(
            next_event(&mut ws).await,
            ServerEvent::ShareChanged { ref path, .. } if path == "docs"
        ));
        host.server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_share_root_is_the_empty_path() {
        let root = TempDir::new("share");
        let (host, token, share) = start_with_real_watch(&root.0, &[]).await;
        let mut ws = connect_ws(&host, &token).await;
        handshake(&mut ws).await;
        host.events.touch_armed(&share, "").await;
        fs::create_dir(root.0.join("newdir")).unwrap();
        assert_eq!(
            next_event(&mut ws).await,
            ServerEvent::ShareChanged {
                share: share.clone(),
                path: String::new()
            }
        );
        host.server.shutdown().await;
    }

    /// The watch never leaves the share: paths the resolver refuses are not watched.
    #[tokio::test(flavor = "multi_thread")]
    async fn directories_the_resolver_refuses_are_not_watched() {
        let root = TempDir::new("share");
        fs::create_dir_all(root.0.join("docs")).unwrap();
        fs::write(root.0.join("file.txt"), b"x").unwrap();
        let (host, token, share) = start_with_real_watch(&root.0, &["**/.git"]).await;
        let mut ws = connect_ws(&host, &token).await;
        handshake(&mut ws).await;
        fs::create_dir_all(root.0.join("proj").join(".git")).unwrap();
        for dir in [
            "../outside",
            "docs/../..",
            ".git",
            "proj/.git",
            "file.txt",
            "missing",
            "docs\\..\\..",
        ] {
            host.events.touch_armed(&share, dir).await;
        }
        assert_eq!(
            host.events.watch_registry().watching(),
            0,
            "a refused directory must never be watched"
        );
        assert_eq!(host.events.watch_registry().interested(), 0);
        host.server.shutdown().await;
    }
}
