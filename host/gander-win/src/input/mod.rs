// SPDX-License-Identifier: Apache-2.0
//! Stream input injection on Windows (WP-52; stream-launch-policy spec "Input mapping",
//! `security.md` rule 12, stream-spike findings decision 4).
//!
//! The decisions live in [`gander_core::input`] (mapping math, key table, pause rules); this
//! module observes the OS and calls `SendInput`. One [`InputController`] per stream session:
//!
//! ```no_run
//! # fn demo(target: &gander_win::launch::WindowTarget) {
//! use std::sync::Arc;
//! use std::time::Duration;
//! use gander_core::input::{ChannelKind, RecordingSink};
//! use gander_win::input::{InputController, InputOptions, query_lock_probe, spawn_poller};
//!
//! let sink = Arc::new(RecordingSink::default()); // the engine's own InputSink in production
//! let controller = InputController::new(target.into(), sink, query_lock_probe(), InputOptions::default());
//! let controller = Arc::new(std::sync::Mutex::new(controller));
//! // Pause/resume reports even when no input arrives (focus lost, local user active).
//! let _poller = spawn_poller(Arc::clone(&controller), Duration::from_millis(100));
//! // For every data-channel message:
//! let _ = controller.lock().unwrap().handle_bytes(ChannelKind::Input, br#"{"t":"move","x":0.5,"y":0.5}"#);
//! # }
//! ```
//!
//! Before **every** message the controller re-reads the facts (session lock through the
//! [`LockProbe`], fail closed; foreground window; `GetLastInputInfo` against its own injection
//! log) and the window's live geometry (DWM frame bounds, client rectangle, virtual desktop, all
//! physical: the thread is switched to per-monitor v2 for the call). Pointer events then pass the
//! pre-click check (the top-level window under the point must be the target, a window it owns,
//! or its popup menu); keys and text need nothing more than the foreground check, because the
//! keyboard goes to the foreground window, which the gate has just verified is the target.
//!
//! What is sent: an absolute move over the virtual desktop
//! (`MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`) followed by the button or wheel event in
//! the same `SendInput` batch; keys as scan codes (`KEYEVENTF_SCANCODE`, the host's layout
//! decides the character); text as `KEYEVENTF_UNICODE`. Every event carries
//! [`os::GANDER_EXTRA_INFO`] in `dwExtraInfo`. `SendInput` moves the real cursor (the local user
//! sees it jump); `PostMessage` is not used (it works while locked and through covering windows,
//! stream-spike findings, so it is not a path Gander takes).
//!
//! Held buttons and keys are released when the gate pauses (except while locked: nothing is
//! sent then), on [`InputController::release_all`] and on drop.

pub mod foreground;
pub(crate) mod os;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gander_core::input::activity::{InjectionLog, Origin};
use gander_core::input::channel::{channel_allows, decode};
use gander_core::input::keys::{
    KeySpec, TextEvent, check_key, key_for_code, modifier_code, modifier_of, text_events,
};
use gander_core::input::mapping::{map_normalised, to_absolute, wheel_data};
use gander_core::input::{
    ChannelKind, Facts, GateConfig, GateState, InputGate, InputSink, MessageKind, OutOfRange,
    PauseCause, PictureArea, Point, Refusal, WindowGeometry,
};
use gander_core::launch::discovery::Hwnd;
use gander_core::protocol::input::{InputMessage, Modifier, MouseButton};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
};

pub use foreground::{AcquirePolicy, Attempt, ForegroundReport, Method};
pub use os::{GANDER_EXTRA_INFO, cursor, set_cursor};

use crate::launch::WindowTarget;
use crate::launch::dpi::DpiScope;
use crate::session::{self, SessionWatcher};

/// The window input goes to: handle plus owning process (checked before every call, handles
/// are reused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputTarget {
    /// Window handle.
    pub hwnd: Hwnd,
    /// Owning process.
    pub pid: u32,
}

impl InputTarget {
    /// A target from a handle and its process.
    pub const fn new(hwnd: Hwnd, pid: u32) -> Self {
        Self { hwnd, pid }
    }

