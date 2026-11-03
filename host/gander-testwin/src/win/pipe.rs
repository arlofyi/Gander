// SPDX-License-Identifier: Apache-2.0
//! The control pipe server (protocol in `gander_testwin::protocol`).
//!
//! One thread, one pipe instance, overlapped I/O so the thread can be woken by the stop event and
//! so an idle or half-open client cannot hold the pipe forever.

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gander_testwin::layout::{BOX_PALETTE, COUNTER_BITS, Layout, PATCH_A_RGB, PATCH_B_RGB};
use gander_testwin::protocol::{self, Command, MAX_LINE};
use gander_testwin::sec;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, HWND, INVALID_HANDLE_VALUE,
    LPARAM, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_FLAGS_AND_ATTRIBUTES,
    PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindow, PostMessageW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SendMessageTimeoutW,
};
use windows::core::{Error, HRESULT, PCWSTR};

use super::audio::{self, ToneError};
use super::shared::{lock, shared};
use super::ticker::request_quit;
use super::window::{
    WM_APP_DIALOG, WM_APP_DISMISS, WM_APP_MINIMISE, WM_APP_OPEN_SECOND, WM_APP_PARK, WM_APP_POPUP,
    WM_APP_RESTORE,
};

/// A connected client that sends nothing for this long is dropped.
const IDLE_TIMEOUT_MS: u32 = 60_000;
/// How long the pipe thread waits for the window thread to answer a synchronous request.
const UI_TIMEOUT_MS: u32 = 2_000;

/// Handles shared with the server thread, as integers so they are `Send`.
#[derive(Debug, Clone, Copy)]
struct Handles {
    pipe: isize,
    io_event: isize,
    stop: isize,
}

impl Handles {
    fn pipe(&self) -> HANDLE {
        HANDLE(self.pipe as *mut c_void)
    }
    fn io_event(&self) -> HANDLE {
        HANDLE(self.io_event as *mut c_void)
    }
    fn stop(&self) -> HANDLE {
        HANDLE(self.stop as *mut c_void)
    }
}

/// A running server; stop it with [`PipeServer::shutdown`].
#[derive(Debug)]
pub struct PipeServer {
    thread: Option<JoinHandle<()>>,
    handles: Handles,
    /// The pipe name, `\\.\pipe\gander-testwin-<pid>`.
    pub name: String,
}

/// Creates the pipe (restricted to the current user) and starts serving it.
pub fn start(pid: u32) -> Result<PipeServer> {
    start_named(protocol::pipe_name(pid))
}

/// Like [`start`] under another name (the `--reuse` single-instance pipe). Fails if a pipe with
/// that name already exists.
pub fn start_named(name: String) -> Result<PipeServer> {
    let sd = sec::current_user_only().context("building the pipe security descriptor")?;
    let attrs = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.as_ptr(),
        bInheritHandle: false.into(),
    };
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let open_mode = FILE_FLAGS_AND_ATTRIBUTES(
        PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_OVERLAPPED.0 | FILE_FLAG_FIRST_PIPE_INSTANCE.0,
    );
    // SAFETY: `wide` is NUL terminated and `attrs` (and the descriptor it points to, owned by
    // `sd`) outlive the call; the system copies the descriptor. FIRST_PIPE_INSTANCE makes the
    // call fail instead of joining a pipe somebody else created under this name.
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            Some(&attrs),
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        bail!("CreateNamedPipeW({name}): {}", Error::from_thread());
    }
    drop(sd);

    // SAFETY: plain event creation without security attributes or name.
    let io_event =
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.context("CreateEventW (io)")?;
    // SAFETY: as above.
    let stop = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .context("CreateEventW (stop)")?;
    let handles = Handles {
        pipe: pipe.0 as isize,
        io_event: io_event.0 as isize,
        stop: stop.0 as isize,
    };
    let thread = thread::Builder::new()
        .name(format!("testwin-pipe {name}"))
        .spawn(move || serve(handles))
        .context("spawning the pipe thread")?;
    Ok(PipeServer {
        thread: Some(thread),
        handles,
        name,
    })
}

