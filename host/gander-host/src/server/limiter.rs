// SPDX-License-Identifier: Apache-2.0
//! Per-device concurrency limiter (protocol spec: 8 in-flight requests per device; excess gets
//! 429 `too_many_requests` with `Retry-After`).
//!
//! Requests are keyed by the authenticated device when the [`super::Authenticator`] identified
//! one, otherwise by the peer IP address (unauthenticated `GET /v1/host` and pairing calls). A
//! [`Permit`] is held until the response body has been sent completely, so a long download counts
//! as in flight for its whole duration.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use gander_core::protocol::DeviceId;

/// What the limiter counts against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LimitKey {
    /// An authenticated device.
    Device(DeviceId),
    /// An unauthenticated caller, by source address.
    Peer(IpAddr),
}

/// Counts in-flight requests per key.
#[derive(Debug, Clone)]
pub struct ConcurrencyLimiter {
    limit: usize,
    inner: Arc<Mutex<HashMap<LimitKey, usize>>>,
}

impl ConcurrencyLimiter {
    /// A limiter allowing `limit` in-flight requests per key.
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The configured limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Takes a slot for `key`, or `None` when the key is at its limit.
    pub fn try_acquire(&self, key: LimitKey) -> Option<Permit> {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let n = map.entry(key.clone()).or_insert(0);
        if *n >= self.limit {
            return None;
        }
        *n += 1;
        Some(Permit {
            key,
            inner: Arc::clone(&self.inner),
        })
    }

    /// In-flight requests for `key`.
    pub fn in_flight(&self, key: &LimitKey) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .copied()
            .unwrap_or(0)
    }
}

/// A taken slot; released on drop.
#[derive(Debug)]
pub struct Permit {
    key: LimitKey,
    inner: Arc<Mutex<HashMap<LimitKey, usize>>>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = map.get_mut(&self.key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key_and_releases() {
        let l = ConcurrencyLimiter::new(8);
        let a = LimitKey::Device(DeviceId::from("dev_a"));
        let b = LimitKey::Peer("100.64.0.1".parse().unwrap());
        let permits: Vec<Permit> = (0..8).map(|_| l.try_acquire(a.clone()).unwrap()).collect();
        assert!(l.try_acquire(a.clone()).is_none());
        assert!(
            l.try_acquire(b.clone()).is_some(),
            "other keys are independent"
        );
        drop(permits);
        assert_eq!(l.in_flight(&a), 0);
        assert!(l.try_acquire(a).is_some());
    }
}
