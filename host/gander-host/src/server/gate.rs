// SPDX-License-Identifier: Apache-2.0
//! The request gate: middleware every request passes before reaching a route.
//!
//! Order (outermost first):
//!
//! 1. **response headers**: `X-Gander-Protocol: 1` and `X-Gander-Request: <ulid>` on every
//!    response, including refusals; the id is also put in the request extensions ([`RequestId`]).
//! 2. **browser markers**: `Origin`, `Sec-Fetch-Mode` or `Cookie` -> 403 `browser_request`.
//! 3. **Host header**: must name an address the host serves (or `localhost` on a loopback
//!    connection), with the connection's port if a port is given -> otherwise 400 `bad_host`.
//! 4. **authentication**: the [`Authenticator`] decides who the caller is ([`Caller`] extension)
//!    or refuses. The default, [`HelloOnly`], admits only `GET /v1/host`; WP-13 supplies the
//!    token authenticator.
//! 5. **concurrency**: [`ConcurrencyLimiter`] per device (or per peer IP when anonymous) ->
//!    429 `too_many_requests` with `Retry-After`. The permit lives until the body is sent.
//! 6. **protocol errors**: an error response that is not a protocol error body (axum's own
//!    extractor rejections and method-not-allowed answers are plain text) is replaced by one:
//!    413 -> `too_large`, any other 4xx -> 400 `bad_request`, 5xx -> 500 `internal`.
//!
//! Routes then read [`ConnInfo`], [`Caller`] and [`RequestId`] with `Extension<...>`.

use std::collections::HashSet;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use gander_core::error::ErrorCode;
use gander_core::net::tailscale::PeerIdentity;
use gander_core::protocol::host::TrustAdapterKind;
use gander_core::protocol::{DeviceId, PROTOCOL_VERSION, headers};
use hyper::body::{Body as HttpBody, Frame, SizeHint};

use super::error::ApiError;
use super::limiter::{ConcurrencyLimiter, LimitKey, Permit};

/// Per-connection facts, inserted into every request's extensions.
#[derive(Debug, Clone)]
pub struct ConnInfo {
    /// The client's address.
    pub peer: SocketAddr,
    /// The local socket the connection arrived on.
    pub local: SocketAddr,
    /// The trust adapter active when the connection was accepted.
    pub adapter: TrustAdapterKind,
    /// The peer's private-network identity, looked up once per connection (cached 60 s).
    pub network: PeerNetwork,
    /// Fires when the owner drops every session (kill switch) or the server shuts down. Long-lived
    /// handlers (WebSocket, streams) must watch it: an upgraded connection is no longer covered
    /// by the HTTP connection task.
    pub disconnect: DisconnectSignal,
}

/// Disconnect notification for long-lived handlers; see [`ConnInfo::disconnect`].
#[derive(Debug, Clone)]
pub struct DisconnectSignal(pub tokio::sync::watch::Receiver<u64>);

impl DisconnectSignal {
    /// A signal that never fires (tests).
    pub fn never() -> Self {
        let (tx, rx) = tokio::sync::watch::channel(0);
        // Keep the sender alive for the life of the process so `fired` never resolves.
        std::mem::forget(tx);
        Self(rx)
    }

    /// Resolves when the kill switch fires or the server goes away.
    pub async fn fired(&mut self) {
        let _ = self.0.changed().await;
    }
}

/// What the trust adapter knows about the peer of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerNetwork {
    /// The connection came over loopback.
    Loopback,
    /// Tailscale WhoIs resolved the peer.
    Identified(PeerIdentity),
    /// No network identity: generic adapter, or WhoIs said "not a tailnet node".
    NoIdentity,
    /// WhoIs could not be asked (tailscaled unreachable, timeout).
    Unavailable,
}

impl PeerNetwork {
    /// The identity, if any.
    pub fn identity(&self) -> Option<&PeerIdentity> {
        match self {
            PeerNetwork::Identified(p) => Some(p),
            _ => None,
        }
    }
}

/// Who is calling, decided by the [`Authenticator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// No token (allowed only on the unauthenticated endpoints).
    Anonymous,
    /// A paired device with a valid token.
    Device(DeviceId),
}

/// The ULID of the request, also sent as `X-Gander-Request`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

/// Boxed future returned by [`Authenticator::authenticate`].
pub type AuthFuture<'a> = Pin<Box<dyn Future<Output = Result<Caller, ApiError>> + Send + 'a>>;

