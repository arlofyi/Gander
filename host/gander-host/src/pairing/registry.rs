// SPDX-License-Identifier: Apache-2.0
//! [`DeviceRegistry`]: the paired devices (`devices.toml`), token verification, network binding,
//! rotation, revocation and the `device.revoked` push hook.
//!
//! - Tokens: 32 CSPRNG bytes, base64url; only SHA-256 is stored. Verification hashes the presented
//!   token once and compares it in constant time with every record (`gander_core::pairing::token`).
//! - Network binding (Tailscale adapter): a record with `network_identity` requires the request's
//!   WhoIs login to match ([`check_binding`]); otherwise 403 `network_identity_mismatch`, audited.
//!   A request with no identity (loopback, LAN, generic adapter, tailscaled unreachable) does not
//!   match a recorded identity: the binding fails closed. A node id change is a warning unless
//!   `strict_node_binding`.
//! - Revocation marks the record (kept 30 days, then purged), broadcasts
//!   [`DeviceEvent::Revoked`] to [`DeviceRegistry::subscribe`] (WP-17's `/events` sends
//!   `device.revoked`), and closes every connection the device authenticated on through its
//!   [`ConnHandle`] (HTTP keep-alive connections drop at once; WebSocket handlers see
//!   [`ConnHandle::closed`] resolve with [`CloseReason::Revoked`]).
//! - `last_seen_at` / `last_address` are updated in memory on every request and written to disk
//!   at most every [`SEEN_PERSIST_INTERVAL`] per device, or when the address changes.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use gander_core::config::{
    ConfigError, ConfigFile as _, DeviceRecord, DevicesFile, SecureFileStore, TrustConfig,
};
use gander_core::error::ErrorCode;
use gander_core::net::tailscale::{BindingCheck, WhoIsAnswer, check_binding};
use gander_core::pairing::token::{
    find_by_hash, hash_token, hash_token_text, parse_bearer, token_from_bytes,
};
use gander_core::pairing::window::PendingView;
use gander_core::protocol::{BearerToken, DeviceId, Timestamp};
use tokio::sync::broadcast;

use super::random_bytes;
use crate::audit::{AuditEvent, AuditLog, Outcome, kind};
use crate::server::{ApiError, CloseReason, ConnHandle, ConnInfo, PeerNetwork, WeakConnHandle};

/// Revoked records are kept this long for the audit trail.
pub const REVOKED_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Minimum interval between `last_seen_at` writes per device.
pub const SEEN_PERSIST_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// A store for `devices.toml` shareable across threads.
pub type SharedStore = Arc<dyn SecureFileStore + Send + Sync>;

/// Something that happened to a device. Subscribe with [`DeviceRegistry::subscribe`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceEvent {
    /// A device was paired.
    Added {
        /// The device.
        device_id: DeviceId,
    },
    /// The device was revoked; an open `/events` socket sends `device.revoked {}` and closes.
    Revoked {
        /// The device.
        device_id: DeviceId,
    },
    /// The device rotated its token (the old one stopped working).
    TokenRotated {
        /// The device.
        device_id: DeviceId,
    },
    /// The record was deleted (abandoned pairing, or purged after 30 days revoked).
    Removed {
        /// The device.
        device_id: DeviceId,
    },
}

/// Errors from registry changes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// No such device.
    #[error("no such device")]
    NotFound,
    /// The device is revoked.
    #[error("the device is revoked")]
    Revoked,
    /// Saving `devices.toml` failed; nothing changed.
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// The paired devices. Share it as `Arc<DeviceRegistry>`.
pub struct DeviceRegistry {
    store: SharedStore,
    dir: PathBuf,
    devices: Mutex<DevicesFile>,
    seen_persisted: Mutex<HashMap<DeviceId, Instant>>,
    trust: RwLock<TrustConfig>,
    events: broadcast::Sender<DeviceEvent>,
    conns: Mutex<HashMap<DeviceId, Vec<WeakConnHandle>>>,
    audit: Arc<AuditLog>,
}

