// SPDX-License-Identifier: Apache-2.0
// Moved from the stream spike (spikes/stream/capture, spikes/stream/encode) as production code.
//! The QueryPerformanceCounter clock in 100 ns units, the time base of
//! `Direct3D11CaptureFrame::SystemRelativeTime` (and of Media Foundation sample times).

use std::sync::OnceLock;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

fn frequency() -> i64 {
    static FREQ: OnceLock<i64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer; the call cannot fail on XP and later.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// Now, in 100 ns units of the QPC clock.
pub fn now_100ns() -> i64 {
    let mut c = 0i64;
    // SAFETY: valid out pointer; the call cannot fail on XP and later.
    let _ = unsafe { QueryPerformanceCounter(&mut c) };
    ((c as i128) * 10_000_000 / frequency() as i128) as i64
}