/// Decides who a request comes from. Runs after the browser and `Host` checks and before the
/// concurrency limiter and the routes.
///
/// Implementations must refuse (401 `token_missing` / `token_invalid` / `token_revoked`, 403
/// `network_identity_mismatch`) rather than return [`Caller::Anonymous`] for endpoints that need
/// a token.
pub trait Authenticator: Send + Sync + std::fmt::Debug + 'static {
    /// Authenticates one request.
    fn authenticate<'a>(&'a self, head: &'a Parts, conn: &'a ConnInfo) -> AuthFuture<'a>;
}

/// The default authenticator until pairing exists: `GET`/`HEAD /v1/host` is anonymous, every
/// other request gets 401 `token_missing`. Deny by default.
#[derive(Debug, Clone, Copy, Default)]
pub struct HelloOnly;

impl Authenticator for HelloOnly {
    fn authenticate<'a>(&'a self, head: &'a Parts, _conn: &'a ConnInfo) -> AuthFuture<'a> {
        let open = (head.method == Method::GET || head.method == Method::HEAD)
            && head.uri.path() == "/v1/host";
        Box::pin(async move {
            if open {
                Ok(Caller::Anonymous)
            } else {
                Err(ApiError::new(
                    ErrorCode::TokenMissing,
                    "This request needs a device token.",
                ))
            }
        })
    }
}

/// The Android emulator's alias for the host computer's loopback. Emulator traffic arrives on
/// 127.0.0.1 with `Host: 10.0.2.2:<port>`; only the integration-test mode accepts it (see
/// [`HostAllowList::set_loopback_aliases`]).
pub const ANDROID_EMULATOR_HOST: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 2, 2));

/// Names the `Host` header may carry. Kept current by the listener (served addresses); extra
/// names (for example the MagicDNS name) may be added by the application.
#[derive(Debug, Default)]
pub struct HostAllowList {
    addresses: RwLock<HashSet<IpAddr>>,
    names: RwLock<HashSet<String>>,
    loopback_aliases: RwLock<HashSet<IpAddr>>,
}

impl HostAllowList {
    /// Replaces the served addresses.
    pub fn set_addresses(&self, addrs: impl IntoIterator<Item = IpAddr>) {
        *self.addresses.write().unwrap_or_else(|e| e.into_inner()) = addrs.into_iter().collect();
    }

    /// Replaces the extra DNS names (lower-cased, trailing dot removed).
    pub fn set_names(&self, names: impl IntoIterator<Item = String>) {
        *self.names.write().unwrap_or_else(|e| e.into_inner()) = names
            .into_iter()
            .map(|n| n.trim_end_matches('.').to_ascii_lowercase())
            .filter(|n| !n.is_empty())
            .collect();
    }

    /// Replaces the address literals that are also accepted, but only on a connection that
    /// arrived on a loopback socket. Empty by default; the binary's `--test-approve` mode sets
    /// [`ANDROID_EMULATOR_HOST`] so an emulator can reach a loopback-only test host.
    pub fn set_loopback_aliases(&self, addrs: impl IntoIterator<Item = IpAddr>) {
        *self
            .loopback_aliases
            .write()
            .unwrap_or_else(|e| e.into_inner()) = addrs.into_iter().collect();
    }

    /// Whether a `Host` header value is acceptable on a connection to `local`.
    pub fn allows(&self, host: &str, local: SocketAddr) -> bool {
        let Some((name, port)) = split_host(host) else {
            return false;
        };
        if port.is_some_and(|p| p != local.port()) {
            return false;
        }
        if let Ok(ip) = name.parse::<IpAddr>() {
            return self
                .addresses
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&ip)
                || (local.ip().is_loopback()
                    && self
                        .loopback_aliases
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(&ip));
        }
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if name == "localhost" {
            return local.ip().is_loopback();
        }
        self.names
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&name)
    }
}

/// Splits `host[:port]` / `[v6][:port]`. `None` when malformed.
fn split_host(host: &str) -> Option<(&str, Option<u16>)> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if let Some(rest) = host.strip_prefix('[') {
        let (inner, after) = rest.split_once(']')?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':')?.parse().ok()?),
        };
        return Some((inner, port));
    }
    match host.rsplit_once(':') {
        // A bare IPv6 address without brackets is not a valid Host header.
        Some((h, _)) if h.contains(':') => None,
        Some((h, p)) => Some((h, Some(p.parse().ok()?))),
        None => Some((host, None)),
    }
}

