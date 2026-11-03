// SPDX-License-Identifier: Apache-2.0
//! WP-12 integration tests: the listener on loopback with a temp config, pinned TLS, the gate over
//! a real connection, exclusive binding, re-planning and the kill switch.
//!
//! Every socket is bound on 127.0.0.1 with port 0. No test binds a wildcard address.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gander_core::config::{ConfigFile as _, HostConfig, ListenerConfig};
use gander_core::net::plan::ListenerPlan;
use gander_core::protocol::HostId;
use gander_core::protocol::host::{HostHello, TrustAdapterKind};
use gander_host::server::listener::bind_exclusive;
use gander_host::server::tls::pinned_client_config;
use gander_host::server::{HostIdentity, PlanSource, Server, ServerOptions, SystemPlan, hello};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp12-{}", ulid::Ulid::new()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The system plan (real interfaces, config from disk) restricted to 127.0.0.1 on port 0.
#[derive(Debug)]
struct TestPlan(SystemPlan);

impl PlanSource for TestPlan {
    fn plan(&self) -> Result<ListenerPlan, String> {
        Ok(self.0.plan()?.with_port(0))
    }
}

/// Loads `config.toml` from a temp dir the way the host does.
fn temp_config(dir: &std::path::Path) -> HostConfig {
    // Generic adapter with no interface or CIDR: network side empty, loopback only.
    std::fs::write(
        dir.join("config.toml"),
        "version = 1\n[listener]\nport = 42637\nadapter = \"generic\"\n",
    )
    .unwrap();
    #[cfg(windows)]
    let store = gander_win::config_store::WindowsConfigStore;
    #[cfg(windows)]
    return HostConfig::load(&store, dir).unwrap();
    #[cfg(not(windows))]
    HostConfig::from_toml_str(&std::fs::read_to_string(dir.join("config.toml")).unwrap()).unwrap()
}

async fn start(dir: &TempDir) -> Server {
    let config = temp_config(&dir.0);
    let identity = HostIdentity::load_or_create(&dir.0.join("local")).unwrap();
    let plan = TestPlan(SystemPlan {
        listener: config.listener,
        loopback: vec![LOOPBACK],
    });
    let mut options = ServerOptions::new(identity.clone(), Arc::new(plan));
    options.watch_address_changes = false;
    let routes = hello::router(identity.host_id().clone(), Arc::default());
    Server::start(options, routes).await.unwrap()
}

/// Sends one raw HTTP/1.1 request over pinned TLS and returns (status, head, body).
async fn request(
    addr: SocketAddr,
    pin: &HostId,
    raw: &str,
) -> std::io::Result<(u16, String, String)> {
    let connector = TlsConnector::from(pinned_client_config(pin.clone()).unwrap());
    let tcp = TcpStream::connect(addr).await?;
    let mut tls = connector
        .connect(ServerName::IpAddress(addr.ip().into()), tcp)
        .await?;
    tls.write_all(raw.as_bytes()).await?;
    let mut buf = Vec::new();
    tls.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((status, head.to_owned(), body.to_owned()))
}

