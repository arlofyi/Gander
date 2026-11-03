// SPDX-License-Identifier: Apache-2.0
//! Pausing the listener (tray menu "Pause listener 10 min"; security.md principle 7: the kill
//! switch "optionally pauses the listener").
//!
//! While paused, the plan the supervisor sees has no private-network addresses: the next re-plan
//! closes every non-loopback socket (so nobody on the private network can even connect), loopback
//! stays up like it does when the private network is down. [`PausablePlan`] wraps the real
//! [`PlanSource`]; [`ListenerPause`] is the switch the tray flips.
//!
//! The pause is in memory only: restarting the host ends it. The caller is responsible for
//! re-planning when it starts or ends a pause (`ServerHandle::replan_now`), because the
//! supervisor otherwise only notices at its 30 s safety-net poll.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gander_core::net::plan::ListenerPlan;

use super::listener::PlanSource;

/// The pause switch. Share as `Arc<ListenerPause>`.
#[derive(Debug, Default)]
pub struct ListenerPause {
    state: Mutex<PauseState>,
}

#[derive(Debug, Default)]
struct PauseState {
    until: Option<Instant>,
    /// Bumped on every change, so a stale auto-resume timer can tell it was superseded.
    epoch: u64,
}

impl ListenerPause {
    /// A switch that is off.
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PauseState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Pauses for `duration` from now (a second call restarts the clock). Returns the epoch the
    /// pause started in; pass it to [`ListenerPause::resume_if_epoch`] from the auto-resume timer.
    pub fn pause_for(&self, duration: Duration) -> u64 {
        let mut s = self.lock();
        s.epoch += 1;
        s.until = Some(Instant::now() + duration);
        s.epoch
    }

    /// Ends the pause now. Returns whether one was in force.
    pub fn resume(&self) -> bool {
        let mut s = self.lock();
        let was = s.until.is_some_and(|u| u > Instant::now());
        s.epoch += 1;
        s.until = None;
        was
    }

    /// Ends the pause only if no later change superseded `epoch` (the auto-resume timer).
    pub fn resume_if_epoch(&self, epoch: u64) -> bool {
        let mut s = self.lock();
        if s.epoch != epoch {
            return false;
        }
        s.epoch += 1;
        s.until.take().is_some()
    }

    /// Time left, or `None` when not paused.
    pub fn remaining(&self) -> Option<Duration> {
        let s = self.lock();
        s.until
            .and_then(|u| u.checked_duration_since(Instant::now()))
            .filter(|d| !d.is_zero())
    }

    /// Whether the listener is paused now.
    pub fn is_paused(&self) -> bool {
        self.remaining().is_some()
    }
}

/// A [`PlanSource`] that serves loopback only while the [`ListenerPause`] is on.
#[derive(Debug)]
pub struct PausablePlan {
    inner: Arc<dyn PlanSource>,
    pause: Arc<ListenerPause>,
}

impl PausablePlan {
    /// Wraps `inner`; `pause` is the switch.
    pub fn new(inner: Arc<dyn PlanSource>, pause: Arc<ListenerPause>) -> Self {
        Self { inner, pause }
    }
}

impl PlanSource for PausablePlan {
    fn plan(&self) -> Result<ListenerPlan, String> {
        let plan = self.inner.plan()?;
        if !self.pause.is_paused() {
            return Ok(plan);
        }
        ListenerPlan::new(
            plan.adapter(),
            plan.port(),
            plan.loopback().to_vec(),
            Vec::new(),
        )
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use gander_core::protocol::host::TrustAdapterKind;

    use super::super::listener::FixedPlan;
    use super::*;

    fn plan() -> ListenerPlan {
        ListenerPlan::new(
            TrustAdapterKind::Tailscale,
            42637,
            vec!["127.0.0.1".parse::<IpAddr>().unwrap()],
            vec!["100.101.102.103".parse::<IpAddr>().unwrap()],
        )
        .unwrap()
    }

    #[test]
    fn paused_plan_is_loopback_only_and_resumes() {
        let pause = ListenerPause::new();
        let source = PausablePlan::new(Arc::new(FixedPlan(plan())), Arc::clone(&pause));
        assert_eq!(source.plan().unwrap().network().len(), 1);

        pause.pause_for(Duration::from_secs(600));
        assert!(pause.is_paused());
        assert!(pause.remaining().unwrap() > Duration::from_secs(590));
        let paused = source.plan().unwrap();
        assert!(paused.network().is_empty());
        assert_eq!(paused.loopback().len(), 1);
        assert_eq!(paused.port(), 42637);
        assert_eq!(paused.adapter(), TrustAdapterKind::Tailscale);

        assert!(pause.resume());
        assert!(!pause.is_paused());
        assert_eq!(source.plan().unwrap().network().len(), 1);
        assert!(!pause.resume(), "nothing to resume");
    }

    #[test]
    fn a_stale_auto_resume_does_not_end_a_newer_pause() {
        let pause = ListenerPause::new();
        let first = pause.pause_for(Duration::from_secs(600));
        let second = pause.pause_for(Duration::from_secs(600));
        assert!(!pause.resume_if_epoch(first), "superseded");
        assert!(pause.is_paused());
        assert!(pause.resume_if_epoch(second));
        assert!(!pause.is_paused());
    }

    #[test]
    fn an_expired_pause_reads_as_not_paused() {
        let pause = ListenerPause::new();
        pause.pause_for(Duration::ZERO);
        assert!(!pause.is_paused());
        assert!(pause.remaining().is_none());
    }
}
