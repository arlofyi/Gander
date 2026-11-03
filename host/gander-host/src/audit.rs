// SPDX-License-Identifier: Apache-2.0
//! The host audit log (pairing spec "Host-side visibility and audit"; security.md principle 8).
//!
//! One JSON object per line in `%LOCALAPPDATA%\Gander\logs\audit.jsonl`:
//!
//! ```json
//! {"ts":"2026-11-02T20:11:03.123Z","event":"pair.approved","device_id":"dev_01J...","source_addr":"100.101.102.104","outcome":"ok","detail":{"flow":"qr","login":"alice@example.com"}}
//! ```
//!
//! - Rotated at 10 MiB, five old files kept (`audit.jsonl.1` newest .. `audit.jsonl.5` oldest).
//! - **Never contains tokens, secrets or file contents.** Callers pass only the facts below, and
//!   [`AuditEvent::detail`] redacts again as a backstop: a key naming a secret (`token`, `secret`,
//!   `authorization`, `password`, `cookie`, `qr`, `nonce`) has its value replaced by
//!   `"<redacted>"`, and in every string except a `path` value a run of 32 or more base64url
//!   characters (the shape of a token or secret) is replaced as well. Paths do appear in access
//!   events (the owner's own data, local only).
//! - The most recent [`RECENT_CAPACITY`] events are also kept in memory for the tray's Activity
//!   window ([`AuditLog::recent`]).
//! - Writing is best effort: an I/O error is reported through `tracing` and never fails the
//!   request being audited.
//!
//! Event names are the constants in [`kind`]. WP-14 records access events with
//! [`kind::ACCESS`] and reports a path-resolver bug (`RejectReason::is_bug()`) with
//! [`AuditLog::resolver_bug`], which also logs at error level.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gander_core::protocol::{DeviceId, ShareId, Timestamp};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// File name inside the logs directory.
pub const AUDIT_FILE: &str = "audit.jsonl";
/// Rotation threshold.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// Rotated files kept.
pub const KEEP_FILES: usize = 5;
/// Events kept in memory for [`AuditLog::recent`].
pub const RECENT_CAPACITY: usize = 500;

const REDACTED: &str = "<redacted>";
const SECRET_KEYS: [&str; 7] = [
    "token",
    "secret",
    "authorization",
    "password",
    "cookie",
    "qr",
    "nonce",
];
const SECRET_RUN: usize = 32;

