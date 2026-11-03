// SPDX-License-Identifier: Apache-2.0
//! Session state of the interactive user: locked or not, plus idle time and power source.
//!
//! # Why the host must know the lock state itself
//!
//! The capture spike (`docs/specs/2026-11-01-stream-spike-findings.md`, C6) measured that Windows
//! Graphics Capture keeps delivering a window's real content, at full rate, while the session is
//! LOCKED. The OS therefore does not enforce "Stream only works on an unlocked session"
//! (`docs/security.md`); the host has to. This module is the one place that knows:
//!
//! - [`query_session_state`]: one authoritative read (`WTSQuerySessionInformationW` with
//!   `WTSSessionInfoEx`, `SessionFlags` and `SessionState` of the calling process's session).
//! - [`watch`]: an always-current view that also pushes changes. A message-only window on a
//!   dedicated thread registers with `WTSRegisterSessionNotification` and receives
//!   `WM_WTSSESSION_CHANGE`. The initial state is queried AFTER registering, so no transition can
//!   fall into the gap. A 15 s timer re-queries as a safety net against missed notifications.
//! - [`idle`] (`GetLastInputInfo`) and [`power`] (`GetSystemPowerStatus`): the other two inputs of
//!   the `host.state` event.
//!
//! # Fail closed
//!
//! [`SessionState::is_locked`] is true unless the session is positively known to be unlocked AND
//! connected to a display (an RDP-disconnected or fast-user-switched session is not interactive).
//! A failed query yields [`LockState::Unknown`], which counts as locked.
//!
//! # For the stream engine (Phase 2)
//!
//! ```no_run
//! # fn demo() -> std::io::Result<()> {
//! use gander_win::session;
//!
//! // Refuse a new Stream session (412 host_locked) and end running ones when this turns true.
//! let watcher = session::watch(|state| {
//!     if state.is_locked() {
//!         // stop the capture session now
//!     }
//! })?;
//! if watcher.state().is_locked() { /* refuse POST /stream */ }
//! # Ok(()) }
//! ```
//!
//! The callback runs on the watcher thread and must return quickly (send a signal, do not do the
//! teardown inline).

pub mod idle;
pub mod power;

use std::ffi::c_void;
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::HBRUSH;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK,
    WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSDisconnected, WTSFreeMemory, WTSINFOEXW,
    WTSQuerySessionInformationW, WTSRegisterSessionNotification, WTSSessionInfoEx,
    WTSUnRegisterSessionNotification,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, KillTimer, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, SetTimer, SetWindowLongPtrW, TranslateMessage, WM_CLOSE, WM_DESTROY,
    WM_NCCREATE, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_CONSOLE_CONNECT,
    WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
    WTS_SESSION_LOGOFF, WTS_SESSION_LOGON, WTS_SESSION_UNLOCK,
};

/// How often the watcher re-queries the OS to catch a missed notification.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(15);
const RECONCILE_TIMER_ID: usize = 1;
const CLASS_NAME: &str = "GanderSessionWatcher";

/// The lock flag of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockState {
    /// `WTS_SESSIONSTATE_LOCK`: the lock screen is showing.
    Locked,
    /// `WTS_SESSIONSTATE_UNLOCK`.
    Unlocked,
    /// `WTS_SESSIONSTATE_UNKNOWN`, or the query failed. Counts as locked.
    Unknown,
}

/// Whether the session is attached to a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Connection {
    /// `WTSActive`: a user is attached (console or remote).
    Active,
    /// `WTSDisconnected`: logged on but detached (RDP disconnect, fast user switching).
    Disconnected,
    /// Any other `WTS_CONNECTSTATE_CLASS`, with its value.
    Other(i32),
}

/// What the OS says about the session this process runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionState {
    /// The session id (0 is the services session, 1 is typically the first interactive user).
    pub session_id: u32,
    /// Lock flag.
    pub lock: LockState,
    /// Connection state.
    pub connection: Connection,
}

impl SessionState {
    /// Fail closed: `true` unless the session is unlocked and active.
    pub fn is_locked(&self) -> bool {
        !self.is_interactive()
    }

