// SPDX-License-Identifier: Apache-2.0
//! Window discovery, fit-to-phone and restore against real `gander-testwin` processes (WP-51,
//! stream-launch-policy spec "Tests": new process, re-used process via a named pipe,
//! splash-then-main, two candidates -> pick).
//!
//! The launcher under test is `gander_win::launch`; it starts ONLY this crate's test window,
//! through the same policy gate as production (a static association table maps the made-up
//! extension `.gtw` to the test window, and an approved catalogue entry carries the launch
//! template). The "file" is an empty fixture in a temp folder; the test window never opens it,
//! it only uses the path as its title. Every window shown is shown without activation, every
//! window moved is restored, and every process is quit over its pipe (terminated as a last
//! resort) when the test ends.

#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gander_core::config::{AppCatalogue, AppEntry, CloseOnEnd, LaunchSpec, WindowMatch};
use gander_core::launch::discovery::{DiscoveryTiming, MatchReason};
use gander_core::launch::plan::{Authorisation, PlanError};
use gander_core::policy::{
    HandlerInfo, PolicyDecision, PolicyEnvironment, PolicyInput, Signature, StaticAssociations,
    StreamPolicy,
};
use gander_core::protocol::AppId;
use gander_core::protocol::files::StreamDecision;
use gander_core::protocol::stream::Viewport;
use gander_testwin::protocol::pipe_name;
use gander_win::launch::{
    DiscoveryOutcome, LaunchError, LaunchRequest, Launched, WindowError, WindowTarget,
    fit_to_phone, launch_and_discover, placement, restore, secondary_windows,
    target_from_candidate,
};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos,
};
use windows::core::BOOL;

const EXE: &str = env!("CARGO_BIN_EXE_gander-testwin");
const EXT: &str = "gtw";
const MAIN_CLASS: &str = "GanderTestWindow";

/// Quits test windows over their pipe when dropped (terminating any that do not exit).
#[derive(Default)]
struct Cleanup(Vec<u32>, Vec<Child>);