/// Event names.
pub mod kind {
    /// The owner opened the "Add a device" window.
    pub const PAIR_WINDOW_OPENED: &str = "pair.window_opened";
    /// The window closed (`detail.reason`: owner, expired, approved, denied).
    pub const PAIR_WINDOW_CLOSED: &str = "pair.window_closed";
    /// A `POST /v1/pair` (`outcome` ok or refused, `detail.reason`, `detail.failures`).
    pub const PAIR_ATTEMPT: &str = "pair.attempt";
    /// The owner approved a device.
    pub const PAIR_APPROVED: &str = "pair.approved";
    /// The owner denied a pairing.
    pub const PAIR_DENIED: &str = "pair.denied";
    /// A pending pairing timed out.
    pub const PAIR_EXPIRED: &str = "pair.expired";
    /// The device collected its token.
    pub const PAIR_COLLECTED: &str = "pair.collected";
    /// An approved token was never collected; the device record was removed.
    pub const PAIR_ABANDONED: &str = "pair.abandoned";
    /// A device was revoked.
    pub const DEVICE_REVOKED: &str = "device.revoked";
    /// Revoked records older than 30 days were removed.
    pub const DEVICE_PURGED: &str = "device.purged";
    /// A device rotated its token.
    pub const TOKEN_ROTATED: &str = "token.rotated";
    /// A request was refused by the authenticator (unknown or revoked token).
    pub const AUTH_REFUSED: &str = "auth.refused";
    /// The network identity did not match the one recorded at pairing (refused).
    pub const IDENTITY_MISMATCH: &str = "identity.mismatch";
    /// Same login, different node id (warning, or refusal with `strict_node_binding`).
    pub const IDENTITY_NODE_CHANGED: &str = "identity.node_changed";
    /// The listener bound or closed sockets.
    pub const LISTENER_CHANGED: &str = "listener.changed";
    /// The kill switch dropped every session.
    pub const KILL_SWITCH: &str = "kill_switch";
    /// The owner saved a different set of shares or permissions in the tray (`detail.shares`).
    pub const SHARES_CHANGED: &str = "shares.changed";
    /// The owner paused the listener from the tray (`detail.seconds`).
    pub const LISTENER_PAUSED: &str = "listener.paused";
    /// The listener resumed after a pause (`detail.reason`: owner, timer).
    pub const LISTENER_RESUMED: &str = "listener.resumed";
    /// The owner changed a setting in the tray (`detail.setting`).
    pub const SETTINGS_CHANGED: &str = "settings.changed";
    /// The owner emptied the thumbnail and preview caches.
    pub const CACHE_PURGED: &str = "cache.purged";
    /// A file-system access by a device (WP-14: `detail.action`, `detail.share`, `detail.path`).
    pub const ACCESS: &str = "access";
    /// The path resolver's final check fired: a resolver bug (error level).
    pub const RESOLVER_BUG: &str = "path.resolver_bug";
    /// A `POST /v1/stream` that passed the launch policy was refused by a host precondition
    /// (`detail.reason`: `host_locked`, `stream_busy`, `host_unattended`).
    pub const STREAM_REFUSED: &str = "stream.refused";
    /// The owner answered (or did not answer) a Stream prompt (`detail.prompt`: approval,
    /// attended; `detail.answer`: allow_once, always, deny, no_answer).
    pub const STREAM_PROMPT: &str = "stream.prompt";
    /// A Stream session started capturing a window (`detail.session`, `share`, `path`, `app`,
    /// `handler`, `decision` (the policy's decision path), `approval`, `launched_by_us`).
    pub const STREAM_STARTED: &str = "stream.started";
    /// A Stream session was resumed on a new peer connection (`detail.session`).
    pub const STREAM_RESUMED: &str = "stream.resumed";
    /// A Stream session ended (`detail.session`, `reason`, `seconds`, `streamed_seconds`,
    /// `app_closed`, `left_open_unsaved`).
    pub const STREAM_ENDED: &str = "stream.ended";
    /// A device read or wrote the computer's clipboard text during a stream (WP-56:
    /// `detail.direction` `to_phone` or `from_phone`, `detail.session`, and `detail.bytes` for
    /// outcome ok or `detail.reason` for outcome refused; never the text).
    pub const CLIPBOARD: &str = "clipboard";
    /// The integration-test approval hook approved a pairing.
    pub const TEST_APPROVE: &str = "pair.test_approved";
}

/// Outcome of an audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It happened.
    Ok,
    /// It was refused.
    Refused,
    /// Allowed, but worth the owner's attention.
    Warning,
    /// Something is broken (a bug or an I/O failure).
    Error,
}

/// One audit line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub ts: Timestamp,
    /// What ([`kind`]).
    pub event: String,
    /// Which device, if known.
    pub device_id: Option<DeviceId>,
    /// From where, if a network peer was involved.
    pub source_addr: Option<IpAddr>,
    /// How it ended.
    pub outcome: Outcome,
    /// Event-specific facts (redacted on insertion).
    pub detail: Map<String, Value>,
}

impl AuditEvent {
    /// A new event stamped now, outcome [`Outcome::Ok`].
    pub fn new(event: &str) -> Self {
        Self {
            ts: time::OffsetDateTime::now_utc(),
            event: event.to_owned(),
            device_id: None,
            source_addr: None,
            outcome: Outcome::Ok,
            detail: Map::new(),
        }
    }

    /// Sets the device.
    pub fn device(mut self, id: &DeviceId) -> Self {
        self.device_id = Some(id.clone());
        self
    }

    /// Sets the source address.
    pub fn source(mut self, addr: IpAddr) -> Self {
        self.source_addr = Some(addr);
        self
    }

    /// Sets the outcome.
    pub fn outcome(mut self, outcome: Outcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// Adds a detail field, redacting secret-shaped content.
    pub fn detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        let value = redact_value(key, value.into());
        self.detail.insert(key.to_owned(), value);
        self
    }
}

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|s| k.contains(s))
}