/// State shared by the gate middleware.
#[derive(Debug, Clone)]
pub struct GateState {
    /// `Host` header allow list.
    pub hosts: Arc<HostAllowList>,
    /// Who is calling.
    pub authenticator: Arc<dyn Authenticator>,
    /// Per-device concurrency.
    pub limiter: ConcurrencyLimiter,
}

impl GateState {
    /// A gate with the default limit (`PER_DEVICE_CONCURRENCY`).
    pub fn new(hosts: Arc<HostAllowList>, authenticator: Arc<dyn Authenticator>) -> Self {
        Self {
            hosts,
            authenticator,
            limiter: ConcurrencyLimiter::new(
                gander_core::protocol::PER_DEVICE_CONCURRENCY as usize,
            ),
        }
    }
}

/// Wraps `routes` in the gate. `routes` is any `Router` (state already provided). Unknown paths
/// get 404 `not_found`; bodies over `MAX_REQUEST_BODY` are refused by axum's body limit.
pub fn apply_gate(routes: Router, gate: GateState) -> Router {
    routes
        .fallback(|| async { ApiError::new(ErrorCode::NotFound, "No such endpoint.") })
        .layer(axum::extract::DefaultBodyLimit::max(
            gander_core::protocol::MAX_REQUEST_BODY,
        ))
        .layer(middleware::from_fn(protocol_errors))
        .layer(middleware::from_fn_with_state(gate.clone(), limit))
        .layer(middleware::from_fn_with_state(gate.clone(), authenticate))
        .layer(middleware::from_fn_with_state(gate, check_host))
        .layer(middleware::from_fn(refuse_browsers))
        .layer(middleware::from_fn(protocol_headers))
}

/// Every error leaves the host as a protocol error body (`{"error": {...}}`), including the
/// plain-text rejections axum produces itself (body over the limit, malformed extractor input,
/// method not allowed). Found by the WP-1B e2e suite: an oversized `POST /v1/stream` body came
/// back as `413 text/plain`.
async fn protocol_errors(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let status = resp.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return resp;
    }
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/json"));
    if is_json {
        return resp;
    }
    let error = match status.as_u16() {
        413 => ApiError::new(ErrorCode::TooLarge, "The request body is too large."),
        s if s >= 500 => ApiError::new(ErrorCode::Internal, "Internal error."),
        _ => ApiError::new(ErrorCode::BadRequest, "Malformed request."),
    };
    error.into_response()
}

async fn protocol_headers(mut req: Request, next: Next) -> Response {
    let id = ulid::Ulid::new().to_string();
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        headers::PROTOCOL,
        HeaderValue::from_str(&PROTOCOL_VERSION.to_string()).expect("digits"),
    );
    if let Ok(v) = HeaderValue::from_str(&id) {
        h.insert(headers::REQUEST_ID, v);
    }
    resp
}

async fn refuse_browsers(req: Request, next: Next) -> Response {
    if headers::BROWSER_MARKERS
        .iter()
        .any(|m| req.headers().contains_key(*m))
    {
        return ApiError::new(
            ErrorCode::BrowserRequest,
            "Browser requests are not accepted.",
        )
        .into_response();
    }
    next.run(req).await
}

fn conn_of(req: &Request) -> Option<ConnInfo> {
    req.extensions().get::<ConnInfo>().cloned()
}

/// Every connection inserts [`ConnInfo`]; its absence is a wiring bug.
fn missing_conn() -> Response {
    ApiError::new(ErrorCode::Internal, "Internal error.").into_response()
}

async fn check_host(State(gate): State<GateState>, req: Request, next: Next) -> Response {
    let Some(conn) = conn_of(&req) else {
        return missing_conn();
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.to_string()));
    match host {
        Some(h) if gate.hosts.allows(&h, conn.local) => next.run(req).await,
        _ => ApiError::new(ErrorCode::BadHost, "Unexpected Host header.").into_response(),
    }
}

async fn authenticate(State(gate): State<GateState>, req: Request, next: Next) -> Response {
    let Some(conn) = conn_of(&req) else {
        return missing_conn();
    };
    let (mut parts, body) = req.into_parts();
    let caller = match gate.authenticator.authenticate(&parts, &conn).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    parts.extensions.insert(caller);
    next.run(Request::from_parts(parts, body)).await
}