impl Cleanup {
    fn add(&mut self, pid: u32) {
        if !self.0.contains(&pid) {
            self.0.push(pid);
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for &pid in &self.0 {
            let _ = request(pid, "quit");
            // SAFETY: plain calls on a handle opened here and closed below.
            unsafe {
                if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid) {
                    if WaitForSingleObject(h, 3000).0 != 0 {
                        let _ = TerminateProcess(h, 9);
                    }
                    let _ = CloseHandle(h);
                }
            }
        }
        for child in &mut self.1 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One request on a test window's control pipe.
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

/// A unique, empty fixture file `<stem>.gtw` in a per-process temp folder.
fn fixture(stem: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gander-wp51-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{stem}.{EXT}"));
    std::fs::write(&path, b"").unwrap();
    path
}

/// The test window's path as the policy sees a handler (verbatim prefix removed).
fn testwin_exe() -> PathBuf {
    let p = std::fs::canonicalize(EXE).unwrap();
    let s = p.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned())
}

struct Setup {
    catalogue: AppCatalogue,
    decision: PolicyDecision,
    file: PathBuf,
}

/// Runs the real policy for `stem.gtw` with an approved catalogue entry for the test window.
fn setup(stem: &str, template: &str, window_match: WindowMatch) -> Setup {
    let exe = testwin_exe();
    let catalogue = AppCatalogue {
        apps: vec![AppEntry {
            id: AppId::from("testwin".to_owned()),
            name: "Gander test window".into(),
            exe: exe.clone(),
            publisher: None,
            extensions: vec![EXT.into()],
            approved_by_owner: Some(time::OffsetDateTime::UNIX_EPOCH),
            launch: LaunchSpec::Template(template.into()),
            window_match,
            close_on_end: CloseOnEnd::IfLaunchedByUs,
            slow_start: false,
            package_family_name: None,
        }],
        ..AppCatalogue::default()
    };
    let assoc = StaticAssociations::new().with(EXT, HandlerInfo::new(exe, Signature::Unsigned));
    let file = fixture(stem);
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let env = PolicyEnvironment::default();
    let decision = StreamPolicy::new(&catalogue, &assoc, &env).evaluate(&PolicyInput {
        file_name: &name,
        stream_permitted: true,
        share_default: StreamDecision::Ask,
        requested_app: None,
    });
    assert_eq!(decision.outcome, StreamDecision::Allowed, "{decision:?}");
    Setup {
        catalogue,
        decision,
        file,
    }
}

fn timing(timeout_s: u64) -> DiscoveryTiming {
    DiscoveryTiming {
        poll: Duration::from_millis(100),
        timeout: Duration::from_secs(timeout_s),
        settle: Duration::from_millis(400),
    }
}

fn launch(s: &Setup, timeout_s: u64, viewport: Option<Viewport>) -> Result<Launched, LaunchError> {
    launch_and_discover(&LaunchRequest {
        decision: &s.decision,
        catalogue: &s.catalogue,
        file: &s.file,
        authorisation: Authorisation::Policy,
        browser_profile_dir: None,
        viewport,
        timing: Some(timing(timeout_s)),
    })
}

fn unique(tag: &str) -> String {
    format!("wp51 {tag} {}", std::process::id())
}

fn expect_found(l: &Launched) -> &WindowTarget {
    match &l.outcome {
        DiscoveryOutcome::Found(t) => t,
        other => panic!("expected Found, got {other:?}"),
    }
}

fn main_hwnd(pid: u32) -> u64 {
    let reply = request(pid, "windows?").expect("windows?");
    let first = field(&reply, "hwnd=0x").split(',').next().unwrap();
    u64::from_str_radix(first, 16).unwrap()
}

const BASE: &str = "{exe} --self-test-secs 90 --no-focus";

#[test]
fn new_process_found_by_process_tree_with_owned_dialog() {
    let stem = unique("newproc");
    let s = setup(
        &stem,
        &format!("{BASE} --title {{file}}"),
        WindowMatch::NewWindowOfProcess,
    );
    let mut cleanup = Cleanup::default();
    let launched = launch(&s, 10, None).expect("launch");
    cleanup.add(launched.pid);
    let t = expect_found(&launched);
    assert_eq!(t.pid, launched.pid);
    assert_eq!(t.class, MAIN_CLASS);
    assert!(t.title.contains(&stem), "{}", t.title);
    assert!(t.launched_by_us);
    assert_eq!(t.hwnd, main_hwnd(t.pid));
    assert_eq!(launched.close_on_end, CloseOnEnd::IfLaunchedByUs);

    // A modal dialog owned by the window is reported as a secondary window.
    assert_eq!(request(t.pid, "dialog").as_deref(), Some("ok"));
    let deadline = Instant::now() + Duration::from_secs(5);
    let owned = loop {
        let owned = secondary_windows(t);
        if !owned.is_empty() || Instant::now() > deadline {
            break owned;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(request(t.pid, "dismiss").as_deref(), Some("ok"));
    assert_eq!(owned.len(), 1, "{owned:?}");
    assert_eq!(owned[0].class, "#32770");
    assert_eq!(owned[0].owner, Some(t.hwnd));

    // Gone after quitting: every window call refuses instead of touching a reused handle.
    let mut t = t.clone();
    drop(cleanup);
    let vp = Viewport {
        width: 1080,
        height: 2400,
        dpr: 2.625,
    };
    assert!(matches!(
        fit_to_phone(&mut t, vp, None),
        Err(WindowError::Gone)
    ));
}

#[test]
fn new_process_found_by_title() {
    let stem = unique("title");
    let s = setup(
        &stem,
        &format!("{BASE} --title {{file}}"),
        WindowMatch::TitleContainsFilename,
    );
    let mut cleanup = Cleanup::default();
    let launched = launch(&s, 10, None).expect("launch");
    cleanup.add(launched.pid);
    let t = expect_found(&launched);
    assert_eq!(t.pid, launched.pid);
    assert_eq!(t.class, MAIN_CLASS);
    assert!(t.title.contains(&stem));
}

/// Starts a `--reuse` instance directly and returns its pid (from the ready line).
fn start_reuse_instance(key: &str, title: &str, cleanup: &mut Cleanup) -> u32 {
    let mut child = Command::new(EXE)
        .args([
            "--self-test-secs",
            "90",
            "--no-focus",
            "--reuse",
            key,
            "--title",
            title,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the first instance");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("gander-testwin ready "), "{line:?}");
    let pid = field(&line, "pid=").parse().unwrap();
    cleanup.add(pid);
    cleanup.1.push(child);
    pid
}

#[test]
fn reused_process_via_pipe() {
    let stem = unique("reuse");
    let key = format!("wp51-{}-a", std::process::id());
    let mut cleanup = Cleanup::default();
    let first = start_reuse_instance(&key, "Existing instance", &mut cleanup);

    // The launched process forwards the file to the running instance and exits; the window is
    // a new window of the OTHER process, found by its title.
    let s = setup(
        &stem,
        &format!("{BASE} --reuse {key} --title {{file}}"),
        WindowMatch::TitleContainsFilename,
    );
    let launched = launch(&s, 10, None).expect("launch");
    let t = expect_found(&launched);
    assert_ne!(launched.pid, first);
    assert_eq!(t.pid, first, "the window belongs to the running instance");
    assert!(!t.launched_by_us);
    assert!(t.title.contains(&stem));
    assert_ne!(
        t.hwnd,
        main_hwnd(first),
        "a new window, not the existing one"
    );
    let reply = request(first, "windows?").unwrap();
    assert_eq!(field(&reply, "count="), "2");
}

#[test]
fn reused_process_with_process_strategy_falls_back_to_pick() {
    let stem = unique("reusepick");
    let key = format!("wp51-{}-b", std::process::id());
    let mut cleanup = Cleanup::default();
    let first = start_reuse_instance(&key, "Existing instance", &mut cleanup);
    let s = setup(
        &stem,
        &format!("{BASE} --reuse {key} --title {{file}}"),
        WindowMatch::NewWindowOfProcess,
    );
    let launched = launch(&s, 3, None).expect("launch");
    let DiscoveryOutcome::Pick(c) = &launched.outcome else {
        panic!(
            "expected Pick after the timeout, got {:?}",
            launched.outcome
        )
    };
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].window.pid, first);
    assert_eq!(c[0].reason, MatchReason::Title);
    let t = target_from_candidate(&c[0], false).expect("still there");
    assert_eq!(t.pid, first);
}

#[test]
fn splash_then_main() {
    for (tag, wm) in [
        ("splashtitle", WindowMatch::TitleContainsFilename),
        ("splashproc", WindowMatch::NewWindowOfProcess),
    ] {
        let stem = unique(tag);
        // The splash has class MsoSplash and the SAME title as the main window, so only the
        // skip list keeps it from being chosen.
        let s = setup(&stem, &format!("{BASE} --splash 1500 --title {{file}}"), wm);
        let mut cleanup = Cleanup::default();
        let started = Instant::now();
        let launched = launch(&s, 10, None).expect("launch");
        cleanup.add(launched.pid);
        let t = expect_found(&launched);
        assert_eq!(t.class, MAIN_CLASS, "{tag}: the splash was skipped");
        assert_eq!(t.hwnd, main_hwnd(t.pid), "{tag}");
        assert!(
            started.elapsed() >= Duration::from_millis(1500),
            "{tag}: discovery waited for the main window"
        );
    }
}

#[test]
fn two_candidates_pick() {
    let stem = unique("twin");
    let s = setup(
        &stem,
        &format!("{BASE} --twin --title {{file}}"),
        WindowMatch::NewWindowOfProcess,
    );
    let mut cleanup = Cleanup::default();
    let launched = launch(&s, 10, None).expect("launch");
    cleanup.add(launched.pid);
    let DiscoveryOutcome::Pick(c) = &launched.outcome else {
        panic!("expected Pick, got {:?}", launched.outcome)
    };
    assert_eq!(c.len(), 2, "{c:?}");
    assert!(c.iter().all(|c| c.window.pid == launched.pid));
    assert!(c.iter().all(|c| c.window.title.contains(&stem)));
    assert!(c.iter().any(|c| c.window.title.ends_with("(second)")));
    // The user picks the second; it becomes the target.
    let second = c
        .iter()
        .find(|c| c.window.title.ends_with("(second)"))
        .unwrap();
    let t = target_from_candidate(second, true).unwrap();
    assert_eq!(t.hwnd, second.window.hwnd);
}

#[test]
fn refused_and_unapproved_decisions_launch_nothing() {
    let stem = unique("refused");
    let mut s = setup(
        &stem,
        &format!("{BASE} --title {{file}}"),
        WindowMatch::NewWindowOfProcess,
    );
    let mut refused = s.decision.clone();
    refused.outcome = StreamDecision::Refused;
    let mut ask = s.decision.clone();
    ask.outcome = StreamDecision::Ask;
    for (d, want_refused) in [(refused, true), (ask, false)] {
        s.decision = d;
        match launch(&s, 2, None) {
            Err(LaunchError::Plan(PlanError::Refused(_))) if want_refused => {}
            Err(LaunchError::Plan(PlanError::NeedsOwnerApproval)) if !want_refused => {}
            other => panic!("expected a plan refusal, got {other:?}"),
        }
    }
}

struct Monitor {
    work: RECT,
}

unsafe extern "system" fn monitor_cb(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<Monitor>` passed below; `info` is correctly sized.
    unsafe {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(m, &mut info).as_bool() {
            list.push(Monitor { work: info.rcWork });
        }
    }
    true.into()
}

fn monitors() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    // SAFETY: the callback only pushes into `list`, which outlives the call. Per-monitor v2 for
    // this thread, or the work areas come back virtualised (scaled to 96 dpi).
    unsafe {
        let old = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(monitor_cb),
            LPARAM(&mut list as *mut Vec<Monitor> as isize),
        );
        SetThreadDpiAwarenessContext(old);
    }
    list
}

/// Moves a window (without resizing or activating it) to a point, in physical pixels.
fn move_to(hwnd: u64, x: i32, y: i32) {
    // SAFETY: thread-level DPI context for this call only; SetWindowPos on a test window.
    unsafe {
        let old = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        SetWindowPos(
            HWND(hwnd as usize as *mut _),
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
        .expect("move the test window");
        SetThreadDpiAwarenessContext(old);
    }
}

/// `layout?` from the window itself: its own client size and dpi.
fn window_view(pid: u32) -> (u32, u32, u32) {
    let reply = request(pid, "layout?").unwrap();
    let dpi = field(&reply, "dpi=").parse().unwrap();
    let (w, h) = field(&reply, "client=").split_once('x').unwrap();
    (w.parse().unwrap(), h.parse().unwrap(), dpi)
}

#[test]
fn fit_to_phone_and_restore_on_every_monitor() {
    let stem = unique("fit");
    let s = setup(
        &stem,
        &format!("{BASE} --title {{file}}"),
        WindowMatch::NewWindowOfProcess,
    );
    let mut cleanup = Cleanup::default();
    let phone = Viewport {
        width: 1080,
        height: 2400,
        dpr: 2.625,
    };
    // Found and fitted in one call.
    let launched = launch(&s, 10, Some(phone)).expect("launch");
    cleanup.add(launched.pid);
    assert_eq!(launched.fit_error, None);
    let mut t = expect_found(&launched).clone();
    let before = t.saved_placement().expect("placement saved before the fit");
    let fit = t.fit.expect("fitted");
    assert_eq!(
        fit.client, fit.plan.client,
        "the window took the planned size"
    );
    let (w, h, dpi) = window_view(t.pid);
    assert_eq!((w, h, dpi), (fit.client.width, fit.client.height, fit.dpi));
    println!(
        "launch fit: dpi={} client={}x{} limit={:?} scale={:.3}",
        fit.dpi, fit.client.width, fit.client.height, fit.plan.limit, fit.plan.scale
    );
    restore(&mut t).expect("restore");
    assert_eq!(placement(&t).unwrap(), before, "restored exactly");

    // Then on every monitor this machine has (150 % internal and 100 % external here).
    let tablet = Viewport {
        width: 1600,
        height: 2560,
        dpr: 2.0,
    };
    for (i, m) in monitors().iter().enumerate() {
        move_to(t.hwnd, m.work.left + 40, m.work.top + 40);
        let original = placement(&t).unwrap();
        for vp in [phone, tablet] {
            let fit = fit_to_phone(&mut t, vp, None).expect("fit");
            let (w, h, dpi) = window_view(t.pid);
            assert_eq!((w, h, dpi), (fit.client.width, fit.client.height, fit.dpi));
            assert_eq!(fit.client, fit.plan.client, "monitor {i}");
            // The window stays inside the monitor's work area.
            assert!(fit.outer.left >= m.work.left && fit.outer.right <= m.work.right);
            assert!(
                fit.outer.top >= m.work.top && fit.outer.bottom <= m.work.bottom,
                "monitor {i}: plan {:?} actual {:?} work {:?}",
                fit.plan.outer,
                fit.outer,
                m.work
            );
            println!(
                "monitor {i}: work={}x{} dpi={} viewport={}x{}@{} -> client={}x{} limit={:?} \
                 scale={:.3}",
                m.work.right - m.work.left,
                m.work.bottom - m.work.top,
                fit.dpi,
                vp.width,
                vp.height,
                vp.dpr,
                fit.client.width,
                fit.client.height,
                fit.plan.limit,
                fit.plan.scale
            );
            // Fitting twice keeps the first saved placement.
            assert_eq!(t.saved_placement(), Some(original));
        }
        restore(&mut t).expect("restore");
        assert_eq!(placement(&t).unwrap(), original, "monitor {i} restored");
    }
    // Back where discovery found it (workspace and screen coordinates agree with a bottom
    // taskbar; the window is quit next anyway).
    move_to(t.hwnd, before.normal.left, before.normal.top);
}
