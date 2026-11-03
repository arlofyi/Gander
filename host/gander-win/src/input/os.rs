// SPDX-License-Identifier: Apache-2.0
//! The Win32 calls behind the injector: live geometry, the pre-click check, foreground queries,
//! `GetLastInputInfo` ticks and `SendInput`. Every coordinate is physical: callers hold a
//! per-monitor v2 [`DpiScope`](crate::launch::dpi::DpiScope) around geometry and hit tests.

use gander_core::input::mapping::{Point, WindowGeometry};
use gander_core::launch::discovery::{Hwnd, Rect};
use windows_sys::Win32::Foundation::{GetLastError, HWND, POINT, RECT, SetLastError};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    LASTINPUTINFO, MOUSE_EVENT_FLAGS, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GA_ROOTOWNER, GetAncestor, GetClassNameW, GetClientRect, GetCursorPos,
    GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId, IsHungAppWindow, IsWindow,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos,
    WindowFromPoint,
};

use gander_core::input::Refusal;

/// `dwExtraInfo` of every event Gander injects ("GNDR"), so a future low-level hook or a
/// diagnostic tool can tell them apart.
pub const GANDER_EXTRA_INFO: usize = 0x474E_4452;

/// The `HWND` of an integer handle.
pub(crate) fn hwnd(h: Hwnd) -> HWND {
    h as usize as HWND
}

/// `(thread, pid)` of a window, `None` when the handle is gone.
pub(crate) fn thread_and_pid(h: HWND) -> Option<(u32, u32)> {
    let mut pid = 0u32;
    // SAFETY: plain query with a valid out pointer; any handle value is accepted.
    let thread = unsafe { GetWindowThreadProcessId(h, &mut pid) };
    (thread != 0).then_some((thread, pid))
}

/// The class name of a window.
pub(crate) fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: the buffer and its length match.
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Checks that `h` still exists, still belongs to `pid` (handles are reused) and answers.
pub(crate) fn live(h: HWND, pid: u32) -> Result<(), Refusal> {
    // SAFETY: plain queries; any handle value is accepted.
    if unsafe { IsWindow(h) } == 0 || thread_and_pid(h).map(|t| t.1) != Some(pid) {
        return Err(Refusal::WindowGone);
    }
    // SAFETY: as above.
    if unsafe { IsHungAppWindow(h) } != 0 {
        return Err(Refusal::NotResponding);
    }
    Ok(())
}