impl PipeServer {
    /// Wakes the server thread, waits for it to finish and closes the handles.
    pub fn shutdown(mut self) {
        // SAFETY: the stop event is valid until closed below, after the thread has been joined.
        unsafe {
            let _ = SetEvent(self.handles.stop());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // SAFETY: the thread is gone, so nothing else uses these handles; each is closed once.
        unsafe {
            let _ = CloseHandle(self.handles.pipe());
            let _ = CloseHandle(self.handles.io_event());
            let _ = CloseHandle(self.handles.stop());
        }
    }
}

/// Outcome of one overlapped operation.
enum Io {
    Done(u32),
    Stopped,
    TimedOut,
    Failed(Error),
}

/// Waits for the overlapped operation that `started` describes.
fn finish(h: &Handles, ov: &mut OVERLAPPED, started: windows::core::Result<()>, ms: u32) -> Io {
    let pending = HRESULT::from_win32(ERROR_IO_PENDING.0);
    match started {
        Ok(()) => {}
        Err(e) if e.code() == pending => {
            // SAFETY: both handles are valid events owned by the server.
            let r = unsafe { WaitForMultipleObjects(&[h.io_event(), h.stop()], false, ms) };
            if r != WAIT_OBJECT_0 {
                // Stop event or timeout: cancel and wait until the kernel is done with `ov`.
                // SAFETY: `ov` is the structure the pending operation was started with.
                unsafe {
                    let _ = CancelIoEx(h.pipe(), Some(ov));
                    let mut n = 0u32;
                    let _ = GetOverlappedResult(h.pipe(), ov, &mut n, true);
                }
                return if r == WAIT_TIMEOUT {
                    Io::TimedOut
                } else {
                    Io::Stopped
                };
            }
        }
        Err(e) => return Io::Failed(e),
    }
    let mut n = 0u32;
    // SAFETY: the operation has completed (signalled or finished synchronously).
    match unsafe { GetOverlappedResult(h.pipe(), ov, &mut n, false) } {
        Ok(()) => Io::Done(n),
        Err(e) => Io::Failed(e),
    }
}

fn new_overlapped(h: &Handles) -> OVERLAPPED {
    // SAFETY: the event is valid; resetting it before each operation is required for a
    // manual-reset event reused across operations.
    unsafe {
        let _ = ResetEvent(h.io_event());
    }
    OVERLAPPED {
        hEvent: h.io_event(),
        ..Default::default()
    }
}

fn connect(h: &Handles) -> Io {
    let mut ov = new_overlapped(h);
    // SAFETY: `ov` outlives the operation because `finish` waits for it.
    let started = unsafe { ConnectNamedPipe(h.pipe(), Some(&mut ov)) };
    match started {
        // The client connected between CreateNamedPipe and ConnectNamedPipe.
        Err(e) if e.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => Io::Done(0),
        other => finish(h, &mut ov, other, INFINITE),
    }
}

fn read(h: &Handles, buf: &mut [u8], ms: u32) -> Io {
    let mut ov = new_overlapped(h);
    // SAFETY: `buf` and `ov` outlive the operation because `finish` waits for it.
    let started = unsafe { ReadFile(h.pipe(), Some(buf), None, Some(&mut ov)) };
    finish(h, &mut ov, started, ms)
}

fn write_all(h: &Handles, data: &[u8]) -> bool {
    let mut ov = new_overlapped(h);
    // SAFETY: `data` and `ov` outlive the operation because `finish` waits for it.
    let started = unsafe { WriteFile(h.pipe(), Some(data), None, Some(&mut ov)) };
    matches!(finish(h, &mut ov, started, IDLE_TIMEOUT_MS), Io::Done(n) if n as usize == data.len())
}

fn serve(h: Handles) {
    let s = shared();
    while !s.stop.load(Ordering::Relaxed) {
        match connect(&h) {
            Io::Done(_) => {}
            Io::Stopped => break,
            Io::TimedOut => continue,
            Io::Failed(e) => {
                eprintln!("gander-testwin: pipe connect failed: {e}");
                thread::sleep(Duration::from_millis(200));
                continue;
            }
        }
        session(&h);
        // SAFETY: the pipe handle is valid; disconnecting readies the instance for the next client.
        unsafe {
            let _ = DisconnectNamedPipe(h.pipe());
        }
    }
}

/// Serves one connected client until it disconnects, idles out, or sends `quit`.
fn session(h: &Handles) {
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
            if pos > MAX_LINE {
                let _ = write_all(h, b"err line-too-long\n");
                return;
            }
            let line: Vec<u8> = pending.drain(..=pos).collect();
            let reply = match std::str::from_utf8(&line[..pos]) {
                Ok(text) => handle_line(text),
                Err(_) => Reply::text("err bad-argument not-utf8"),
            };
            let mut out = reply.text.into_bytes();
            out.push(b'\n');
            if !write_all(h, &out) {
                return;
            }
            if reply.quit {
                request_quit();
                return;
            }
        }
        if pending.len() > MAX_LINE {
            let _ = write_all(h, b"err line-too-long\n");
            return;
        }
        match read(h, &mut chunk, IDLE_TIMEOUT_MS) {
            Io::Done(0) => return,
            Io::Done(n) => pending.extend_from_slice(&chunk[..n as usize]),
            Io::Stopped | Io::TimedOut | Io::Failed(_) => return,
        }
    }
}