    /// Unlocked and attached to a display: the only state in which Stream may run.
    pub fn is_interactive(&self) -> bool {
        self.lock == LockState::Unlocked && self.connection == Connection::Active
    }

    /// The state a failed query yields: unknown, so locked.
    pub fn unknown(session_id: u32) -> Self {
        Self {
            session_id,
            lock: LockState::Unknown,
            connection: Connection::Other(-1),
        }
    }
}

/// Maps `SessionFlags` of `WTSINFOEX_LEVEL1` to a [`LockState`].
pub fn lock_from_flags(flags: i32) -> LockState {
    match flags as u32 {
        WTS_SESSIONSTATE_LOCK => LockState::Locked,
        WTS_SESSIONSTATE_UNLOCK => LockState::Unlocked,
        _ => LockState::Unknown,
    }
}

/// Maps a `WTS_CONNECTSTATE_CLASS` to a [`Connection`].
pub fn connection_from_state(state: i32) -> Connection {
    if state == WTSActive {
        Connection::Active
    } else if state == WTSDisconnected {
        Connection::Disconnected
    } else {
        Connection::Other(state)
    }
}

/// The lock state a `WM_WTSSESSION_CHANGE` `wParam` announces, if it announces one.
fn lock_from_event(code: u32) -> Option<LockState> {
    match code {
        WTS_SESSION_LOCK => Some(LockState::Locked),
        WTS_SESSION_UNLOCK | WTS_SESSION_LOGON => Some(LockState::Unlocked),
        _ => None,
    }
}

/// Applies a session-change notification to a known state (used when the re-query fails).
pub fn apply_event(mut state: SessionState, code: u32) -> SessionState {
    match code {
        WTS_SESSION_LOCK => state.lock = LockState::Locked,
        WTS_SESSION_UNLOCK | WTS_SESSION_LOGON => state.lock = LockState::Unlocked,
        WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => state.connection = Connection::Active,
        WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => {
            state.connection = Connection::Disconnected;
        }
        WTS_SESSION_LOGOFF => state.lock = LockState::Unknown,
        _ => {}
    }
    state
}

/// Frees WTS memory on drop.
struct WtsBuffer(*mut u16);

impl Drop for WtsBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from WTSQuerySessionInformationW and is freed once.
            unsafe { WTSFreeMemory(self.0.cast()) };
        }
    }
}

/// One authoritative read of the calling process's session.
///
/// Cheap (one RPC to the session service); safe to call from any thread, for example right before
/// accepting a Stream request.
pub fn query_session_state() -> io::Result<SessionState> {
    let mut buffer: *mut u16 = std::ptr::null_mut();
    let mut len: u32 = 0;
    // SAFETY: out-pointers are valid; the buffer is released by `WtsBuffer`.
    let ok = unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut len,
        )
    };
    let guard = WtsBuffer(buffer);
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if guard.0.is_null() || (len as usize) < std::mem::size_of::<WTSINFOEXW>() {
        return Err(io::Error::other("WTSSessionInfoEx returned a short buffer"));
    }
    // SAFETY: the buffer holds at least one WTSINFOEXW (checked above); read unaligned in case
    // the allocator's alignment is lower than the struct's.
    let info = unsafe { std::ptr::read_unaligned(guard.0.cast::<WTSINFOEXW>()) };
    if info.Level != 1 {
        return Err(io::Error::other("unsupported WTSINFOEX level"));
    }
    // SAFETY: Level 1 selects the `WTSInfoExLevel1` member of the union.
    let level1 = unsafe { info.Data.WTSInfoExLevel1 };
    Ok(SessionState {
        session_id: level1.SessionId,
        lock: lock_from_flags(level1.SessionFlags),
        connection: connection_from_state(level1.SessionState),
    })
}

type QueryFn = Arc<dyn Fn() -> io::Result<SessionState> + Send + Sync>;
type ChangeCallback = Box<dyn Fn(SessionState) + Send + Sync>;

/// State shared between the window procedure and the owner.
struct Ctx {
    query: QueryFn,
    current: Mutex<SessionState>,
    callback: ChangeCallback,
}

