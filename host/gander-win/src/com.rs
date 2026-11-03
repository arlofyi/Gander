// SPDX-License-Identifier: Apache-2.0
//! COM apartment for shell association queries.
//!
//! `AssocQueryStringW` is implemented on top of COM: for a packaged handler (Photos, packaged
//! Notepad on Windows 11) it activates WinRT objects through `RoGetActivationFactory`. Shell APIs
//! require the calling thread to have initialised COM. Calling it from a thread that never did
//! (a plain `std::thread`, a tokio worker, a libtest thread) puts the call in the process's
//! *implicit* MTA, which exists only while some other thread holds an explicit MTA reference.
//! When that last reference goes away the MTA is torn down and COM unloads the in-process
//! servers it loaded, while the shell still holds objects from them. The next association
//! query then calls into an unloaded DLL: `STATUS_ACCESS_VIOLATION` with an execute fault at an
//! address that belongs to no module (measured 2026-11-02, Windows 11 build 26200, in the
//! `gander-win` lib tests, see `CHANGELOG.md`).
//!
//! [`ComScope`] fixes both halves: it keeps the MTA alive for the life of the process
//! (`CoIncrementMTAUsage`, taken once and never released: the MTA is process-global state and
//! releasing it is exactly what unloads the servers) and it joins the calling thread to the MTA
//! for the duration of the call, balanced with `CoUninitialize`. A thread that is already in an
//! STA keeps its apartment (`RPC_E_CHANGED_MODE`), which is also valid for shell calls.

use std::sync::OnceLock;

use windows_sys::Win32::System::Com::{
    CO_MTA_USAGE_COOKIE, COINIT_MULTITHREADED, CoIncrementMTAUsage, CoInitializeEx, CoUninitialize,
};

/// Whether the process-wide MTA usage reference was taken.
static MTA_PINNED: OnceLock<bool> = OnceLock::new();

/// Keeps the process's multithreaded apartment alive until the process exits. Idempotent.
fn pin_mta() {
    MTA_PINNED.get_or_init(|| {
        let mut cookie: CO_MTA_USAGE_COOKIE = std::ptr::null_mut();
        // SAFETY: valid out-pointer. The cookie is deliberately never passed to
        // `CoDecrementMTAUsage`: the reference lives as long as the process.
        unsafe { CoIncrementMTAUsage(&mut cookie) >= 0 }
    });
}

/// The calling thread is in a COM apartment while this value lives. Not `Send`: COM
/// initialisation is per thread and must be undone on the thread that did it.
pub(crate) struct ComScope {
    uninit: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComScope {
    /// Pins the process MTA and joins the calling thread to it (or keeps its STA).
    pub(crate) fn enter() -> Self {
        pin_mta();
        // SAFETY: plain per-thread initialisation; balanced in `Drop` when it succeeded
        // (`S_OK` or `S_FALSE`). `RPC_E_CHANGED_MODE` (negative) means the thread is already an
        // STA, which must not be uninitialised here.
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
        Self {
            uninit: hr >= 0,
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.uninit {
            // SAFETY: balances the successful `CoInitializeEx` of `enter`, on the same thread
            // (the type is not `Send`).
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_nest_and_balance() {
        let outer = ComScope::enter();
        let inner = ComScope::enter();
        assert!(
            outer.uninit && inner.uninit,
            "MTA join must succeed on a fresh thread"
        );
        drop(inner);
        drop(outer);
        assert_eq!(MTA_PINNED.get(), Some(&true));
    }
}
