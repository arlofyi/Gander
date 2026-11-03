// SPDX-License-Identifier: Apache-2.0
//! WP-13 integration tests: pairing flows A and B over a real loopback TLS listener, MITM SAS,
//! rate limits, closed window, tokens, rotation, revocation closing a WebSocket, identity binding
//! with a fake WhoIs, and the test hook.
//!
//! Every socket is bound on 127.0.0.1 port 0. Secrets and tokens are generated per run and never
//! written to fixtures.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Router};
use futures_util::{SinkExt as _, StreamExt as _};
use gander_core::config::SecureFileStore;
use gander_core::net::plan::ListenerPlan;
use gander_core::net::tailscale::{FakeTailscale, PeerIdentity};
use gander_core::pairing::qr::QrPayload;
use gander_core::pairing::sas::derive_sas;
use gander_core::pairing::{b64url, b64url_fixed};
use gander_core::protocol::events::ServerEvent;
use gander_core::protocol::host::{HostHello, TrustAdapterKind};
use gander_core::protocol::pairing::{PairApproved, PairStarted};
use gander_core::protocol::{HostId, PairingId};
use gander_host::audit::AuditLog;
use gander_host::pairing::{DeviceRegistry, PairingService, SharedStore, host_details};
use gander_host::server::gate::{GateState, apply_gate};
use gander_host::server::identity::{host_id_for_spki, spki_from_cert};
use gander_host::server::tls::{pinned_client_config, provider, server_config};
use gander_host::server::{
    Caller, CloseReason, ConnHandle, ConnInfo, DisconnectSignal, FixedPlan, HostAllowList,
    HostIdentity, Server, ServerOptions, TrustNetwork, hello,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tower::ServiceExt as _;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("gander-wp13-{}", ulid::Ulid::new()));
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
        fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
            match std::fs::read(path) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn write_owner_only(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, contents)
        }
    }
    Arc::new(Plain)
}

#[allow(dead_code)]
fn _store_trait_in_scope(_: &dyn SecureFileStore) {}

struct Host {
    _dir: TempDir,
    dir: PathBuf,
    server: Server,
    svc: Arc<PairingService>,
    audit: Arc<AuditLog>,
    identity: HostIdentity,
    addr: SocketAddr,
}

/// Test-only WebSocket route standing in for WP-17's `/events`: closes when its connection
/// handle fires and sends `device.revoked` first when the reason is revocation.
async fn test_ws(
    ws: WebSocketUpgrade,
    Extension(handle): Extension<ConnHandle>,
    Extension(caller): Extension<Caller>,
) -> axum::response::Response {
    assert!(matches!(caller, Caller::Device(_)));
    ws.on_upgrade(move |mut socket| async move {
        loop {
            tokio::select! {
                reason = handle.closed() => {
                    if reason == CloseReason::Revoked {
                        let text = serde_json::to_string(&ServerEvent::DeviceRevoked {}).unwrap();
                        let _ = socket.send(Message::Text(text.into())).await;
                    }
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
                msg = socket.recv() => {
                    if msg.is_none() { break; }
                }
            }
        }
    })
    .into_response()
}

fn test_routes() -> Router {
    Router::new()
        .route(
            "/v1/test/whoami",
            get(|Extension(caller): Extension<Caller>| async move {
                match caller {
                    Caller::Device(id) => id.to_string(),
                    Caller::Anonymous => "anonymous".to_owned(),
                }
            }),
        )
        .route("/v1/test/ws", get(test_ws))
}

async fn start_host() -> Host {
    let dir = TempDir::new();
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
        registry,
        audit.clone(),
        host_details(identity.host_id().clone(), vec![LOOPBACK], None),
        0,
    );
    let routes = Router::new()
        .merge(hello::router(
            identity.host_id().clone(),
            svc.pairing_open_flag(),
        ))
        .merge(svc.router())
        .merge(test_routes());
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
        dir: path,
        server,
        svc,
        audit,
        identity,
        addr,
    }
}

// ---------- TLS clients ----------