impl Ctx {
    fn current(&self) -> SessionState {
        *self.current.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Re-reads the state after a notification (`code`) or a timer tick (`None`), stores it and
    /// calls the callback when it changed.
    fn refresh(&self, code: Option<u32>) {
        let before = self.current();
        let mut next = match (self.query)() {
            Ok(s) => s,
            Err(_) => match code {
                Some(c) => apply_event(before, c),
                None => before,
            },
        };
        // A lock or unlock notification is the most direct signal; prefer it over a query that
        // might race the transition. The next timer tick reconciles if it was wrong.
        if let Some(lock) = code.and_then(lock_from_event) {
            next.lock = lock;
        }
        let changed = {
            let mut cur = self.current.lock().unwrap_or_else(|e| e.into_inner());
            let changed = *cur != next;
            *cur = next;
            changed
        };
        if changed {
            // Never unwind across the window procedure.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (self.callback)(next);
            }));
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

static REGISTER_CLASS: Once = Once::new();

fn register_class() {
    REGISTER_CLASS.call_once(|| {
        let name = wide(CLASS_NAME);
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            // SAFETY: a null module name returns the handle of the current executable.
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut::<c_void>() as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        // SAFETY: `class` and the name outlive the call; the name is copied by the OS. A
        // registration failure surfaces as a window-creation failure in `spawn_window`.
        unsafe { RegisterClassW(&class) };
    });
}

/// Window procedure of the message-only window.
///
/// # Safety
/// Called by the OS with a valid window handle on the thread that created the window.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE `lparam` points to the CREATESTRUCTW the window is created
        // with; `lpCreateParams` is the `Ctx` pointer passed by `spawn_window`.
        unsafe {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    // SAFETY: GWLP_USERDATA is either 0 or the `Ctx` pointer stored above; the owning thread
    // keeps its `Arc<Ctx>` alive until the window is destroyed.
    let ctx = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Ctx).as_ref() };
    match msg {
        WM_WTSSESSION_CHANGE => {
            if let Some(ctx) = ctx {
                ctx.refresh(Some(wparam as u32));
            }
            0
        }
        WM_TIMER => {
            if let Some(ctx) = ctx {
                ctx.refresh(None);
            }
            0
        }
        WM_CLOSE => {
            // SAFETY: `hwnd` is this window; both calls are valid on its thread.
            unsafe {
                WTSUnRegisterSessionNotification(hwnd);
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: valid on the window's own thread.
            unsafe {
                KillTimer(hwnd, RECONCILE_TIMER_ID);
                PostQuitMessage(0);
            }
            0
        }
        // SAFETY: forwarding the unhandled message with the original arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Creates the window, registers for notifications, reads the initial state and pumps messages
/// until the window is closed. Reports the outcome of the setup through `ready`.
fn window_thread(ctx: Arc<Ctx>, ready: mpsc::Sender<io::Result<usize>>) {
    register_class();
    let class = wide(CLASS_NAME);
    // SAFETY: all pointers are valid for the call; `Arc::as_ptr(&ctx)` stays valid because `ctx`
    // lives until this function returns, which is after the window is destroyed.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            Arc::as_ptr(&ctx).cast(),
        )
    };
    if hwnd.is_null() {
        let _ = ready.send(Err(io::Error::last_os_error()));
        return;
    }
    // SAFETY: `hwnd` is the window just created on this thread.
    if unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } == 0 {
        let e = io::Error::last_os_error();
        // SAFETY: as above.
        unsafe { DestroyWindow(hwnd) };
        let _ = ready.send(Err(e));
        return;
    }
    // SAFETY: as above. The timer is best effort; without it only the safety net is missing.
    unsafe {
        SetTimer(
            hwnd,
            RECONCILE_TIMER_ID,
            RECONCILE_INTERVAL.as_millis() as u32,
            None,
        );
    }
    // Query AFTER registering: a transition between query and registration would be lost the
    // other way round. A failed query leaves the state unknown (locked) until the timer retries.
    {
        let state = (ctx.query)().unwrap_or_else(|_| ctx.current());
        *ctx.current.lock().unwrap_or_else(|e| e.into_inner()) = state;
    }
    if ready.send(Ok(hwnd as usize)).is_err() {
        // SAFETY: as above; the caller is gone, so tear the window down.
        unsafe {
            WTSUnRegisterSessionNotification(hwnd);
            DestroyWindow(hwnd);
        }
    }
    // SAFETY: a standard message loop on the thread that owns the window.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Keeps the session state current and calls back on changes. Dropping it unregisters the
