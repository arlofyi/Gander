// SPDX-License-Identifier: Apache-2.0
//! The Windows implementations behind the events channel: the real [`HostProbe`] and the
//! `ReadDirectoryChangesW` [`WatchBackend`].

use std::io;
use std::sync::Arc;
use std::time::Duration;

use gander_core::path::{HiddenPatterns, is_always_omitted};
use gander_core::protocol::ShareId;
use gander_core::protocol::files::EntryKind;
use gander_win::path::ShareRoot;
use gander_win::session::{self, SessionWatcher};
use gander_win::watch::{Batch, Watcher};
use tokio::sync::Notify;

use super::ConfigShares;
use super::hoststate::{HostProbe, HostStateConfig, HostStateMonitor, OverrideProbe};
use super::watch::{ChangeNotifier, WatchBackend, WatchEvent, WatchHandle};

/// The computer's real state: session lock from `WTSRegisterSessionNotification` (pushed, with a
/// 15 s safety-net re-query), idle time from `GetLastInputInfo` minus Gander's own injections
/// (`gander_win::input::local_idle_duration`), power from
/// `GetSystemPowerStatus`.
#[derive(Debug)]
pub struct SystemProbe {
    /// `None` when the notification window could not be created: every read then queries the OS.
    session: Option<SessionWatcher>,
}

impl SystemProbe {
    /// Starts watching the session; `wake` is signalled on every lock change so the
    /// [`HostStateMonitor`] re-reads at once.
    pub fn start(wake: Arc<Notify>) -> Self {
        let session = match session::watch(move |_| wake.notify_one()) {
            Ok(watcher) => Some(watcher),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "session lock notifications unavailable; the lock state is polled"
                );
                None
            }
        };
        Self { session }
    }
}

impl HostProbe for SystemProbe {
    fn locked(&self) -> bool {
        match &self.session {
            Some(watcher) => watcher.state().is_locked(),
            // Fail closed: if even the query fails the session counts as locked.
            None => session::query_session_state().map_or(true, |s| s.is_locked()),
        }
    }

    fn idle(&self) -> Option<Duration> {
        // Input the stream injected is not someone at the computer (WP-52).
        gander_win::input::local_idle_duration().ok()
    }

    fn on_battery(&self) -> bool {
        session::power::power_status().is_ok_and(|p| p.on_battery())
    }
}

/// A [`HostStateMonitor`] over the real computer. Must be called inside a tokio runtime.
pub fn host_state_monitor(config: &HostStateConfig) -> io::Result<HostStateMonitor> {
    let wake = Arc::new(Notify::new());
    let probe = Arc::new(SystemProbe::start(Arc::clone(&wake)));
    Ok(HostStateMonitor::spawn(probe, wake, config.clone()))
}

/// A [`HostStateMonitor`] over the real computer behind an [`OverrideProbe`] (test host: a
/// simulated lock and a forced attended state). Must be called inside a tokio runtime.
pub fn overridable_host_state_monitor(
    config: &HostStateConfig,
) -> io::Result<(HostStateMonitor, Arc<OverrideProbe>)> {
    let wake = Arc::new(Notify::new());
    let real: Arc<dyn HostProbe> = Arc::new(SystemProbe::start(Arc::clone(&wake)));
    let probe = Arc::new(OverrideProbe::new(real));
    let monitor = HostStateMonitor::spawn(probe.clone(), wake, config.clone());
    Ok((monitor, probe))
}

/// Watches share directories with `ReadDirectoryChangesW` (one completion port and thread for
/// all of them, see `gander_win::watch`).
///
/// A directory is opened through [`ShareRoot::resolve`], so the path resolver's rules apply:
/// hidden directories, links and short names are refused and nothing outside the share is ever
/// watched. Changes to entries the share hides (`hidden_patterns`, `desktop.ini` and the other
/// always-omitted names) do not raise `share.changed`.
#[derive(Debug)]
pub struct WindowsWatch {
    shares: Arc<ConfigShares>,
    watcher: Watcher,
}

impl WindowsWatch {
    /// A backend over the configured shares.
    pub fn new(shares: Arc<ConfigShares>) -> io::Result<Self> {
        Ok(Self {
            shares,
            watcher: Watcher::new()?,
        })
    }
}

/// Whether a change to `name` inside the watched directory is something a listing would show.
fn visible(hidden: &HiddenPatterns, dir_segments: &[String], name: &str) -> bool {
    if is_always_omitted(name) {
        return false;
    }
    let mut segments: Vec<&str> = dir_segments.iter().map(String::as_str).collect();
    segments.push(name);
    !hidden.is_hidden(&segments)
}

impl WatchBackend for WindowsWatch {
    fn watch(
        &self,
        share: &ShareId,
        dir: &str,
        changed: ChangeNotifier,
    ) -> io::Result<WatchHandle> {
        let config = self
            .shares
            .share(share)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such share"))?;
        let root = ShareRoot::from_config(&config)?;
        let resolved = root
            .resolve(dir.as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;
        if resolved.kind() != EntryKind::Dir {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            ));
        }
        let hidden = root.hidden_patterns().clone();
        let dir_segments: Vec<String> = resolved.path().segments().to_vec();
        let guard = self
            .watcher
            .watch(resolved.handle(), move |batch: &Batch| {
                if batch.ended {
                    changed(WatchEvent::Ended);
                } else if batch.overflow
                    || batch
                        .changes
                        .iter()
                        .any(|c| visible(&hidden, &dir_segments, &c.name))
                {
                    changed(WatchEvent::Changed);
                }
            })?;
        Ok(Box::new(guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real probe and monitor on this machine. Prints the state so a run can be recorded
    /// (`cargo test -p gander-host --lib events::system -- --nocapture`).
    #[tokio::test(flavor = "multi_thread")]
    async fn real_monitor_reports_this_computer() {
        let monitor = host_state_monitor(&HostStateConfig::default()).expect("monitor");
        let state = monitor.current();
        eprintln!("WP-17 host.state on this machine: {state:?}");
        assert!(
            !state.attended || !state.locked,
            "locked hosts are never attended"
        );
        assert!(!state.stream_available, "Phase 1 has no stream engine");
        // The monitor agrees with a fresh query of the session (unless it changed this instant).
        let direct = session::query_session_state().expect("query").is_locked();
        monitor.refresh_now();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(monitor.current().locked, direct);
    }

    #[test]
    fn hidden_and_always_omitted_names_do_not_count() {
        let hidden = HiddenPatterns::new(&["**/.git", "secret/*.key"]).unwrap();
        let root: Vec<String> = Vec::new();
        assert!(visible(&hidden, &root, "report.docx"));
        assert!(!visible(&hidden, &root, ".git"));
        assert!(!visible(&hidden, &root, "Desktop.ini"));
        assert!(!visible(&hidden, &root, "THUMBS.DB"));
        let secret = vec!["secret".to_owned()];
        assert!(!visible(&hidden, &secret, "a.key"));
        assert!(visible(&hidden, &secret, "a.txt"));
        let nested = vec!["proj".to_owned()];
        assert!(
            !visible(&hidden, &nested, ".git"),
            "pattern applies at any depth"
        );
    }
}