async fn limit(
    State(gate): State<GateState>,
    Extension(conn): Extension<ConnInfo>,
    Extension(caller): Extension<Caller>,
    req: Request,
    next: Next,
) -> Response {
    let key = match caller {
        Caller::Device(id) => LimitKey::Device(id),
        Caller::Anonymous => LimitKey::Peer(conn.peer.ip()),
    };
    let Some(permit) = gate.limiter.try_acquire(key) else {
        return ApiError::retry_after(
            ErrorCode::TooManyRequests,
            "Too many requests in flight from this device.",
            1000,
        )
        .into_response();
    };
    let resp = next.run(req).await;
    let (parts, body) = resp.into_parts();
    Response::from_parts(
        parts,
        Body::new(PermitBody {
            inner: body,
            _permit: permit,
        }),
    )
}

/// A response body that holds a limiter permit until it is fully sent or dropped.
struct PermitBody {
    inner: Body,
    _permit: Permit,
}

impl HttpBody for PermitBody {
    type Data = axum::body::Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::routing::get;
    use tower::ServiceExt;

    fn local() -> SocketAddr {
        "100.64.0.1:42637".parse().unwrap()
    }

    fn app(auth: Arc<dyn Authenticator>, routes: Router) -> (Router, GateState) {
        let hosts = Arc::new(HostAllowList::default());
        hosts.set_addresses(["100.64.0.1".parse().unwrap(), "127.0.0.1".parse().unwrap()]);
        let gate = GateState::new(hosts, auth);
        (apply_gate(routes, gate.clone()), gate)
    }

    fn req(path: &str, host: &str, local: SocketAddr) -> Request {
        let mut r = Request::builder()
            .uri(path)
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        r.extensions_mut().insert(ConnInfo {
            peer: "100.64.0.2:50000".parse().unwrap(),
            local,
            adapter: TrustAdapterKind::Tailscale,
            network: PeerNetwork::NoIdentity,
            disconnect: DisconnectSignal::never(),
        });
        r
    }

    fn hello() -> Router {
        Router::new().route("/v1/host", get(|| async { "hi" }))
    }

