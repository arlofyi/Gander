// SPDX-License-Identifier: Apache-2.0
//! Fit-to-phone and restore: `GetWindowPlacement` before the first change, `SW_RESTORE` when
//! minimised or maximised (window capture gets no frames from a minimised window), `SetWindowPos`
//! to the planned size, and `SetWindowPlacement` at the end of the session.

use std::ptr;

use gander_core::launch::discovery::Rect;
use gander_core::launch::fit::{FitPlan, Size, WindowGeometry, plan_fit};
use gander_core::protocol::stream::Viewport;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowPlacement, IsHungAppWindow, IsIconic, IsWindow, IsZoomed, SW_RESTORE,
    SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetWindowPlacement, SetWindowPos, ShowWindow,
    WINDOWPLACEMENT,
};

use super::dpi::DpiScope;
use super::snapshot::{hwnd, window_pid, window_rect};
use super::{FitResult, SavedPlacement, WindowError, WindowTarget};

/// The live handle of `target`, after checking it still exists, still belongs to the same
/// process (handles are reused) and answers messages.
fn live(target: &WindowTarget) -> Result<HWND, WindowError> {
    let h = hwnd(target.hwnd);
    // SAFETY: plain query; any handle value is accepted.
    if unsafe { IsWindow(h) } == 0 || window_pid(h) != Some(target.pid) {
        return Err(WindowError::Gone);
    }
    // SAFETY: as above.
    if unsafe { IsHungAppWindow(h) } != 0 {
        return Err(WindowError::NotResponding);
    }
    Ok(h)
}

fn client_size(h: HWND) -> Option<Size> {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: `r` is a valid out pointer.
    if unsafe { GetClientRect(h, &mut r) } == 0 {
        return None;
    }
    Some(Size::new(
        (r.right - r.left).max(0) as u32,
        (r.bottom - r.top).max(0) as u32,
    ))
}

fn work_area(h: HWND) -> Option<Rect> {
    // SAFETY: plain query; MONITOR_DEFAULTTONEAREST always returns a monitor.
    let monitor = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) };
    // SAFETY: all-zero MONITORINFO is valid; cbSize is set before the call.
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    // SAFETY: `info` is a correctly sized out structure.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let r = info.rcWork;
    Some(Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    })
}

fn get_placement(h: HWND) -> Option<SavedPlacement> {
    // SAFETY: all-zero WINDOWPLACEMENT is valid; length is set before the call.
    let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
    wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
    // SAFETY: `wp` is a correctly sized out structure.
    if unsafe { GetWindowPlacement(h, &mut wp) } == 0 {
        return None;
    }
    let r = wp.rcNormalPosition;
    Some(SavedPlacement {
        flags: wp.flags,
        show_cmd: wp.showCmd,
        min_position: (wp.ptMinPosition.x, wp.ptMinPosition.y),
        max_position: (wp.ptMaxPosition.x, wp.ptMaxPosition.y),
        normal: Rect {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        },
    })
}

/// The geometry the fit plan needs, read inside a per-monitor v2 scope.
fn geometry(h: HWND, dpi: Option<u32>) -> Result<WindowGeometry, WindowError> {
    let outer = window_rect(h).ok_or(WindowError::Gone)?;
    let client = client_size(h).ok_or(WindowError::Gone)?;
    let work_area = work_area(h).ok_or(WindowError::Gone)?;
    // SAFETY: plain query.
    let dpi = dpi.unwrap_or_else(|| unsafe { GetDpiForWindow(h) });
    Ok(WindowGeometry {
        outer,
        client,
        dpi,
        work_area,
    })
}

/// Resizes `target` for `viewport` (see [`gander_core::launch::fit`]). The original placement is
/// saved on the first call, so [`restore`] returns the window to where the owner had it. `dpi`
/// overrides the window's monitor dpi (tests); `None` reads it.
pub fn fit_to_phone(
    target: &mut WindowTarget,
    viewport: Viewport,
    dpi: Option<u32>,
) -> Result<FitResult, WindowError> {
    let _scope = DpiScope::per_monitor_v2();
    let h = live(target)?;
    if target.saved.is_none() {
        target.saved = Some(get_placement(h).ok_or(WindowError::Gone)?);
    }
    // SAFETY: plain queries and a show call on a live window.
    unsafe {
        if IsIconic(h) != 0 || IsZoomed(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
    }
    let geometry = geometry(h, dpi)?;
    let plan: FitPlan = plan_fit(viewport, &geometry).map_err(WindowError::Fit)?;
    let o = plan.outer;
    // SAFETY: plain call on a live window. Synchronous: Windows applies the app's
    // WM_GETMINMAXINFO limits while handling it, so the size read back below is the real one.
    let ok = unsafe {
        SetWindowPos(
            h,
            ptr::null_mut(),
            o.left,
            o.top,
            o.width(),
            o.height(),
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    };
    if ok == 0 {
        return Err(WindowError::Os(std::io::Error::last_os_error()));
    }
    let result = FitResult {
        plan,
        dpi: geometry.dpi,
        client: client_size(h).ok_or(WindowError::Gone)?,
        outer: window_rect(h).ok_or(WindowError::Gone)?,
    };
    target.fit = Some(result);
    Ok(result)
}

/// Puts the window back where it was before the first [`fit_to_phone`] (same normal rectangle,
/// same minimised or maximised state), without activating it. A target never fitted is left
/// alone. Gone windows are not an error worth more than reporting: the caller ends the session.
pub fn restore(target: &mut WindowTarget) -> Result<(), WindowError> {
    let Some(saved) = target.saved else {
        return Ok(());
    };
    let _scope = DpiScope::per_monitor_v2();
    let h = live(target)?;
    let show_cmd = match saved.show_cmd as i32 {
        x if x == SW_SHOWMINIMIZED => SW_SHOWMINNOACTIVE,
        x if x == SW_SHOWMAXIMIZED => SW_SHOWMAXIMIZED,
        x if x == SW_SHOWNORMAL => SW_SHOWNOACTIVATE,
        other => other,
    };
    let wp = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        flags: saved.flags,
        showCmd: show_cmd as u32,
        ptMinPosition: POINT {
            x: saved.min_position.0,
            y: saved.min_position.1,
        },
        ptMaxPosition: POINT {
            x: saved.max_position.0,
            y: saved.max_position.1,
        },
        rcNormalPosition: RECT {
            left: saved.normal.left,
            top: saved.normal.top,
            right: saved.normal.right,
            bottom: saved.normal.bottom,
        },
    };
    // SAFETY: `wp` is fully initialised; plain call on a live window.
    if unsafe { SetWindowPlacement(h, &wp) } == 0 {
        return Err(WindowError::Os(std::io::Error::last_os_error()));
    }
    target.saved = None;
    target.fit = None;
    Ok(())
}

/// The window's current placement (tests compare it before fit and after restore).
pub fn placement(target: &WindowTarget) -> Result<SavedPlacement, WindowError> {
    let _scope = DpiScope::per_monitor_v2();
    let h = live(target)?;
    get_placement(h).ok_or(WindowError::Gone)
}