/// Accepts any certificate and records the leaf (flow B's trust-on-first-use contact).
#[derive(Debug, Default)]
struct CaptureVerifier {
    leaf: Mutex<Option<Vec<u8>>>,
}

impl ServerCertVerifier for CaptureVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        *self.leaf.lock().unwrap() = Some(end_entity.to_vec());
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("no TLS 1.2".into()))
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Clone)]
enum Trust {
    Pin(HostId),
    Capture(Arc<CaptureVerifier>),
}

impl Trust {
    fn connector(&self) -> TlsConnector {
        match self {
            Trust::Pin(id) => TlsConnector::from(pinned_client_config(id.clone()).unwrap()),
            Trust::Capture(v) => {
                let mut cfg = rustls::ClientConfig::builder_with_provider(provider())
                    .with_protocol_versions(&[&rustls::version::TLS13])
                    .unwrap()
                    .dangerous()
                    .with_custom_certificate_verifier(v.clone())
                    .with_no_client_auth();
                cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
                TlsConnector::from(Arc::new(cfg))
            }
        }
    }

    /// SPKI of the certificate the client saw.
    fn seen_spki(&self) -> Vec<u8> {
        match self {
            Trust::Capture(v) => {
                let leaf = v.leaf.lock().unwrap().clone().expect("handshake done");
                spki_from_cert(&leaf).unwrap().to_vec()
            }
            Trust::Pin(_) => panic!("pinned client does not capture"),
        }
    }
}

async fn tls(addr: SocketAddr, trust: &Trust) -> tokio_rustls::client::TlsStream<TcpStream> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    trust
        .connector()
        .connect(ServerName::IpAddress(addr.ip().into()), tcp)
        .await
        .unwrap()
}

