// SPDX-License-Identifier: Apache-2.0
//! Bringing the captured window to the foreground (stream-launch-policy spec, "Launch and window
//! discovery" step 7, and the phone's "Bring to front").
//!
//! Windows only lets a process take the foreground under the rules of `SetForegroundWindow`
//! (the caller is the foreground process, was started by it, received the last input event, or
//! the foreground lock timeout passed, among others). A background host usually meets none of
//! them, so [`acquire`] tries a documented sequence and records each step in a
//! [`ForegroundReport`]:
//!
//! 1. Restore the window if it is minimised (`SW_RESTORE`; capture gets no frames either).
//! 2. [`Method::SetForegroundWindow`]: the plain call.
//! 3. [`Method::AllowSetForegroundWindow`]: `AllowSetForegroundWindow(target pid)`, then let the
//!    target's own activation succeed. It only grants what the caller has, so it helps only
//!    while Gander itself may set the foreground (for example right after the owner clicked the
//!    tray). The spec's "`AllowSetForegroundWindow` for our own pid at process start" does
//!    nothing: the function grants rights to another process.
//! 4. [`Method::AttachThreadInput`]: attach this thread's input state to the foreground window's
//!    thread, call `SetForegroundWindow` and `BringWindowToTop`, detach. Skipped when the
//!    foreground app is hung (attaching to a hung thread can block). Sends no input to anyone.
//! 5. [`Method::AltKey`] (opt-in, [`AcquirePolicy::alt_key`]): inject an Alt press, call
//!    `SetForegroundWindow` while it is held, then release it. Injected input makes this process
//!    the one that "received the last input event". The Alt press is delivered to the window
//!    that is in front at that moment, which is **another application's window**; a lone Alt may
//!    move keyboard focus to that app's menu bar. Off by default for that reason; the WP-52 tests
//!    enable it only when the window in front is a test window.
//!
//! After every step the foreground is re-read (it can change asynchronously) for up to
//! [`AcquirePolicy::settle`]. The run stops at the first step that works.
//!
//! Measured on the WP-52 machine (Windows 11 Pro 26200, unlocked, a second test window in front,
//! called from a console test process that is not the foreground process): see the WP-52 report
//! and `foreground_methods_one_by_one` in `host/gander-testwin/tests/input.rs`.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_MENU};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, IsHungAppWindow, IsIconic, MSG, PM_NOREMOVE,
    PeekMessageW, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

use gander_core::input::Refusal;
use gander_core::input::activity::InjectionLog;

use super::InputTarget;
use super::os;

/// One way of taking the foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// The window was already in front.
    AlreadyForeground,
    /// `ShowWindow(SW_RESTORE)` of a minimised window.
    Restore,
    /// `SetForegroundWindow`.
    SetForegroundWindow,
    /// `AllowSetForegroundWindow(target pid)` then `SetForegroundWindow`.
    AllowSetForegroundWindow,
    /// `AttachThreadInput` to the foreground thread around `SetForegroundWindow`.
    AttachThreadInput,
    /// An injected Alt press around `SetForegroundWindow`.
    AltKey,
}

/// Which fallbacks [`acquire`] may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcquirePolicy {
    /// Try `AttachThreadInput` (default on).
    pub attach_thread_input: bool,
    /// Try the Alt-key trick (default off: the Alt press reaches another application's window).
    pub alt_key: bool,
    /// How long to wait for the foreground to change after a step.
    pub settle: Duration,
}

impl Default for AcquirePolicy {
    fn default() -> Self {
        Self {
            attach_thread_input: true,
            alt_key: false,
            settle: Duration::from_millis(250),
        }
    }
}

/// One step of [`acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    /// The method.
    pub method: Method,
    /// What the API call(s) returned (`SetForegroundWindow` non-zero, `AttachThreadInput`
    /// succeeded, ...).
    pub api_ok: bool,
    /// The target was in front afterwards.
    pub foreground_after: bool,
}

/// What [`acquire`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForegroundReport {
    /// The steps, in order.
    pub attempts: Vec<Attempt>,
    /// The target ended up in front.
    pub acquired: bool,
}

impl ForegroundReport {
    /// The method that worked, if any.
    pub fn method(&self) -> Option<Method> {
        self.attempts
            .iter()
            .find(|a| a.foreground_after)
            .map(|a| a.method)
    }
}

