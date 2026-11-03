// SPDX-License-Identifier: Apache-2.0
//! One `/v1/events` socket: send `hello` and `host.state`, then forward what the host pushes and
//! answer the client until something ends the connection.
//!
//! The loop ends when (first one wins):
//!
//! - the connection handle fires (device revoked: `device.revoked`, then close);
//! - the registry reports this device revoked or removed;
//! - the kill switch fires ([`crate::server::ConnInfo::disconnect`]);
//! - a newer socket of the same device pushed this one out (per-device cap);
//! - the client closes, errors, or misses two heartbeats;
//! - a send takes longer than the send timeout, or the socket falls behind the broadcast buffer.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use gander_core::protocol::events::{ClientEvent, ServerEvent};
use gander_core::protocol::{DeviceId, PROTOCOL_VERSION};
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{MissedTickBehavior, interval_at, timeout};

use super::{HubEvent, Inner};
use crate::pairing::DeviceEvent;
use crate::server::{CloseReason, ConnHandle, DisconnectSignal};

/// Normal closure (after `device.revoked`).
const CLOSE_NORMAL: u16 = 1000;
/// Going away (kill switch, replaced by a newer socket).
const CLOSE_GOING_AWAY: u16 = 1001;
/// Policy violation (message type the channel does not carry).
const CLOSE_POLICY: u16 = 1008;
/// Try again later (this socket fell behind).
const CLOSE_TRY_AGAIN: u16 = 1013;

/// How long a close handshake may take before the socket is simply dropped.
const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// Sends one event as a JSON text frame. `false`: the socket is unusable (error or timeout).
async fn send(socket: &mut WebSocket, event: &ServerEvent, limit: Duration) -> bool {
    let Ok(text) = serde_json::to_string(event) else {
        return false;
    };
    matches!(
        timeout(limit, socket.send(Message::Text(text.into()))).await,
        Ok(Ok(()))
    )
}

/// Sends a close frame and waits briefly for the peer's, so the client sees a clean close and
/// not a reset.
async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: reason.into(),
    };
    if timeout(CLOSE_GRACE, socket.send(Message::Close(Some(frame))))
        .await
        .is_err()
    {
        return;
    }
    let _ = timeout(CLOSE_GRACE, async {
        while let Some(Ok(msg)) = socket.recv().await {
            if matches!(msg, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

/// Revocation: tell the client why, then close.
async fn revoked(socket: &mut WebSocket, limit: Duration) {
    let _ = send(socket, &ServerEvent::DeviceRevoked {}, limit).await;
    close(socket, CLOSE_NORMAL, "device revoked").await;
}

/// Serves one authenticated socket until it ends.
pub(super) async fn serve(
    mut socket: WebSocket,
    inner: Arc<Inner>,
    device: DeviceId,
    handle: ConnHandle,
    mut disconnect: DisconnectSignal,
) {
    let limit = inner.config.send_timeout;
    // Subscribe BEFORE the "still active" check: a revocation between authentication and here
    // is then seen either by the check or as an event, never neither.
    let mut registry_rx = inner.registry.subscribe();
    let mut hub_rx = inner.hub.subscribe();
    let mut host_rx = inner.host.subscribe();
    let registration = inner.register_socket(&device);

    if !inner.registry.is_active(&device) {
        revoked(&mut socket, limit).await;
        return;
    }
    let hello = ServerEvent::Hello {
        protocol: PROTOCOL_VERSION,
        host_time: time::OffsetDateTime::now_utc(),
    };
    if !send(&mut socket, &hello, limit).await {
        return;
    }
    let state = *host_rx.borrow_and_update();
    if !send(&mut socket, &ServerEvent::HostState(state), limit).await {
        return;
    }

    let heartbeat = inner.config.heartbeat_interval;
    let mut ticker = interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Pings sent since the last frame of any kind from the client.
    let mut unanswered = 0u32;

    loop {
        tokio::select! {
            reason = handle.closed() => {
                match reason {
                    CloseReason::Revoked => revoked(&mut socket, limit).await,
                }
                break;
            }
            () = disconnect.fired() => {
                close(&mut socket, CLOSE_GOING_AWAY, "host disconnected all sessions").await;
                break;
            }
            () = registration.evicted.notified() => {
                close(&mut socket, CLOSE_GOING_AWAY, "replaced by a newer connection").await;
                break;
            }
            event = registry_rx.recv() => match event {
                Ok(DeviceEvent::Revoked { device_id }) if device_id == device => {
                    revoked(&mut socket, limit).await;
                    break;
                }
                Ok(DeviceEvent::Removed { device_id }) if device_id == device => {
                    close(&mut socket, CLOSE_POLICY, "device removed").await;
                    break;
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    // Events about other devices were skipped; make sure ours was not among them.
                    if !inner.registry.is_active(&device) {
                        revoked(&mut socket, limit).await;
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            event = hub_rx.recv() => match event {
                Ok(HubEvent::SharesUpdated) => {
                    if !send(&mut socket, &ServerEvent::SharesUpdated {}, limit).await {
                        break;
                    }
                }
                Ok(HubEvent::ShareChanged { share, path }) => {
                    if inner.shares.may_list(&device, &share)
                        && !send(&mut socket, &ServerEvent::ShareChanged { share, path }, limit).await
                    {
                        break;
                    }
                }
                Ok(HubEvent::Direct { device: to, event }) => {
                    if to == device && !send(&mut socket, &event, limit).await {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Events were lost; the client reconnects and re-fetches everything.
                    close(&mut socket, CLOSE_TRY_AGAIN, "events lagged").await;
                    break;
                }
                Err(RecvError::Closed) => break,
            },
            changed = host_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let state = *host_rx.borrow_and_update();
                if !send(&mut socket, &ServerEvent::HostState(state), limit).await {
                    break;
                }
            }
            _ = ticker.tick() => {
                if unanswered >= inner.config.missed_pongs {
                    // Dead peer: nothing to say to it.
                    break;
                }
                if timeout(limit, socket.send(Message::Ping(Vec::new().into()))).await.is_err() {
                    break;
                }
                unanswered += 1;
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                unanswered = 0;
                match message {
                    Message::Text(text) => {
                        if let Some(reply) = on_text(&inner, &device, &text)
                            && !send(&mut socket, &reply, limit).await
                        {
                            break;
                        }
                    }
                    Message::Binary(_) => {
                        close(&mut socket, CLOSE_POLICY, "text frames only").await;
                        break;
                    }
                    // Pings are answered by the WebSocket layer; both only prove liveness here.
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => break,
                }
            }
        }
    }
}

/// A text frame from the client. Unknown or malformed messages are ignored (compatibility rule).
fn on_text(inner: &Inner, device: &DeviceId, text: &str) -> Option<ServerEvent> {
    match serde_json::from_str::<ClientEvent>(text) {
        Ok(ClientEvent::Ping {}) => Some(ServerEvent::Pong {}),
        Ok(other) => {
            let handler = Arc::clone(&inner.streams.read().unwrap_or_else(|e| e.into_inner()));
            handler.on_client_event(device, other)
        }
        Err(_) => None,
    }
}
