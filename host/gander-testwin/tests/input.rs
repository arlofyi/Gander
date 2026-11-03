// SPDX-License-Identifier: Apache-2.0
//! Stream input against real `gander-testwin` processes (WP-52): injected clicks at the
//! available DPI settings, focus-loss and local-activity pauses, the covered-window refusal,
//! keys and text, foreground acquisition, and the locked-session refusal.
//!
//! The live tests are `#[ignore]`: they move the real cursor and type, so they run only on
//! request, on an unlocked and attended desktop:
//!
//! ```text
//! cargo test -p gander-testwin --test input -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Rules every test keeps: input goes ONLY to test windows started here (the controller's own
//! gate refuses keys unless a test window is the foreground window, and the pre-click check
//! refuses clicks that would land on anything else); the Alt-key fallback is enabled only while
//! the window in front is one of these test windows; the cursor is put back after each test;
//! every process is quit (terminated as a last resort). If the session is locked the live tests
//! print `DEFERRED` and pass without injecting.

#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gander_core::input::mapping::normalise;
use gander_core::input::{
    ChannelKind, GateState, InputSink, PauseCause, Point, RecordingSink, Refusal,
};
use gander_core::protocol::SessionId;
use gander_core::protocol::input::{InputMessage, Modifier, MouseButton};
use gander_testwin::protocol::pipe_name;
use gander_win::input::foreground::{self, Method};
use gander_win::input::{
    AcquirePolicy, InputController, InputOptions, InputTarget, query_lock_probe,
};
use gander_win::session;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, SetWindowPos,
};
use windows::core::BOOL;

const EXE: &str = env!("CARGO_BIN_EXE_gander-testwin");
const CLASS: &str = "GanderTestWindow";

/// The live tests share the desktop (foreground, cursor): one at a time.
static DESKTOP: Mutex<()> = Mutex::new(());

fn desktop() -> MutexGuard<'static, ()> {
    DESKTOP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Physical coordinates for every call on this thread.
fn per_monitor_v2() {
    // SAFETY: plain call with a predefined context.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// A test window process; quit (or killed) on drop.
struct Win {
    child: Child,
    pid: u32,
    hwnd: u64,
}

impl Win {
    fn spawn(title: &str, extra: &[&str]) -> Win {
        let mut child = Command::new(EXE)
            .args(["--self-test-secs", "90"])
            .args(extra)
            .args(["--title", title])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn gander-testwin");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .expect("ready line");
        assert!(line.starts_with("gander-testwin ready "), "{line:?}");
        let pid = field(&line, "pid=").parse().unwrap();
        let hwnd = u64::from_str_radix(field(&line, "hwnd=0x"), 16).unwrap();
        Win { child, pid, hwnd }
    }

    fn target(&self) -> InputTarget {
        InputTarget::new(self.hwnd, self.pid)
    }

    fn handle(&self) -> HWND {
        HWND(self.hwnd as usize as *mut _)
    }

    fn ask(&self, line: &str) -> String {
        request(self.pid, line).unwrap_or_else(|| panic!("no reply to {line}"))
    }

    fn counts(&self) -> (u32, u32) {
        let r = self.ask("colour?");
        (
            field(&r, "hits=").parse().unwrap(),
            field(&r, "clicks=").parse().unwrap(),
        )
    }

    fn typed(&self) -> (u32, String) {
        let r = self.ask("typed?");
        let hex = field(&r, "text=");
        let bytes: Vec<u8> = if hex == "-" {
            Vec::new()
        } else {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect()
        };
        (
            field(&r, "keydowns=").parse().unwrap(),
            String::from_utf8(bytes).unwrap(),
        )
    }

    /// The box centre: client point (window units) and its normalised picture coordinates.
    fn aim(&self) -> ((i32, i32), (f64, f64)) {
        let r = self.ask("state?");
        let b: Vec<i32> = field(&r, "box=")
            .split(',')
            .map(|v| v.parse().unwrap())
            .collect();
        let client = (b[0] + b[2] / 2, b[1] + b[3] / 2);
        let mut p = POINT {
            x: client.0,
            y: client.1,
        };
        // SAFETY: valid in/out point; the thread is per-monitor v2, so the result is physical.
        let _ = unsafe { ClientToScreen(self.handle(), &mut p) };
        let g = self.target().geometry().expect("geometry");
        (client, normalise(Point::new(p.x, p.y), g.frame))
    }

    fn last_click(&self) -> Option<(i32, i32)> {
        let r = self.ask("state?");
        let v = field(&r, "last_click=");
        v.split_once(',')
            .map(|(x, y)| (x.parse().unwrap(), y.parse().unwrap()))
    }

    fn dpi(&self) -> u32 {
        field(&self.ask("layout?"), "dpi=").parse().unwrap()
    }
}

impl Drop for Win {
    fn drop(&mut self) {
        let _ = request(self.pid, "quit");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn request(pid: u32, line: &str) -> Option<String> {
    let name = pipe_name(pid);
    let deadline = Instant::now() + Duration::from_secs(3);
    let f = loop {
        match OpenOptions::new().read(true).write(true).open(&name) {
            Ok(f) => break f,
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(_) => return None,
        }
    };
    let mut w = &f;
    w.write_all(format!("{line}\n").as_bytes()).ok()?;
    let mut reply = String::new();
    BufReader::new(&f).read_line(&mut reply).ok()?;
    Some(reply.trim_end().to_owned())
}

fn field<'a>(reply: &'a str, key: &str) -> &'a str {
    reply
        .split_whitespace()
        .find_map(|w| w.strip_prefix(key))
        .unwrap_or_else(|| panic!("no {key} in {reply:?}"))
}

/// Puts the cursor back where it was.
struct CursorGuard(Option<Point>);

impl CursorGuard {
    fn save() -> Self {
        Self(gander_win::input::cursor())
    }
}

impl Drop for CursorGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0 {
            gander_win::input::set_cursor(p);
        }
    }
}

