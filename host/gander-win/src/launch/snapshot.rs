// SPDX-License-Identifier: Apache-2.0
//! Window and process snapshots for discovery: `EnumWindows` (z-order, top first), the owner,
//! cloak state, tool-window style, the packaged app behind an `ApplicationFrameWindow`, and the
//! `(pid, parent pid)` table from a Toolhelp snapshot.
//!
//! Reading another process's window title with `GetWindowTextW` does not send it a message
//! (the system returns the stored caption), so a hung app cannot block a snapshot.

use std::ffi::c_void;
use std::ptr;

use gander_core::launch::discovery::{
    APPLICATION_FRAME_CLASS, CORE_WINDOW_CLASS, HostedApp, Hwnd, Rect, WindowInfo,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, RECT,
};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, WS_EX_TOOLWINDOW,
};

use super::dpi::DpiScope;

/// Longest title read (characters); longer titles are cut.
const MAX_TITLE: usize = 1024;

/// The `HWND` for an integer handle.
pub(crate) fn hwnd(h: Hwnd) -> HWND {
    h as usize as HWND
}

/// The integer for an `HWND`.
pub(crate) fn hwnd_value(h: HWND) -> Hwnd {
    h as usize as Hwnd
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn collect(h: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: `lparam` is the `&mut Vec<HWND>` passed by `snapshot`, alive for the whole
    // synchronous `EnumWindows` call.
    let list = unsafe { &mut *(lparam as *mut Vec<HWND>) };
    list.push(h);
    1
}

/// All top-level windows, top of the z-order first. Coordinates are physical pixels (the calling
/// thread is made per-monitor v2 aware for the duration).
pub fn snapshot() -> Vec<WindowInfo> {
    let _dpi = DpiScope::per_monitor_v2();
    let mut handles: Vec<HWND> = Vec::with_capacity(512);
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    unsafe {
        EnumWindows(Some(collect), &mut handles as *mut Vec<HWND> as LPARAM);
    }
    handles.into_iter().filter_map(window_info).collect()
}

/// The owning process of `h`, `None` if the handle is gone.
pub(crate) fn window_pid(h: HWND) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: plain query with a valid out pointer; any handle value is accepted.
    let thread = unsafe { GetWindowThreadProcessId(h, &mut pid) };
    (thread != 0).then_some(pid)
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn title(h: HWND) -> String {
    // SAFETY: plain query.
    let len = unsafe { GetWindowTextLengthW(h) }.max(0) as usize;
    if len == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len.min(MAX_TITLE) + 1];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn cloaked(h: HWND) -> bool {
    let mut value = 0u32;
    // SAFETY: `value` is a writable u32, the size passed matches.
    let hr = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED as u32,
            (&mut value as *mut u32).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    hr >= 0 && value != 0
}

/// `GetWindowRect`, physical pixels when the thread is per-monitor v2 aware.
pub(crate) fn window_rect(h: HWND) -> Option<Rect> {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `r` is a valid out pointer.
    if unsafe { GetWindowRect(h, &mut r) } == 0 {
        return None;
    }
    Some(Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    })
}

/// `GetPackageFamilyName` of a process; `None` for unpackaged or unreadable processes.
pub(crate) fn package_family(pid: u32) -> Option<String> {
    // SAFETY: plain call; the handle is closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` characters; `process` is a valid handle.
    let rc = unsafe { GetPackageFamilyName(process, &mut len, buf.as_mut_ptr()) };
    // SAFETY: the handle came from OpenProcess and is closed once.
    unsafe {
        CloseHandle(process);
    }
    if rc != ERROR_SUCCESS || len == 0 {
        return None;
    }
    // `len` includes the terminating NUL.
    let n = (len as usize).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn hosted_app(frame: HWND) -> Option<HostedApp> {
    let class = wide(CORE_WINDOW_CLASS);
    // SAFETY: `class` is NUL terminated; a null child-after and title mean "first of the class".
    let core = unsafe { FindWindowExW(frame, ptr::null_mut(), class.as_ptr(), ptr::null()) };
    if core.is_null() {
        return None;
    }
    let pid = window_pid(core)?;
    Some(HostedApp {
        pid,
        package_family: package_family(pid),
    })
}

fn window_info(h: HWND) -> Option<WindowInfo> {
    let pid = window_pid(h)?;
    let class = class_name(h);
    // SAFETY: plain queries; any handle value is accepted.
    let (owner, visible, minimised, ex_style) = unsafe {
        (
            GetWindow(h, GW_OWNER),
            IsWindowVisible(h) != 0,
            IsIconic(h) != 0,
            GetWindowLongPtrW(h, GWL_EXSTYLE) as u32,
        )
    };
    let hosted = if class.eq_ignore_ascii_case(APPLICATION_FRAME_CLASS) {
        hosted_app(h)
    } else {
        None
    };
    Some(WindowInfo {
        hwnd: hwnd_value(h),
        pid,
        owner: (!owner.is_null()).then(|| hwnd_value(owner)),
        title: title(h),
        class,
        visible,
        cloaked: visible && cloaked(h),
        minimised,
        tool_window: ex_style & WS_EX_TOOLWINDOW != 0,
        rect: window_rect(h).unwrap_or_default(),
        hosted,
    })
}

/// One window by handle (for re-checking a chosen candidate).
pub fn window(h: Hwnd) -> Option<WindowInfo> {
    let _dpi = DpiScope::per_monitor_v2();
    window_info(hwnd(h))
}

/// `(pid, parent pid)` of every process (Toolhelp snapshot). Empty on failure.
pub fn process_parents() -> Vec<(u32, u32)> {
    // SAFETY: plain call; the handle is closed below.
    let snap: HANDLE = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE || snap.is_null() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(512);
    // SAFETY: an all-zero PROCESSENTRY32W is valid; dwSize is set before use.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    // SAFETY: `snap` is valid and `entry` is a correctly sized out structure.
    let mut ok = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while ok {
        out.push((entry.th32ProcessID, entry.th32ParentProcessID));
        // SAFETY: as above.
        ok = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    // SAFETY: the handle came from CreateToolhelp32Snapshot and is closed once.
    unsafe {
        CloseHandle(snap);
    }
    out
}