struct Reply {
    text: String,
    quit: bool,
}

impl Reply {
    fn text(t: impl Into<String>) -> Reply {
        Reply {
            text: t.into(),
            quit: false,
        }
    }
}

fn handle_line(line: &str) -> Reply {
    match protocol::parse(line) {
        Ok(cmd) => execute(cmd),
        Err(e) => Reply::text(protocol::err_reply(&e)),
    }
}

fn main_hwnd() -> HWND {
    HWND(shared().main_hwnd.load(Ordering::Relaxed) as *mut c_void)
}

/// Sends a message to the window thread and waits (bounded) for the handler's result.
fn send_ui(msg: u32) -> Option<usize> {
    let mut result = 0usize;
    // SAFETY: the handle is a window of this process (or stale, in which case the call fails);
    // `result` outlives the call.
    let ok = unsafe {
        SendMessageTimeoutW(
            main_hwnd(),
            msg,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            UI_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    (ok.0 != 0).then_some(result)
}

fn post_ui(msg: u32) -> bool {
    // SAFETY: PostMessageW accepts any handle value.
    unsafe { PostMessageW(Some(main_hwnd()), msg, WPARAM(0), LPARAM(0)) }.is_ok()
}

fn execute(cmd: Command) -> Reply {
    let s = shared();
    let flag = |v: bool| u8::from(v);
    match cmd {
        Command::Ping => Reply::text("ok pong"),
        Command::Frame => Reply::text(format!(
            "ok frame={}",
            s.painted_frame.load(Ordering::Relaxed)
        )),
        Command::Colour => Reply::text(format!(
            "ok colour={} index={} hits={} clicks={}",
            s.box_rgb().hex(),
            s.box_index(),
            s.hits.load(Ordering::Relaxed),
            s.clicks.load(Ordering::Relaxed)
        )),
        Command::Typed => {
            let units = lock(&s.typed).clone();
            let text = String::from_utf16_lossy(&units);
            let hex: String = text.bytes().map(|b| format!("{b:02x}")).collect();
            Reply::text(format!(
                "ok keydowns={} chars={} text={}",
                s.keydowns.load(Ordering::Relaxed),
                text.chars().count(),
                if hex.is_empty() { "-".to_owned() } else { hex }
            ))
        }
        Command::State => {
            let (w, h, dpi) = *lock(&s.client);
            let frame = s.painted_frame.load(Ordering::Relaxed);
            let boxr = Layout::new(w, h, dpi).box_rect(frame);
            let windows = lock(&s.windows).len();
            // SAFETY: IsIconic only reads window state and accepts any handle value.
            let minimised = unsafe { IsIconic(main_hwnd()) }.as_bool();
            let click = s
                .last_click()
                .map_or_else(|| "none".to_owned(), |(x, y)| format!("{x},{y}"));
            Reply::text(format!(
                "ok frame={frame} colour={} hits={} clicks={} last_click={click} windows={windows} \
                 minimised={} menu={} tooltip={} dialog={} box={}",
                s.box_rgb().hex(),
                s.hits.load(Ordering::Relaxed),
                s.clicks.load(Ordering::Relaxed),
                flag(minimised),
                flag(s.menu_open.load(Ordering::Relaxed)),
                flag(s.tooltip_visible.load(Ordering::Relaxed)),
                flag(s.dialog_open.load(Ordering::Relaxed)),
                boxr.csv()
            ))
        }
        Command::Layout => {
            let (w, h, dpi) = *lock(&s.client);
            let l = Layout::new(w, h, dpi);
            Reply::text(format!(
                "ok dpi={} client={}x{} patch_a={} patch_a_rgb={} patch_b={} patch_b_rgb={} \
                 strip={} strip_cell={} strip_bits={} arena={} box_size={} counter_text={},{},{} \
                 clock_text={},{},{} palette={}",
                l.dpi,
                l.client_w,
                l.client_h,
                l.patch_a.csv(),
                PATCH_A_RGB.hex(),
                l.patch_b.csv(),
                PATCH_B_RGB.hex(),
                l.strip.csv(),
                l.strip_cell,
                COUNTER_BITS,
                l.arena.csv(),
                l.box_size,
                l.counter_text.x,
                l.counter_text.y,
                l.counter_text.scale,
                l.clock_text.x,
                l.clock_text.y,
                l.clock_text.scale,
                BOX_PALETTE
                    .iter()
                    .map(|c| c.hex())
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
        Command::Windows => {
            let list: Vec<isize> = lock(&s.windows)
                .iter()
                .copied()
                // SAFETY: IsWindow only reads window state and accepts any handle value.
                .filter(|h| unsafe { IsWindow(Some(HWND(*h as *mut c_void))) }.as_bool())
                .collect();
            let text = list
                .iter()
                .map(|h| format!("0x{h:X}"))
                .collect::<Vec<_>>()
                .join(",");
            Reply::text(format!("ok count={} hwnd={text}", list.len()))
        }
        Command::OpenSecond { title } => {
            *lock(&s.pending_title) = title;
            match send_ui(WM_APP_OPEN_SECOND) {
                Some(0) => Reply::text("err failed could not create the window"),
                Some(h) => Reply::text(format!("ok hwnd=0x{h:X}")),
                None => Reply::text("err timeout"),
            }
        }
        Command::Minimise => ui_ack(send_ui(WM_APP_MINIMISE)),
        Command::Restore => ui_ack(send_ui(WM_APP_RESTORE)),
        Command::Dismiss => ui_ack(send_ui(WM_APP_DISMISS)),
        Command::Popup => post_ack(post_ui(WM_APP_POPUP)),
        Command::Dialog => post_ack(post_ui(WM_APP_DIALOG)),
        Command::Tone { hz, ms } => match audio::play_tone(hz, ms) {
            Ok(()) => Reply::text("ok"),
            Err(ToneError::Busy) => Reply::text("err busy"),
            Err(e) => Reply::text(format!("err tone {e}")),
        },
        Command::Freeze => {
            s.frozen.store(true, Ordering::Relaxed);
            Reply::text("ok")
        }
        Command::Park => {
            s.frozen.store(true, Ordering::Relaxed);
            s.parked.store(true, Ordering::Relaxed);
            ui_ack(send_ui(WM_APP_PARK))
        }
        Command::Animate => {
            s.frozen.store(false, Ordering::Relaxed);
            s.parked.store(false, Ordering::Relaxed);
            Reply::text("ok")
        }
        Command::Quit => Reply {
            text: "ok".into(),
            quit: true,
        },
    }
}

fn ui_ack(r: Option<usize>) -> Reply {
    Reply::text(if r.is_some() { "ok" } else { "err timeout" })
}

fn post_ack(posted: bool) -> Reply {
    Reply::text(if posted {
        "ok"
    } else {
        "err failed window is gone"
    })
}