/// notification and stops the thread.
pub struct SessionWatcher {
    ctx: Arc<Ctx>,
    hwnd: usize,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SessionWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionWatcher")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl SessionWatcher {
    /// The current state; never blocks.
    pub fn state(&self) -> SessionState {
        self.ctx.current()
    }

    /// Re-queries the OS now and updates the state (and calls the callback if it changed).
    pub fn refresh(&self) {
        self.ctx.refresh(None);
    }

    #[cfg(test)]
    fn hwnd(&self) -> HWND {
        self.hwnd as HWND
    }
}

impl Drop for SessionWatcher {
    fn drop(&mut self) {
        // SAFETY: posting WM_CLOSE to a window of this process is always valid; if the window is
        // already gone the call fails harmlessly.
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts watching the session lock state.
///
/// `on_change` is called on the watcher thread each time the state changes (lock, unlock,
/// connect, disconnect, logon), with the new state. The initial state is available at once from
/// [`SessionWatcher::state`] and does not trigger the callback.
pub fn watch(
    on_change: impl Fn(SessionState) + Send + Sync + 'static,
) -> io::Result<SessionWatcher> {
    watch_with(Arc::new(query_session_state), Box::new(on_change))
}

fn watch_with(query: QueryFn, callback: ChangeCallback) -> io::Result<SessionWatcher> {
    let ctx = Arc::new(Ctx {
        current: Mutex::new(SessionState::unknown(0)),
        query,
        callback,
    });
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("gander-session-watch".to_owned())
        .spawn({
            let ctx = Arc::clone(&ctx);
            move || window_thread(ctx, tx)
        })?;
    match rx.recv() {
        Ok(Ok(hwnd)) => Ok(SessionWatcher {
            ctx,
            hwnd,
            thread: Some(thread),
        }),
        Ok(Err(e)) => {
            let _ = thread.join();
            Err(e)
        }
        Err(_) => {
            let _ = thread.join();
            Err(io::Error::other(
                "session watcher thread ended during setup",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    fn state(lock: LockState, connection: Connection) -> SessionState {
        SessionState {
            session_id: 1,
            lock,
            connection,
        }
    }

    #[test]
    fn only_unlocked_and_active_is_interactive() {
        assert!(state(LockState::Unlocked, Connection::Active).is_interactive());
        assert!(!state(LockState::Unlocked, Connection::Active).is_locked());
        for locked in [
            state(LockState::Locked, Connection::Active),
            state(LockState::Unknown, Connection::Active),
            state(LockState::Unlocked, Connection::Disconnected),
            state(LockState::Unlocked, Connection::Other(1)),
            SessionState::unknown(1),
        ] {
            assert!(locked.is_locked(), "{locked:?} must count as locked");
        }
    }

    #[test]
    fn flag_and_state_mapping() {
        assert_eq!(lock_from_flags(0), LockState::Locked);
        assert_eq!(lock_from_flags(1), LockState::Unlocked);
        assert_eq!(lock_from_flags(-1), LockState::Unknown);
        assert_eq!(lock_from_flags(7), LockState::Unknown);
        assert_eq!(connection_from_state(0), Connection::Active);
        assert_eq!(connection_from_state(4), Connection::Disconnected);
        assert_eq!(connection_from_state(1), Connection::Other(1));
    }

    #[test]
    fn events_move_the_state() {
        let s = state(LockState::Unlocked, Connection::Active);
        let s = apply_event(s, WTS_SESSION_LOCK);
        assert_eq!(s.lock, LockState::Locked);
        let s = apply_event(s, WTS_REMOTE_DISCONNECT);
        assert_eq!(s.connection, Connection::Disconnected);
        let s = apply_event(s, WTS_CONSOLE_CONNECT);
        assert_eq!(s.connection, Connection::Active);
        let s = apply_event(s, WTS_SESSION_UNLOCK);
        assert!(s.is_interactive());
        let s = apply_event(s, 0xFFFF);
        assert!(s.is_interactive(), "unknown codes change nothing");
        assert_eq!(apply_event(s, WTS_SESSION_LOGOFF).lock, LockState::Unknown);
    }

    /// The real query works in the session this test runs in. Prints the answer so the run
    /// can be recorded (`cargo test -p gander-win session -- --nocapture`).
    #[test]
    fn real_query_reports_this_session() {
        let s = query_session_state().expect("WTSQuerySessionInformationW");
        eprintln!(
            "WP-17 session query on this machine: {s:?} locked={}",
            s.is_locked()
        );
        // Tests run inside an interactive logon session (id > 0).
        assert!(s.session_id > 0, "{s:?}");
        assert_ne!(
            s.lock,
            LockState::Unknown,
            "SessionFlags must be LOCK or UNLOCK on Windows 11: {s:?}"
        );
    }

    #[test]
    fn real_watcher_starts_reports_a_state_and_stops() {
        let w = watch(|_| {}).expect("register for session notifications");
        let direct = query_session_state().unwrap();
        assert_eq!(w.state().lock, direct.lock);
        assert_eq!(w.state().session_id, direct.session_id);
        drop(w);
        // A second watcher can be created after the first is gone (class stays registered).
        drop(watch(|_| {}).expect("second watcher"));
    }

    /// Plumbing: a posted `WM_WTSSESSION_CHANGE` reaches the callback through the real message
    /// loop. The query is faked so the test never depends on (or touches) the real lock state.
    #[test]
    fn posted_session_messages_reach_the_callback() {
        let fake = Arc::new(Mutex::new(state(LockState::Unlocked, Connection::Active)));
        let seen = Arc::new(Mutex::new(Vec::<SessionState>::new()));
        let calls = Arc::new(AtomicU32::new(0));
        let query: QueryFn = {
            let fake = Arc::clone(&fake);
            Arc::new(move || Ok(*fake.lock().unwrap()))
        };
        let callback: ChangeCallback = {
            let seen = Arc::clone(&seen);
            let calls = Arc::clone(&calls);
            Box::new(move |s| {
                seen.lock().unwrap().push(s);
                calls.fetch_add(1, Ordering::SeqCst);
            })
        };
        let w = watch_with(query, callback).expect("watcher");
        assert!(w.state().is_interactive());

        let wait_for = |n: u32| {
            let t0 = Instant::now();
            while calls.load(Ordering::SeqCst) < n {
                assert!(t0.elapsed() < Duration::from_secs(2), "no callback #{n}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        // Lock: the OS state and the notification agree.
        fake.lock().unwrap().lock = LockState::Locked;
        // SAFETY: posting to the watcher's own window.
        unsafe { PostMessageW(w.hwnd(), WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize, 1) };
        wait_for(1);
        assert!(w.state().is_locked());
        // Unlock notification wins even if the query still says locked (racing the transition).
        // SAFETY: as above.
        unsafe {
            PostMessageW(
                w.hwnd(),
                WM_WTSSESSION_CHANGE,
                WTS_SESSION_UNLOCK as usize,
                1,
            )
        };
        wait_for(2);
        assert!(w.state().is_interactive());
        // A notification that changes nothing does not call back.
        fake.lock().unwrap().lock = LockState::Unlocked;
        // SAFETY: as above.
        unsafe {
            PostMessageW(
                w.hwnd(),
                WM_WTSSESSION_CHANGE,
                WTS_SESSION_UNLOCK as usize,
                1,
            )
        };
        // A timer-less manual refresh also reconciles.
        fake.lock().unwrap().connection = Connection::Disconnected;
        w.refresh();
        wait_for(3);
        assert!(w.state().is_locked());
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 3, "{seen:?}");
        drop(w);
    }

    #[test]
    fn failed_query_after_event_still_follows_the_event() {
        let ctx = Ctx {
            query: Arc::new(|| Err(io::Error::other("rpc down"))),
            current: Mutex::new(state(LockState::Unlocked, Connection::Active)),
            callback: Box::new(|_| {}),
        };
        ctx.refresh(Some(WTS_SESSION_LOCK));
        assert!(ctx.current().is_locked());
        ctx.refresh(None);
        assert!(
            ctx.current().is_locked(),
            "a failed timer query keeps the state"
        );
    }
}
