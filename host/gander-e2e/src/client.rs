// SPDX-License-Identifier: Apache-2.0
//! A pinned TLS 1.3 HTTP/1.1 client: one connection per request, pinned to the host id the host
//! printed, `Host: 127.0.0.1:<port>` unless a case overrides it.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use gander_core::protocol::HostId;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// Longest a single request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One request. Built with [`Req::get`] / [`Req::new`] and the builder methods.
#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Send the client's bearer token (default true).
    pub auth: bool,
    /// Replace the `Host` header (`None`: `127.0.0.1:<port>`).
    pub host: Option<String>,
}

impl Req {
    pub fn new(method: &str, target: impl Into<String>) -> Self {
        Self {
            method: method.to_owned(),
            target: target.into(),
            headers: Vec::new(),
            body: Vec::new(),
            auth: true,
            host: None,
        }
    }

    pub fn get(target: impl Into<String>) -> Self {
        Self::new("GET", target)
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_owned(), value.into()));
        self
    }

    pub fn json(mut self, body: &Value) -> Self {
        self.body = serde_json::to_vec(body).unwrap_or_default();
        self.header("Content-Type", "application/json")
    }

    pub fn raw_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    pub fn anonymous(mut self) -> Self {
        self.auth = false;
        self
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }
}

/// A complete response.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Resp {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// `error.code` of an error body.
    pub fn error_code(&self) -> Option<String> {
        self.json()?
            .pointer("/error/code")?
            .as_str()
            .map(str::to_owned)
    }

    /// Short description for a failure message (status, code, never the body of a success).
    pub fn describe(&self) -> String {
        match self.error_code() {
            Some(c) => format!("HTTP {} {c}", self.status),
            None => format!(
                "HTTP {} ({} bytes, {})",
                self.status,
                self.body.len(),
                self.header("content-type").unwrap_or("no content-type")
            ),
        }
    }
}

/// A response whose body is still on the wire (the connection stays open while it lives).
#[derive(Debug)]
pub struct Held {
    pub status: u16,
    pub _body: hyper::Response<Incoming>,
    pub _conn: tokio::task::JoinHandle<()>,
}

/// The pinned client.
#[derive(Clone)]
pub struct Client {
    pub addr: SocketAddr,
    pub host_id: HostId,
    tls: TlsConnector,
    pub token: Option<String>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never prints the token.
        f.debug_struct("Client")
            .field("addr", &self.addr)
            .field("host_id", &self.host_id)
            .finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(addr: SocketAddr, host_id: HostId) -> Result<Self> {
        let config = gander_host::server::tls::pinned_client_config(host_id.clone())
            .context("client TLS config")?;
        Ok(Self {
            addr,
            host_id,
            tls: TlsConnector::from(config),
            token: None,
        })
    }

    /// The same client pinned to another host id (the pin check case).
    pub fn with_pin(&self, host_id: HostId) -> Result<Self> {
        let mut c = Self::new(self.addr, host_id)?;
        c.token.clone_from(&self.token);
        Ok(c)
    }

    pub fn with_token(&self, token: Option<String>) -> Self {
        let mut c = self.clone();
        c.token = token;
        c
    }

    pub fn default_host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }

    /// A TLS connection to the host.
    pub async fn connect(&self) -> Result<TlsStream<TcpStream>> {
        let tcp = TcpStream::connect(self.addr).await.context("tcp connect")?;
        let name = ServerName::IpAddress(self.addr.ip().into());
        self.tls.connect(name, tcp).await.context("tls handshake")
    }

    fn build(&self, req: &Req) -> Result<hyper::Request<Full<Bytes>>> {
        let mut b = hyper::Request::builder()
            .method(req.method.as_str())
            .uri(req.target.as_str());
        let headers = b.headers_mut().context("request builder")?;
        let host = req.host.clone().unwrap_or_else(|| self.default_host());
        headers.insert(hyper::header::HOST, HeaderValue::from_str(&host)?);
        if req.auth
            && let Some(t) = &self.token
        {
            headers.insert(
                hyper::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {t}"))?,
            );
        }
        for (k, v) in &req.headers {
            headers.append(
                HeaderName::from_bytes(k.as_bytes())?,
                HeaderValue::from_str(v)?,
            );
        }
        Ok(b.body(Full::new(Bytes::from(req.body.clone())))?)
    }

    async fn open(
        &self,
        req: &Req,
    ) -> Result<(hyper::Response<Incoming>, tokio::task::JoinHandle<()>)> {
        let request = self.build(req)?;
        let io = TokioIo::new(self.connect().await?);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .context("http handshake")?;
        let task = tokio::spawn(async move {
            let _ = conn.await;
        });
        let resp = sender.send_request(request).await.context("send request")?;
        Ok((resp, task))
    }

    /// Sends `req` and reads the whole response.
    pub async fn send(&self, req: &Req) -> Result<Resp> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let (resp, task) = self.open(req).await?;
            let (parts, body) = resp.into_parts();
            let body = body.collect().await.context("read body")?.to_bytes();
            task.abort();
            Ok(Resp {
                status: parts.status.as_u16(),
                headers: parts.headers,
                body: body.to_vec(),
            })
        })
        .await
        .context("request timed out")?
    }

    /// Sends `req` and returns once the head arrived, leaving the body unread.
    pub async fn send_held(&self, req: &Req) -> Result<Held> {
        tokio::time::timeout(REQUEST_TIMEOUT, async {
            let (resp, task) = self.open(req).await?;
            Ok(Held {
                status: resp.status().as_u16(),
                _body: resp,
                _conn: task,
            })
        })
        .await
        .context("request timed out")?
    }
}

/// Percent-encodes a share-relative path for a query value (UTF-8; `/` kept).
pub fn enc(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn enc_keeps_slashes_and_escapes_the_rest() {
        assert_eq!(super::enc("a b/c+d/\u{e9}"), "a%20b/c%2Bd/%C3%A9");
    }
}
