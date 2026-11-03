// SPDX-License-Identifier: Apache-2.0
//! Mapping a normalised tap onto a window and injecting it two ways (WP-44, matrix items C19 and
//! C20): `SendInput` (the real mouse, subject to focus, z-order and the input desktop) and
//! `PostMessage` (straight into the window's queue).
//!
//! Only `gander-testwin` windows are ever clicked: `SendInput` is refused unless the top-level
//! window under the point is the target itself (production policy) or, for the occlusion
//! experiment, another test window of the same class.

use anyhow::{Context, Result, ensure};
use spike_capture::wgc::WindowGeometry;
use windows::Win32::Foundation::{
    GetLastError, HWND, LPARAM, POINT, SetLastError, WIN32_ERROR, WPARAM,
};
use windows::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK, WTSFreeMemory, WTSINFOEXW,
    WTSQuerySessionInformationW, WTSSessionInfoEx,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, OpenInputDesktop,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow, GetSystemMetrics,
    GetWindowThreadProcessId, PostMessageW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SetCursorPos, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WindowFromPoint,
};

/// A tap mapped onto a window, in physical pixels (the process is per-monitor DPI aware).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mapped {
    /// Point on the virtual screen.
    pub screen: POINT,
    /// The same point in the window's client area.
    pub client: POINT,
    /// Size of the captured frame the tap was normalised to (DWM visible frame bounds).
    pub frame: (i32, i32),
    /// Client size.
    pub client_size: (i32, i32),
    /// `GetDpiForWindow`.
    pub dpi: u32,
}

/// Maps a point normalised to the streamed picture (0..1, origin top-left) onto the window.
///
/// The streamed picture is the WGC frame of the window (its visible frame bounds, title bar and
/// borders included) scaled to the encode size, so the normalised point is first expanded to the
/// frame, then offset by the frame's screen position. The client point is the screen point minus
/// the client origin; it can be negative or outside the client when the tap hit the title bar.
pub fn map_tap(hwnd: HWND, nx: f64, ny: f64) -> Result<Mapped> {
    let g = WindowGeometry::of(hwnd)?;
    let (fw, fh) = g.frame_size();
    ensure!(fw > 0 && fh > 0, "window has no visible frame");
    let fx = ((nx.clamp(0.0, 1.0) * f64::from(fw)).floor() as i32).clamp(0, fw - 1);
    let fy = ((ny.clamp(0.0, 1.0) * f64::from(fh)).floor() as i32).clamp(0, fh - 1);
    let screen = POINT {
        x: g.frame.left + fx,
        y: g.frame.top + fy,
    };
    let client = POINT {
        x: screen.x - g.client_origin.x,
        y: screen.y - g.client_origin.y,
    };
    Ok(Mapped {
        screen,
        client,
        frame: (fw, fh),
        client_size: (g.client_w, g.client_h),
        dpi: g.dpi,
    })
}

/// The inverse of [`map_tap`]: the normalised picture point of a client pixel (for the tests).
pub fn normalise_client(hwnd: HWND, cx: i32, cy: i32) -> Result<(f64, f64)> {
    let g = WindowGeometry::of(hwnd)?;
    let (fw, fh) = g.frame_size();
    let (ox, oy) = g.client_offset();
    Ok((
        (f64::from(cx + ox) + 0.5) / f64::from(fw),
        (f64::from(cy + oy) + 0.5) / f64::from(fh),
    ))
}

/// What a `PostMessage` injection did.
#[derive(Debug, Clone)]
pub struct PostOutcome {
    /// Messages the system accepted into the queue (of 3: move, down, up).
    pub posted: u32,
    /// First failure, as a Win32 error code.
    pub error: Option<u32>,
}

fn lparam(x: i32, y: i32) -> LPARAM {
    LPARAM((((y & 0xFFFF) as isize) << 16) | ((x & 0xFFFF) as isize))
}

