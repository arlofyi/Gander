// SPDX-License-Identifier: Apache-2.0
//! Test harness: the `gander-testwin` process and its control pipe, an occluder window this
//! process owns, and small Win32 helpers (monitors, window moves, cursor).

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW,
    GetDC, GetMonitorInfoW, GetPixel, HBRUSH, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    ReleaseDC,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows, GetClassNameW,
    GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, HWND_TOPMOST, IsWindowVisible, MSG, PostMessageW, PostQuitMessage,
    RegisterClassExW, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SetCursorPos, SetForegroundWindow, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WM_CLOSE,
    WM_DESTROY, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    WindowFromPoint,
};
use windows::core::{BOOL, PCWSTR, w};

/// Makes this process per-monitor DPI aware (v2), so window and client rectangles are physical.
pub fn become_dpi_aware() {
    // SAFETY: plain process-wide call with a valid constant.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

/// Locates `gander-testwin.exe` next to this binary, building it with cargo if it is missing.
pub fn testwin_exe(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    let me = std::env::current_exe()?;
    let dir = me.parent().context("exe has no directory")?;
    let exe = dir.join("gander-testwin.exe");
    if !exe.exists() {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args(["build", "--quiet", "-p", "gander-testwin"]);
        if dir.ends_with("release") {
            cmd.arg("--release");
        }
        let status = cmd.status().context("run cargo build -p gander-testwin")?;
        anyhow::ensure!(status.success(), "cargo build -p gander-testwin failed");
    }
    anyhow::ensure!(exe.exists(), "{} not found", exe.display());
    Ok(exe)
}

/// A running test window; killed on drop.
#[derive(Debug)]
pub struct Testwin {
    child: Child,
    pipe: String,
    pub pid: u32,
    pub hwnd: HWND,
}

impl Testwin {
    pub fn spawn(exe: &std::path::Path, title: &str) -> Result<Testwin> {
        // The self-test deadline is the backstop: even if this process dies, the window ends.
        let mut child = Command::new(exe)
            .args(["--self-test-secs", "240", "--title", title])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn gander-testwin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line)?;
        let line = line.trim().to_owned();
        if !line.starts_with("gander-testwin ready ") {
            let _ = child.kill();
            bail!("unexpected testwin line {line:?}");
        }
        let field = |key: &str| -> Result<String> {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .map(str::to_owned)
                .with_context(|| format!("no {key} in {line:?}"))
        };
        let pid = field("pid=")?.parse()?;
        let pipe = field("pipe=")?;
        let h = isize::from_str_radix(&field("hwnd=0x")?, 16)?;
        let w = Testwin {
            child,
            pipe,
            pid,
            hwnd: hwnd(h),
        };
        // Let the first frames paint.
        std::thread::sleep(Duration::from_millis(300));
        Ok(w)
    }

    pub fn client(&self) -> Result<Client> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match OpenOptions::new().read(true).write(true).open(&self.pipe) {
                Ok(f) => {
                    return Ok(Client {
                        io: BufReader::new(f),
                    });
                }
                Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => bail!("cannot open {}: {e}", self.pipe),
            }
        }
    }

    /// One request, one reply, on a fresh connection.
    pub fn send(&self, line: &str) -> Result<String> {
        self.client()?.send(line)
    }

    /// Waits until `state?` has `key=value`.
    pub fn wait_state(&self, key: &str, value: &str, within: Duration) -> Result<()> {
        let mut c = self.client()?;
        let want = format!("{key}={value}");
        let deadline = Instant::now() + within;
        loop {
            let s = c.send("state?")?;
            if s.split_whitespace().any(|w| w == want) {
                return Ok(());
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for {want}: {s}");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    pub fn quit(mut self) {
        let _ = self.send("quit");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Testwin {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[derive(Debug)]
pub struct Client {
    io: BufReader<File>,
}

impl Client {
    pub fn send(&mut self, line: &str) -> Result<String> {
        let f = self.io.get_mut();
        f.write_all(format!("{line}\n").as_bytes())?;
        f.flush()?;
        let mut reply = String::new();
        self.io.read_line(&mut reply)?;
        Ok(reply.trim_end().to_owned())
    }
}

/// `key=value` from a reply line.
pub fn reply_field<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(prefix.as_str()))
}

/// Puts a window above all non-topmost windows without activating it (focus-stealing
/// prevention keeps a window started from a background console behind the foreground window).
pub fn raise_topmost(h: HWND) -> Result<()> {
    // SAFETY: plain z-order change on a window handle; an invalid one returns an error.
    unsafe {
        SetWindowPos(
            h,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    }?;
    Ok(())
}

/// Asks for `h` to become the foreground window; false when Windows refuses.
pub fn try_foreground(h: HWND) -> bool {
    // SAFETY: plain call; refusal is reported as FALSE.
    unsafe { SetForegroundWindow(h) }.as_bool()
}

/// Moves a window (no resize, no activation).
pub fn move_window(h: HWND, x: i32, y: i32) -> Result<()> {
    // SAFETY: plain call on a window handle; an invalid one returns an error.
    unsafe {
        SetWindowPos(
            h,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }?;
    Ok(())
}

/// Moves and resizes a window (outer size).
pub fn set_window_rect(h: HWND, x: i32, y: i32, w: i32, hgt: i32) -> Result<()> {
    // SAFETY: as above.
    unsafe { SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE) }?;
    Ok(())
}

pub fn window_rect(h: HWND) -> RECT {
    let mut rc = RECT::default();
    // SAFETY: valid out pointer.
    let _ = unsafe { GetWindowRect(h, &mut rc) };
    rc
}

pub fn foreground() -> HWND {
    // SAFETY: plain getter.
    unsafe { GetForegroundWindow() }
}

/// One monitor.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub handle: isize,
    pub name: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub dpi: u32,
    pub hz: u32,
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `&mut Vec<HMONITOR>` passed below, alive for the call.
        let v = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        v.push(m);
        TRUE
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            LPARAM(&mut handles as *mut _ as isize),
        )
    };
    handles
        .into_iter()
        .filter_map(|m| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            // SAFETY: cbSize says the buffer is the EX variant.
            let ok = unsafe { GetMonitorInfoW(m, &mut info as *mut _ as *mut MONITORINFO) };
            if !ok.as_bool() {
                return None;
            }
            let (mut dx, mut dy) = (0u32, 0u32);
            // SAFETY: valid out pointers.
            let _ = unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            let mut dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                ..Default::default()
            };
            // SAFETY: device name is NUL-terminated inside the struct; `dm` is valid.
            let _ = unsafe {
                EnumDisplaySettingsW(
                    PCWSTR(info.szDevice.as_ptr()),
                    ENUM_CURRENT_SETTINGS,
                    &mut dm,
                )
            };
            let end = info.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
            Some(Monitor {
                handle: m.0 as isize,
                name: String::from_utf16_lossy(&info.szDevice[..end]),
                rect: info.monitorInfo.rcMonitor,
                work: info.monitorInfo.rcWork,
                primary: info.monitorInfo.dwFlags & 1 != 0,
                dpi: dx,
                hz: dm.dmDisplayFrequency,
            })
        })
        .collect()
}

