// SPDX-License-Identifier: Apache-2.0
//! The 60 Hz frame clock and the self-test deadline.
//!
//! A dedicated thread publishes frame numbers and posts one `WM_APP_TICK` to the main window per
//! frame (coalesced while the window thread is busy). It is a thread, not a `WM_TIMER`, because
//! the window thread spends long stretches inside modal loops (popup menu, message box, window
//! drag) that still dispatch posted messages but would starve or coarsen a low priority timer.

use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, PostThreadMessageW, WM_QUIT};
use windows::core::PCWSTR;

use super::shared::{frame_start_nanos, shared};
use super::window::{WM_APP_DISMISS, WM_APP_TICK};

/// How long after the self-test deadline a graceful shutdown may take before the process is
/// ended forcibly.
const FORCE_EXIT_GRACE: Duration = Duration::from_secs(3);
/// Exit code of the forced shutdown (a hung window thread); 0 is reserved for a clean exit.
pub const FORCED_EXIT_CODE: i32 = 4;

/// A high resolution waitable timer, or `None` where unsupported (Windows before 10 1803).
struct HighResTimer(HANDLE);

impl HighResTimer {
    fn new() -> Option<HighResTimer> {
        // SAFETY: no security attributes and no name; the flags and access mask are valid
        // constants. The returned handle is owned by the new `HighResTimer`.
        unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        }
        .ok()
        .map(HighResTimer)
    }

    /// Sleeps for `d`; false if the timer misbehaved (caller falls back to `thread::sleep`).
    fn sleep(&self, d: Duration) -> bool {
        // Relative due times are negative, in units of 100 ns.
        let due = -((d.as_nanos() / 100).max(1) as i64);
        // SAFETY: `self.0` is a valid timer handle and `due` outlives the call; no completion
        // routine is registered.
        let armed = unsafe { SetWaitableTimer(self.0, &due, 0, None, None, false) };
        if armed.is_err() {
            return false;
        }
        // SAFETY: `self.0` is a valid handle.
        unsafe { WaitForSingleObject(self.0, INFINITE) };
        true
    }
}

impl Drop for HighResTimer {
    fn drop(&mut self) {
        // SAFETY: the handle was created in `new` and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Starts the ticker thread. `self_test` is the exit deadline measured from process start.
pub fn spawn(self_test: Option<Duration>) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("testwin-ticker".into())
        .spawn(move || run(self_test))
}

fn run(self_test: Option<Duration>) {
    let s = shared();
    let timer = HighResTimer::new();
    let deadline = self_test.map(|d| s.start + d);
    let mut quit_started_at: Option<Instant> = None;
    let mut next: u64 = 0;

    while !s.stop.load(Ordering::Relaxed) {
        // Sleep until the first instant of frame `next`.
        let due = Duration::from_nanos(frame_start_nanos(next));
        let remaining = due.saturating_sub(s.start.elapsed());
        if !remaining.is_zero() {
            let slept = timer.as_ref().is_some_and(|t| t.sleep(remaining));
            if !slept {
                thread::sleep(remaining);
            }
        }
        if s.stop.load(Ordering::Relaxed) {
            break;
        }

        // If we woke late, skip the frames that have already passed: the counter on screen then
        // shows a gap, which is the truth about what was displayed.
        let frame = u64::from(s.frame_now()).max(next);
        s.latest_frame.store(frame as u32, Ordering::Relaxed);
        next = frame + 1;
        if !s.frozen.load(Ordering::Relaxed) && !s.tick_pending.swap(true, Ordering::AcqRel) {
            let hwnd = HWND(s.main_hwnd.load(Ordering::Relaxed) as *mut _);
            // SAFETY: PostMessageW is safe to call with any handle value; a stale handle just
            // fails.
            if unsafe {
                PostMessageW(
                    Some(hwnd),
                    WM_APP_TICK,
                    Default::default(),
                    Default::default(),
                )
            }
            .is_err()
            {
                s.tick_pending.store(false, Ordering::Release);
            }
        }

        let now = Instant::now();
        let quit_due =
            s.quit_requested.load(Ordering::Relaxed) || deadline.is_some_and(|d| now >= d);
        if quit_due {
            match quit_started_at {
                None => quit_started_at = Some(now),
                Some(at) if now.duration_since(at) > FORCE_EXIT_GRACE => {
                    eprintln!("gander-testwin: window thread did not exit; forcing exit");
                    std::process::exit(FORCED_EXIT_CODE);
                }
                Some(_) => {}
            }
            // Repeated every frame until the process ends: a modal loop may swallow one attempt.
            post_quit_messages();
        }
    }
}

/// Requests a clean shutdown (the `quit` command). The ticker thread keeps repeating the request
/// until the process has exited, and forces the exit if that takes more than a few seconds.
pub fn request_quit() {
    shared().quit_requested.store(true, Ordering::Relaxed);
    post_quit_messages();
}

/// Asks the window thread to leave its message loop: first close any open menu or dialog (a
/// popup menu's modal loop ignores a bare `WM_QUIT`), then post `WM_QUIT` to the thread, which
/// also ends the message box loop.
fn post_quit_messages() {
    let s = shared();
    let hwnd = HWND(s.main_hwnd.load(Ordering::Relaxed) as *mut _);
    // SAFETY: PostMessageW accepts any handle value; a stale handle just fails.
    let _ = unsafe {
        PostMessageW(
            Some(hwnd),
            WM_APP_DISMISS,
            Default::default(),
            Default::default(),
        )
    };
    let thread_id = s.ui_thread.load(Ordering::Relaxed);
    if thread_id != 0 {
        // SAFETY: PostThreadMessageW takes plain integers; an unknown thread id just fails.
        let _ = unsafe {
            PostThreadMessageW(thread_id, WM_QUIT, Default::default(), Default::default())
        };
    }
}