/// Posts `WM_MOUSEMOVE`, `WM_LBUTTONDOWN`, `WM_LBUTTONUP` at a client point, to the window
/// itself: no focus, no z-order, no input desktop involved. Coordinates are physical client
/// pixels (the target is per-monitor DPI aware v2, as is this process).
pub fn post_click(hwnd: HWND, client: POINT) -> PostOutcome {
    const MK_LBUTTON: usize = 1;
    let lp = lparam(client.x, client.y);
    let mut out = PostOutcome {
        posted: 0,
        error: None,
    };
    for (msg, wp) in [
        (WM_MOUSEMOVE, 0usize),
        (WM_LBUTTONDOWN, MK_LBUTTON),
        (WM_LBUTTONUP, 0usize),
    ] {
        // SAFETY: plain call on a window handle; an invalid handle returns an error.
        match unsafe { PostMessageW(Some(hwnd), msg, WPARAM(wp), lp) } {
            Ok(()) => out.posted += 1,
            Err(e) => {
                out.error.get_or_insert((e.code().0 as u32) & 0xFFFF);
            }
        }
    }
    out
}

/// Which top-level windows `SendInput` may click through to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Only the target window itself (production: a covered window pauses input).
    TargetOnly,
    /// Any `gander-testwin` window (the occlusion experiment lands on a second test window).
    AnyTestwin,
}

/// The verdict of the pre-click check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// The top-level window under the point is acceptable.
    Ok,
    /// Another window covers the point.
    Covered {
        /// The covering top-level window (HWND value).
        by: isize,
        /// Its class name.
        class: String,
        /// Its process id.
        pid: u32,
    },
    /// No window under the point.
    NoWindow,
}

/// Class name of a window.
pub fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: valid buffer.
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn pid_of(h: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out pointer.
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// The top-level window under a screen point (what a real click would hit).
pub fn root_under(p: POINT) -> Option<HWND> {
    // SAFETY: plain getters.
    let h = unsafe { WindowFromPoint(p) };
    if h.is_invalid() {
        return None;
    }
    // SAFETY: plain getter.
    let r = unsafe { GetAncestor(h, GA_ROOT) };
    Some(if r.is_invalid() { h } else { r })
}

/// Checks what a click at `p` would hit, without clicking.
pub fn guard(target: HWND, p: POINT, policy: Policy) -> Guard {
    let Some(root) = root_under(p) else {
        return Guard::NoWindow;
    };
    let ok = match policy {
        Policy::TargetOnly => root == target,
        Policy::AnyTestwin => root == target || class_of(root) == class_of(target),
    };
    if ok {
        Guard::Ok
    } else {
        Guard::Covered {
            by: root.0 as isize,
            class: class_of(root),
            pid: pid_of(root),
        }
    }
}

/// What a `SendInput` injection did.
#[derive(Debug, Clone)]
pub struct SendOutcome {
    /// The pre-click verdict; `SendInput` is only called for [`Guard::Ok`].
    pub guard: Guard,
    /// Events the system accepted (of 3: move, down, up); `None` when refused by the guard.
    pub accepted: Option<u32>,
    /// `GetLastError` when fewer than 3 were accepted.
    pub error: Option<u32>,
    /// Cursor position before the call (`None` if it could not be read).
    pub cursor_before: Option<POINT>,
    /// Cursor position right after the call, before it was restored.
    pub cursor_after: Option<POINT>,
}

fn cursor() -> Option<POINT> {
    let mut p = POINT::default();
    // SAFETY: valid out pointer.
    unsafe { GetCursorPos(&mut p) }.ok().map(|()| p)
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Normalised absolute coordinate (0..65535 over the virtual desktop) of the centre of pixel
/// `p` on an axis that starts at `origin` and is `len` pixels long. Windows maps an absolute
/// coordinate `a` back to pixel `a * len / 65536`, so the centre of pixel `p` is
/// `((2 * (p - origin) + 1) * 65536) / (2 * len)`.
pub fn absolute(p: i32, origin: i32, len: i32) -> i32 {
    let len = i64::from(len.max(1));
    (((2 * i64::from(p - origin) + 1) * 65536) / (2 * len)) as i32
}

/// Moves the real cursor onto `screen` and left-clicks, then puts the cursor back.
///
/// Refused (nothing is sent) unless the top-level window under the point is acceptable under
/// `policy`: a click that would land in another application must pause, not be delivered.
pub fn send_click(target: HWND, screen: POINT, policy: Policy) -> SendOutcome {
    let verdict = guard(target, screen, policy);
    let before = cursor();
    if verdict != Guard::Ok {
        return SendOutcome {
            guard: verdict,
            accepted: None,
            error: None,
            cursor_before: before,
            cursor_after: None,
        };
    }
    // SAFETY: plain getters.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    let ax = absolute(screen.x, vx, vw);
    let ay = absolute(screen.y, vy, vh);
    let inputs = [
        mouse(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ax,
            ay,
        ),
        mouse(MOUSEEVENTF_LEFTDOWN, 0, 0),
        mouse(MOUSEEVENTF_LEFTUP, 0, 0),
    ];
    // SAFETY: valid slice of fully initialised INPUT structs and the matching size.
    let accepted = unsafe {
        SetLastError(WIN32_ERROR(0));
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32)
    };
    // SAFETY: plain getter.
    let error = (accepted != 3).then(|| unsafe { GetLastError() }.0);
    let after = cursor();
    if accepted > 0
        && let Some(b) = before
    {
        // SAFETY: plain call.
        let _ = unsafe { SetCursorPos(b.x, b.y) };
    }
    SendOutcome {
        guard: verdict,
        accepted: Some(accepted),
        error,
        cursor_before: before,
        cursor_after: after,
    }
}

/// Result of [`probe_send_input`].
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    /// Events accepted of a relative mouse move by (0, 0).
    pub relative: u32,
    /// `GetLastError` after the relative move when it was refused.
    pub relative_error: Option<u32>,
    /// Events accepted of an absolute move onto the cursor's current pixel.
    pub absolute: Option<u32>,
    /// `GetLastError` after the absolute move when it was refused.
    pub absolute_error: Option<u32>,
    /// Cursor before and after (a no-op probe must leave it where it was).
    pub cursor: (Option<POINT>, Option<POINT>),
}

