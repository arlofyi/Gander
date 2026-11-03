// SPDX-License-Identifier: Apache-2.0
//! Time since the last keyboard or mouse input in this session (`GetLastInputInfo`).
//!
//! `GetLastInputInfo` is per session: it sees input to the session of the calling process only,
//! which is exactly what "someone is at this computer" means for a user-session application.
//! The tick counters are 32-bit milliseconds since boot, so the difference is taken with
//! wrapping arithmetic (correct while the idle time is under 49.7 days).

use std::io;
use std::time::Duration;

use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// Idle time from two 32-bit tick counts (`now` and the tick of the last input).
pub fn idle_from_ticks(now: u32, last_input: u32) -> Duration {
    Duration::from_millis(u64::from(now.wrapping_sub(last_input)))
}

/// How long since the last input in this session.
pub fn idle_duration() -> io::Result<Duration> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a valid, correctly sized LASTINPUTINFO.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: no arguments, no preconditions.
    let now = unsafe { GetTickCount() };
    Ok(idle_from_ticks(now, info.dwTime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_difference_wraps() {
        assert_eq!(idle_from_ticks(5_000, 2_000), Duration::from_secs(3));
        assert_eq!(idle_from_ticks(1_000, 1_000), Duration::ZERO);
        // The counter wrapped between the last input and now: 2 s before the wrap + 3 s after.
        assert_eq!(
            idle_from_ticks(3_000, u32::MAX - 1_999),
            Duration::from_millis(5_000)
        );
    }

    #[test]
    fn real_idle_time_is_readable() {
        let d = idle_duration().expect("GetLastInputInfo");
        eprintln!("WP-17 idle time on this machine: {d:?}");
        assert!(d < Duration::from_secs(49 * 24 * 3600));
    }
}
