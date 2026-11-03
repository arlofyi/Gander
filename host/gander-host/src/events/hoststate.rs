// SPDX-License-Identifier: Apache-2.0
//! `host.state`: locked, attended, on battery, stream available.
//!
//! The facts come from a [`HostProbe`] (production: [`super::system::SystemProbe`], Windows
//! session, input and power APIs; tests: a fake). The [`HostStateMonitor`] combines them into the
//! protocol's [`HostStateEvent`] and publishes it on a `watch` channel, so every `/events` socket
//! sees the same value and only changes are pushed.
//!
//! - **locked** is event driven: the probe's owner calls [`HostStateMonitor::refresh_now`] (or
//!   fires the `Notify` given to [`HostStateMonitor::spawn`]) when the OS reports a lock change.
//!   Fail closed: an unknown lock state is `locked: true`.
//! - **attended** and **on_battery** have no notification worth listening to, so they are polled
//!   (default every 5 s). `attended` means the session is unlocked and the last input is more
//!   recent than `stream.idle_prompt_seconds` (config default 120 s, `GetLastInputInfo`). If the
//!   idle time cannot be read, an unlocked host counts as attended: the safe side is to ask the
//!   person at the keyboard.
//! - **stream_available** is `stream engine present && !locked`. The engine is present once the
//!   host installed it ([`HostStateMonitor::set_stream_engine`], WP-53) or when
//!   [`HostStateConfig::stream_engine`] says so from the start.
//!
//! [`OverrideProbe`] wraps a probe for the test host: a simulated lock (ORed with the real one,
//! so it can only add a lock) and a forced attended/idle answer.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use gander_core::protocol::events::HostStateEvent;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

/// What the platform layer can tell about the computer.
pub trait HostProbe: Send + Sync + fmt::Debug + 'static {
    /// The session is locked, disconnected or of unknown state (fail closed).
    fn locked(&self) -> bool;
    /// Time since the last keyboard or mouse input; `None` when it cannot be read.
    fn idle(&self) -> Option<Duration>;
    /// Running on battery power.
    fn on_battery(&self) -> bool;
}

/// How the monitor derives and refreshes the state.
#[derive(Debug, Clone)]
pub struct HostStateConfig {
    /// How often `attended` and `on_battery` are re-read (5 s).
    pub poll_interval: Duration,
    /// Input more recent than this means someone is at the computer
    /// (`stream.idle_prompt_seconds`, 120 s).
    pub attended_within: Duration,
    /// The Stream engine exists (Phase 2). Without it `stream_available` is always false.
    pub stream_engine: bool,
}

impl Default for HostStateConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            attended_within: Duration::from_secs(120),
            stream_engine: false,
        }
    }
}

impl HostStateConfig {
    /// Defaults with `attended_within` taken from `[stream] idle_prompt_seconds`.
    pub fn from_stream_config(stream: &gander_core::config::StreamConfig) -> Self {
        Self {
            attended_within: Duration::from_secs(u64::from(stream.idle_prompt_seconds)),
            ..Self::default()
        }
    }
}

/// Derives the protocol state from the probe's facts.
pub fn compute(probe: &dyn HostProbe, config: &HostStateConfig) -> HostStateEvent {
    let locked = probe.locked();
    let attended = !locked
        && probe
            .idle()
            .is_none_or(|idle| idle < config.attended_within);
    HostStateEvent {
        locked,
        attended,
        on_battery: probe.on_battery(),
        stream_available: config.stream_engine && !locked,
    }
}

/// A probe driven by hand: for tests and headless harnesses that must control the state without
/// touching the real session. Starts unlocked, idle 0 s, on mains power.
#[derive(Debug, Default)]
pub struct ManualProbe {
    locked: AtomicBool,
    /// Idle milliseconds; `u64::MAX` means "unreadable".
    idle_ms: AtomicU64,
    battery: AtomicBool,
}

impl ManualProbe {
    /// Sets the lock state.
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
    }

    /// Sets the idle time (`None`: unreadable).
    pub fn set_idle(&self, idle: Option<Duration>) {
        let ms = idle.map_or(u64::MAX, |d| {
            u64::try_from(d.as_millis()).unwrap_or(u64::MAX - 1)
        });
        self.idle_ms.store(ms, Ordering::SeqCst);
    }

    /// Sets the power source.
    pub fn set_on_battery(&self, on_battery: bool) {
        self.battery.store(on_battery, Ordering::SeqCst);
    }
}