/// Calls `SendInput` with events that change nothing: a relative move by (0, 0) and an absolute
/// move onto the pixel the cursor is already on. It reports whether the system accepts injected
/// mouse input at all in the current state (locked session, other desktop), without clicking and
/// therefore without touching any window; used where a real click would land outside a test
/// window.
pub fn probe_send_input() -> ProbeOutcome {
    let before = cursor();
    let rel = [mouse(MOUSEEVENTF_MOVE, 0, 0)];
    // SAFETY: valid slice of initialised INPUT structs and the matching size.
    let (relative, relative_error) = unsafe {
        SetLastError(WIN32_ERROR(0));
        let n = SendInput(&rel, std::mem::size_of::<INPUT>() as i32);
        (n, (n != 1).then(|| GetLastError().0))
    };
    let (absolute, absolute_error) = if let Some(c) = before {
        // SAFETY: plain getters.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let abs = [mouse(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            absolute(c.x, vx, vw),
            absolute(c.y, vy, vh),
        )];
        // SAFETY: as above.
        unsafe {
            SetLastError(WIN32_ERROR(0));
            let n = SendInput(&abs, std::mem::size_of::<INPUT>() as i32);
            (Some(n), (n != 1).then(|| GetLastError().0))
        }
    } else {
        (None, None)
    };
    let after = cursor();
    if let (Some(b), Some(a)) = (before, after)
        && (a.x != b.x || a.y != b.y)
    {
        // SAFETY: plain call; puts the cursor back where it was.
        let _ = unsafe { SetCursorPos(b.x, b.y) };
    }
    ProbeOutcome {
        relative,
        relative_error,
        absolute,
        absolute_error,
        cursor: (before, after),
    }
}

/// True when the interactive session is locked (WTS session flags); `None` if unknown.
pub fn session_locked() -> Option<bool> {
    let mut buf = windows::core::PWSTR::null();
    let mut len = 0u32;
    // SAFETY: the API allocates `buf`, which is read as WTSINFOEXW only if it is large enough,
    // and freed with WTSFreeMemory.
    unsafe {
        WTSQuerySessionInformationW(
            None,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buf,
            &mut len,
        )
        .ok()?;
        let out = if (len as usize) >= std::mem::size_of::<WTSINFOEXW>() && !buf.is_null() {
            let info = &*(buf.0 as *const WTSINFOEXW);
            match info.Data.WTSInfoExLevel1.SessionFlags as u32 {
                WTS_SESSIONSTATE_LOCK => Some(true),
                WTS_SESSIONSTATE_UNLOCK => Some(false),
                _ => None,
            }
        } else {
            None
        };
        WTSFreeMemory(buf.0.cast());
        out
    }
}

/// Can this process open the input desktop? While the session is locked the input desktop is the
/// secure Winlogon desktop and the answer is an access error; `SendInput` cannot reach a window on
/// the default desktop then.
pub fn input_desktop() -> std::result::Result<(), u32> {
    // SAFETY: plain calls; the handle is closed.
    unsafe {
        match OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(0x0001),
        ) {
            Ok(h) => {
                let _ = CloseDesktop(h);
                Ok(())
            }
            Err(e) => Err((e.code().0 as u32) & 0xFFFF),
        }
    }
}

