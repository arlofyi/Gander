// SPDX-License-Identifier: Apache-2.0
//! The end of a session (WP-53, launch-policy spec "Session lifecycle"): is the streamed window
//! the only one of its process, and closing it politely with `WM_CLOSE` (never
//! `TerminateProcess`). An app that answers with a "save changes?" dialog stays open; the caller
//! tells the phone "left open: unsaved changes".

use std::thread;
use std::time::{Duration, Instant};

use gander_core::launch::discovery::WindowInfo;
use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, PostMessageW, WM_CLOSE};

use super::snapshot::{hwnd, snapshot, window_pid};
use super::{WindowError, WindowTarget};

/// How a close attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The window is gone.
    Closed,
    /// The window is still open after the wait. `dialog`: it owns a visible window now (most
    /// likely "save changes?").
    LeftOpen {
        /// A dialog owned by the window is showing.
        dialog: bool,
    },
}

/// Visible, unowned, uncloaked top-level windows of `pid` other than `except` (what "the only
/// window of that process" counts).
pub fn other_windows(windows: &[WindowInfo], pid: u32, except: u64) -> Vec<WindowInfo> {
    windows
        .iter()
        .filter(|w| {
            w.pid == pid && w.hwnd != except && w.visible && !w.cloaked && w.owner.is_none()
        })
        .cloned()
        .collect()
}

/// Whether `target` is the only visible top-level window of its process right now.
pub fn is_only_window(target: &WindowTarget) -> bool {
    other_windows(&snapshot(), target.pid, target.hwnd).is_empty()
}

fn alive(target: &WindowTarget) -> bool {
    let h = hwnd(target.hwnd);
    // SAFETY: plain query; any handle value is accepted.
    unsafe { IsWindow(h) != 0 && window_pid(h) == Some(target.pid) }
}

/// Posts `WM_CLOSE` to `target` and waits up to `wait` for it to go. Refuses a window whose
/// handle now belongs to another process.
pub fn close_window(target: &WindowTarget, wait: Duration) -> Result<CloseOutcome, WindowError> {
    if !alive(target) {
        return Err(WindowError::Gone);
    }
    // SAFETY: posting a message to a live top-level window of the checked process.
    let ok = unsafe { PostMessageW(hwnd(target.hwnd), WM_CLOSE, 0, 0) };
    if ok == 0 {
        return Err(WindowError::Os(std::io::Error::last_os_error()));
    }
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if !alive(target) {
            return Ok(CloseOutcome::Closed);
        }
        thread::sleep(Duration::from_millis(100));
    }
    if !alive(target) {
        return Ok(CloseOutcome::Closed);
    }
    let dialog = snapshot()
        .iter()
        .any(|w| w.owner == Some(target.hwnd) && w.visible);
    Ok(CloseOutcome::LeftOpen { dialog })
}

#[cfg(test)]
mod tests {
    use gander_core::launch::discovery::Rect;

    use super::*;

    fn w(hwnd: u64, pid: u32, owner: Option<u64>, visible: bool) -> WindowInfo {
        WindowInfo {
            hwnd,
            pid,
            owner,
            title: String::new(),
            class: String::new(),
            visible,
            cloaked: false,
            minimised: false,
            tool_window: false,
            rect: Rect {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            hosted: None,
        }
    }

    #[test]
    fn only_unowned_visible_windows_of_the_process_count() {
        let all = vec![
            w(1, 10, None, true),
            w(2, 10, Some(1), true), // a dialog of the target
            w(3, 10, None, false),   // hidden
            w(4, 11, None, true),    // another process
        ];
        assert!(other_windows(&all, 10, 1).is_empty());
        let mut more = all.clone();
        more.push(w(5, 10, None, true));
        assert_eq!(other_windows(&more, 10, 1).len(), 1);
    }
}