    #[tokio::test]
    async fn headers_on_success_and_refusal() {
        let (app, _) = app(Arc::new(HelloOnly), hello());
        let r = app
            .clone()
            .oneshot(req("/v1/host", "100.64.0.1:42637", local()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[headers::PROTOCOL], "1");
        assert_eq!(r.headers()[headers::REQUEST_ID].len(), 26);
        let r = app
            .oneshot(req("/v1/shares", "100.64.0.1", local()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert!(r.headers().contains_key(headers::REQUEST_ID));
    }

    #[tokio::test]
    async fn browser_markers_refused() {
        let (app, _) = app(Arc::new(HelloOnly), hello());
        for marker in headers::BROWSER_MARKERS {
            let mut r = req("/v1/host", "100.64.0.1", local());
            r.headers_mut()
                .insert(marker, HeaderValue::from_static("x"));
            let resp = app.clone().oneshot(r).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{marker}");
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            assert!(String::from_utf8_lossy(&body).contains("browser_request"));
        }
    }

    #[tokio::test]
    async fn host_header_rules() {
        let (app, gate) = app(Arc::new(HelloOnly), hello());
        let lo: SocketAddr = "127.0.0.1:42637".parse().unwrap();
        let cases = [
            ("100.64.0.1", local(), StatusCode::OK),
            ("100.64.0.1:42637", local(), StatusCode::OK),
            ("100.64.0.1:1", local(), StatusCode::BAD_REQUEST),
            ("evil.example", local(), StatusCode::BAD_REQUEST),
            ("localhost", local(), StatusCode::BAD_REQUEST),
            ("localhost:42637", lo, StatusCode::OK),
            ("127.0.0.1", lo, StatusCode::OK),
            ("10.0.0.1", local(), StatusCode::BAD_REQUEST),
            ("", local(), StatusCode::BAD_REQUEST),
            (
                "studio-pc.tail0000.ts.net",
                local(),
                StatusCode::BAD_REQUEST,
            ),
        ];
        for (host, l, want) in cases {
            let r = app.clone().oneshot(req("/v1/host", host, l)).await.unwrap();
            assert_eq!(r.status(), want, "Host: {host:?}");
        }
        gate.hosts
            .set_names(["Studio-PC.tail0000.ts.net.".to_owned()]);
        let r = app
            .oneshot(req("/v1/host", "studio-pc.tail0000.ts.net", local()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[test]
    fn emulator_alias_only_in_test_mode_and_only_on_loopback() {
        let lo: SocketAddr = "127.0.0.1:42637".parse().unwrap();
        let list = HostAllowList::default();
        list.set_addresses(["127.0.0.1".parse().unwrap()]);
        // Normal mode: the emulator alias is an unknown address.
        assert!(!list.allows("10.0.2.2:42637", lo));
        assert!(!list.allows("10.0.2.2", lo));
        // Test mode (`--test-approve`): accepted on a loopback connection only.
        list.set_loopback_aliases([ANDROID_EMULATOR_HOST]);
        assert!(list.allows("10.0.2.2:42637", lo));
        assert!(list.allows("10.0.2.2", lo));
        assert!(!list.allows("10.0.2.2:1", lo), "port must still match");
        assert!(
            !list.allows("10.0.2.2:42637", local()),
            "never on a non-loopback socket"
        );
        assert!(!list.allows("10.0.2.3:42637", lo));
        list.set_loopback_aliases([]);
        assert!(!list.allows("10.0.2.2:42637", lo));
    }

    #[test]
    fn split_host_forms() {
        assert_eq!(split_host("[::1]:80"), Some(("::1", Some(80))));
        assert_eq!(split_host("[::1]"), Some(("::1", None)));
        assert_eq!(split_host("::1"), None);
        assert_eq!(split_host("a:b"), None);
        assert_eq!(split_host("[::1]x"), None);
    }

    #[tokio::test]
    async fn concurrency_limit_holds_until_body_done() {
        #[derive(Debug)]
        struct AsDevice;
        impl Authenticator for AsDevice {
            fn authenticate<'a>(&'a self, _: &'a Parts, _: &'a ConnInfo) -> AuthFuture<'a> {
                Box::pin(async { Ok(Caller::Device(DeviceId::from("dev_1"))) })
            }
        }
        let (app, gate) = app(Arc::new(AsDevice), hello());
        let key = LimitKey::Device(DeviceId::from("dev_1"));
        let mut held = Vec::new();
        for _ in 0..gate.limiter.limit() {
            let r = app
                .clone()
                .oneshot(req("/v1/host", "100.64.0.1", local()))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            held.push(r); // body not consumed: still in flight
        }
        assert_eq!(gate.limiter.in_flight(&key), 8);
        let r = app
            .clone()
            .oneshot(req("/v1/host", "100.64.0.1", local()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()[header::RETRY_AFTER], "1");
        let first = held.remove(0);
        let _ = axum::body::to_bytes(first.into_body(), 1024).await.unwrap();
        assert_eq!(gate.limiter.in_flight(&key), 7);
        drop(held);
        assert_eq!(gate.limiter.in_flight(&key), 0);
    }

    #[tokio::test]
    async fn framework_rejections_become_protocol_errors() {
        #[derive(Debug)]
        struct Anyone;
        impl Authenticator for Anyone {
            fn authenticate<'a>(&'a self, _: &'a Parts, _: &'a ConnInfo) -> AuthFuture<'a> {
                Box::pin(async { Ok(Caller::Anonymous) })
            }
        }
        let routes = Router::new().route(
            "/v1/echo",
            axum::routing::post(|body: axum::body::Bytes| async move { body.len().to_string() }),
        );
        let (app, _) = app(Arc::new(Anyone), routes);
        let post = |len: usize| {
            let mut r = req("/v1/echo", "100.64.0.1", local());
            *r.method_mut() = Method::POST;
            *r.body_mut() = Body::from(vec![b'x'; len]);
            r
        };
        let ok = app.clone().oneshot(post(10)).await.unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let cases = [
            (
                post(gander_core::protocol::MAX_REQUEST_BODY + 1),
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
            ),
            (
                req("/v1/echo", "100.64.0.1", local()),
                StatusCode::BAD_REQUEST,
                "bad_request",
            ),
        ];
        for (r, status, code) in cases {
            let resp = app.clone().oneshot(r).await.unwrap();
            assert_eq!(resp.status(), status);
            assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
            assert!(resp.headers().contains_key(headers::REQUEST_ID));
            let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["error"]["code"], code);
        }
    }

    #[tokio::test]
    async fn unknown_path_is_not_found_after_auth() {
        #[derive(Debug)]
        struct Anyone;
        impl Authenticator for Anyone {
            fn authenticate<'a>(&'a self, _: &'a Parts, _: &'a ConnInfo) -> AuthFuture<'a> {
                Box::pin(async { Ok(Caller::Anonymous) })
            }
        }
        let (app, _) = app(Arc::new(Anyone), hello());
        let r = app
            .oneshot(req("/v1/nope", "100.64.0.1", local()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }
}