impl std::fmt::Debug for DeviceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceRegistry")
            .field("dir", &self.dir)
            .field(
                "devices",
                &self.devices.lock().map(|d| d.devices.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl DeviceRegistry {
    /// Loads `devices.toml` from `config_dir` (missing file: no devices) and purges revoked
    /// records older than 30 days.
    pub fn load(
        store: SharedStore,
        config_dir: &Path,
        trust: TrustConfig,
        audit: Arc<AuditLog>,
    ) -> Result<Self, ConfigError> {
        let devices = DevicesFile::load(&*store, config_dir)?;
        let (events, _) = broadcast::channel(64);
        let registry = Self {
            store,
            dir: config_dir.to_owned(),
            devices: Mutex::new(devices),
            seen_persisted: Mutex::new(HashMap::new()),
            trust: RwLock::new(trust),
            events,
            conns: Mutex::new(HashMap::new()),
            audit,
        };
        if let Err(e) = registry.purge_revoked(time::OffsetDateTime::now_utc()) {
            tracing::warn!(error = %e, "purging old revoked devices failed");
        }
        Ok(registry)
    }

    /// Replaces the trust options (after a `config.toml` change).
    pub fn set_trust(&self, trust: TrustConfig) {
        *self.trust.write().unwrap_or_else(|e| e.into_inner()) = trust;
    }

    /// The trust options in force.
    pub fn trust(&self) -> TrustConfig {
        self.trust.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Subscribes to device events (WP-17 `/events`, WP-18 Devices window).
    pub fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.events.subscribe()
    }

    /// Every record, active and revoked (hashes included; never display them).
    pub fn list(&self) -> Vec<DeviceRecord> {
        self.lock().devices.clone()
    }

    /// One record.
    pub fn get(&self, id: &DeviceId) -> Option<DeviceRecord> {
        self.lock().get(id).cloned()
    }

    /// Whether `id` exists and is not revoked.
    pub fn is_active(&self, id: &DeviceId) -> bool {
        self.lock().get(id).is_some_and(|d| !d.is_revoked())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, DevicesFile> {
        self.devices.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies `change` to a copy, saves it, and only then makes it current.
    fn commit<T>(
        &self,
        change: impl FnOnce(&mut DevicesFile) -> Result<T, RegistryError>,
    ) -> Result<T, RegistryError> {
        let mut guard = self.lock();
        let mut next = guard.clone();
        let out = change(&mut next)?;
        next.save(&*self.store, &self.dir)?;
        *guard = next;
        Ok(out)
    }

    fn emit(&self, event: DeviceEvent) {
        // No receivers is fine.
        let _ = self.events.send(event);
    }

    /// Verifies a request's `Authorization` header and network identity.
    ///
    /// On success records `last_seen`, remembers `handle` for revocation and returns the device.
    pub fn authenticate(
        &self,
        authorization: Option<&str>,
        conn: &ConnInfo,
        handle: Option<&ConnHandle>,
    ) -> Result<DeviceId, ApiError> {
        let text = match parse_bearer(authorization) {
            Ok(Some(t)) => t,
            Ok(None) => {
                return Err(ApiError::new(
                    ErrorCode::TokenMissing,
                    "This request needs a device token.",
                ));
            }
            Err(()) => return Err(invalid_token()),
        };
        let hash = hash_token_text(text);
        let source = conn.peer.ip();
        let record = {
            let devices = self.lock();
            find_by_hash(&devices, &hash).cloned()
        };
        let Some(record) = record else {
            return Err(invalid_token());
        };
        if record.is_revoked() {
            self.audit.record(
                AuditEvent::new(kind::AUTH_REFUSED)
                    .device(&record.id)
                    .source(source)
                    .outcome(Outcome::Refused)
                    .detail("reason", "token_revoked"),
            );
            return Err(ApiError::new(
                ErrorCode::TokenRevoked,
                "This device was removed on the computer.",
            ));
        }
        let answer = match &conn.network {
            PeerNetwork::Identified(p) => WhoIsAnswer::Found(p.clone()),
            _ => WhoIsAnswer::NotFound,
        };
        let trust = self.trust();
        match check_binding(record.network_identity.as_ref(), &answer, &trust) {
            BindingCheck::Ok => {}
            BindingCheck::NodeChanged => {
                tracing::warn!(device = %record.id, "tailscale node id changed since pairing");
                self.audit.record(
                    AuditEvent::new(kind::IDENTITY_NODE_CHANGED)
                        .device(&record.id)
                        .source(source)
                        .outcome(Outcome::Warning)
                        .detail("node", answer_node(&answer)),
                );
            }
            BindingCheck::Mismatch => {
                let mut ev = AuditEvent::new(kind::IDENTITY_MISMATCH)
                    .device(&record.id)
                    .source(source)
                    .outcome(Outcome::Refused)
                    .detail("network", network_kind(&conn.network));
                if let WhoIsAnswer::Found(p) = &answer {
                    ev = ev
                        .detail("login", p.login.as_str())
                        .detail("node", p.node_name.as_str());
                }
                self.audit.record(ev);
                tracing::warn!(device = %record.id, "network identity mismatch; request refused");
                return Err(ApiError::new(
                    ErrorCode::NetworkIdentityMismatch,
                    "This device is not signed in to the private network it was paired on.",
                ));
            }
        }
        self.touch(&record.id, source);
        if let Some(h) = handle {
            self.track(&record.id, h);
        }
        Ok(record.id)
    }

    fn touch(&self, id: &DeviceId, source: IpAddr) {
        let now = time::OffsetDateTime::now_utc();
        let persist = {
            let mut devices = self.lock();
            let Some(rec) = devices.get_mut(id) else {
                return;
            };
            let moved = rec.last_address != Some(source);
            rec.last_seen_at = Some(now);
            rec.last_address = Some(source);
            let mut seen = self
                .seen_persisted
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let due = seen
                .get(id)
                .is_none_or(|at| at.elapsed() >= SEEN_PERSIST_INTERVAL);
            if moved || due {
                seen.insert(id.clone(), Instant::now());
                Some(devices.clone())
            } else {
                None
            }
        };
        if let Some(file) = persist
            && let Err(e) = file.save(&*self.store, &self.dir)
        {
            tracing::warn!(error = %e, "saving last-seen time failed");
        }
    }

    fn track(&self, id: &DeviceId, handle: &ConnHandle) {
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        let list = conns.entry(id.clone()).or_default();
        list.retain(|w| w.upgrade().is_some());
        if !list.iter().any(|w| w.id() == handle.id()) {
            list.push(handle.downgrade());
        }
    }

    /// Open connections the device authenticated on (still alive).
    pub fn open_connections(&self, id: &DeviceId) -> usize {
        let conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        conns
            .get(id)
            .map_or(0, |l| l.iter().filter(|w| w.upgrade().is_some()).count())
    }

    /// Creates the record for an approved pairing and its token (persisted before returning).
    pub fn add_from_pairing(
        &self,
        view: &PendingView,
    ) -> Result<(DeviceId, BearerToken), RegistryError> {
        let token = token_from_bytes(&random_bytes::<32>());
        let id = DeviceId::from(format!("dev_{}", ulid::Ulid::new()));
        let record = DeviceRecord {
            id: id.clone(),
            name: view.device.name.clone(),
            platform: view.device.platform.clone(),
            model: view.device.model.clone(),
            app_version: view.device.app_version.clone(),
            token_hash: hash_token(&token),
            created_at: time::OffsetDateTime::now_utc(),
            last_seen_at: None,
            last_address: Some(view.source),
            network_identity: view.identity.as_ref().map(|p| p.to_recorded()),
            revoked_at: None,
        };
        self.commit(|f| {
            f.devices.push(record);
            Ok(())
        })?;
        self.emit(DeviceEvent::Added {
            device_id: id.clone(),
        });
        Ok((id, token))
    }

    /// Deletes a record outright (an approved pairing whose token was never collected).
    pub fn remove(&self, id: &DeviceId) -> Result<(), RegistryError> {
        self.commit(|f| {
            let before = f.devices.len();
            f.devices.retain(|d| &d.id != id);
            if f.devices.len() == before {
                return Err(RegistryError::NotFound);
            }
            Ok(())
        })?;
        self.emit(DeviceEvent::Removed {
            device_id: id.clone(),
        });
        Ok(())
    }

    /// `POST /v1/token/rotate`: a new token for an active device; the old one stops working now.
    pub fn rotate(&self, id: &DeviceId) -> Result<BearerToken, RegistryError> {
        let token = token_from_bytes(&random_bytes::<32>());
        let hash = hash_token(&token);
        self.commit(|f| {
            let rec = f.get_mut(id).ok_or(RegistryError::NotFound)?;
            if rec.is_revoked() {
                return Err(RegistryError::Revoked);
            }
            rec.token_hash = hash;
            Ok(())
        })?;
        self.audit
            .record(AuditEvent::new(kind::TOKEN_ROTATED).device(id));
        self.emit(DeviceEvent::TokenRotated {
            device_id: id.clone(),
        });
        Ok(token)
    }

    /// Revokes a device: marks the record, broadcasts [`DeviceEvent::Revoked`] and closes every
    /// connection the device authenticated on. Returns `false` if it was already revoked.
    pub fn revoke(&self, id: &DeviceId) -> Result<bool, RegistryError> {
        let now = time::OffsetDateTime::now_utc();
        let changed = self.commit(|f| {
            let rec = f.get_mut(id).ok_or(RegistryError::NotFound)?;
            if rec.is_revoked() {
                return Ok(false);
            }
            rec.revoked_at = Some(now);
            Ok(true)
        })?;
        if !changed {
            return Ok(false);
        }
        self.emit(DeviceEvent::Revoked {
            device_id: id.clone(),
        });
        let handles = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .unwrap_or_default();
        let mut closed = 0;
        for h in handles.iter().filter_map(WeakConnHandle::upgrade) {
            h.close(CloseReason::Revoked);
            closed += 1;
        }
        self.audit.record(
            AuditEvent::new(kind::DEVICE_REVOKED)
                .device(id)
                .detail("connections_closed", closed),
        );
        Ok(true)
    }

    /// Removes revoked records older than [`REVOKED_RETENTION`] (on load; call daily).
    pub fn purge_revoked(&self, now: Timestamp) -> Result<usize, RegistryError> {
        let cutoff = now - REVOKED_RETENTION;
        let old: Vec<DeviceId> = self
            .lock()
            .devices
            .iter()
            .filter(|d| d.revoked_at.is_some_and(|r| r < cutoff))
            .map(|d| d.id.clone())
            .collect();
        if old.is_empty() {
            return Ok(0);
        }
        self.commit(|f| {
            f.devices.retain(|d| !old.contains(&d.id));
            Ok(())
        })?;
        for id in &old {
            self.audit
                .record(AuditEvent::new(kind::DEVICE_PURGED).device(id));
            self.emit(DeviceEvent::Removed {
                device_id: id.clone(),
            });
        }
        Ok(old.len())
    }
}

fn invalid_token() -> ApiError {
    ApiError::new(ErrorCode::TokenInvalid, "This device token is not known.")
}

fn answer_node(answer: &WhoIsAnswer) -> String {
    match answer {
        WhoIsAnswer::Found(p) => p.node_name.clone(),
        WhoIsAnswer::NotFound => String::new(),
    }
}

pub(crate) fn network_kind(n: &PeerNetwork) -> &'static str {
    match n {
        PeerNetwork::Loopback => "loopback",
        PeerNetwork::Identified(_) => "identified",
        PeerNetwork::NoIdentity => "no_identity",
        PeerNetwork::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io;

    #[derive(Debug, Default)]
    struct MemStore(Mutex<BTreeMap<PathBuf, Vec<u8>>>);

    impl SecureFileStore for MemStore {
        fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(path).cloned())
        }
        fn write_owner_only(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(path.to_owned(), contents.to_vec());
            Ok(())
        }
    }

    fn view() -> PendingView {
        PendingView {
            pairing_id: "pair_x".into(),
            device: gander_core::protocol::pairing::PairingDevice {
                name: "Pixel 9".into(),
                platform: "android".into(),
                model: "Pixel 9".into(),
                app_version: "0.1.0".into(),
            },
            flow: gander_core::pairing::window::Flow::Manual,
            identity: None,
            sas: gander_core::pairing::sas::derive_sas(b"", &[0; 16], &[0; 16]),
            source: "127.0.0.1".parse().unwrap(),
            expires_in: Duration::from_secs(1),
        }
    }

    #[test]
    fn stores_hash_only_and_purges_revoked_after_thirty_days() {
        let store = Arc::new(MemStore::default());
        let dir = Path::new("cfg");
        let reg = DeviceRegistry::load(
            store.clone(),
            dir,
            TrustConfig::default(),
            Arc::new(AuditLog::memory()),
        )
        .unwrap();
        let mut events = reg.subscribe();
        let (id, token) = reg.add_from_pairing(&view()).unwrap();
        assert_eq!(
            events.try_recv().unwrap(),
            DeviceEvent::Added {
                device_id: id.clone()
            }
        );
        let text =
            String::from_utf8(store.0.lock().unwrap().values().next().unwrap().clone()).unwrap();
        assert!(!text.contains(token.expose()));
        assert!(text.contains(&hash_token(&token).to_hex()));
        assert!(reg.revoke(&id).unwrap());
        assert!(!reg.revoke(&id).unwrap(), "second revoke is a no-op");
        assert_eq!(
            events.try_recv().unwrap(),
            DeviceEvent::Revoked {
                device_id: id.clone()
            }
        );
        assert!(matches!(reg.rotate(&id), Err(RegistryError::Revoked)));
        let now = time::OffsetDateTime::now_utc();
        assert_eq!(
            reg.purge_revoked(now + Duration::from_secs(29 * 86_400))
                .unwrap(),
            0
        );
        assert!(reg.get(&id).is_some(), "kept for the audit trail");
        assert_eq!(
            reg.purge_revoked(now + Duration::from_secs(31 * 86_400))
                .unwrap(),
            1
        );
        assert!(reg.get(&id).is_none());
        // Reloading from the store sees the same state.
        let again = DeviceRegistry::load(
            store,
            dir,
            TrustConfig::default(),
            Arc::new(AuditLog::memory()),
        )
        .unwrap();
        assert!(again.list().is_empty());
    }
}