fn unlocked() -> bool {
    let s = session::query_session_state();
    eprintln!("WP-52 session state: {s:?}");
    s.is_ok_and(|s| s.is_interactive())
}

fn foreground_class() -> String {
    // SAFETY: plain calls; valid buffer.
    unsafe {
        let h = GetForegroundWindow();
        let mut buf = [0u16; 128];
        let n = GetClassNameW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Brings `w` to the front; the Alt fallback only while a test window is in front.
fn front(w: &Win) -> foreground::ForegroundReport {
    let policy = AcquirePolicy {
        alt_key: foreground_class() == CLASS,
        ..AcquirePolicy::default()
    };
    let mut log = gander_core::input::activity::InjectionLog::new();
    foreground::acquire(&w.target(), &policy, &mut log).expect("acquire")
}

fn controller(w: &Win, sink: &Arc<RecordingSink>) -> InputController {
    InputController::new(
        w.target(),
        Arc::clone(sink) as Arc<dyn InputSink>,
        query_lock_probe(),
        InputOptions {
            foreground: AcquirePolicy {
                alt_key: false,
                ..AcquirePolicy::default()
            },
            ..InputOptions::default()
        },
    )
}

fn down(x: f64, y: f64) -> InputMessage {
    InputMessage::Down {
        b: MouseButton::Left,
        x,
        y,
    }
}

fn up(x: f64, y: f64) -> InputMessage {
    InputMessage::Up {
        b: MouseButton::Left,
        x,
        y,
    }
}

/// Clicks the box `n` times, each aimed fresh; returns (hits gained, max |last_click - aim|).
fn click_box(w: &Win, c: &mut InputController, n: u32) -> (u32, i32) {
    let (h0, _) = w.counts();
    let mut worst = 0;
    for _ in 0..n {
        let _cursor = CursorGuard::save();
        let (client, (nx, ny)) = w.aim();
        c.handle(ChannelKind::Input, &down(nx, ny))
            .expect("down injected");
        c.handle(ChannelKind::Input, &up(nx, ny))
            .expect("up injected");
        std::thread::sleep(Duration::from_millis(60));
        if let Some(lc) = w.last_click() {
            worst = worst
                .max((lc.0 - client.0).abs())
                .max((lc.1 - client.1).abs());
        }
    }
    (w.counts().0 - h0, worst)
}

struct MonitorRect {
    work: RECT,
}

unsafe extern "system" fn monitor_cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<MonitorRect>` passed below; `info` is correctly sized.
    unsafe {
        let list = &mut *(data.0 as *mut Vec<MonitorRect>);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(m, &mut info).as_bool() {
            list.push(MonitorRect { work: info.rcWork });
        }
    }
    true.into()
}

fn monitors() -> Vec<MonitorRect> {
    let mut list = Vec::new();
    // SAFETY: the callback only pushes into `list`, which outlives the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_cb),
            LPARAM(&mut list as *mut Vec<MonitorRect> as isize),
        );
    }
    list
}

fn move_window(w: &Win, x: i32, y: i32) {
    // SAFETY: SetWindowPos on our own test window, without activating it.
    unsafe {
        SetWindowPos(
            w.handle(),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .expect("move test window");
    }
    std::thread::sleep(Duration::from_millis(400));
}

// --- Always run: no real input ------------------------------------------------------------

/// The locked-session refusal: with the lock probe reporting locked (as `session::watch` does
/// for a locked, disconnected or unknown session) nothing is injected and the gate reports
/// `Paused(Locked)`. If the real session is locked right now, the real probe is checked too.
#[test]
fn locked_session_refuses_and_injects_nothing() {
    let _d = desktop();
    per_monitor_v2();
    let w = Win::spawn("wp52 locked refusal", &["--no-focus"]);
    let sink = Arc::new(RecordingSink::default());
    let mut c = InputController::new(
        w.target(),
        Arc::clone(&sink) as Arc<dyn InputSink>,
        Arc::new(|| true),
        InputOptions::default(),
    );
    let (_, (nx, ny)) = w.aim();
    for m in [
        InputMessage::Move { x: nx, y: ny },
        down(nx, ny),
        up(nx, ny),
        InputMessage::Text { s: "nope".into() },
        InputMessage::Key {
            code: "KeyA".into(),
            down: true,
            mods: vec![],
        },
    ] {
        assert_eq!(
            c.handle(ChannelKind::Control, &m),
            Err(Refusal::Paused(PauseCause::Locked))
        );
    }
    assert_eq!(c.bring_to_front(), Err(Refusal::Paused(PauseCause::Locked)));
    assert_eq!(w.counts(), (0, 0), "no click reached the window");
    assert_eq!(w.typed(), (0, String::new()), "no key reached the window");
    assert_eq!(sink.states(), vec![GateState::Paused(PauseCause::Locked)]);
    assert!(
        gander_core::input::channel::stream_state_event(
            SessionId::from("ses_test"),
            GateState::Paused(PauseCause::Locked),
            None
        )
        .is_none()
    );

    let real = session::query_session_state();
    if real.as_ref().is_ok_and(|s| s.is_locked()) {
        let mut c = InputController::new(
            w.target(),
            Arc::new(RecordingSink::default()),
            query_lock_probe(),
            InputOptions::default(),
        );
        assert_eq!(
            c.handle(ChannelKind::Input, &down(nx, ny)),
            Err(Refusal::Paused(PauseCause::Locked))
        );
        assert_eq!(w.counts(), (0, 0));
        eprintln!("WP-52: the real session is locked ({real:?}); the real probe refused too");
    } else {
        eprintln!("WP-52: the real session is unlocked ({real:?}); stub probe only");
    }
}

/// A gone window is refused, and a test window that is not in front pauses with focus_lost
/// (without injecting anything: the window was shown without activation and nothing brings it
/// forward).
#[test]
fn background_window_is_paused_not_injected() {
    let _d = desktop();
    per_monitor_v2();
    let w = Win::spawn("wp52 background", &["--no-focus"]);
    let sink = Arc::new(RecordingSink::default());
    let mut c = controller(&w, &sink);
    if c.state() == GateState::Paused(PauseCause::Locked) {
        eprintln!("WP-52: locked; focus check not reachable");
        return;
    }
    if w.target().is_foreground() {
        eprintln!("WP-52: the no-focus window came to front anyway; skipped");
        return;
    }
    let (_, (nx, ny)) = w.aim();
    assert_eq!(
        c.handle(ChannelKind::Input, &down(nx, ny)),
        Err(Refusal::Paused(PauseCause::FocusLost))
    );
    assert_eq!(w.counts(), (0, 0));
}

/// Live geometry and mapping on real windows, no input: on every monitor (150 % and 100 % on the
/// WP-52 machine), for a per-monitor-aware and a DPI-unaware (bitmap-scaled) test window,
/// 25 client pixels including the first and last are normalised against the DWM frame and mapped
/// back with 0 px error, and the pre-click check answers (the target, or, while the session is
/// locked, the lock screen covering it).
#[test]
fn live_geometry_maps_client_pixels_exactly_on_every_monitor() {
    let _d = desktop();
    per_monitor_v2();
    let locked = !unlocked();
    for (label, extra) in [
        ("aware", vec!["--no-focus"]),
        ("unaware", vec!["--no-focus", "--dpi-unaware"]),
    ] {
        let w = Win::spawn(&format!("wp52 geometry {label}"), &extra);
        for m in monitors() {
            move_window(&w, m.work.left + 30, m.work.top + 30);
            let g = w.target().geometry().expect("geometry");
            let (cw, ch) = (g.client.width(), g.client.height());
            assert!(cw > 0 && ch > 0);
            assert!(
                g.frame.left <= g.client.left
                    && g.frame.right >= g.client.right
                    && g.frame.bottom >= g.client.bottom,
                "client inside frame: {g:?}"
            );
            let layout = w.ask("layout?");
            let (lw, _) = field(&layout, "client=").split_once('x').unwrap();
            let lw: i32 = lw.parse().unwrap();
            let scale = f64::from(cw) / f64::from(lw);
            let mut worst = 0;
            for fx in [0.0, 0.001, 0.5, 0.999, 1.0] {
                for fy in [0.0, 0.001, 0.5, 0.999, 1.0] {
                    let p = Point::new(
                        g.client.left + ((f64::from(cw - 1)) * fx) as i32,
                        g.client.top + ((f64::from(ch - 1)) * fy) as i32,
                    );
                    let (nx, ny) = normalise(p, g.frame);
                    let back = gander_core::input::mapping::map_normalised(
                        nx,
                        ny,
                        g.frame,
                        gander_core::input::OutOfRange::Refuse,
                    )
                    .unwrap();
                    worst = worst.max((back.x - p.x).abs()).max((back.y - p.y).abs());
                }
            }
            let (_, (bx, by)) = w.aim();
            let check = w
                .target()
                .check_point(bx, by, gander_core::input::PictureArea::Frame);
            eprintln!(
                "WP-52 geometry: window={label} monitor_work={:?} window_dpi={} testwin_dpi={} frame={:?} client={}x{} logical_client_w={lw} scale={scale:.3} round_trip_error_px={worst} pre_click={check:?} locked={locked}",
                (m.work.left, m.work.top, m.work.right, m.work.bottom),
                g.dpi,
                w.dpi(),
                (g.frame.left, g.frame.top, g.frame.right, g.frame.bottom),
                cw,
                ch,
            );
            assert_eq!(worst, 0);
            if label == "aware" {
                assert_eq!(cw, lw, "an aware window's client is physical");
            }
            if locked {
                assert!(
                    matches!(&check, Err(Refusal::Covered { .. })),
                    "while locked the lock screen covers every point: {check:?}"
                );
            }
        }
    }
}

// --- Live: real cursor and keyboard -------------------------------------------------------

/// Verify: testwin reacts to injected clicks at the available DPI settings (`colour?` hits
/// increase), on every monitor, plus a DPI-unaware window (system bitmap scaling) on each.
#[test]
#[ignore = "moves the real cursor and clicks test windows; needs an unlocked, attended desktop"]
fn injected_clicks_hit_testwin_on_every_monitor() {
    let _d = desktop();
    per_monitor_v2();
    if !unlocked() {
        eprintln!("WP-52 DEFERRED: the session is locked; live SendInput clicks not run");
        return;
    }
    let mons = monitors();
    for (label, extra) in [("aware", vec![]), ("unaware", vec!["--dpi-unaware"])] {
        let w = Win::spawn(&format!("wp52 clicks {label}"), &extra);
        let sink = Arc::new(RecordingSink::default());
        for m in &mons {
            move_window(&w, m.work.left + 40, m.work.top + 40);
            let report = front(&w);
            assert!(report.acquired, "foreground: {report:?}");
            let mut c = controller(&w, &sink);
            // Settle the gate (our own move and activation must not count as local input).
            std::thread::sleep(Duration::from_millis(50));
            assert_eq!(c.poll(), GateState::Active, "{:?}", sink.records());
            let g = w.target().geometry().unwrap();
            let (hits, worst) = click_box(&w, &mut c, 5);
            eprintln!(
                "WP-52 clicks: window={label} monitor={:?} window_dpi={} testwin_dpi={} frame={:?} client={:?} desktop={:?} hits={hits}/5 last_click_error_px={worst} via={:?}",
                (m.work.left, m.work.top, m.work.right, m.work.bottom),
                g.dpi,
                w.dpi(),
                g.frame,
                g.client,
                g.desktop,
                report.method()
            );
            assert_eq!(hits, 5, "{label} on {:?}", (m.work.left, m.work.top));
            let tolerance = if label == "aware" { 0 } else { 1 };
            assert!(worst <= tolerance, "mapping error {worst} px ({label})");
        }
    }
}

/// Verify: focus-loss pause. The test brings a second test window to the front; the session
/// pauses with `focus_lost`, an injected click is refused (neither window counts it), and
/// "Bring to front" resumes.
#[test]
#[ignore = "changes the foreground window and clicks test windows; needs an unlocked, attended desktop"]
fn focus_loss_pauses_and_bring_to_front_resumes() {
    let _d = desktop();
    per_monitor_v2();
    if !unlocked() {
        eprintln!("WP-52 DEFERRED: the session is locked; focus-loss test not run");
        return;
    }
    let a = Win::spawn("wp52 focus A", &[]);
    let b = Win::spawn("wp52 focus B", &["--no-focus"]);
    let _cursor = CursorGuard::save();
    let sink = Arc::new(RecordingSink::default());
    assert!(front(&a).acquired);
    let mut c = controller(&a, &sink);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(c.poll(), GateState::Active);

    let rb = front(&b);
    assert!(rb.acquired, "B to front: {rb:?}");
    let state = c.poll();
    assert_eq!(state, GateState::Paused(PauseCause::FocusLost));
    let ev =
        gander_core::input::channel::stream_state_event(SessionId::from("ses_wp52"), state, None)
            .unwrap();
    let json = serde_json::to_string(&ev).unwrap();
    eprintln!("WP-52 focus lost -> {json}");
    assert!(json.contains(r#""state":"paused""#) && json.contains(r#""reason":"focus_lost""#));

    let (_, (nx, ny)) = a.aim();
    assert_eq!(
        c.handle(ChannelKind::Input, &down(nx, ny)),
        Err(Refusal::Paused(PauseCause::FocusLost))
    );
    assert_eq!(
        c.handle(ChannelKind::Control, &InputMessage::Text { s: "x".into() }),
        Err(Refusal::Paused(PauseCause::FocusLost))
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(a.counts().1, 0, "A got no click");
    assert_eq!(b.counts().1, 0, "B got no click");
    assert_eq!(b.typed().1, "", "B got no text");

    let report = c.bring_to_front().expect("bring to front");
    eprintln!("WP-52 bring to front after focus loss: {report:?}");
    assert!(report.acquired);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(c.poll(), GateState::Active);
    let (hits, _) = click_box(&a, &mut c, 2);
    assert_eq!(hits, 2);
    assert_eq!(
        sink.states(),
        vec![
            GateState::Active,
            GateState::Paused(PauseCause::FocusLost),
            GateState::Active
        ]
    );
}

/// Local user activity: input the controller did not inject (here a 1 px relative mouse move
/// sent by the test itself, outside the controller, standing in for the physical mouse) pauses
/// with `local_user_active`; 2 s later it resumes.
#[test]
#[ignore = "moves the real cursor by 1 px; needs an unlocked, attended desktop"]
fn foreign_input_pauses_for_local_activity_then_resumes() {
    let _d = desktop();
    per_monitor_v2();
    if !unlocked() {
        eprintln!("WP-52 DEFERRED: the session is locked; local-activity test not run");
        return;
    }
    let a = Win::spawn("wp52 local", &[]);
    let _cursor = CursorGuard::save();
    let sink = Arc::new(RecordingSink::default());
    assert!(front(&a).acquired);
    let mut c = controller(&a, &sink);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(c.poll(), GateState::Active);
    // Our own injections do not count as local activity.
    let (hits, _) = click_box(&a, &mut c, 1);
    assert_eq!(hits, 1);
    assert_eq!(c.poll(), GateState::Active, "{:?}", sink.records());

    std::thread::sleep(Duration::from_millis(300));
    let nudge = |dx: i32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: valid INPUT array and size.
    let n = unsafe { SendInput(&[nudge(1), nudge(-1)], std::mem::size_of::<INPUT>() as i32) };
    assert_eq!(n, 2);
    std::thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    assert_eq!(c.poll(), GateState::Paused(PauseCause::LocalUserActive));
    let (_, (nx, ny)) = a.aim();
    assert_eq!(
        c.handle(ChannelKind::Input, &down(nx, ny)),
        Err(Refusal::Paused(PauseCause::LocalUserActive))
    );
    while c.poll() != GateState::Active {
        assert!(start.elapsed() < Duration::from_secs(4), "did not resume");
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!(
        "WP-52 local activity: paused, resumed {:?} after detection",
        start.elapsed()
    );
}

/// The pre-click check: another window kept on top of the box (topmost, not activated) while
/// the target stays in front -> the click is refused as covered; nothing reaches either window.
#[test]
#[ignore = "changes window z-order and clicks test windows; needs an unlocked, attended desktop"]
fn covered_point_is_refused() {
    let _d = desktop();
    per_monitor_v2();
    if !unlocked() {
        eprintln!("WP-52 DEFERRED: the session is locked; covered-window test not run");
        return;
    }
    let a = Win::spawn("wp52 covered A", &[]);
    let b = Win::spawn("wp52 covered B", &["--no-focus"]);
    let _cursor = CursorGuard::save();
    let sink = Arc::new(RecordingSink::default());
    assert!(front(&a).acquired);
    let mut c = controller(&a, &sink);
    let g = a.target().geometry().unwrap();
    // B topmost over the whole of A's client area, without activation.
    // SAFETY: SetWindowPos on our own test window.
    unsafe {
        SetWindowPos(
            b.handle(),
            Some(HWND_TOPMOST),
            g.client.left,
            g.client.top,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(a.target().is_foreground(), "A must still be in front");
    assert_eq!(c.poll(), GateState::Active);
    let (_, (nx, ny)) = a.aim();
    let r = c.handle(ChannelKind::Input, &down(nx, ny));
    eprintln!("WP-52 covered click -> {r:?}");
    assert!(
        matches!(&r, Err(Refusal::Covered { pid, class }) if *pid == b.pid && class == CLASS),
        "{r:?}"
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(a.counts().1, 0);
    assert_eq!(b.counts().1, 0);
    // SAFETY: as above.
    unsafe {
        let _ = SetWindowPos(
            b.handle(),
            Some(HWND_NOTOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE,
        );
    }
}

/// Keys (scan codes) and text (Unicode) reach the test window when it is in front; keys on the
/// lossy channel and desktop shortcuts are refused.
#[test]
#[ignore = "types into a test window; needs an unlocked, attended desktop"]
fn keys_and_text_reach_testwin() {
    let _d = desktop();
    per_monitor_v2();
    if !unlocked() {
        eprintln!("WP-52 DEFERRED: the session is locked; keyboard test not run");
        return;
    }
    let a = Win::spawn("wp52 keys", &[]);
    let sink = Arc::new(RecordingSink::default());
    assert!(front(&a).acquired);
    let mut c = controller(&a, &sink);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(c.poll(), GateState::Active);
    assert!(
        a.target().is_foreground(),
        "never type unless the test window is in front"
    );

    let key = |code: &str, down: bool, mods: Vec<Modifier>| InputMessage::Key {
        code: code.into(),
        down,
        mods,
    };
    assert_eq!(
        c.handle(ChannelKind::Input, &key("KeyA", true, vec![])),
        Err(Refusal::WrongChannel)
    );
    assert!(matches!(
        c.handle(ChannelKind::Control, &key("MetaLeft", true, vec![])),
        Err(Refusal::Key(_))
    ));
    assert!(matches!(
        c.handle(ChannelKind::Control, &key("Tab", true, vec![Modifier::Alt])),
        Err(Refusal::Key(_))
    ));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(a.typed(), (0, String::new()), "refused keys sent nothing");

    c.handle(ChannelKind::Control, &key("KeyA", true, vec![]))
        .unwrap();
    c.handle(ChannelKind::Control, &key("KeyA", false, vec![]))
        .unwrap();
    // Shift+KeyB: an auto-pressed modifier, released with the key.
    c.handle(
        ChannelKind::Control,
        &key("KeyB", true, vec![Modifier::Shift]),
    )
    .unwrap();
    c.handle(
        ChannelKind::Control,
        &key("KeyB", false, vec![Modifier::Shift]),
    )
    .unwrap();
    let text = "Gander \u{2713} \u{e9} \u{1F600}";
    c.handle(ChannelKind::Control, &InputMessage::Text { s: text.into() })
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let (keydowns, typed) = a.typed();
    eprintln!("WP-52 keys: keydowns={keydowns} typed={typed:?}");
    // KeyA, ShiftLeft, KeyB (the Unicode events arrive as VK_PACKET key-downs too).
    assert!(keydowns >= 3, "{keydowns}");
    let mut chars = typed.chars();
    let first = chars.next().unwrap();
    let second = chars.next().unwrap();
    assert!(first.is_alphabetic() && first.is_lowercase(), "{typed:?}");
    assert!(second.is_alphabetic() && second.is_uppercase(), "{typed:?}");
    assert!(typed.ends_with(text), "{typed:?}");
}

/// Foreground acquisition findings: each method alone, with a second test window in front.
/// Prints one row per method; asserts only that the combined procedure works.
#[test]
#[ignore = "changes the foreground window and injects Alt into test windows; needs an unlocked, attended desktop"]
fn foreground_methods_one_by_one() {
    let _d = desktop();
    per_monitor_v2();
    if !unlocked() {
        eprintln!("WP-52 DEFERRED: the session is locked; foreground findings not run");
        return;
    }
    let a = Win::spawn("wp52 fg A", &["--no-focus"]);
    let b = Win::spawn("wp52 fg B", &[]);
    let mut log = gander_core::input::activity::InjectionLog::new();
    for method in [
        Method::SetForegroundWindow,
        Method::AllowSetForegroundWindow,
        Method::AttachThreadInput,
        Method::AltKey,
    ] {
        // B in front first (whatever it takes), so A has to be brought forward.
        if !b.target().is_foreground() {
            let r = front(&b);
            assert!(r.acquired, "setup: B to front {r:?}");
        }
        std::thread::sleep(Duration::from_millis(200));
        let fg_before = foreground_class();
        assert!(!a.target().is_foreground());
        if method == Method::AltKey && fg_before != CLASS {
            eprintln!("WP-52 foreground: AltKey skipped, the window in front is {fg_before}");
            continue;
        }
        let attempt =
            foreground::try_method(&a.target(), method, Duration::from_millis(300), &mut log)
                .unwrap();
        eprintln!(
            "WP-52 foreground: method={method:?} api_ok={} foreground_after={} (in front before: {fg_before})",
            attempt.api_ok, attempt.foreground_after
        );
        if a.target().is_foreground() {
            // Give B the front again for the next row.
            let r = front(&b);
            assert!(r.acquired, "reset: {r:?}");
        }
    }
    let report = front(&a);
    eprintln!("WP-52 foreground: full procedure -> {report:?}");
    assert!(report.acquired);
}