impl HostProbe for ManualProbe {
    fn locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    fn idle(&self) -> Option<Duration> {
        match self.idle_ms.load(Ordering::SeqCst) {
            u64::MAX => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    fn on_battery(&self) -> bool {
        self.battery.load(Ordering::SeqCst)
    }
}

/// A probe with test overrides: `locked` is the inner probe's OR a simulated lock (it can only
/// add a lock, never hide a real one); `idle` can be forced to "someone is here" (0 s) or
/// "nobody" (unreadable is not used: a very long idle time).
#[derive(Debug)]
pub struct OverrideProbe {
    inner: Arc<dyn HostProbe>,
    locked: AtomicBool,
    /// 0: the inner probe's idle time, 1: attended, 2: idle.
    attended: AtomicU8,
}

impl OverrideProbe {
    /// Wraps `inner` with no override.
    pub fn new(inner: Arc<dyn HostProbe>) -> Self {
        Self {
            inner,
            locked: AtomicBool::new(false),
            attended: AtomicU8::new(0),
        }
    }

    /// Simulates a lock (`true`) or its end.
    pub fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
    }

    /// Forces attended (`Some(true)`), idle (`Some(false)`) or the real answer (`None`).
    pub fn set_attended(&self, attended: Option<bool>) {
        let v = match attended {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        };
        self.attended.store(v, Ordering::SeqCst);
    }
}

impl HostProbe for OverrideProbe {
    fn locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst) || self.inner.locked()
    }

    fn idle(&self) -> Option<Duration> {
        match self.attended.load(Ordering::SeqCst) {
            1 => Some(Duration::ZERO),
            2 => Some(Duration::from_secs(u64::from(u32::MAX))),
            _ => self.inner.idle(),
        }
    }

    fn on_battery(&self) -> bool {
        self.inner.on_battery()
    }
}

/// Publishes the current [`HostStateEvent`]. Dropping it stops the polling task.
#[derive(Debug)]
pub struct HostStateMonitor {
    tx: Arc<watch::Sender<HostStateEvent>>,
    wake: Arc<Notify>,
    task: JoinHandle<()>,
    engine: Arc<AtomicBool>,
}

impl HostStateMonitor {
    /// Reads the probe once (so [`HostStateMonitor::current`] is right from the first call) and
    /// starts the task. Must be called inside a tokio runtime.
    ///
    /// `wake` is the signal the probe's owner fires when something changed (a lock
    /// notification); pass a fresh `Arc<Notify>` and give a clone to the platform callback.
    pub fn spawn(probe: Arc<dyn HostProbe>, wake: Arc<Notify>, config: HostStateConfig) -> Self {
        let (tx, _) = watch::channel(compute(&*probe, &config));
        let tx = Arc::new(tx);
        let engine = Arc::new(AtomicBool::new(config.stream_engine));
        let task = tokio::spawn({
            let tx = Arc::clone(&tx);
            let wake = Arc::clone(&wake);
            let engine = Arc::clone(&engine);
            async move {
                loop {
                    tokio::select! {
                        () = wake.notified() => {}
                        () = tokio::time::sleep(config.poll_interval) => {}
                    }
                    let (p, mut c) = (Arc::clone(&probe), config.clone());
                    c.stream_engine = engine.load(Ordering::SeqCst);
                    // The probes are cheap syscalls, but a wedged session service must not stall
                    // the runtime.
                    let Ok(next) = tokio::task::spawn_blocking(move || compute(&*p, &c)).await
                    else {
                        continue;
                    };
                    tx.send_if_modified(|cur| {
                        let changed = *cur != next;
                        *cur = next;
                        changed
                    });
                }
            }
        });
        Self {
            tx,
            wake,
            task,
            engine,
        }
    }

    /// The Stream engine is (or is no longer) installed: `stream_available` follows at once.
    pub fn set_stream_engine(&self, present: bool) {
        self.engine.store(present, Ordering::SeqCst);
        self.refresh_now();
    }

    /// A receiver that yields the current value and every change.
    pub fn subscribe(&self) -> watch::Receiver<HostStateEvent> {
        self.tx.subscribe()
    }

    /// The current state.
    pub fn current(&self) -> HostStateEvent {
        *self.tx.borrow()
    }

    /// Re-reads the probe now instead of waiting for the next poll.
    pub fn refresh_now(&self) {
        self.wake.notify_one();
    }
}