fn wait_foreground(h: HWND, settle: Duration) -> bool {
    let end = Instant::now() + settle;
    loop {
        if os::is_foreground(h) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn set_foreground(h: HWND) -> bool {
    // SAFETY: plain call; any handle value is accepted.
    unsafe { SetForegroundWindow(h) != 0 }
}

/// Makes sure the calling thread has a message queue (`AttachThreadInput` needs one).
fn ensure_queue() {
    // SAFETY: an all-zero MSG is valid; PM_NOREMOVE leaves the queue untouched.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
    }
}

/// Runs one method alone (the findings test calls each in isolation).
pub fn try_method(
    target: &InputTarget,
    method: Method,
    settle: Duration,
    log: &mut InjectionLog,
) -> Result<Attempt, Refusal> {
    let h = target.handle();
    os::live(h, target.pid)?;
    let api_ok = match method {
        Method::AlreadyForeground => os::is_foreground(h),
        Method::Restore => {
            // SAFETY: plain calls on a live window.
            unsafe { IsIconic(h) != 0 && ShowWindow(h, SW_RESTORE) != 0 }
        }
        Method::SetForegroundWindow => set_foreground(h),
        Method::AllowSetForegroundWindow => {
            // SAFETY: plain call.
            let allowed = unsafe { AllowSetForegroundWindow(target.pid) != 0 };
            set_foreground(h) && allowed
        }
        Method::AttachThreadInput => {
            let fg = os::foreground();
            let Some((fg_thread, _)) = (!fg.is_null()).then(|| os::thread_and_pid(fg)).flatten()
            else {
                // No foreground window: nothing to attach to; the plain call is allowed then.
                return Ok(Attempt {
                    method,
                    api_ok: set_foreground(h),
                    foreground_after: wait_foreground(h, settle),
                });
            };
            // SAFETY: plain query on the foreground window.
            if unsafe { IsHungAppWindow(fg) } != 0 {
                return Ok(Attempt {
                    method,
                    api_ok: false,
                    foreground_after: os::is_foreground(h),
                });
            }
            ensure_queue();
            // SAFETY: plain calls; the attachment is undone below on every path.
            unsafe {
                let me = GetCurrentThreadId();
                let attached = me != fg_thread && AttachThreadInput(me, fg_thread, 1) != 0;
                let ok = set_foreground(h);
                BringWindowToTop(h);
                if attached {
                    AttachThreadInput(me, fg_thread, 0);
                }
                attached && ok
            }
        }
        Method::AltKey => {
            let down = os::send(&[os::key(VK_MENU, 0, 0)]);
            log.record(down.ticks.0, down.ticks.1);
            let ok = set_foreground(h);
            let up = os::send(&[os::key(VK_MENU, 0, KEYEVENTF_KEYUP)]);
            log.record(up.ticks.0, up.ticks.1);
            down.accepted == 1 && up.accepted == 1 && ok
        }
    };
    Ok(Attempt {
        method,
        api_ok,
        foreground_after: wait_foreground(h, settle),
    })
}

/// Brings `target` to the foreground with the documented fallbacks (module documentation).
///
/// The caller must have checked the session is unlocked: [`super::InputController::bring_to_front`]
/// does.
pub fn acquire(
    target: &InputTarget,
    policy: &AcquirePolicy,
    log: &mut InjectionLog,
) -> Result<ForegroundReport, Refusal> {
    let h = target.handle();
    os::live(h, target.pid)?;
    let mut report = ForegroundReport::default();
    // SAFETY: plain query.
    if unsafe { IsIconic(h) } != 0 {
        report
            .attempts
            .push(try_method(target, Method::Restore, policy.settle, log)?);
    }
    if os::is_foreground(h) {
        report.attempts.push(Attempt {
            method: Method::AlreadyForeground,
            api_ok: true,
            foreground_after: true,
        });
        report.acquired = true;
        return Ok(report);
    }
    let mut steps = vec![
        Method::SetForegroundWindow,
        Method::AllowSetForegroundWindow,
    ];
    if policy.attach_thread_input {
        steps.push(Method::AttachThreadInput);
    }
    if policy.alt_key {
        steps.push(Method::AltKey);
    }
    for m in steps {
        let a = try_method(target, m, policy.settle, log)?;
        report.attempts.push(a);
        if a.foreground_after {
            report.acquired = true;
            break;
        }
    }
    Ok(report)
}