/// One HTTP/1.1 request (`Connection: close`); returns (status, body).
async fn call(
    addr: SocketAddr,
    trust: &Trust,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = tls(addr, trust).await;
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

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn pair_body(secret: Option<&str>, nonce: &[u8; 16]) -> String {
    let mut v = serde_json::json!({
        "device": {"name": "Pixel 9", "platform": "android", "model": "Pixel 9", "app_version": "0.1.0"},
        "nonce": b64url(nonce),
    });
    if let Some(s) = secret {
        v["secret"] = s.into();
    }
    v.to_string()
}

fn fresh_nonce() -> [u8; 16] {
    gander_host::pairing::random_bytes::<16>()
}

fn host_nonce(started: &PairStarted) -> [u8; 16] {
    b64url_fixed::<16>(&started.host_nonce).unwrap()
}

fn assert_no_secret_on_disk(host: &Host, secrets: &[&str]) {
    for path in walk(&host.dir) {
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for s in secrets {
            assert!(!text.contains(s), "secret found in {}", path.display());
        }
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Pairs a device through flow B with the owner's approval; returns the token.
async fn pair_device(host: &Host) -> (String, gander_core::protocol::DeviceId) {
    host.svc.open_window();
    let pin = Trust::Pin(host.identity.host_id().clone());
    let (status, body) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(None, &fresh_nonce())),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let started: PairStarted = serde_json::from_str(&body).unwrap();
    host.svc.approve(&started.pairing_id).unwrap();
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let approved: PairApproved = serde_json::from_str(&body).unwrap();
    (approved.token.expose().to_owned(), approved.device_id)
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread")]
async fn flow_a_qr_full_pairing() {
    let host = start_host().await;
    let qr_uri = host.svc.open_window().to_uri();
    let qr = QrPayload::parse(&qr_uri).unwrap();
    assert_eq!(&qr.host_id, host.identity.host_id());
    assert_eq!(qr.port, host.addr.port());
    assert_eq!(qr.addresses, vec![LOOPBACK]);

    // The phone pins the QR's host id from the first byte.
    let pin = Trust::Pin(qr.host_id.clone());
    let (status, body) = call(host.addr, &pin, "GET", "/v1/host", &[], None).await;
    assert_eq!(status, 200);
    let hello: HostHello = serde_json::from_str(&body).unwrap();
    assert!(hello.pairing_open);

    let nonce = fresh_nonce();
    let (status, body) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(Some(qr.secret.expose()), &nonce)),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let started: PairStarted = serde_json::from_str(&body).unwrap();
    // The phone computes the same SAS from the certificate it pinned.
    let phone_sas = derive_sas(host.identity.spki_der(), &nonce, &host_nonce(&started));
    assert_eq!(started.sas, phone_sas.to_string());
    let snap = host.svc.snapshot();
    let pending = snap.pending.expect("pending shown on the host");
    assert_eq!(pending.sas, phone_sas);
    assert_eq!(pending.flow.as_str(), "qr");
    assert_eq!(pending.identity, None, "loopback: identity unknown");

    let poll = format!("/v1/pair/{}", started.pairing_id);
    let (status, body) = call(host.addr, &pin, "GET", &poll, &[], None).await;
    assert_eq!((status, body.trim()), (202, r#"{"state":"pending"}"#));

    let device_id = host.svc.approve(&started.pairing_id).unwrap();
    let (status, body) = call(host.addr, &pin, "GET", &poll, &[], None).await;
    assert_eq!(status, 200, "{body}");
    let approved: PairApproved = serde_json::from_str(&body).unwrap();
    assert_eq!(approved.device_id, device_id);
    assert_eq!(&approved.host.id, host.identity.host_id());
    let token = approved.token.expose().to_owned();
    assert_eq!(token.len(), 43);
    // Exactly once.
    let (status, body) = call(host.addr, &pin, "GET", &poll, &[], None).await;
    assert_eq!(status, 410, "{body}");
    assert!(body.contains("pairing_expired"));
    // The window closed on approval.
    let (_, body) = call(host.addr, &pin, "GET", "/v1/host", &[], None).await;
    assert!(
        !serde_json::from_str::<HostHello>(&body)
            .unwrap()
            .pairing_open
    );

    // The token authenticates; missing and unknown tokens do not.
    let auth = bearer(&token);
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        "/v1/test/whoami",
        &[("Authorization", &auth)],
        None,
    )
    .await;
    assert_eq!((status, body.as_str()), (200, device_id.as_str()));
    let (status, body) = call(host.addr, &pin, "GET", "/v1/test/whoami", &[], None).await;
    assert_eq!(status, 401);
    assert!(body.contains("token_missing"));
    let other = bearer(&b64url(&gander_host::pairing::random_bytes::<32>()));
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        "/v1/test/whoami",
        &[("Authorization", &other)],
        None,
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("token_invalid"));

    // Only the hash is stored; neither the token nor the QR secret is on disk (devices.toml, audit).
    let record = host.svc.registry().get(&device_id).unwrap();
    assert!(gander_core::pairing::token::verify_token(
        &approved.token,
        &record.token_hash
    ));
    assert!(record.last_seen_at.is_some());
    assert_no_secret_on_disk(&host, &[&token, qr.secret.expose()]);
    let events: Vec<String> = host.audit.recent(50).into_iter().map(|e| e.event).collect();
    for want in [
        "pair.window_opened",
        "pair.attempt",
        "pair.approved",
        "pair.collected",
        "pair.window_closed",
    ] {
        assert!(
            events.iter().any(|e| e == want),
            "missing {want} in {events:?}"
        );
    }
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn flow_b_manual_pairing_with_tofu_and_sas() {
    let host = start_host().await;
    host.svc.open_window();
    let tofu = Trust::Capture(Arc::default());
    let (status, body) = call(host.addr, &tofu, "GET", "/v1/host", &[], None).await;
    assert_eq!(status, 200);
    let hello: HostHello = serde_json::from_str(&body).unwrap();
    // The certificate seen hashes to the advertised host id.
    assert_eq!(host_id_for_spki(&tofu.seen_spki()), hello.host_id);

    let nonce = fresh_nonce();
    let (status, body) = call(
        host.addr,
        &tofu,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(None, &nonce)),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let started: PairStarted = serde_json::from_str(&body).unwrap();
    let phone_sas = derive_sas(&tofu.seen_spki(), &nonce, &host_nonce(&started));
    let host_sas = host.svc.snapshot().pending.unwrap().sas;
    assert_eq!(phone_sas, host_sas, "codes match without a MITM");
    assert_eq!(host.svc.snapshot().pending.unwrap().flow.as_str(), "manual");

    host.svc.approve(&started.pairing_id).unwrap();
    let pin = Trust::Pin(hello.host_id.clone());
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        &[],
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let approved: PairApproved = serde_json::from_str(&body).unwrap();
    let (status, _) = call(
        host.addr,
        &pin,
        "GET",
        "/v1/test/whoami",
        &[("Authorization", &bearer(approved.token.expose()))],
        None,
    )
    .await;
    assert_eq!(status, 200);
    host.server.shutdown().await;
}

/// A TLS-terminating man in the middle with its own certificate, relaying to the real host.
async fn start_mitm(
    target: SocketAddr,
    target_id: HostId,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let mitm_identity = HostIdentity::generate().unwrap();
    let acceptor = TlsAcceptor::from(server_config(&mitm_identity).unwrap());
    let listener = TcpListener::bind((LOOPBACK, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let id = target_id.clone();
            tokio::spawn(async move {
                let Ok(mut client) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut upstream = tls(target, &Trust::Pin(id)).await;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    (addr, task)
}

#[tokio::test(flavor = "multi_thread")]
async fn mitm_with_a_different_certificate_yields_a_different_sas() {
    let host = start_host().await;
    host.svc.open_window();
    let (mitm, task) = start_mitm(host.addr, host.identity.host_id().clone()).await;

    // The phone types the MITM's address (flow B) and trusts what it sees.
    let tofu = Trust::Capture(Arc::default());
    let nonce = fresh_nonce();
    let (status, body) = call(
        mitm,
        &tofu,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(None, &nonce)),
    )
    .await;
    assert_eq!(status, 200, "relayed to the real host: {body}");
    let started: PairStarted = serde_json::from_str(&body).unwrap();
    let seen = tofu.seen_spki();
    assert_ne!(
        seen,
        host.identity.spki_der(),
        "the phone saw the MITM's key"
    );
    let phone_sas = derive_sas(&seen, &nonce, &host_nonce(&started));
    let host_sas = host.svc.snapshot().pending.unwrap().sas;
    assert_ne!(
        phone_sas, host_sas,
        "MITM must change the code the phone shows"
    );
    // The relayed response still carries the host's SAS, which the phone must ignore: the phone
    // displays the code IT computed.
    assert_eq!(started.sas, host_sas.to_string());
    task.abort();
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sixth_attempt_is_rate_limited() {
    let host = start_host().await;
    let qr = host.svc.open_window();
    let pin = Trust::Pin(host.identity.host_id().clone());
    let wrong = b64url(&gander_host::pairing::random_bytes::<32>());
    for i in 1..=5 {
        let (status, body) = call(
            host.addr,
            &pin,
            "POST",
            "/v1/pair",
            &[],
            Some(&pair_body(Some(&wrong), &fresh_nonce())),
        )
        .await;
        assert_eq!(status, 403, "attempt {i}: {body}");
    }
    assert_eq!(host.svc.snapshot().failures, 5);
    let (status, body) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(Some(qr.secret.expose()), &fresh_nonce())),
    )
    .await;
    assert_eq!(status, 429, "{body}");
    assert!(body.contains("pairing_rate_limited"));
    // Malformed bodies count too; re-opening resets.
    host.svc.open_window();
    for _ in 0..5 {
        let (status, _) = call(host.addr, &pin, "POST", "/v1/pair", &[], Some("{not json")).await;
        assert_eq!(status, 400);
    }
    let (status, _) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(None, &fresh_nonce())),
    )
    .await;
    assert_eq!(status, 429);
    assert_no_secret_on_disk(&host, &[qr.secret.expose(), &wrong]);
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_window_is_404_and_browser_requests_403() {
    let host = start_host().await;
    let pin = Trust::Pin(host.identity.host_id().clone());
    let body = pair_body(None, &fresh_nonce());
    let (status, resp) = call(host.addr, &pin, "POST", "/v1/pair", &[], Some(&body)).await;
    assert_eq!(status, 404, "{resp}");
    assert!(resp.contains("pairing_closed"));
    host.svc.open_window();
    host.svc.close_window();
    let (status, resp) = call(host.addr, &pin, "POST", "/v1/pair", &[], Some(&body)).await;
    assert_eq!(status, 404);
    assert!(resp.contains("pairing_closed"));
    host.svc.open_window();
    let (status, resp) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[("Origin", "https://evil.example")],
        Some(&body),
    )
    .await;
    assert_eq!(status, 403);
    assert!(resp.contains("browser_request"));
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn second_request_while_pending_is_409_and_deny_is_403_state() {
    let host = start_host().await;
    host.svc.open_window();
    let pin = Trust::Pin(host.identity.host_id().clone());
    let (status, body) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(None, &fresh_nonce())),
    )
    .await;
    assert_eq!(status, 200);
    let started: PairStarted = serde_json::from_str(&body).unwrap();
    let (status, body) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/pair",
        &[],
        Some(&pair_body(None, &fresh_nonce())),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("pairing_in_progress"));
    host.svc.deny(&started.pairing_id).unwrap();
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        &format!("/v1/pair/{}", started.pairing_id),
        &[],
        None,
    )
    .await;
    assert_eq!((status, body.trim()), (403, r#"{"state":"denied"}"#));
    let (status, _) = call(host.addr, &pin, "GET", "/v1/pair/pair_unknown", &[], None).await;
    assert_eq!(status, 404);
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn token_rotation_invalidates_the_old_token() {
    let host = start_host().await;
    let (old, device_id) = pair_device(&host).await;
    let pin = Trust::Pin(host.identity.host_id().clone());
    let (status, body) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/token/rotate",
        &[("Authorization", &bearer(&old))],
        Some(""),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let new: serde_json::Value = serde_json::from_str(&body).unwrap();
    let new = new["token"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        "/v1/test/whoami",
        &[("Authorization", &bearer(&old))],
        None,
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("token_invalid"));
    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        "/v1/test/whoami",
        &[("Authorization", &bearer(&new))],
        None,
    )
    .await;
    assert_eq!((status, body.as_str()), (200, device_id.as_str()));
    assert_no_secret_on_disk(&host, &[&old, &new]);
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_closes_an_open_websocket_within_one_second() {
    let host = start_host().await;
    let (token, device_id) = pair_device(&host).await;
    let pin = Trust::Pin(host.identity.host_id().clone());

    let stream = tls(host.addr, &pin).await;
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            "ws://127.0.0.1/v1/test/ws",
        )
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", bearer(&token).parse().unwrap());
    let (mut ws, resp) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    assert_eq!(resp.status(), 101);
    ws.send(tokio_tungstenite::tungstenite::Message::Ping(
        Vec::new().into(),
    ))
    .await
    .unwrap();
    assert!(host.svc.registry().open_connections(&device_id) >= 1);

    let t0 = Instant::now();
    assert!(host.svc.registry().revoke(&device_id).unwrap());
    let mut got_event = false;
    let closed = tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                    let ev: ServerEvent = serde_json::from_str(&t).unwrap();
                    assert_eq!(ev, ServerEvent::DeviceRevoked {});
                    got_event = true;
                }
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "socket still open 1 s after revocation");
    assert!(got_event, "device.revoked was not sent");
    assert!(t0.elapsed() < Duration::from_secs(1));

    let (status, body) = call(
        host.addr,
        &pin,
        "GET",
        "/v1/test/whoami",
        &[("Authorization", &bearer(&token))],
        None,
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("token_revoked"));
    let (status, _) = call(
        host.addr,
        &pin,
        "POST",
        "/v1/token/rotate",
        &[("Authorization", &bearer(&token))],
        Some(""),
    )
    .await;
    assert_eq!(status, 401);
    assert!(
        host.audit
            .recent(50)
            .iter()
            .any(|e| e.event == "device.revoked")
    );
    host.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_drops_an_idle_keep_alive_connection() {
    let host = start_host().await;
    let (token, device_id) = pair_device(&host).await;
    let pin = Trust::Pin(host.identity.host_id().clone());
    let mut stream = tls(host.addr, &pin).await;
    let req = format!(
        "GET /v1/test/whoami HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\n\r\n",
        bearer(&token)
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    host.svc.registry().revoke(&device_id).unwrap();
    let r = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut buf)).await;
    assert!(
        matches!(r, Ok(Ok(0)) | Ok(Err(_))),
        "keep-alive connection not closed: {r:?}"
    );
    host.server.shutdown().await;
}

