// SPDX-License-Identifier: Apache-2.0
//! Telling the local user's input from Gander's own injections.
//!
//! `GetLastInputInfo` reports one tick (32-bit milliseconds since boot, `GetTickCount` clock) for
//! the last input of any origin in the session: physical keyboard and mouse, and every event
//! `SendInput` injected, ours included. The injector therefore records a window of ticks around
//! each `SendInput` call ([`InjectionLog::record`]); a last-input tick inside a recorded window is
//! ours, any other new tick is the local user ([`InjectionLog::classify`]).
//!
//! Limits (stated, not hidden): the tick clock has a resolution of about 15.6 ms, so the windows
//! carry slack ([`SLACK_BEFORE`], [`SLACK_AFTER`]); a physical input that falls inside one of our
//! windows is taken for ours, and one that happens just before an injection is overwritten by it
//! unless it was observed first (the injector observes right before every injection and on every
//! poll). A low-level hook (`WH_MOUSE_LL`/`WH_KEYBOARD_LL`, whose `LLMHF_INJECTED` flag marks
//! every injected event exactly) is the precise alternative the spec allows; it puts a hook in the
//! path of every input event on the desktop and needs its own message loop, so it is left for when
//! these limits show up in practice.

use std::collections::VecDeque;

/// Slack before a recorded `SendInput` call (tick resolution).
pub const SLACK_BEFORE: u32 = 32;
/// Slack after it (the raw input thread stamps the event when it processes it).
pub const SLACK_AFTER: u32 = 150;
/// Recorded windows kept (older ones are irrelevant: only the newest tick is ever classified).
const KEEP: usize = 64;

/// Origin of a last-input tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Unchanged since the last observation.
    Unchanged,
    /// Inside one of our injection windows.
    Injected,
    /// Anything else: the local user (or another program's injection, which is treated alike).
    Local,
}

/// Recent injection windows and the last tick observed.
#[derive(Debug, Clone, Default)]
pub struct InjectionLog {
    windows: VecDeque<(u32, u32)>,
    last_seen: Option<u32>,
}

/// `a` is at or after `b` on the wrapping tick clock (within 24.8 days).
const fn at_or_after(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) < (1 << 31)
}

impl InjectionLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a `SendInput` call that ran between ticks `start` and `end`.
    pub fn record(&mut self, start: u32, end: u32) {
        if self.windows.len() == KEEP {
            self.windows.pop_front();
        }
        self.windows.push_back((
            start.wrapping_sub(SLACK_BEFORE),
            end.wrapping_add(SLACK_AFTER),
        ));
    }

    /// `tick` falls inside a recorded window.
    pub fn is_ours(&self, tick: u32) -> bool {
        self.windows
            .iter()
            .any(|&(from, to)| at_or_after(tick, from) && at_or_after(to, tick))
    }

    /// Classifies the current `GetLastInputInfo` tick and remembers it. The first observation
    /// only sets the baseline (input before the injector started is not "activity during the
    /// stream").
    pub fn classify(&mut self, last_input: u32) -> Origin {
        let previous = self.last_seen.replace(last_input);
        match previous {
            None => Origin::Unchanged,
            Some(p) if p == last_input => Origin::Unchanged,
            Some(_) if self.is_ours(last_input) => Origin::Injected,
            Some(_) => Origin::Local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_then_ours_then_local() {
        let mut log = InjectionLog::new();
        assert_eq!(log.classify(1000), Origin::Unchanged, "baseline");
        assert_eq!(log.classify(1000), Origin::Unchanged);
        log.record(2000, 2005);
        assert_eq!(log.classify(2016), Origin::Injected);
        assert_eq!(log.classify(2016), Origin::Unchanged);
        assert_eq!(
            log.classify(2140),
            Origin::Injected,
            "inside the slack after"
        );
        assert_eq!(log.classify(2500), Origin::Local);
        assert_eq!(
            log.classify(1990),
            Origin::Injected,
            "inside the slack before"
        );
    }

    #[test]
    fn windows_across_the_tick_wrap() {
        let mut log = InjectionLog::new();
        log.classify(u32::MAX - 100);
        log.record(u32::MAX - 5, 3);
        assert_eq!(log.classify(1), Origin::Injected);
        assert_eq!(log.classify(u32::MAX - 2), Origin::Injected);
        assert_eq!(log.classify(400), Origin::Local);
    }

    #[test]
    fn keeps_a_bounded_history() {
        let mut log = InjectionLog::new();
        for i in 0..1000u32 {
            log.record(i * 1000, i * 1000 + 1);
        }
        assert!(log.windows.len() <= KEEP);
        assert!(log.is_ours(999_000));
        assert!(!log.is_ours(0));
    }
}
