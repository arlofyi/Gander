// SPDX-License-Identifier: Apache-2.0
//! What the engine needs from the rest of the host, as traits so tests can fake them.
//!
//! | Trait | Production (Windows) | Test |
//! | --- | --- | --- |
//! | [`LockSource`] | [`crate::win::SessionLock`] over `gander_win::session::watch` | [`FakeLock`] |
//! | [`PowerSource`] | [`crate::win::SystemPower`] over `gander_win::session::power` | [`FixedPower`] |
//! | [`BindAddrs`] | `gander_host::stream::ServerAddrs` (the listener plan) | [`StaticAddrs`] |
//! | [`SignalSink`] | `gander_host::stream::EventsSink` (`Events::send_to`) | `testing::CollectingSink` |
//! | [`crate::source::VideoSource`] | [`crate::win::WgcSource`] | `testing::SyntheticSource` |

use std::fmt;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use gander_core::protocol::DeviceId;
use gander_core::protocol::events::ServerEvent;
use tokio::sync::watch;

/// The lock state as a lock source publishes it.
///
/// `locks` counts transitions into the locked state, so a lock that is undone before a receiver
/// looks (a watch channel keeps only the newest value) is still seen: a running session ends when
/// `locks` moved since it started, even if `locked` is false again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LockStatus {
    /// Locked now. An unknown state must be reported as locked.
    pub locked: bool,
    /// Transitions into the locked state so far.
    pub locks: u64,
}

impl LockStatus {
    /// Applies a new observation (counts an unlocked -> locked transition).
    pub fn observe(&mut self, locked: bool) {
        if locked && !self.locked {
            self.locks += 1;
        }
        self.locked = locked;
    }
}

/// The session lock state (fail closed: Windows Graphics Capture keeps capturing while the
/// session is locked, so this is the only guard). A receiver whose sender is gone counts as
/// locked.
pub trait LockSource: Send + Sync + fmt::Debug + 'static {
    /// A receiver that always holds the current state and wakes on every change.
    fn subscribe(&self) -> watch::Receiver<LockStatus>;
}

/// A lock source the test drives.
#[derive(Debug)]
pub struct FakeLock {
    tx: watch::Sender<LockStatus>,
}

impl FakeLock {
    /// Starts in `locked`.
    pub fn new(locked: bool) -> Self {
        Self {
            tx: watch::Sender::new(LockStatus {
                locked,
                locks: u64::from(locked),
            }),
        }
    }

    /// Simulates a lock or unlock event.
    pub fn set_locked(&self, locked: bool) {
        self.tx.send_modify(|s| s.observe(locked));
    }
}

impl LockSource for FakeLock {
    fn subscribe(&self) -> watch::Receiver<LockStatus> {
        self.tx.subscribe()
    }
}

/// The power source, for the battery bitrate cap.
pub trait PowerSource: Send + Sync + fmt::Debug + 'static {
    /// Running on battery (`false` when unknown).
    fn on_battery(&self) -> bool;
}

/// A power source the test drives.
#[derive(Debug, Default)]
pub struct FixedPower(AtomicBool);

impl FixedPower {
    /// On battery or not.
    pub fn new(on_battery: bool) -> Self {
        Self(AtomicBool::new(on_battery))
    }
    /// Changes it.
    pub fn set_on_battery(&self, v: bool) {
        self.0.store(v, Ordering::Relaxed);
    }
}

impl PowerSource for FixedPower {
    fn on_battery(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// The addresses ICE may bind: the addresses the host's listener serves (loopback plus the
/// private-network address), never a wildcard.
pub trait BindAddrs: Send + Sync + fmt::Debug + 'static {
    /// The IPs, read at the start of each session.
    fn ice_ips(&self) -> Vec<IpAddr>;
}

/// A fixed address list.
#[derive(Debug, Clone)]
pub struct StaticAddrs(pub Vec<IpAddr>);

impl BindAddrs for StaticAddrs {
    fn ice_ips(&self) -> Vec<IpAddr> {
        self.0.clone()
    }
}

/// Pushes signalling (`stream.offer`, `stream.ice`, `stream.state`, `error`) to one device's
/// `/v1/events` sockets.
pub trait SignalSink: Send + Sync + fmt::Debug + 'static {
    /// Sends `event` to every open socket of `device`; dropped when there is none.
    fn send(&self, device: &DeviceId, event: ServerEvent);
}