fn get(path: &str, host: &str, extra: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_handshake_succeeds_only_with_the_pinned_spki() {
    let dir = TempDir::new();
    let server = start(&dir).await;
    let addr = server.bound_addrs()[0];
    assert_eq!(addr.ip(), LOOPBACK);

    let (status, head, body) = request(addr, server.host_id(), &get("/v1/host", "127.0.0.1", ""))
        .await
        .expect("pinned client connects");
    assert_eq!(status, 200, "{head}");
    assert!(head.to_ascii_lowercase().contains("x-gander-protocol: 1"));
    let hello: HostHello = serde_json::from_str(body.trim()).unwrap();
    assert_eq!(&hello.host_id, server.host_id());
    assert_eq!(hello.protocol, vec![1]);
    assert!(!hello.pairing_open);

    // A client pinned to a different certificate (another host's key) must fail the handshake.
    let other = HostIdentity::generate().unwrap();
    assert_ne!(other.host_id(), server.host_id());
    let err = request(addr, other.host_id(), &get("/v1/host", "127.0.0.1", ""))
        .await
        .expect_err("handshake must fail against a different pin");
    assert!(
        err.to_string().contains("pinned host id"),
        "unexpected error: {err}"
    );

    // Restarting with the same key directory keeps the pin.
    server.shutdown().await;
    let server = start(&dir).await;
    assert_eq!(server.host_id(), &hello.host_id);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn no_bound_socket_is_a_wildcard() {
    let dir = TempDir::new();
    let server = start(&dir).await;
    let ours = server.bound_addrs();
    assert!(!ours.is_empty());
    for a in &ours {
        assert!(!a.ip().is_unspecified(), "wildcard bound: {a}");
        assert_eq!(a.ip(), LOOPBACK);
    }
    // The OS view of every listening socket of this test process agrees.
    #[cfg(windows)]
    {
        let os = gander_win::net::socket::listening_sockets_of_current_process().unwrap();
        for a in &ours {
            assert!(os.contains(a), "{a} not in OS table {os:?}");
        }
        for a in &os {
            assert!(
                !a.ip().is_unspecified(),
                "wildcard listener in process: {a}"
            );
        }
    }
    // And binding a wildcard is refused before any socket is created.
    let err = bind_exclusive(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_refusals_over_tls() {
    let dir = TempDir::new();
    let server = start(&dir).await;
    let addr = server.bound_addrs()[0];
    let pin = server.host_id().clone();
    let host_port = format!("127.0.0.1:{}", addr.port());

    let cases = [
        (get("/v1/host", &host_port, ""), 200, ""),
        (get("/v1/host", "localhost", ""), 200, ""),
        (get("/v1/host", "evil.example", ""), 400, "bad_host"),
        (get("/v1/host", "127.0.0.1:1", ""), 400, "bad_host"),
        (
            get("/v1/host", "127.0.0.1", "Origin: https://evil.example\r\n"),
            403,
            "browser_request",
        ),
        (
            get("/v1/host", "127.0.0.1", "Sec-Fetch-Mode: cors\r\n"),
            403,
            "browser_request",
        ),
        (
            get("/v1/host", "127.0.0.1", "Cookie: a=b\r\n"),
            403,
            "browser_request",
        ),
        (get("/v1/shares", "127.0.0.1", ""), 401, "token_missing"),
    ];
    for (raw, want, code) in cases {
        let (status, head, body) = request(addr, &pin, &raw).await.unwrap();
        assert_eq!(status, want, "{raw:?} -> {head}\n{body}");
        assert!(body.contains(code), "{raw:?} -> {body}");
        assert!(head.to_ascii_lowercase().contains("x-gander-request: "));
        assert!(!head.to_ascii_lowercase().contains("access-control-"));
    }
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exclusive_bind_blocks_a_second_bind() {
    let first = bind_exclusive(SocketAddr::new(LOOPBACK, 0)).unwrap();
    let addr = first.local_addr().unwrap();
    assert!(std::net::TcpListener::bind(addr).is_err());
    assert!(bind_exclusive(addr).is_err());
}

/// A plan the test can switch between "loopback" and "nothing".
#[derive(Debug)]
struct Switch(Mutex<bool>);

impl PlanSource for Switch {
    fn plan(&self) -> Result<ListenerPlan, String> {
        let on = *self.0.lock().unwrap();
        let loopback = if on { vec![LOOPBACK] } else { vec![] };
        ListenerPlan::new(TrustAdapterKind::Generic, 0, loopback, vec![]).map_err(|e| e.to_string())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn replan_closes_and_rebinds_and_kill_switch_drops_connections() {
    let identity = HostIdentity::generate().unwrap();
    let switch = Arc::new(Switch(Mutex::new(true)));
    let mut options = ServerOptions::new(identity.clone(), switch.clone());
    options.watch_address_changes = false;
    let routes = hello::router(identity.host_id().clone(), Arc::default());
    let server = Server::start(options, routes).await.unwrap();
    let addr = server.bound_addrs()[0];

    // Kill switch: an idle keep-alive connection is dropped.
    let connector = TlsConnector::from(pinned_client_config(server.host_id().clone()).unwrap());
    let tcp = TcpStream::connect(addr).await.unwrap();
    let mut tls = connector
        .connect(ServerName::IpAddress(LOOPBACK.into()), tcp)
        .await
        .unwrap();
    tls.write_all(
        get("/v1/host", "127.0.0.1", "")
            .replace("Connection: close\r\n", "")
            .as_bytes(),
    )
    .await
    .unwrap();
    let mut first = [0u8; 12];
    tls.read_exact(&mut first).await.unwrap();
    assert_eq!(&first, b"HTTP/1.1 200");
    server.disconnect_all();
    let mut rest = Vec::new();
    let drained = tokio::time::timeout(Duration::from_secs(2), tls.read_to_end(&mut rest)).await;
    assert!(
        drained.is_ok(),
        "connection still open 2 s after disconnect_all"
    );

    // Address leaves the plan: socket closed. Comes back: re-bound.
    *switch.0.lock().unwrap() = false;
    server.replan().await;
    assert!(server.bound_addrs().is_empty());
    let mut closed = false;
    for _ in 0..40 {
        if TcpStream::connect(addr).await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(closed, "socket still accepting 2 s after leaving the plan");
    *switch.0.lock().unwrap() = true;
    server.replan().await;
    assert_eq!(server.bound_addrs().len(), 1);
    server.shutdown().await;
}

#[test]
fn listener_config_has_no_bind_address_knob() {
    // The only way to choose addresses is the adapter; a wildcard cannot be configured.
    let bad = "version = 1\n[listener]\nbind = \"0.0.0.0\"\n";
    assert!(HostConfig::from_toml_str(bad).is_err());
    let _ = ListenerConfig::default();
}

/// Plans (does not bind) on this machine's real interfaces. Run with
/// `cargo test -p gander-host -- --ignored live_`.
#[test]
#[ignore = "needs the Tailscale adapter on this machine"]
fn live_system_plan_detects_tailscale() {
    let plan = SystemPlan::new(ListenerConfig::default()).plan().unwrap();
    assert_eq!(plan.adapter(), TrustAdapterKind::Tailscale);
    assert!(!plan.network().is_empty());
    for ip in plan.network() {
        assert!(gander_core::net::plan::is_tailscale_address(*ip), "{ip}");
    }
    for sa in plan.socket_addrs() {
        assert!(!sa.ip().is_unspecified());
    }
}
