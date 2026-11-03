// SPDX-License-Identifier: Apache-2.0
//! Power source (`GetSystemPowerStatus`), read only.

use std::io;

use std::sync::mpsc;
use std::thread::JoinHandle;

use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, GetSystemPowerStatus,
    SYSTEM_POWER_STATUS, SetThreadExecutionState,
};

/// `BatteryFlag` bit: no system battery.
const BATTERY_FLAG_NO_BATTERY: u8 = 128;
/// `BatteryFlag` value: status unknown.
const BATTERY_FLAG_UNKNOWN: u8 = 255;
/// `BatteryLifePercent` value: unknown.
const PERCENT_UNKNOWN: u8 = 255;

/// What the OS reports about the power source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerStatus {
    /// `Some(true)` on mains power, `Some(false)` on battery, `None` when unknown.
    pub ac_online: Option<bool>,
    /// The machine has a system battery.
    pub has_battery: bool,
    /// Charge in percent, when known.
    pub battery_percent: Option<u8>,
}

impl PowerStatus {
    /// Running on battery (`false` when the source is unknown, so nothing is throttled on a
    /// guess).
    pub fn on_battery(&self) -> bool {
        self.ac_online == Some(false)
    }

    /// Builds the status from the raw `SYSTEM_POWER_STATUS` fields.
    pub fn from_raw(ac_line_status: u8, battery_flag: u8, battery_life_percent: u8) -> Self {
        Self {
            ac_online: match ac_line_status {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            },
            has_battery: battery_flag != BATTERY_FLAG_UNKNOWN
                && battery_flag & BATTERY_FLAG_NO_BATTERY == 0,
            battery_percent: (battery_life_percent != PERCENT_UNKNOWN
                && battery_life_percent <= 100)
                .then_some(battery_life_percent),
        }
    }
}

/// Reads the power status.
pub fn power_status() -> io::Result<PowerStatus> {
    let mut raw = SYSTEM_POWER_STATUS {
        ACLineStatus: 0,
        BatteryFlag: 0,
        BatteryLifePercent: 0,
        SystemStatusFlag: 0,
        BatteryLifeTime: 0,
        BatteryFullLifeTime: 0,
    };
    // SAFETY: `raw` is a valid out-parameter.
    if unsafe { GetSystemPowerStatus(&mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PowerStatus::from_raw(
        raw.ACLineStatus,
        raw.BatteryFlag,
        raw.BatteryLifePercent,
    ))
}

/// Keeps the computer and the display awake while alive (Stream sessions, WP-53):
/// `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)` on a
/// thread of its own, cleared with `ES_CONTINUOUS` alone when dropped. The execution state
/// belongs to the calling thread, so a dedicated thread holds it for exactly the guard's life.
/// Power settings are never changed.
#[derive(Debug)]
pub struct KeepAwake {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl KeepAwake {
    /// Starts holding the system and display awake.
    pub fn start() -> io::Result<Self> {
        let (stop, rx) = mpsc::channel::<()>();
        let (ready_tx, ready) = mpsc::channel::<bool>();
        let thread = std::thread::Builder::new()
            .name("gander-keep-awake".to_owned())
            .spawn(move || {
                // SAFETY: plain call; the flags are documented constants.
                let previous = unsafe {
                    SetThreadExecutionState(
                        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
                    )
                };
                let _ = ready_tx.send(previous != 0);
                // Until the guard is dropped (or its sender is gone).
                let _ = rx.recv();
                // SAFETY: as above; ES_CONTINUOUS alone clears this thread's requirement.
                unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
            })?;
        let ok = ready.recv().unwrap_or(false);
        let guard = Self {
            stop: Some(stop),
            thread: Some(thread),
        };
        if ok {
            Ok(guard)
        } else {
            drop(guard);
            Err(io::Error::other("SetThreadExecutionState failed"))
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn keep_awake_starts_and_clears() {
        let guard = super::KeepAwake::start().expect("keep awake");
        drop(guard);
        // A second guard after the first is gone works the same (nothing left set).
        drop(super::KeepAwake::start().expect("again"));
    }

    use super::*;

    #[test]
    fn raw_values_map_as_documented() {
        let on_ac = PowerStatus::from_raw(1, 8, 100);
        assert_eq!(on_ac.ac_online, Some(true));
        assert!(!on_ac.on_battery());
        assert!(on_ac.has_battery);
        assert_eq!(on_ac.battery_percent, Some(100));

        let on_battery = PowerStatus::from_raw(0, 1, 63);
        assert!(on_battery.on_battery());
        assert_eq!(on_battery.battery_percent, Some(63));

        let desktop = PowerStatus::from_raw(1, 128, 255);
        assert!(!desktop.has_battery);
        assert_eq!(desktop.battery_percent, None);

        let unknown = PowerStatus::from_raw(255, 255, 255);
        assert_eq!(unknown.ac_online, None);
        assert!(!unknown.on_battery(), "unknown source is not 'on battery'");
        assert!(!unknown.has_battery);
    }

    #[test]
    fn real_status_is_readable() {
        let s = power_status().expect("GetSystemPowerStatus");
        eprintln!(
            "WP-17 power status on this machine: {s:?} on_battery={}",
            s.on_battery()
        );
    }
}