impl Drop for HostStateMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HostStateConfig {
        HostStateConfig {
            poll_interval: Duration::from_millis(20),
            attended_within: Duration::from_secs(120),
            stream_engine: true,
        }
    }

    #[test]
    fn compute_follows_the_rules() {
        let p = ManualProbe::default();
        p.set_idle(Some(Duration::from_millis(10_000)));
        let s = compute(&p, &cfg());
        assert_eq!(
            s,
            HostStateEvent {
                locked: false,
                attended: true,
                on_battery: false,
                stream_available: true
            }
        );

        p.set_idle(Some(Duration::from_millis(120_000)));
        assert!(
            !compute(&p, &cfg()).attended,
            "exactly the threshold is idle"
        );

        p.set_locked(true);
        p.set_idle(Some(Duration::from_millis(1)));
        let s = compute(&p, &cfg());
        assert!(s.locked && !s.attended && !s.stream_available, "{s:?}");

        p.set_locked(false);
        p.set_idle(None);
        assert!(
            compute(&p, &cfg()).attended,
            "unreadable idle time on an unlocked host counts as attended"
        );

        p.set_on_battery(true);
        assert!(compute(&p, &cfg()).on_battery);

        let phase1 = HostStateConfig {
            stream_engine: false,
            ..cfg()
        };
        assert!(
            !compute(&p, &phase1).stream_available,
            "no engine, no stream"
        );
    }

    #[test]
    fn idle_threshold_comes_from_the_stream_config() {
        let stream = gander_core::config::StreamConfig {
            idle_prompt_seconds: 30,
            ..Default::default()
        };
        let c = HostStateConfig::from_stream_config(&stream);
        assert_eq!(c.attended_within, Duration::from_secs(30));
        assert!(!c.stream_engine);
    }

    #[tokio::test]
    async fn monitor_publishes_changes_on_wake_and_on_poll() {
        let probe = Arc::new(ManualProbe::default());
        probe.set_idle(Some(Duration::from_millis(1_000)));
        let wake = Arc::new(Notify::new());
        let monitor = HostStateMonitor::spawn(
            probe.clone(),
            wake.clone(),
            HostStateConfig {
                poll_interval: Duration::from_secs(3600),
                ..cfg()
            },
        );
        let mut rx = monitor.subscribe();
        assert!(!monitor.current().locked);

        // A lock notification is picked up through the wake signal, not the (hour-long) poll.
        probe.set_locked(true);
        wake.notify_one();
        tokio::time::timeout(Duration::from_secs(2), rx.changed())
            .await
            .expect("change pushed")
            .unwrap();
        assert!(rx.borrow_and_update().locked);

        probe.set_locked(false);
        monitor.refresh_now();
        tokio::time::timeout(Duration::from_secs(2), rx.changed())
            .await
            .expect("change pushed")
            .unwrap();
        assert!(!rx.borrow_and_update().locked);

        // No change, no message.
        monitor.refresh_now();
        assert!(
            tokio::time::timeout(Duration::from_millis(150), rx.changed())
                .await
                .is_err(),
            "an unchanged state must not be re-published"
        );
    }

    #[tokio::test]
    async fn engine_presence_switches_stream_available() {
        let probe = Arc::new(ManualProbe::default());
        let monitor = HostStateMonitor::spawn(
            probe,
            Arc::default(),
            HostStateConfig {
                stream_engine: false,
                ..cfg()
            },
        );
        let mut rx = monitor.subscribe();
        assert!(!monitor.current().stream_available);
        monitor.set_stream_engine(true);
        tokio::time::timeout(Duration::from_secs(2), rx.wait_for(|s| s.stream_available))
            .await
            .expect("pushed")
            .unwrap();
    }

    #[test]
    fn override_probe_only_adds_a_lock_and_forces_attended() {
        let inner = Arc::new(ManualProbe::default());
        inner.set_idle(Some(Duration::from_secs(1)));
        let p = OverrideProbe::new(inner.clone());
        assert!(!p.locked());
        p.set_locked(true);
        assert!(p.locked());
        p.set_locked(false);
        inner.set_locked(true);
        assert!(p.locked(), "a real lock is never hidden");
        inner.set_locked(false);
        p.set_attended(Some(false));
        assert!(!compute(&p, &cfg()).attended);
        p.set_attended(Some(true));
        assert!(compute(&p, &cfg()).attended);
        p.set_attended(None);
        assert_eq!(p.idle(), Some(Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn monitor_polls_attended_and_battery() {
        let probe = Arc::new(ManualProbe::default());
        probe.set_idle(Some(Duration::from_millis(1_000)));
        let monitor = HostStateMonitor::spawn(probe.clone(), Arc::default(), cfg());
        let mut rx = monitor.subscribe();
        assert!(monitor.current().attended);
        probe.set_idle(Some(Duration::from_millis(500_000)));
        probe.set_on_battery(true);
        tokio::time::timeout(Duration::from_secs(2), rx.changed())
            .await
            .expect("poll noticed")
            .unwrap();
        let s = *rx.borrow_and_update();
        assert!(!s.attended && s.on_battery, "{s:?}");
    }
}