/// The foreground window as `(hwnd, class, pid)`, `None` when there is none.
pub fn foreground() -> Option<(isize, String, u32)> {
    // SAFETY: plain getter.
    let h = unsafe { GetForegroundWindow() };
    if h.is_invalid() {
        return None;
    }
    Some((h.0 as isize, class_of(h), pid_of(h)))
}

/// Everything the harness knows about the interactive state, for the findings.
pub fn desktop_state() -> String {
    format!(
        "session_locked={:?} input_desktop={:?} foreground={:?} cursor={:?}",
        session_locked(),
        input_desktop(),
        foreground(),
        cursor().map(|p| (p.x, p.y))
    )
}

/// Parses `key=value` pairs of a testwin pipe reply (`ok a=1 b=2,3`).
pub fn parse_reply(reply: &str) -> std::collections::HashMap<String, String> {
    reply
        .split_whitespace()
        .filter_map(|w| w.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// `colour?` as numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    /// Palette index.
    pub index: u32,
    /// Clicks that landed on the box.
    pub hits: u32,
    /// All clicks.
    pub clicks: u32,
}

/// Reads `colour?` from a testwin pipe.
pub fn read_colour(pipe: &str) -> Result<Colour> {
    let r = spike_webrtc::testwin::pipe_send(pipe, "colour?")?;
    let m = parse_reply(&r);
    let n = |k: &str| -> Result<u32> {
        m.get(k)
            .and_then(|v| v.parse().ok())
            .with_context(|| format!("no {k} in {r:?}"))
    };
    Ok(Colour {
        index: n("index")?,
        hits: n("hits")?,
        clicks: n("clicks")?,
    })
}

/// `state?` fields the harness needs: the box rectangle and the last click, in client pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxState {
    /// Frame number.
    pub frame: u32,
    /// Box rectangle (x, y, w, h) at the last painted frame.
    pub rect: (i32, i32, i32, i32),
    /// Last click, if any.
    pub last_click: Option<(i32, i32)>,
}

/// Reads `state?` from a testwin pipe.
pub fn read_state(pipe: &str) -> Result<BoxState> {
    let r = spike_webrtc::testwin::pipe_send(pipe, "state?")?;
    let m = parse_reply(&r);
    let nums =
        |k: &str| -> Option<Vec<i32>> { m.get(k)?.split(',').map(|v| v.parse().ok()).collect() };
    let b = nums("box").with_context(|| format!("no box in {r:?}"))?;
    ensure!(b.len() == 4, "bad box in {r:?}");
    let last_click = nums("last_click")
        .filter(|v| v.len() == 2)
        .map(|v| (v[0], v[1]));
    Ok(BoxState {
        frame: m.get("frame").and_then(|v| v.parse().ok()).unwrap_or(0),
        rect: (b[0], b[1], b[2], b[3]),
        last_click,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows turns an absolute coordinate `a` back into pixel `a * len / 65536`; the centre
    /// mapping must round-trip for every pixel of every plausible desktop width.
    #[test]
    fn absolute_round_trips() {
        for len in [1280, 1920, 2560, 3840, 4480, 7680] {
            for p in 0..len {
                let a = absolute(p, 0, len);
                assert!((0..=65535).contains(&a), "{a} out of range");
                assert_eq!(
                    i64::from(a) * i64::from(len) / 65536,
                    i64::from(p),
                    "len {len} pixel {p}"
                );
            }
        }
        // A virtual desktop that starts left of the primary monitor.
        assert_eq!(absolute(-1280, -1280, 3840), absolute(0, 0, 3840));
    }

    #[test]
    fn lparam_packs_signed_coordinates() {
        let lp = lparam(-3, 17);
        assert_eq!((lp.0 & 0xFFFF) as i16, -3);
        assert_eq!(((lp.0 >> 16) & 0xFFFF) as i16, 17);
    }

    #[test]
    fn reply_parsing() {
        let m = parse_reply("ok colour=FF8000 index=1 hits=2 clicks=3 box=10,20,64,64");
        assert_eq!(m["index"], "1");
        assert_eq!(m["box"], "10,20,64,64");
    }
}
