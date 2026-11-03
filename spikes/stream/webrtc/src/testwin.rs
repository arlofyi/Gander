// SPDX-License-Identifier: Apache-2.0
//! Copied from `spikes/stream/encode/src/testwin.rs` (WP-42; a private module of that binary).
//! The `gander-testwin` process (the only window this spike captures) and the few Win32 calls
//! the encode runs need: DPI awareness, placing and sizing the window on its monitor.

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOACTIVATE, SetWindowPos};

/// Makes this process per-monitor DPI aware (v2), so window rectangles are physical pixels.
pub fn become_dpi_aware() {
    // SAFETY: plain process-wide call with a valid constant.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Locates `gander-testwin.exe` next to this binary, building it with cargo if missing.
pub fn exe(explicit: Option<PathBuf>) -> Result<PathBuf> {
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

/// One request line, one reply line on a test window's control pipe (the pipe is one client at a
/// time: a busy pipe is retried for up to 5 s).
pub fn pipe_send(pipe: &str, line: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let f = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe)
        {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(e) => bail!("cannot open {pipe}: {e}"),
        }
    };
    let mut io = BufReader::new(f);
    io.get_mut().write_all(format!("{line}\n").as_bytes())?;
    io.get_mut().flush()?;
    let mut reply = String::new();
    io.read_line(&mut reply)?;
    Ok(reply.trim_end().to_owned())
}

/// A running test window; killed on drop.
#[derive(Debug)]
pub struct Testwin {
    child: Child,
    pipe: String,
    /// Main window.
    pub hwnd: HWND,
}

impl Testwin {
    /// Starts the window with a self-test deadline of `secs` (the backstop if this process
    /// dies) and waits for its ready line.
    pub fn spawn(exe: &Path, title: &str, secs: u32) -> Result<Testwin> {
        let mut child = Command::new(exe)
            .args(["--self-test-secs", &secs.to_string(), "--title", title])
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
        let pipe = field("pipe=")?;
        let h = isize::from_str_radix(&field("hwnd=0x")?, 16)?;
        std::thread::sleep(Duration::from_millis(300));
        Ok(Testwin {
            child,
            pipe,
            hwnd: HWND(h as *mut c_void),
        })
    }

    /// The control pipe name (`\\.\pipe\gander-testwin-<pid>`).
    pub fn pipe(&self) -> &str {
        &self.pipe
    }

    /// One request line, one reply line.
    pub fn send(&self, line: &str) -> Result<String> {
        pipe_send(&self.pipe, line)
    }

    /// Clean shutdown through the pipe; the drop kills it if that fails.
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

/// Work area (physical pixels) of the monitor the window is on.
pub fn work_area(h: HWND) -> RECT {
    // SAFETY: plain getters with a sized out struct.
    unsafe {
        let m = MonitorFromWindow(h, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(m, &mut info);
        info.rcWork
    }
}

/// Window outer size with the aspect of `w` x `h` that fits the work area with a margin:
/// exactly `w` x `h` when it fits, otherwise scaled down.
pub fn fit(work: RECT, w: u32, h: u32, margin: i32) -> (i32, i32) {
    let max_w = (work.right - work.left - 2 * margin).max(64) as f64;
    let max_h = (work.bottom - work.top - 2 * margin).max(64) as f64;
    let s = (max_w / w as f64).min(max_h / h as f64).min(1.0);
    ((w as f64 * s).round() as i32, (h as f64 * s).round() as i32)
}

/// Moves and resizes the window (outer size), topmost, without activating it.
pub fn place(h: HWND, x: i32, y: i32, w: i32, hgt: i32) -> Result<()> {
    // SAFETY: plain call on a window handle; an invalid one returns an error.
    unsafe { SetWindowPos(h, Some(HWND_TOPMOST), x, y, w, hgt, SWP_NOACTIVATE) }?;
    Ok(())
}