    pub(crate) fn handle(&self) -> HWND {
        os::hwnd(self.hwnd)
    }

    /// The live geometry (physical pixels), `None` when the window is gone.
    pub fn geometry(&self) -> Option<WindowGeometry> {
        let _dpi = DpiScope::per_monitor_v2();
        os::live(self.handle(), self.pid).ok()?;
        os::geometry(self.handle())
    }

    /// The target (or a window it owns) is the foreground window.
    pub fn is_foreground(&self) -> bool {
        os::is_foreground(self.handle())
    }

    /// Maps a normalised picture point onto the live window and runs the pre-click check,
    /// without injecting anything: the screen point a press there would go to, or why it would
    /// be refused (diagnostics and tests; [`InputController`] runs the same steps).
    pub fn check_point(&self, x: f64, y: f64, area: PictureArea) -> Result<Point, Refusal> {
        let _dpi = DpiScope::per_monitor_v2();
        let h = self.handle();
        os::live(h, self.pid)?;
        let g = os::geometry(h).ok_or(Refusal::WindowGone)?;
        let p = map_normalised(x, y, g.picture(area), OutOfRange::Refuse)?;
        match os::hit_test(h, p) {
            os::Hit::Target => Ok(p),
            os::Hit::Other { pid, class } => Err(Refusal::Covered { pid, class }),
            os::Hit::Nothing => Err(Refusal::NoWindowUnderPoint),
        }
    }
}

impl From<&WindowTarget> for InputTarget {
    fn from(t: &WindowTarget) -> Self {
        Self::new(t.hwnd, t.pid)
    }
}

/// Answers "is the session locked?" (fail closed: true when unknown). Called before every
/// message.
pub type LockProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// A probe that queries `WTSQuerySessionInformationW` on every call (one RPC; fail closed).
pub fn query_lock_probe() -> LockProbe {
    Arc::new(|| session::query_session_state().map_or(true, |s| s.is_locked()))
}

/// A probe over a running [`SessionWatcher`] (no OS call per message).
pub fn watcher_lock_probe(watcher: Arc<SessionWatcher>) -> LockProbe {
    Arc::new(move || watcher.state().is_locked())
}

/// Per-session settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputOptions {
    /// What the streamed picture shows (WGC window capture: the frame).
    pub picture: PictureArea,
    /// Pause timing.
    pub gate: GateConfig,
    /// Foreground fallbacks for [`InputController::bring_to_front`].
    pub foreground: AcquirePolicy,
}

/// The injector of one stream session (module documentation).
pub struct InputController {
    target: InputTarget,
    options: InputOptions,
    sink: Arc<dyn InputSink>,
    locked: LockProbe,
    gate: InputGate,
    log: InjectionLog,
    started: Instant,
    last_local: Option<Duration>,
    buttons: BTreeSet<Button>,
    keys: BTreeMap<String, HeldKey>,
    last_refusal: Option<(MessageKind, Refusal)>,
}