fn rect(r: RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

/// The virtual desktop (bounding box of all monitors), physical inside a per-monitor v2 scope.
pub(crate) fn virtual_desktop() -> Rect {
    // SAFETY: plain getters.
    unsafe {
        Rect::from_xywh(
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

fn client_to_screen(h: HWND, x: i32, y: i32) -> Option<Point> {
    let mut p = POINT { x, y };
    // SAFETY: `p` is a valid in/out point.
    (unsafe { ClientToScreen(h, &mut p) } != 0).then_some(Point::new(p.x, p.y))
}

/// The live geometry of `h`: DWM visible frame bounds and the client area, both on the screen.
///
/// Both client corners go through `ClientToScreen`, so a window that is not per-monitor aware
/// (bitmap-scaled by the system, its client rectangle in logical units) still yields its physical
/// client rectangle.
pub(crate) fn geometry(h: HWND) -> Option<WindowGeometry> {
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the out buffer is a RECT of the size passed.
    let hr = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut frame as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if hr < 0 {
        return None;
    }
    let mut c = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: valid out pointer.
    if unsafe { GetClientRect(h, &mut c) } == 0 {
        return None;
    }
    let tl = client_to_screen(h, 0, 0)?;
    let br = client_to_screen(h, c.right, c.bottom)?;
    // SAFETY: plain query.
    let dpi = unsafe { GetDpiForWindow(h) };
    Some(WindowGeometry {
        frame: rect(frame),
        client: Rect {
            left: tl.x,
            top: tl.y,
            right: br.x,
            bottom: br.y,
        },
        dpi,
        desktop: virtual_desktop(),
    })
}

/// What a real click at `p` would hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Hit {
    /// The target, a window it owns, or its popup menu.
    Target,
    /// Another top-level window.
    Other {
        /// Its process.
        pid: u32,
        /// Its class.
        class: String,
    },
    /// Nothing (off every monitor).
    Nothing,
}

/// Class of the system popup menu window.
const MENU_CLASS: &str = "#32768";

/// The pre-click check (stream-spike WP-44): `WindowFromPoint` + `GetAncestor(GA_ROOT)` must be
/// the target, a window the target owns (`GA_ROOTOWNER`), or a popup menu of the target's thread.
pub(crate) fn hit_test(target: HWND, p: Point) -> Hit {
    // SAFETY: plain query.
    let h = unsafe { WindowFromPoint(POINT { x: p.x, y: p.y }) };
    if h.is_null() {
        return Hit::Nothing;
    }
    // SAFETY: plain queries.
    let root = match unsafe { GetAncestor(h, GA_ROOT) } {
        r if r.is_null() => h,
        r => r,
    };
    // SAFETY: plain query.
    let owner = unsafe { GetAncestor(root, GA_ROOTOWNER) };
    if root == target || owner == target {
        return Hit::Target;
    }
    let under = thread_and_pid(root);
    let class = class_of(root);
    if class == MENU_CLASS
        && under.is_some()
        && under.map(|t| t.0) == thread_and_pid(target).map(|t| t.0)
    {
        return Hit::Target;
    }
    Hit::Other {
        pid: under.map_or(0, |t| t.1),
        class,
    }
}

/// The foreground window.
pub(crate) fn foreground() -> HWND {
    // SAFETY: plain getter.
    unsafe { GetForegroundWindow() }
}

/// The foreground window is `target` or a window `target` owns (a dialog of the app).
pub(crate) fn is_foreground(target: HWND) -> bool {
    let fg = foreground();
    if fg.is_null() {
        return false;
    }
    // SAFETY: plain query.
    fg == target || unsafe { GetAncestor(fg, GA_ROOTOWNER) } == target
}

/// `GetTickCount`.
pub(crate) fn tick() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetTickCount() }
}

/// `GetLastInputInfo().dwTime`.
pub(crate) fn last_input_tick() -> Option<u32> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: correctly sized structure.
    (unsafe { GetLastInputInfo(&mut info) } != 0).then_some(info.dwTime)
}

/// The cursor position.
pub fn cursor() -> Option<Point> {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: valid out pointer.
    (unsafe { GetCursorPos(&mut p) } != 0).then_some(Point::new(p.x, p.y))
}

/// Puts the cursor at `p` (tests restore the cursor with it).
pub fn set_cursor(p: Point) -> bool {
    // SAFETY: plain call.
    unsafe { SetCursorPos(p.x, p.y) != 0 }
}

/// A mouse event.
pub(crate) fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: GANDER_EXTRA_INFO,
            },
        },
    }
}

/// A keyboard event.
pub(crate) fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: GANDER_EXTRA_INFO,
            },
        },
    }
}

/// Result of one `SendInput` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sent {
    /// Events accepted.
    pub accepted: u32,
    /// `GetLastError` when fewer than all were accepted.
    pub error: u32,
    /// Ticks before and after the call (for the injection log).
    pub ticks: (u32, u32),
}

/// `SendInput` of `events` as one uninterruptible batch.
pub(crate) fn send(events: &[INPUT]) -> Sent {
    let start = tick();
    // SAFETY: a valid slice of initialised INPUT structures and the matching size.
    let (accepted, error) = unsafe {
        SetLastError(0);
        let n = SendInput(
            events.len() as u32,
            events.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
        (
            n,
            if n as usize == events.len() {
                0
            } else {
                GetLastError()
            },
        )
    };
    Sent {
        accepted,
        error,
        ticks: (start, tick()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gone_window_is_refused() {
        // Handle values are multiples of 2; this one is never a live window.
        assert_eq!(live(hwnd(0x7FFF_FFF0), 1), Err(Refusal::WindowGone));
    }

    #[test]
    fn event_constructors_carry_the_marker() {
        let m = mouse(1, 2, 3, -120);
        // SAFETY: the union was written as `mi` just above.
        let mi = unsafe { m.Anonymous.mi };
        assert_eq!(mi.dwExtraInfo, GANDER_EXTRA_INFO);
        assert_eq!(mi.mouseData as i32, -120);
        let k = key(0x41, 0x1E, 8);
        // SAFETY: written as `ki`.
        let ki = unsafe { k.Anonymous.ki };
        assert_eq!((ki.wVk, ki.wScan, ki.dwFlags), (0x41, 0x1E, 8));
    }
}