fn redact_value(key: &str, value: Value) -> Value {
    if is_secret_key(key) {
        return Value::String(REDACTED.to_owned());
    }
    match value {
        Value::String(s) if key == "path" => Value::String(s),
        Value::String(s) => Value::String(redact_text(&s)),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| redact_value("", v)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = redact_value(&k, v);
                    (k, v)
                })
                .collect(),
        ),
        other => other,
    }
}

/// Replaces every run of [`SECRET_RUN`] or more base64url characters with `<redacted>`.
pub fn redact_text(text: &str) -> String {
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.chars().count() >= SECRET_RUN {
            out.push_str(REDACTED);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if is_b64(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[derive(Debug)]
struct Sink {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    size: u64,
}

impl Sink {
    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.file.is_none() {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            self.size = file.metadata()?.len();
            self.file = Some(file);
        }
        if self.size > 0 && self.size + line.len() as u64 > self.max_bytes {
            self.rotate()?;
            return self.write_line(line);
        }
        if let Some(f) = self.file.as_mut() {
            f.write_all(line)?;
            f.flush()?;
            self.size += line.len() as u64;
        }
        Ok(())
    }

    fn rotated(&self, n: usize) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let oldest = self.rotated(self.keep);
        if oldest.exists() {
            std::fs::remove_file(&oldest)?;
        }
        for n in (1..self.keep).rev() {
            let from = self.rotated(n);
            if from.exists() {
                std::fs::rename(&from, self.rotated(n + 1))?;
            }
        }
        if self.keep > 0 {
            std::fs::rename(&self.path, self.rotated(1))?;
        } else {
            std::fs::remove_file(&self.path)?;
        }
        self.size = 0;
        Ok(())
    }
}

/// The audit log. Share it as `Arc<AuditLog>`.
#[derive(Debug)]
pub struct AuditLog {
    sink: Mutex<Option<Sink>>,
    recent: Mutex<VecDeque<AuditEvent>>,
}

impl AuditLog {
    /// Logs to `<logs_dir>/audit.jsonl` (the directory is created), 10 MiB x 5.
    pub fn open(logs_dir: &Path) -> io::Result<Self> {
        Self::with_limits(logs_dir, MAX_FILE_BYTES, KEEP_FILES)
    }

    /// Like [`AuditLog::open`] with explicit rotation limits (tests).
    pub fn with_limits(logs_dir: &Path, max_bytes: u64, keep: usize) -> io::Result<Self> {
        std::fs::create_dir_all(logs_dir)?;
        Ok(Self {
            sink: Mutex::new(Some(Sink {
                path: logs_dir.join(AUDIT_FILE),
                max_bytes,
                keep,
                file: None,
                size: 0,
            })),
            recent: Mutex::new(VecDeque::new()),
        })
    }

    /// An in-memory log (tests, or when the logs directory cannot be created).
    pub fn memory() -> Self {
        Self {
            sink: Mutex::new(None),
            recent: Mutex::new(VecDeque::new()),
        }
    }

    /// The file being written, if any.
    pub fn path(&self) -> Option<PathBuf> {
        self.sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.path.clone())
    }

    /// Records an event.
    pub fn record(&self, event: AuditEvent) {
        if let Some(sink) = self.sink.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            match serde_json::to_vec(&event) {
                Ok(mut line) => {
                    line.push(b'\n');
                    if let Err(e) = sink.write_line(&line) {
                        tracing::warn!(error = %e, "audit log write failed");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "audit event not serialisable"),
            }
        }
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.len() == RECENT_CAPACITY {
            recent.pop_front();
        }
        recent.push_back(event);
    }

    /// The last `n` events, oldest first.
    pub fn recent(&self, n: usize) -> Vec<AuditEvent> {
        let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent
            .iter()
            .skip(recent.len().saturating_sub(n))
            .cloned()
            .collect()
    }

    /// Records a path-resolver bug (`RejectReason::is_bug()`, step 7 fired) at error level.
    /// `reason` is the reject reason code; `path` is the protocol path the device sent.
    pub fn resolver_bug(
        &self,
        device: &DeviceId,
        source: Option<IpAddr>,
        share: &ShareId,
        path: &str,
        reason: &str,
    ) {
        tracing::error!(device = %device, share = %share, reason, "path resolver bug: final handle outside the share root");
        let mut ev = AuditEvent::new(kind::RESOLVER_BUG)
            .device(device)
            .outcome(Outcome::Error)
            .detail("share", share.as_str())
            .detail("path", path)
            .detail("reason", reason);
        if let Some(s) = source {
            ev = ev.source(s);
        }
        self.record(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("gander-audit-{}", ulid::Ulid::new()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Token-shaped text generated per run (never a real token).
    fn fake_token() -> String {
        gander_core::pairing::b64url(ulid::Ulid::new().to_string().repeat(2).as_bytes())
    }

    #[test]
    fn redaction_of_secret_keys_and_token_shaped_text() {
        let tok = fake_token();
        let ev = AuditEvent::new(kind::AUTH_REFUSED)
            .detail("token", tok.as_str())
            .detail("Authorization", format!("Bearer {tok}"))
            .detail("note", format!("got {tok} from phone"))
            .detail(
                "nested",
                serde_json::json!({"secret": "x", "list": [tok.clone()]}),
            )
            .detail("reason", "token_invalid")
            .detail(
                "path",
                "Photos/a-very-long-file-name-without-separators-0123456789.jpg",
            );
        let line = serde_json::to_string(&ev).unwrap();
        assert!(!line.contains(&tok), "{line}");
        assert_eq!(ev.detail["token"], "<redacted>");
        assert_eq!(ev.detail["Authorization"], "<redacted>");
        assert_eq!(ev.detail["note"], "got <redacted> from phone");
        assert_eq!(ev.detail["nested"]["secret"], "<redacted>");
        assert_eq!(ev.detail["reason"], "token_invalid");
        assert!(ev.detail["path"].as_str().unwrap().contains("0123456789"));
        assert_eq!(redact_text("short_words and-more"), "short_words and-more");
    }

    #[test]
    fn writes_jsonl_and_rotates_keeping_five() {
        let dir = TempDir::new();
        let log = AuditLog::with_limits(&dir.0, 400, 5).unwrap();
        for i in 0..60 {
            log.record(
                AuditEvent::new(kind::PAIR_ATTEMPT)
                    .source("100.64.0.2".parse().unwrap())
                    .outcome(Outcome::Refused)
                    .detail("n", i),
            );
        }
        let main = dir.0.join(AUDIT_FILE);
        assert!(std::fs::metadata(&main).unwrap().len() <= 400);
        for n in 1..=5 {
            let p = dir.0.join(format!("{AUDIT_FILE}.{n}"));
            assert!(p.exists(), "{p:?}");
            assert!(std::fs::metadata(&p).unwrap().len() <= 400);
        }
        assert!(!dir.0.join(format!("{AUDIT_FILE}.6")).exists());
        // Every line is a JSON event; the newest is in the main file.
        let text = std::fs::read_to_string(&main).unwrap();
        let last: AuditEvent = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last.detail["n"], 59);
        assert_eq!(last.outcome, Outcome::Refused);
        let older = std::fs::read_to_string(dir.0.join(format!("{AUDIT_FILE}.1"))).unwrap();
        let first_older: AuditEvent = serde_json::from_str(older.lines().next().unwrap()).unwrap();
        assert!(first_older.detail["n"].as_u64().unwrap() < 59);
        assert_eq!(log.recent(3).len(), 3);
        assert_eq!(log.recent(3)[2].detail["n"], 59);
    }

    #[test]
    fn resolver_bug_is_an_error_entry() {
        let log = AuditLog::memory();
        log.resolver_bug(
            &DeviceId::from("dev_1"),
            None,
            &ShareId::from("shr_1"),
            "a/b",
            "escaped",
        );
        let ev = &log.recent(1)[0];
        assert_eq!(ev.event, kind::RESOLVER_BUG);
        assert_eq!(ev.outcome, Outcome::Error);
        assert_eq!(ev.detail["path"], "a/b");
    }
}