// ---------- identity binding with a fake WhoIs (in-process: loopback has no identity) ----------

fn peer(login: &str, node: &str, tags: Vec<String>) -> PeerIdentity {
    PeerIdentity {
        login: login.into(),
        node_id: format!("id-{node}"),
        node_name: node.into(),
        tags,
        shared_in: false,
    }
}

async fn conn_from(net: &TrustNetwork, ip: &str) -> ConnInfo {
    let peer: SocketAddr = format!("{ip}:50000").parse().unwrap();
    ConnInfo {
        peer,
        local: "100.64.0.1:42637".parse().unwrap(),
        adapter: TrustAdapterKind::Tailscale,
        network: net.identify(peer).await,
        disconnect: DisconnectSignal::never(),
    }
}

async fn oneshot(
    app: &Router,
    conn: ConnInfo,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut b = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "100.64.0.1")
        .header("Content-Type", "application/json");
    if let Some(a) = auth {
        b = b.header("Authorization", a);
    }
    let mut req = b.body(axum::body::Body::from(body.to_owned())).unwrap();
    req.extensions_mut().insert(conn);
    req.extensions_mut().insert(ConnHandle::new());
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_is_captured_at_approval_and_mismatch_is_refused() {
    let dir = TempDir::new();
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
    let svc = PairingService::new(
        &identity,
        registry.clone(),
        audit.clone(),
        host_details(identity.host_id().clone(), vec![], None),
        42637,
    );
    let fake = FakeTailscale::new()
        .with_peer(
            "100.64.0.2".parse().unwrap(),
            peer("alice@example.com", "pixel-9", vec![]),
        )
        .with_peer(
            "100.64.0.3".parse().unwrap(),
            peer("mallory@example.com", "evil", vec![]),
        )
        .with_peer(
            "100.64.0.4".parse().unwrap(),
            peer("tagged-devices", "server", vec!["tag:srv".into()]),
        )
        .with_peer("100.64.0.5".parse().unwrap(), {
            let mut p = peer("alice@example.com", "pixel-9-new", vec![]);
            p.node_id = "id-reinstalled".into();
            p
        });
    let net = TrustNetwork::new(Arc::new(fake));
    let hosts = Arc::new(HostAllowList::default());
    hosts.set_addresses(["100.64.0.1".parse().unwrap()]);
    let routes = Router::new().merge(svc.router()).merge(test_routes());
    let app = apply_gate(routes, GateState::new(hosts, svc.authenticator()));

    svc.open_window();
    // Tagged node refused at pairing.
    let (status, body) = oneshot(
        &app,
        conn_from(&net, "100.64.0.4").await,
        "POST",
        "/v1/pair",
        None,
        &pair_body(None, &fresh_nonce()),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    // Alice's phone pairs; the dialog shows her login.
    let (status, body) = oneshot(
        &app,
        conn_from(&net, "100.64.0.2").await,
        "POST",
        "/v1/pair",
        None,
        &pair_body(None, &fresh_nonce()),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let started: PairStarted = serde_json::from_str(&body).unwrap();
    let pending = svc.snapshot().pending.unwrap();
    assert_eq!(
        pending.identity.as_ref().unwrap().login,
        "alice@example.com"
    );
    let device_id = svc.approve(&started.pairing_id).unwrap();
    let recorded = registry.get(&device_id).unwrap().network_identity.unwrap();
    assert_eq!(recorded.login, "alice@example.com");
    assert_eq!(recorded.node_name, "pixel-9");
    let poll = format!("/v1/pair/{}", started.pairing_id);
    // Another source cannot collect the token.
    let (status, _) = oneshot(
        &app,
        conn_from(&net, "100.64.0.3").await,
        "GET",
        &poll,
        None,
        "",
    )
    .await;
    assert_eq!(status, 404);
    let (status, body) = oneshot(
        &app,
        conn_from(&net, "100.64.0.2").await,
        "GET",
        &poll,
        None,
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let token = serde_json::from_str::<PairApproved>(&body).unwrap().token;
    let auth = bearer(token.expose());

    let (status, _) = oneshot(
        &app,
        conn_from(&net, "100.64.0.2").await,
        "GET",
        "/v1/test/whoami",
        Some(&auth),
        "",
    )
    .await;
    assert_eq!(status, 200);
    // Same token from another tailnet user: refused and audited.
    let (status, body) = oneshot(
        &app,
        conn_from(&net, "100.64.0.3").await,
        "GET",
        "/v1/test/whoami",
        Some(&auth),
        "",
    )
    .await;
    assert_eq!(status, 403);
    assert!(body.contains("network_identity_mismatch"));
    // From a non-tailnet source (no identity): fails closed.
    let (status, _) = oneshot(
        &app,
        conn_from(&net, "192.168.1.9").await,
        "GET",
        "/v1/test/whoami",
        Some(&auth),
        "",
    )
    .await;
    assert_eq!(status, 403);
    // Same login, new node id (reinstalled app): allowed with a warning.
    let (status, _) = oneshot(
        &app,
        conn_from(&net, "100.64.0.5").await,
        "GET",
        "/v1/test/whoami",
        Some(&auth),
        "",
    )
    .await;
    assert_eq!(status, 200);
    let events: Vec<String> = audit.recent(100).into_iter().map(|e| e.event).collect();
    assert!(events.iter().any(|e| e == "identity.mismatch"));
    assert!(events.iter().any(|e| e == "identity.node_changed"));
    // strict_node_binding turns the node change into a refusal.
    registry.set_trust(gander_core::config::TrustConfig {
        allow_tagged_peers: false,
        strict_node_binding: true,
    });
    let (status, _) = oneshot(
        &app,
        conn_from(&net, "100.64.0.5").await,
        "GET",
        "/v1/test/whoami",
        Some(&auth),
        "",
    )
    .await;
    assert_eq!(status, 403);
}

// ---------- test hook ----------

#[tokio::test(flavor = "multi_thread")]
async fn test_hook_refuses_non_loopback_and_auto_approves_loopback() {
    let host = start_host().await;
    let lan: SocketAddr = "100.64.0.1:42637".parse().unwrap();
    assert!(host.svc.enable_test_approve(&[host.addr, lan]).is_err());
    assert!(host.svc.enable_test_approve(&[]).is_err());
    assert!(!host.svc.test_approve_enabled());
    host.svc
        .enable_test_approve(&host.server.bound_addrs())
        .unwrap();
    let pin = Trust::Pin(host.identity.host_id().clone());
    for _ in 0..2 {
        // No UI involved; the window re-opens itself for the next harness pairing.
        let (status, body) = call(
            host.addr,
            &pin,
            "POST",
            "/v1/pair",
            &[],
            Some(&pair_body(None, &fresh_nonce())),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        let started: PairStarted = serde_json::from_str(&body).unwrap();
        let (status, body) = call(
            host.addr,
            &pin,
            "GET",
            &format!("/v1/pair/{}", started.pairing_id),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }
    assert!(
        host.audit
            .recent(100)
            .iter()
            .any(|e| e.event == "pair.test_approved")
    );
    let _ = PairingId::from("unused");
    host.server.shutdown().await;
}
