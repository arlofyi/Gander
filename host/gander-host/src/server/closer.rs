// SPDX-License-Identifier: Apache-2.0
//! [`ConnHandle`]: closes ONE connection on demand (device revocation, WP-13).
//!
//! The listener creates a handle per accepted connection, puts it in every request's extensions
//! and drops the connection when [`ConnHandle::close`] is called. The token authenticator records
//! the handle of each connection a device authenticated on, so revoking the device closes exactly
//! that device's connections ([`crate::pairing::DeviceRegistry::revoke`]).
//!
//! An upgraded connection (WebSocket, WP-17) is no longer driven by the HTTP connection task, so
//! a long-lived handler takes `Extension<ConnHandle>` and ends when [`ConnHandle::closed`]
//! resolves; the [`CloseReason`] says whether to send `device.revoked` first.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use tokio::sync::watch;

/// Why a connection was closed through its handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseReason {
    /// The device was revoked; a WebSocket sends `device.revoked` before closing.
    Revoked,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Per-connection close handle; cheap to clone.
#[derive(Debug, Clone)]
pub struct ConnHandle {
    id: u64,
    tx: Arc<watch::Sender<Option<CloseReason>>>,
}

/// A weak [`ConnHandle`]: does not keep the connection's state alive.
#[derive(Debug, Clone)]
pub struct WeakConnHandle {
    id: u64,
    tx: Weak<watch::Sender<Option<CloseReason>>>,
}

impl Default for ConnHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnHandle {
    /// A handle for a new connection.
    pub fn new() -> Self {
        let (tx, _) = watch::channel(None);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            tx: Arc::new(tx),
        }
    }

    /// Process-unique connection number.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Asks the connection (and any handler watching it) to close.
    pub fn close(&self, reason: CloseReason) {
        self.tx.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason);
                true
            } else {
                false
            }
        });
    }

    /// The close reason, if [`ConnHandle::close`] was called.
    pub fn close_reason(&self) -> Option<CloseReason> {
        *self.tx.borrow()
    }

    /// Resolves when [`ConnHandle::close`] is called (immediately if it already was).
    pub async fn closed(&self) -> CloseReason {
        let mut rx = self.tx.subscribe();
        loop {
            if let Some(r) = *rx.borrow_and_update() {
                return r;
            }
            if rx.changed().await.is_err() {
                // Unreachable while `self` holds the sender; never resolve spuriously.
                std::future::pending::<()>().await;
            }
        }
    }

    /// A weak reference (what registries keep, so a finished connection is freed).
    pub fn downgrade(&self) -> WeakConnHandle {
        WeakConnHandle {
            id: self.id,
            tx: Arc::downgrade(&self.tx),
        }
    }
}

impl WeakConnHandle {
    /// Connection number.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The handle, if the connection (or a handler holding it) is still alive.
    pub fn upgrade(&self) -> Option<ConnHandle> {
        self.tx.upgrade().map(|tx| ConnHandle { id: self.id, tx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_wakes_waiters_once_and_weak_handles_expire() {
        let h = ConnHandle::new();
        let other = ConnHandle::new();
        assert_ne!(h.id(), other.id());
        let waiter = {
            let h = h.clone();
            tokio::spawn(async move { h.closed().await })
        };
        tokio::task::yield_now().await;
        assert_eq!(h.close_reason(), None);
        h.close(CloseReason::Revoked);
        assert_eq!(waiter.await.unwrap(), CloseReason::Revoked);
        assert_eq!(
            h.closed().await,
            CloseReason::Revoked,
            "already closed resolves at once"
        );
        let weak = h.downgrade();
        assert!(weak.upgrade().is_some());
        drop(h);
        assert!(weak.upgrade().is_none());
    }
}