/// Top-level visible windows of a process: (hwnd, class, title, rect).
pub fn process_windows(pid: u32) -> Vec<(HWND, String, String, RECT)> {
    struct Ctx {
        pid: u32,
        out: Vec<(HWND, String, String, RECT)>,
    }
    unsafe extern "system" fn cb(h: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `&mut Ctx` passed below, alive for the call.
        let ctx = unsafe { &mut *(data.0 as *mut Ctx) };
        let mut pid = 0u32;
        // SAFETY: valid out pointer; the other calls only read from `h` into local buffers.
        unsafe {
            GetWindowThreadProcessId(h, Some(&mut pid));
            if pid == ctx.pid && IsWindowVisible(h).as_bool() {
                let mut class = [0u16; 128];
                let n = GetClassNameW(h, &mut class).max(0) as usize;
                let mut title = [0u16; 256];
                let t = GetWindowTextW(h, &mut title).max(0) as usize;
                let mut rc = RECT::default();
                let _ = GetWindowRect(h, &mut rc);
                ctx.out.push((
                    h,
                    String::from_utf16_lossy(&class[..n]),
                    String::from_utf16_lossy(&title[..t]),
                    rc,
                ));
            }
        }
        TRUE
    }
    let mut ctx = Ctx {
        pid,
        out: Vec::new(),
    };
    // SAFETY: the callback only writes into `ctx`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut ctx as *mut Ctx as isize)) };
    ctx.out
}

/// Class name, owning pid and whether it is this process (diagnostics; no title, no pixels).
pub fn describe(h: HWND) -> String {
    let mut class = [0u16; 128];
    let mut pid = 0u32;
    // SAFETY: local buffers and out pointers.
    let n = unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        GetClassNameW(h, &mut class).max(0) as usize
    };
    format!(
        "class {}{}",
        String::from_utf16_lossy(&class[..n]),
        if pid == std::process::id() {
            " (this process)"
        } else {
            ""
        }
    )
}

