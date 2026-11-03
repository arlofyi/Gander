// SPDX-License-Identifier: Apache-2.0
//! Signalling over a plain WebSocket (spike only: no TLS, no auth; the host binds 127.0.0.1).
//!
//! One JSON object per text frame, tagged by `type`:
//!
//! | Direction | Message |
//! | --- | --- |
//! | viewer -> host | `{"type":"hello","client":"<name>","version":1}` |
//! | host -> viewer | `{"type":"offer","sdp":"v=0..."}` (complete: ICE candidates are in the SDP) |
//! | viewer -> host | `{"type":"answer","sdp":"v=0..."}` |
//! | either | `{"type":"candidate","candidate":"candidate:...","sdpMid":"0","sdpMLineIndex":0}` (trickle; unused here, gathering is instant) |
//! | either | `{"type":"bye"}` |

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

/// A signalling message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Signal {
    /// First message from the viewer.
    Hello {
        /// Free-form client name.
        client: String,
        /// Signalling version (1).
        version: u32,
    },
    /// SDP offer from the host.
    Offer {
        /// SDP text.
        sdp: String,
    },
    /// SDP answer from the viewer.
    Answer {
        /// SDP text.
        sdp: String,
    },
    /// One trickled ICE candidate.
    Candidate {
        /// `candidate:` attribute value.
        candidate: String,
        /// m-section id.
        #[serde(rename = "sdpMid")]
        sdp_mid: Option<String>,
        /// m-section index.
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: Option<u16>,
    },
    /// End of session.
    Bye,
}

/// One end of a signalling connection.
#[derive(Debug)]
pub struct Channel {
    ws: WebSocketStream<TcpStream>,
}

impl Channel {
    /// Sends one message.
    pub async fn send(&mut self, msg: &Signal) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.ws.send(Message::text(text)).await?;
        Ok(())
    }

    /// Receives the next message (skipping pings and other non-text frames).
    pub async fn recv(&mut self) -> Result<Signal> {
        loop {
            match self.ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    return serde_json::from_str(t.as_str()).context("bad signalling JSON");
                }
                Some(Ok(Message::Close(_))) | None => bail!("signalling closed"),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}

/// Listens on 127.0.0.1 with an ephemeral port. Never a wildcard address.
pub async fn listen() -> Result<TcpListener> {
    listen_on(0).await
}

/// Listens on 127.0.0.1 with the given port (0 = ephemeral). Never a wildcard address.
pub async fn listen_on(port: u16) -> Result<TcpListener> {
    let l = TcpListener::bind(("127.0.0.1", port))
        .await
        .context("bind signalling")?;
    Ok(l)
}

/// Accepts one viewer.
pub async fn accept(listener: &TcpListener) -> Result<Channel> {
    let (tcp, _) = listener.accept().await?;
    tcp.set_nodelay(true)?;
    let ws = tokio_tungstenite::accept_async(tcp).await?;
    Ok(Channel { ws })
}

/// Connects to `ws://<addr>/`.
pub async fn connect(addr: std::net::SocketAddr) -> Result<Channel> {
    // Bind the client end to loopback explicitly; an unbound connect shows up on Windows as a
    // `0.0.0.0:<port> Bound` row (not listening, but it looks like a wildcard).
    let sock = tokio::net::TcpSocket::new_v4()?;
    sock.bind((std::net::Ipv4Addr::LOCALHOST, 0).into())?;
    let tcp = sock.connect(addr).await?;
    tcp.set_nodelay(true)?;
    let (ws, _) = tokio_tungstenite::client_async(format!("ws://{addr}/"), tcp).await?;
    Ok(Channel { ws })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_shapes() {
        let j = serde_json::to_string(&Signal::Offer { sdp: "v=0".into() }).unwrap();
        assert_eq!(j, r#"{"type":"offer","sdp":"v=0"}"#);
        let c: Signal = serde_json::from_str(
            r#"{"type":"candidate","candidate":"candidate:1 1 udp 1 127.0.0.1 9 typ host","sdpMid":"0","sdpMLineIndex":0}"#,
        )
        .unwrap();
        assert!(matches!(
            c,
            Signal::Candidate {
                sdp_mline_index: Some(0),
                ..
            }
        ));
        assert_eq!(
            serde_json::to_string(&Signal::Bye).unwrap(),
            r#"{"type":"bye"}"#
        );
    }
}
