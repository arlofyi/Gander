// SPDX-License-Identifier: Apache-2.0
//! Thread DPI awareness for the window calls.
//!
//! Without per-monitor v2 awareness Windows virtualises another window's rectangles for the
//! caller (640x480 reported for a real 960x720 window at 150 %, stream-spike findings), so every
//! size, position and placement in this module is read and written inside a [`DpiScope`].

use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};

/// Per-monitor v2 awareness for the current thread until dropped; the previous context is
/// restored on drop.
#[derive(Debug)]
pub(crate) struct DpiScope {
    previous: DPI_AWARENESS_CONTEXT,
}

impl DpiScope {
    /// Switches the calling thread to per-monitor v2.
    pub(crate) fn per_monitor_v2() -> Self {
        // SAFETY: plain call with a predefined context; returns the old one (null on failure).
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Self { previous }
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        if !self.previous.is_null() {
            // SAFETY: restores the context returned by the matching call on this thread.
            unsafe {
                SetThreadDpiAwarenessContext(self.previous);
            }
        }
    }
}