/// True when the current session is locked (WTS session flags), `None` if unknown.
pub fn session_locked() -> Option<bool> {
    use windows::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK, WTSFreeMemory,
        WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
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

/// True when `upper` is above `lower` in the z-order.
pub fn is_above(upper: HWND, lower: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GW_HWNDPREV, GetWindow};
    let mut h = lower;
    for _ in 0..10_000 {
        // SAFETY: plain z-order walk; ends with an error at the top.
        match unsafe { GetWindow(h, GW_HWNDPREV) } {
            Ok(p) if !p.is_invalid() => {
                if p == upper {
                    return true;
                }
                h = p;
            }
            _ => return false,
        }
    }
    false
}

pub fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    // SAFETY: valid out pointer.
    let _ = unsafe { GetCursorPos(&mut p) };
    p
}

pub fn set_cursor(p: POINT) {
    // SAFETY: plain call.
    let _ = unsafe { SetCursorPos(p.x, p.y) };
}

pub fn window_from_point(p: POINT) -> HWND {
    // SAFETY: plain call.
    unsafe { WindowFromPoint(p) }
}

/// One pixel of the composed screen at a point that this process's own occluder covers.
pub fn screen_pixel(p: POINT) -> (u8, u8, u8) {
    // SAFETY: screen DC acquired and released here; GetPixel only reads.
    let c = unsafe {
        let dc = GetDC(None);
        let c = GetPixel(dc, p.x, p.y);
        ReleaseDC(None, dc);
        c
    };
    (c.0 as u8, (c.0 >> 8) as u8, (c.0 >> 16) as u8)
}

/// A topmost, non-activating, solid-colour popup owned by this process (the C2 occluder).
#[derive(Debug)]
pub struct Occluder {
    hwnd: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

unsafe extern "system" fn occluder_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        // SAFETY: plain call on the window thread.
        unsafe { PostQuitMessage(0) };
        return LRESULT(0);
    }
    // SAFETY: default handling for every other message.
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

impl Occluder {
    /// Shows the occluder over `rect` (screen pixels) in colour `(r, g, b)`.
    pub fn show(rect: RECT, rgb: (u8, u8, u8)) -> Result<Occluder> {
        let (tx, rx) = mpsc::channel::<Result<isize, String>>();
        let thread = std::thread::spawn(move || {
            // SAFETY: standard window-class registration and message loop on this thread; the
            // brush lives for the process (a spike), the class name is a static literal.
            unsafe {
                let module = GetModuleHandleW(None).unwrap_or_default();
                let colour =
                    COLORREF(u32::from(rgb.0) | (u32::from(rgb.1) << 8) | (u32::from(rgb.2) << 16));
                let class = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    lpfnWndProc: Some(occluder_proc),
                    hInstance: module.into(),
                    hbrBackground: HBRUSH(CreateSolidBrush(colour).0),
                    lpszClassName: w!("GanderSpikeOccluder"),
                    ..Default::default()
                };
                // A second registration fails harmlessly; the class is identical.
                RegisterClassExW(&class);
                let h = CreateWindowExW(
                    WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
                    w!("GanderSpikeOccluder"),
                    w!("Gander spike occluder"),
                    WS_POPUP,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    None,
                    None,
                    Some(module.into()),
                    None,
                );
                let h = match h {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(
                    h,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE
                        | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                        | SWP_NOACTIVATE,
                );
                let _ = tx.send(Ok(h.0 as isize));
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if msg.message == WM_CLOSE && msg.hwnd == h {
                        let _ = DestroyWindow(h);
                        continue;
                    }
                    DispatchMessageW(&msg);
                }
            }
        });
        let h = rx
            .recv_timeout(Duration::from_secs(5))
            .context("occluder thread did not start")?
            .map_err(anyhow::Error::msg)?;
        std::thread::sleep(Duration::from_millis(300));
        Ok(Occluder {
            hwnd: h,
            thread: Some(thread),
        })
    }
}

impl Occluder {
    /// The occluder's window handle.
    pub fn hwnd(&self) -> HWND {
        hwnd(self.hwnd)
    }
}

impl Drop for Occluder {
    fn drop(&mut self) {
        // SAFETY: posting to our own window; harmless if it is gone.
        let _ = unsafe { PostMessageW(Some(hwnd(self.hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
