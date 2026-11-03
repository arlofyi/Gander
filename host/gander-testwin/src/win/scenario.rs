// SPDX-License-Identifier: Apache-2.0
//! Start-up scenarios for the window discovery tests (WP-51): a splash window before the main
//! window (`--splash`), and single-instance forwarding over a pipe (`--reuse`).

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gander_testwin::protocol;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_HIGHLIGHT, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassExW, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSEXW, WS_POPUP, WS_VISIBLE,
};
use windows::core::{PCWSTR, w};

/// The splash class: Office's, so the discovery skip list is exercised exactly as in production.
const SPLASH_CLASS: PCWSTR = w!("MsoSplash");

/// `ERROR_FILE_NOT_FOUND`: no instance owns the pipe.
const ERROR_FILE_NOT_FOUND: i32 = 2;
/// `ERROR_PIPE_BUSY`: another client is connected; retry.
const ERROR_PIPE_BUSY: i32 = 231;

extern "system" fn splash_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwards the arguments of the message being processed.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Shows a visible `MsoSplash` popup titled `title` for `duration`, pumping messages, then
/// destroys it. Runs on the window thread before the main window exists.
pub fn show_splash(title: &str, duration: Duration) -> Result<()> {
    // SAFETY: null module name means the current executable.
    let module = unsafe { GetModuleHandleW(None) }?;
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(splash_proc),
        hInstance: HINSTANCE(module.0),
        // A system colour index + 1 is a valid class brush.
        hbrBackground: HBRUSH((COLOR_HIGHLIGHT.0 + 1) as usize as *mut _),
        lpszClassName: SPLASH_CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised; name and procedure are 'static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        bail!(
            "RegisterClassExW(MsoSplash): {}",
            windows::core::Error::from_thread()
        );
    }
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the class is registered and `wide` is NUL terminated and outlives the call.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            SPLASH_CLASS,
            PCWSTR(wide.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            200,
            200,
            480,
            270,
            None,
            None,
            Some(HINSTANCE(module.0)),
            None,
        )
    }
    .context("creating the splash window")?;
    let deadline = Instant::now() + duration;
    let mut msg = MSG::default();
    while Instant::now() < deadline {
        // SAFETY: standard non-blocking pump on the thread that owns the window.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: the splash window belongs to this thread.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

/// Result of [`forward`].
#[derive(Debug)]
pub enum Forward {
    /// An instance with the key took the request; its reply line.
    Forwarded(String),
    /// No instance with the key runs: this process becomes it.
    NoInstance,
}

/// Asks the running instance with `key` to open a window titled `title`.
pub fn forward(key: &str, title: &str) -> Result<Forward> {
    let name = protocol::reuse_pipe_name(key);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pipe = loop {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(p) => break p,
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => {
                return Ok(Forward::NoInstance);
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).with_context(|| format!("opening {name}")),
        }
    };
    let mut writer = &pipe;
    writer
        .write_all(format!("open-second {title}\n").as_bytes())
        .context("writing to the reuse pipe")?;
    let mut line = String::new();
    BufReader::new(&pipe)
        .read_line(&mut line)
        .context("reading the reuse reply")?;
    let line = line.trim_end().to_owned();
    if !line.starts_with("ok") {
        bail!("the running instance refused: {line}");
    }
    Ok(Forward::Forwarded(line))
}
