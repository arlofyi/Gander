// SPDX-License-Identifier: Apache-2.0
//! When input may be injected: the pause and resume rules (stream-launch-policy spec, "Input
//! mapping" and "Session lifecycle"; `security.md` rule 12), as a pure state machine.
//!
//! The platform layer observes the facts ([`Facts`]: session lock, whether the captured window is
//! the foreground window, the last **local** input) every poll and before every injection, and
//! feeds them to [`InputGate::update`], which answers with the new [`GateState`] when it changed.
//!
//! | Cause (highest first) | Paused while | `stream.state` reason |
//! | --- | --- | --- |
//! | [`PauseCause::Locked`] | the session is locked, disconnected or unknown (fail closed) | none in protocol v1 (the lifecycle ends the session) |
//! | [`PauseCause::PhoneBackground`] | the client sent `stream.pause` and not yet `stream.resume` | `phone_background` |
//! | [`PauseCause::FocusLost`] | the captured window (or a window it owns) is not the foreground window | `focus_lost` |
//! | [`PauseCause::LocalUserActive`] | less than [`GateConfig::local_pause`] (2 s) since the last local input | `local_user_active` |
//!
//! Resume is automatic once no cause holds: focus comes back when the phone's "Bring to front"
//! runs the foreground procedure again or the local user switches back, local activity expires
//! 2 s after the last local input, and the lock clears on unlock (the lifecycle normally ends the
//! session first).

use std::time::Duration;

use crate::protocol::stream::{StreamReason, StreamState};

/// Timing of the local-activity rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateConfig {
    /// Injection stays paused this long after the last local input (spec: 2 s).
    pub local_pause: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            local_pause: Duration::from_secs(2),
        }
    }
}

/// What the platform layer observed. Times are on one monotonic clock of the caller's choice
/// (the controller uses the time since it started).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// The session is locked (fail closed: unknown counts as locked).
    pub locked: bool,
    /// The captured window, or a window it owns, is the foreground window.
    pub foreground: bool,
    /// When the last input that Gander did not inject happened, if any since the controller
    /// started.
    pub last_local_input: Option<Duration>,
}

/// Why injection is paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseCause {
    /// The session is locked or of unknown state.
    Locked,
    /// The client asked (`stream.pause`).
    PhoneBackground,
    /// Another window is in front.
    FocusLost,
    /// Someone is using the host's own keyboard or mouse.
    LocalUserActive,
}

impl PauseCause {
    /// The protocol reason for `stream.state: paused`, `None` where protocol v1 has none.
    pub const fn stream_reason(self) -> Option<StreamReason> {
        match self {
            PauseCause::Locked => None,
            PauseCause::PhoneBackground => Some(StreamReason::PhoneBackground),
            PauseCause::FocusLost => Some(StreamReason::FocusLost),
            PauseCause::LocalUserActive => Some(StreamReason::LocalUserActive),
        }
    }
}

/// Injection allowed or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateState {
    /// Input is injected.
    Active,
    /// Input is dropped (and reported) for this cause.
    Paused(PauseCause),
}

impl GateState {
    /// The `stream.state` value this gate state corresponds to.
    pub const fn stream_state(self) -> StreamState {
        match self {
            GateState::Active => StreamState::Streaming,
            GateState::Paused(_) => StreamState::Paused,
        }
    }

    /// The `stream.state` reason.
    pub const fn stream_reason(self) -> Option<StreamReason> {
        match self {
            GateState::Active => None,
            GateState::Paused(c) => c.stream_reason(),
        }
    }
}

/// The pause and resume rules.
#[derive(Debug, Clone)]
pub struct InputGate {
    config: GateConfig,
    state: Option<GateState>,
    client_paused: bool,
}

impl InputGate {
    /// A gate that has not decided yet: the first [`update`](Self::update) always reports.
    pub fn new(config: GateConfig) -> Self {
        Self {
            config,
            state: None,
            client_paused: false,
        }
    }

    /// The current state; `Paused(Locked)` (fail closed) before the first update.
    pub fn state(&self) -> GateState {
        self.state.unwrap_or(GateState::Paused(PauseCause::Locked))
    }

    /// The state the facts imply, without changing anything.
    pub fn evaluate(&self, now: Duration, facts: &Facts) -> GateState {
        let local_active = facts
            .last_local_input
            .is_some_and(|t| now.saturating_sub(t) < self.config.local_pause);
        if facts.locked {
            GateState::Paused(PauseCause::Locked)
        } else if self.client_paused {
            GateState::Paused(PauseCause::PhoneBackground)
        } else if !facts.foreground {
            GateState::Paused(PauseCause::FocusLost)
        } else if local_active {
            GateState::Paused(PauseCause::LocalUserActive)
        } else {
            GateState::Active
        }
    }

