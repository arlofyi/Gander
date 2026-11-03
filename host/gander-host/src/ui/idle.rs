// SPDX-License-Identifier: Apache-2.0
//! The tray-only message loop: waits for a message to the main thread (the tray icon's window
//! lives there) or a timeout, then dispatches everything queued. No winit, no window of our own.
//!
//! Between eframe sessions the thread also owns winit's hidden helper window; its messages are
//! dispatched here too, and winit buffers what arrives while no session runs.

use std::time::Duration;

/// Waits up to `timeout` for a message to this thread, then dispatches every queued message.
#[cfg(windows)]
pub fn pump(timeout: Duration) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
        PeekMessageW, QS_ALLINPUT, TranslateMessage,
    };

    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    // SAFETY: no handles are passed (count 0, null array); the call only blocks this thread until
    // a message arrives or the timeout elapses.
    unsafe {
        MsgWaitForMultipleObjectsEx(0, std::ptr::null(), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
    };
    // SAFETY: MSG is plain data; all-zero is a valid initial value.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // SAFETY: `msg` is a valid, writable MSG; a null window means every message of this thread.
    while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        // WM_QUIT is not expected here (nothing posts it); it is dispatched like any other message
        // and the application's own Quit path stays the only way out.
        // SAFETY: `msg` was just filled by PeekMessageW.
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// There is no tray on this platform yet: just waits.
#[cfg(not(windows))]
pub fn pump(timeout: Duration) {
    std::thread::sleep(timeout);
}

/// Gives the pages a session touched back to the system once the session is gone (the GL driver
/// and egui leave a few tens of MiB resident that nothing uses while only the tray shows). They
/// are faulted back in if needed; private bytes are not changed by this.
#[cfg(windows)]
pub fn trim_working_set() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    // SAFETY: the pseudo-handle of this process; (usize::MAX, usize::MAX) asks the system to
    // remove as many pages as possible from the working set.
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

/// Nothing to do on other platforms.
#[cfg(not(windows))]
pub fn trim_working_set() {}
