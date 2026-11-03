// SPDX-License-Identifier: Apache-2.0
//! Windows production pieces: the capture + encode source ([`WgcSource`]), the session-lock and
//! power adapters, the GPU-selection exports and the timer-resolution guard.
//!
//! Capture and encode are the spike's measured code made production code here (WP-41 capture
//! wrapper, WP-42 encoder), not a dependency on the spike crates:
//!
//! - [`d3d`]: adapters by LUID and a D3D11 device with video support, multithread protected.
//! - [`wgc`]: the own thin Windows Graphics Capture wrapper (frames on the caller's device).
//! - [`convert`]: BGRA -> NV12 with the D3D11 video processor, scaled to the encode size.
//! - [`mft`], [`encoder`]: Media Foundation hardware H.264 encoder MFTs per adapter.
//! - [`pipeline`]: the source thread tying them together ([`WgcSource`]).
//! - [`audio`]: WASAPI process loopback of the window's process tree ([`ProcessLoopback`], WP-55).

pub mod audio;
pub mod clock;
pub mod convert;
pub mod d3d;
pub mod encoder;
pub mod mft;
pub mod optimus;
pub mod pipeline;
pub mod wgc;

use std::sync::Arc;

use tokio::sync::watch;

use crate::deps::{LockSource, LockStatus, PowerSource};

pub use audio::ProcessLoopback;
pub use pipeline::WgcSource;

/// [`LockSource`] over `gander_win::session::watch` (WP-17): `WTSRegisterSessionNotification`
/// plus a 15 s re-query; an unknown, disconnected or switched-away session counts as locked.
pub struct SessionLock {
    tx: Arc<watch::Sender<LockStatus>>,
    watcher: gander_win::session::SessionWatcher,
}

impl std::fmt::Debug for SessionLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLock")
            .field("lock", &*self.tx.borrow())
            .finish_non_exhaustive()
    }
}

impl SessionLock {
    /// Starts watching.
    pub fn start() -> std::io::Result<Self> {
        // Locked until the watcher has said otherwise: fail closed.
        let tx = Arc::new(watch::Sender::new(LockStatus {
            locked: true,
            locks: 0,
        }));
        let cb_tx = Arc::clone(&tx);
        let watcher = gander_win::session::watch(move |state| {
            cb_tx.send_modify(|s| s.observe(state.is_locked()));
        })?;
        tx.send_modify(|s| s.locked = watcher.state().is_locked());
        Ok(Self { tx, watcher })
    }

    /// Re-queries the OS now.
    pub fn refresh(&self) {
        self.watcher.refresh();
        let locked = self.watcher.state().is_locked();
        self.tx.send_modify(|s| s.observe(locked));
    }
}

impl LockSource for SessionLock {
    fn subscribe(&self) -> watch::Receiver<LockStatus> {
        self.tx.subscribe()
    }
}

/// [`PowerSource`] over `gander_win::session::power::power_status` (WP-17). An unreadable
/// status counts as "not on battery" (nothing is throttled on a guess).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPower;

impl PowerSource for SystemPower {
    fn on_battery(&self) -> bool {
        gander_win::session::power::power_status().is_ok_and(|s| s.on_battery())
    }
}

/// Raises the system timer resolution to 1 ms while alive (`timeBeginPeriod(1)`, per process
/// since Windows 10 2004). WP-42: the Intel MFT in low-latency mode falls to 21 fps with the
/// default 15.6 ms timer, 30 fps with 1 ms. Restored by `timeEndPeriod` on drop.
#[derive(Debug)]
pub struct TimerResolution(());

impl TimerResolution {
    /// Requests 1 ms.
    pub fn one_ms() -> Self {
        // SAFETY: plain call; balanced by the `timeEndPeriod` in `Drop`.
        unsafe { windows::Win32::Media::timeBeginPeriod(1) };
        Self(())
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        // SAFETY: matches the `timeBeginPeriod(1)` in `one_ms`.
        unsafe { windows::Win32::Media::timeEndPeriod(1) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lock_reports_the_current_state() {
        // Read only: registers for session notifications and queries the state; changes nothing.
        let lock = SessionLock::start().expect("session watcher");
        let rx = lock.subscribe();
        let status = *rx.borrow();
        eprintln!("WP-50 session lock status on this machine: {status:?}");
        lock.refresh();
        assert!(rx.has_changed().is_ok(), "the sender is alive");
        let _ = SystemPower.on_battery();
    }

    #[test]
    fn timer_resolution_guard_balances() {
        let a = TimerResolution::one_ms();
        let b = TimerResolution::one_ms();
        drop(a);
        drop(b);
    }
}