    /// Applies new facts; returns the new state when it changed (and on the first call).
    pub fn update(&mut self, now: Duration, facts: &Facts) -> Option<GateState> {
        let next = self.evaluate(now, facts);
        if self.state == Some(next) {
            None
        } else {
            self.state = Some(next);
            Some(next)
        }
    }

    /// `stream.pause` (`true`) or `stream.resume` (`false`) from the client, then
    /// [`update`](Self::update).
    pub fn set_client_paused(
        &mut self,
        paused: bool,
        now: Duration,
        facts: &Facts,
    ) -> Option<GateState> {
        self.client_paused = paused;
        self.update(now, facts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn facts(locked: bool, foreground: bool, local: Option<u64>) -> Facts {
        Facts {
            locked,
            foreground,
            last_local_input: local.map(ms),
        }
    }

    #[test]
    fn fail_closed_before_the_first_update() {
        let g = InputGate::new(GateConfig::default());
        assert_eq!(g.state(), GateState::Paused(PauseCause::Locked));
    }

    #[test]
    fn first_update_reports_and_identical_facts_do_not() {
        let mut g = InputGate::new(GateConfig::default());
        assert_eq!(
            g.update(ms(0), &facts(false, true, None)),
            Some(GateState::Active)
        );
        assert_eq!(g.update(ms(100), &facts(false, true, None)), None);
    }

    #[test]
    fn focus_lost_pauses_and_returning_focus_resumes() {
        let mut g = InputGate::new(GateConfig::default());
        g.update(ms(0), &facts(false, true, None));
        let s = g.update(ms(100), &facts(false, false, None)).unwrap();
        assert_eq!(s, GateState::Paused(PauseCause::FocusLost));
        assert_eq!(s.stream_state(), StreamState::Paused);
        assert_eq!(s.stream_reason(), Some(StreamReason::FocusLost));
        assert_eq!(
            g.update(ms(200), &facts(false, true, None)),
            Some(GateState::Active)
        );
    }

    #[test]
    fn local_input_pauses_for_two_seconds_after_the_last_one() {
        let mut g = InputGate::new(GateConfig::default());
        g.update(ms(0), &facts(false, true, None));
        assert_eq!(
            g.update(ms(1000), &facts(false, true, Some(950))),
            Some(GateState::Paused(PauseCause::LocalUserActive))
        );
        // More local input extends the pause.
        assert_eq!(g.update(ms(2500), &facts(false, true, Some(2400))), None);
        assert_eq!(g.update(ms(4399), &facts(false, true, Some(2400))), None);
        assert_eq!(
            g.update(ms(4400), &facts(false, true, Some(2400))),
            Some(GateState::Active)
        );
        // Old local input (before the controller noticed) does not pause.
        let mut g = InputGate::new(GateConfig::default());
        assert_eq!(
            g.update(ms(10_000), &facts(false, true, Some(1000))),
            Some(GateState::Active)
        );
    }

    #[test]
    fn precedence_lock_then_client_then_focus_then_local() {
        let mut g = InputGate::new(GateConfig::default());
        let all = facts(true, false, Some(0));
        assert_eq!(
            g.set_client_paused(true, ms(10), &all),
            Some(GateState::Paused(PauseCause::Locked))
        );
        assert_eq!(GateState::Paused(PauseCause::Locked).stream_reason(), None);
        assert_eq!(
            g.update(ms(20), &facts(false, false, Some(0))),
            Some(GateState::Paused(PauseCause::PhoneBackground))
        );
        assert_eq!(
            g.set_client_paused(false, ms(30), &facts(false, false, Some(0))),
            Some(GateState::Paused(PauseCause::FocusLost))
        );
        assert_eq!(
            g.update(ms(40), &facts(false, true, Some(0))),
            Some(GateState::Paused(PauseCause::LocalUserActive))
        );
        assert_eq!(
            g.update(ms(2000), &facts(false, true, Some(0))),
            Some(GateState::Active)
        );
    }

    #[test]
    fn lock_wins_over_everything_and_unlock_resumes_only_with_focus() {
        let mut g = InputGate::new(GateConfig::default());
        g.update(ms(0), &facts(false, true, None));
        assert_eq!(
            g.update(ms(1), &facts(true, true, None)),
            Some(GateState::Paused(PauseCause::Locked))
        );
        assert_eq!(
            g.update(ms(2), &facts(false, false, None)),
            Some(GateState::Paused(PauseCause::FocusLost))
        );
    }
}