impl std::fmt::Debug for InputController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputController")
            .field("target", &self.target)
            .field("state", &self.gate.state())
            .field("buttons", &self.buttons)
            .field("keys", &self.keys.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Button {
    Left,
    Right,
    Middle,
}

impl From<MouseButton> for Button {
    fn from(b: MouseButton) -> Self {
        match b {
            MouseButton::Left => Button::Left,
            MouseButton::Right => Button::Right,
            MouseButton::Middle => Button::Middle,
        }
    }
}

impl Button {
    fn flags(self, down: bool) -> MOUSE_EVENT_FLAGS {
        match (self, down) {
            (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
            (Button::Left, false) => MOUSEEVENTF_LEFTUP,
            (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
            (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
            (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
            (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
        }
    }
}

/// A key held down by a `key` message, with the modifiers pressed on its behalf.
#[derive(Debug, Clone)]
struct HeldKey {
    spec: KeySpec,
    modifier: Option<Modifier>,
    auto_mods: Vec<KeySpec>,
}

fn key_event(spec: KeySpec, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if spec.extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    os::key(spec.vk, spec.scan, flags)
}

fn move_to(p: Point, desktop: gander_core::launch::discovery::Rect) -> INPUT {
    let (ax, ay) = to_absolute(p, desktop);
    os::mouse(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        ax,
        ay,
        0,
    )
}

impl InputController {
    /// A controller for `target`. Reads the facts once and reports the initial gate state to
    /// `sink`.
    pub fn new(
        target: InputTarget,
        sink: Arc<dyn InputSink>,
        locked: LockProbe,
        options: InputOptions,
    ) -> Self {
        let mut c = Self {
            target,
            options,
            sink,
            locked,
            gate: InputGate::new(options.gate),
            log: InjectionLog::new(),
            started: Instant::now(),
            last_local: None,
            buttons: BTreeSet::new(),
            keys: BTreeMap::new(),
            last_refusal: None,
        };
        c.poll();
        c
    }

    /// The window input goes to.
    pub fn target(&self) -> InputTarget {
        self.target
    }

    /// The gate state as of the last observation.
    pub fn state(&self) -> GateState {
        self.gate.state()
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn observe(&mut self) -> Facts {
        let locked = (self.locked)();
        let foreground = !locked && os::is_foreground(self.target.handle());
        if let Some(t) = os::last_input_tick()
            && self.log.classify(t) == Origin::Local
        {
            let age = Duration::from_millis(u64::from(os::tick().wrapping_sub(t)));
            self.last_local = Some(self.now().saturating_sub(age));
        }
        Facts {
            locked,
            foreground,
            last_local_input: self.last_local,
        }
    }

    fn apply(&mut self, changed: Option<GateState>) {
        if let Some(s) = changed {
            if let GateState::Paused(cause) = s {
                self.release_held(cause != PauseCause::Locked);
            }
            self.sink.state_changed(s);
        }
    }

    /// Re-reads the facts and reports a state change. Call it periodically (every 100 ms,
    /// [`spawn_poller`]) so pauses and resumes are reported when no input arrives.
    pub fn poll(&mut self) -> GateState {
        let facts = self.observe();
        let now = self.now();
        let changed = self.gate.update(now, &facts);
        self.apply(changed);
        self.gate.state()
    }

    /// `stream.pause` / `stream.resume` from the client.
    pub fn set_client_paused(&mut self, paused: bool) -> GateState {
        let facts = self.observe();
        let now = self.now();
        let changed = self.gate.set_client_paused(paused, now, &facts);
        self.apply(changed);
        self.gate.state()
    }

    /// The phone's "Bring to front": the foreground procedure with the configured fallbacks.
    /// Refused while the session is locked.
    pub fn bring_to_front(&mut self) -> Result<ForegroundReport, Refusal> {
        if (self.locked)() {
            self.poll();
            return Err(Refusal::Paused(PauseCause::Locked));
        }
        let policy = self.options.foreground;
        let report = foreground::acquire(&self.target, &policy, &mut self.log);
        self.poll();
        report
    }

    /// Decodes and handles one data-channel message.
    pub fn handle_bytes(&mut self, channel: ChannelKind, bytes: &[u8]) -> Result<(), Refusal> {
        match decode(bytes) {
            Ok(m) => self.handle(channel, &m),
            Err(e) => {
                let r = Refusal::from(e);
                self.report(MessageKind::Undecodable, &r);
                Err(r)
            }
        }
    }

    /// Handles one decoded message: gate, mapping, pre-click check, `SendInput`.
    pub fn handle(&mut self, channel: ChannelKind, message: &InputMessage) -> Result<(), Refusal> {
        let kind = MessageKind::of(message);
        let result = self.dispatch(channel, message);
        match &result {
            Ok(()) => self.last_refusal = None,
            Err(r) => self.report(kind, r),
        }
        result
    }

    fn report(&mut self, kind: MessageKind, r: &Refusal) {
        let key = (kind, r.clone());
        if self.last_refusal.as_ref() != Some(&key) {
            self.sink.refused(kind, r);
            self.last_refusal = Some(key);
        }
    }

    fn dispatch(&mut self, channel: ChannelKind, message: &InputMessage) -> Result<(), Refusal> {
        if !channel_allows(channel, message) {
            return Err(Refusal::WrongChannel);
        }
        if matches!(
            message,
            InputMessage::ClipboardGet {} | InputMessage::Clipboard { .. }
        ) {
            return Err(Refusal::Unsupported);
        }
        let _dpi = DpiScope::per_monitor_v2();
        match self.poll() {
            GateState::Active => {}
            GateState::Paused(cause) => return Err(Refusal::Paused(cause)),
        }
        os::live(self.target.handle(), self.target.pid)?;
        match message {
            InputMessage::Move { x, y } => {
                let (p, desk) = self.pointer(*x, *y, OutOfRange::Clamp)?;
                self.send(&[move_to(p, desk)])
            }
            InputMessage::Down { b, x, y } => {
                let (p, desk) = self.pointer(*x, *y, OutOfRange::Refuse)?;
                let b = Button::from(*b);
                self.send(&[move_to(p, desk), os::mouse(b.flags(true), 0, 0, 0)])?;
                self.buttons.insert(b);
                Ok(())
            }
            InputMessage::Up { b, x, y } => {
                let b = Button::from(*b);
                let up = os::mouse(b.flags(false), 0, 0, 0);
                let r = match self.pointer(*x, *y, OutOfRange::Clamp) {
                    Ok((p, desk)) => self.send(&[move_to(p, desk), up]),
                    // A release over another window still ends our own press, without moving
                    // onto that window.
                    Err(Refusal::Covered { .. } | Refusal::NoWindowUnderPoint)
                        if self.buttons.contains(&b) =>
                    {
                        self.send(&[up])
                    }
                    Err(e) => Err(e),
                };
                if r.is_ok() {
                    self.buttons.remove(&b);
                }
                r
            }
            InputMessage::Scroll { dx, dy, x, y } => {
                let (p, desk) = self.pointer(*x, *y, OutOfRange::Refuse)?;
                let mut events = vec![move_to(p, desk)];
                // Windows wheel convention (`gander_core::input::mapping::wheel_data`): positive
                // dy is the wheel turned away from the user (content up), positive dx a right
                // tilt; both pass through unchanged.
                let (vertical, horizontal) = wheel_data(*dx, *dy);
                if let Some(d) = vertical {
                    events.push(os::mouse(MOUSEEVENTF_WHEEL, 0, 0, d));
                }
                if let Some(d) = horizontal {
                    events.push(os::mouse(MOUSEEVENTF_HWHEEL, 0, 0, d));
                }
                self.send(&events)
            }
            InputMessage::Key { code, down, mods } => self.key(code, *down, mods),
            InputMessage::Text { s } => {
                let mut events = Vec::new();
                for e in text_events(s)? {
                    match e {
                        TextEvent::Unicode(u) => {
                            events.push(os::key(0, u, KEYEVENTF_UNICODE));
                            events.push(os::key(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                        }
                        TextEvent::Key(k) => {
                            events.push(key_event(k, false));
                            events.push(key_event(k, true));
                        }
                    }
                }
                if events.is_empty() {
                    return Ok(());
                }
                self.send(&events)
            }
            InputMessage::ClipboardGet {} | InputMessage::Clipboard { .. } => {
                Err(Refusal::Unsupported)
            }
        }
    }

    /// Maps a pointer position on the live geometry and runs the pre-click check.
    fn pointer(
        &self,
        x: f64,
        y: f64,
        mode: OutOfRange,
    ) -> Result<(Point, gander_core::launch::discovery::Rect), Refusal> {
        let h = self.target.handle();
        let g = os::geometry(h).ok_or(Refusal::WindowGone)?;
        let p = map_normalised(x, y, g.picture(self.options.picture), mode)?;
        match os::hit_test(h, p) {
            os::Hit::Target => Ok((p, g.desktop)),
            os::Hit::Other { pid, class } => Err(Refusal::Covered { pid, class }),
            os::Hit::Nothing => Err(Refusal::NoWindowUnderPoint),
        }
    }

    fn held_modifiers(&self) -> BTreeSet<Modifier> {
        self.keys.values().filter_map(|k| k.modifier).collect()
    }

    fn key(&mut self, code: &str, down: bool, mods: &[Modifier]) -> Result<(), Refusal> {
        if !down {
            // Only keys this session pressed are released; anything else is a no-op.
            let Some(held) = self.keys.get(code).cloned() else {
                return Ok(());
            };
            let mut events = vec![key_event(held.spec, true)];
            events.extend(held.auto_mods.iter().rev().map(|m| key_event(*m, true)));
            self.send(&events)?;
            self.keys.remove(code);
            return Ok(());
        }
        let held_mods = self.held_modifiers();
        let spec = check_key(code, mods, &held_mods)?;
        let mut events = Vec::new();
        let mut auto_mods = Vec::new();
        let repeat = self.keys.contains_key(code);
        if !repeat {
            for m in mods {
                if held_mods.contains(m) || modifier_of(code) == Some(*m) {
                    continue;
                }
                if let Some(ms) = key_for_code(modifier_code(*m)) {
                    events.push(key_event(ms, false));
                    auto_mods.push(ms);
                }
            }
        }
        events.push(key_event(spec, false));
        self.send(&events)?;
        if !repeat {
            self.keys.insert(
                code.to_owned(),
                HeldKey {
                    spec,
                    modifier: modifier_of(code),
                    auto_mods,
                },
            );
        }
        Ok(())
    }

    fn send(&mut self, events: &[INPUT]) -> Result<(), Refusal> {
        let sent = os::send(events);
        self.log.record(sent.ticks.0, sent.ticks.1);
        record_global(sent.ticks);
        if sent.accepted as usize == events.len() {
            Ok(())
        } else {
            Err(Refusal::Blocked {
                accepted: sent.accepted,
                expected: events.len() as u32,
                error: sent.error,
            })
        }
    }

    fn release_held(&mut self, send: bool) {
        let mut events = Vec::new();
        for held in std::mem::take(&mut self.keys).into_values() {
            events.push(key_event(held.spec, true));
            events.extend(held.auto_mods.iter().rev().map(|m| key_event(*m, true)));
        }
        for b in std::mem::take(&mut self.buttons) {
            events.push(os::mouse(b.flags(false), 0, 0, 0));
        }
        if send && !events.is_empty() {
            let _ = self.send(&events);
        }
    }

    /// Releases every key and button this session holds (nothing is sent while the session is
    /// locked).
    pub fn release_all(&mut self) {
        let unlocked = !(self.locked)();
        self.release_held(unlocked);
    }
}

impl Drop for InputController {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// Every injection of this process, for [`local_idle_duration`].
static GLOBAL: Mutex<LocalIdle> = Mutex::new(LocalIdle {
    log: None,
    last_local: None,
});

#[derive(Debug)]
struct LocalIdle {
    log: Option<InjectionLog>,
    last_local: Option<u32>,
}

fn global() -> std::sync::MutexGuard<'static, LocalIdle> {
    GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn record_global(ticks: (u32, u32)) {
    global()
        .log
        .get_or_insert_with(InjectionLog::new)
        .record(ticks.0, ticks.1);
}

/// Time since the last input in this session that Gander did **not** inject: the `host.state`
/// `attended` input (`GetLastInputInfo` alone would count the phone's own taps as someone at the
/// computer). Until the first input after start is classified, the plain idle time is returned
/// (unknown origin counts as local: the safe side is to assume someone is there).
pub fn local_idle_duration() -> std::io::Result<Duration> {
    let t = os::last_input_tick().ok_or_else(std::io::Error::last_os_error)?;
    let now = os::tick();
    let mut g = global();
    let first = g.log.is_none();
    let origin = g.log.get_or_insert_with(InjectionLog::new).classify(t);
    if first || origin == Origin::Local || g.last_local.is_none() {
        g.last_local = Some(t);
    }
    let last = g.last_local.unwrap_or(t);
    Ok(Duration::from_millis(u64::from(now.wrapping_sub(last))))
}

/// Polls a shared controller until dropped.
#[derive(Debug)]
pub struct Poller {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Calls [`InputController::poll`] every `interval` on a background thread until the returned
/// [`Poller`] is dropped.
pub fn spawn_poller(controller: Arc<Mutex<InputController>>, interval: Duration) -> Poller {
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name("gander-input-poll".to_owned())
        .spawn({
            let stop = Arc::clone(&stop);
            move || {
                while !stop.load(Ordering::SeqCst) {
                    {
                        let mut c = controller
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let _dpi = DpiScope::per_monitor_v2();
                        c.poll();
                    }
                    std::thread::sleep(interval);
                }
            }
        })
        .ok();
    Poller { stop, thread }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gander_core::input::{RecordingSink, SinkRecord};

    fn locked_controller(sink: Arc<RecordingSink>) -> InputController {
        // A handle that is no window: nothing could be injected anyway, and the probe says
        // locked, so nothing is even attempted.
        InputController::new(
            InputTarget::new(0x7FFF_FFF0, 1),
            sink,
            Arc::new(|| true),
            InputOptions::default(),
        )
    }

    #[test]
    fn locked_session_refuses_everything_and_reports_once() {
        let sink = Arc::new(RecordingSink::default());
        let mut c = locked_controller(Arc::clone(&sink));
        assert_eq!(c.state(), GateState::Paused(PauseCause::Locked));
        let msgs = [
            InputMessage::Move { x: 0.5, y: 0.5 },
            InputMessage::Down {
                b: MouseButton::Left,
                x: 0.5,
                y: 0.5,
            },
            InputMessage::Text { s: "x".into() },
        ];
        for m in &msgs {
            assert_eq!(
                c.handle(ChannelKind::Control, m),
                Err(Refusal::Paused(PauseCause::Locked))
            );
        }
        assert_eq!(c.bring_to_front(), Err(Refusal::Paused(PauseCause::Locked)));
        assert_eq!(sink.states(), vec![GateState::Paused(PauseCause::Locked)]);
        assert_eq!(sink.refusals().len(), 3, "{:?}", sink.records());
    }

    #[test]
    fn channel_and_decode_refusals_come_before_the_gate() {
        let sink = Arc::new(RecordingSink::default());
        let mut c = locked_controller(Arc::clone(&sink));
        assert_eq!(
            c.handle_bytes(ChannelKind::Input, br#"{"t":"text","s":"hi"}"#),
            Err(Refusal::WrongChannel)
        );
        assert!(matches!(
            c.handle_bytes(ChannelKind::Control, b"not json"),
            Err(Refusal::Decode(_))
        ));
        assert_eq!(
            c.handle(ChannelKind::Control, &InputMessage::ClipboardGet {}),
            Err(Refusal::Unsupported)
        );
        assert!(
            sink.records()
                .iter()
                .any(|r| matches!(r, SinkRecord::Refused(MessageKind::Undecodable, _)))
        );
    }

    #[test]
    fn local_idle_is_readable_and_ignores_our_own_injections() {
        let first = local_idle_duration().expect("GetLastInputInfo");
        // Pretend an injection happened right at the current last-input tick: it must not make
        // the computer look attended.
        let t = os::last_input_tick().unwrap();
        record_global((t, t));
        let again = local_idle_duration().unwrap();
        assert!(
            again + Duration::from_millis(100) >= first,
            "{first:?} -> {again:?}"
        );
    }

    #[test]
    fn unlocked_but_gone_window_is_paused_for_focus() {
        let sink = Arc::new(RecordingSink::default());
        let mut c = InputController::new(
            InputTarget::new(0x7FFF_FFF0, 1),
            Arc::clone(&sink) as Arc<dyn InputSink>,
            Arc::new(|| false),
            InputOptions::default(),
        );
        // A window that does not exist is never the foreground window.
        assert_eq!(c.state(), GateState::Paused(PauseCause::FocusLost));
        assert_eq!(
            c.handle(ChannelKind::Input, &InputMessage::Move { x: 0.1, y: 0.1 }),
            Err(Refusal::Paused(PauseCause::FocusLost))
        );
        assert_eq!(c.bring_to_front(), Err(Refusal::WindowGone));
    }
}
