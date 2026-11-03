// SPDX-License-Identifier: Apache-2.0
//! `gander-host --ui-smoke [--ui-out <dir>] [--ui-no-native]`: the headless smoke test of the UI.
//!
//! It starts no server, binds no port and reads no real configuration: the windows run against
//! [`DemoController`] sample data. Two passes:
//!
//! 1. **Offscreen** ([`super::shot`]): every window is rendered with egui's own tessellator and the
//!    CPU rasteriser to a PNG in `--ui-out` (default `docs/specs/assets/ui`). Nothing on the real
//!    desktop is captured. A blank render is a failure.
//! 2. **Native** (skipped with `--ui-no-native`): the real application runs, creates the tray icon
//!    (and cycles its four states), opens each window once (Shares, Devices, Add a device,
//!    Activity, Network, Settings) plus one toast, checks that each drew frames, closes them all
//!    so the eframe session ends and only the tray is left, opens the Gander window again in a
//!    second session, and quits through the same path as the tray's Quit. Windows are real but
//!    are not captured.
//!
//! Exit code 0 only if every step passed. A tray icon that cannot be created (no notification
//! area, as on a bare server) is reported as a warning, not a failure.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};

use super::controller::{FolderPicker, HostController};
use super::demo::{Call, DemoController};
use super::model::Page;
use super::picker::ScriptedPicker;
use super::tray_state::TrayState;

/// Where the screenshots go when `--ui-out` is not given.
pub const DEFAULT_OUT: &str = "docs/specs/assets/ui";

/// Frames a window must draw to count as opened.
pub const MIN_FRAMES: u32 = 3;

/// Longest the native pass may take.
pub const OVERALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest one step may wait for its window.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// What the native pass does next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Open the Gander window on a page.
    OpenPage(Page),
    /// Open the Add a device window (opens the pairing window).
    OpenAddDevice,
    /// Close it (closes the pairing window).
    CloseAddDevice,
    /// Show a toast.
    ShowToast,
    /// Open the tray flyout.
    OpenFlyout,
    /// Show one native notification from the tray icon.
    NativeNotice,
    /// Remove every toast.
    ClearToasts,
    /// Close every window and toast; waits until the session has ended (tray only).
    CloseAll,
    /// Open the Gander window again (a new session).
    Reopen,
    /// Show a tray state.
    TrayState(TrayState),
    /// Quit.
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Wait {
    Nothing,
    Frames(String, u32),
    Until(Instant),
    TrayOnly,
}

/// What the native pass saw.
#[derive(Debug, Default, Clone)]
pub struct SmokeReport {
    /// Frames drawn per window (`page:shares`, `add-device`, `toast`, ...).
    pub frames: BTreeMap<String, u32>,
    /// Tray states shown on the real icon.
    pub tray_states: Vec<&'static str>,
    /// Why the tray icon was not created, if it was not.
    pub tray_unavailable: Option<String>,
    /// Problems found.
    pub failures: Vec<String>,
    /// What happened to the native notification (`shown`, or why not).
    pub native_notice: Option<String>,
    /// How many times a session root's `update` ran (all sessions).
    pub root_updates: u64,
    /// How many eframe sessions ran.
    pub sessions: u32,
    /// Whether the application shut down through `on_exit`.
    pub finished: bool,
}

/// Steps the native pass through the windows. Shared with the app through a [`SmokeReport`].
#[derive(Debug)]
pub struct SmokeDriver {
    steps: VecDeque<Step>,
    wait: Wait,
    waiting_for: &'static str,
    wait_deadline: Instant,
    started: Instant,
    report: Arc<Mutex<SmokeReport>>,
    quitting: bool,
}

/// The steps of a full smoke run, in order.
pub fn script() -> Vec<Step> {
    let mut steps: Vec<Step> = Page::ALL.iter().map(|p| Step::OpenPage(*p)).collect();
    steps.push(Step::OpenAddDevice);
    steps.push(Step::ShowToast);
    steps.push(Step::OpenFlyout);
    steps.push(Step::NativeNotice);
    steps.extend(TrayState::ALL.iter().map(|s| Step::TrayState(*s)));
    steps.push(Step::ClearToasts);
    steps.push(Step::CloseAddDevice);
    steps.push(Step::CloseAll);
    steps.push(Step::Reopen);
    steps.push(Step::Quit);
    steps
}

/// The frame counter key of a settings page.
pub fn page_key(page: Page) -> String {
    format!("page:{}", page.title().to_lowercase())
}

impl SmokeDriver {
    /// A driver that reports into `report`.
    pub fn new(report: Arc<Mutex<SmokeReport>>) -> Self {
        Self::with_steps(report, script())
    }

    fn with_steps(report: Arc<Mutex<SmokeReport>>, steps: Vec<Step>) -> Self {
        let now = Instant::now();
        Self {
            steps: steps.into(),
            wait: Wait::Nothing,
            waiting_for: "start",
            wait_deadline: now,
            started: now,
            report,
            quitting: false,
        }
    }

    fn report(&self) -> std::sync::MutexGuard<'_, SmokeReport> {
        self.report.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records that the tray icon could not be created.
    pub fn note_tray_unavailable(&self, why: &str) {
        self.report().tray_unavailable = Some(why.to_owned());
    }

    /// Records a tray state shown.
    pub fn note_tray_state(&mut self, state: TrayState) {
        self.report().tray_states.push(state.name());
    }

    /// A tray state step ran without a tray (not a failure; the warning is already recorded).
    pub fn note_tray_state_skipped(&mut self, _state: TrayState) {}

    /// Records what the native notification did.
    pub fn note_native_notice(&mut self, result: Result<(), String>) {
        self.report().native_notice = Some(match result {
            Ok(()) => "shown".to_owned(),
            Err(e) => format!("not shown ({e})"),
        });
    }

    /// Records a failure.
    pub fn fail(&mut self, message: String) {
        self.report().failures.push(message);
    }

    /// The application is exiting: copies the final counters into the report.
    pub fn finish(
        &self,
        root_updates: u64,
        sessions: u32,
        frames: &std::collections::HashMap<String, u32>,
    ) {
        let mut r = self.report();
        r.root_updates = root_updates;
        r.sessions = sessions;
        r.frames = frames.iter().map(|(k, v)| (k.clone(), *v)).collect();
        r.finished = true;
    }

    /// The next step, if the previous one is done. Called every frame of a session and every
    /// tick of the tray-only loop (`in_session` false); `sessions` is how many sessions started.
    pub fn poll(
        &mut self,
        frames: &std::collections::HashMap<String, u32>,
        sessions: u32,
        in_session: bool,
        now: Instant,
    ) -> Option<Step> {
        if self.quitting {
            return None;
        }
        if now.duration_since(self.started) > OVERALL_TIMEOUT {
            self.fail(format!(
                "the native pass took longer than {OVERALL_TIMEOUT:?}"
            ));
            self.quitting = true;
            return Some(Step::Quit);
        }
        match &self.wait {
            Wait::Nothing => {}
            Wait::Until(t) => {
                if now < *t {
                    return None;
                }
            }
            Wait::TrayOnly => {
                if in_session {
                    if now > self.wait_deadline {
                        self.fail(format!(
                            "the session did not end within {STEP_TIMEOUT:?} after every window closed"
                        ));
                        self.wait = Wait::Nothing;
                    }
                    return None;
                }
            }
            Wait::Frames(key, min) => {
                if frames.get(key).copied().unwrap_or(0) < *min {
                    if now > self.wait_deadline {
                        let what = self.waiting_for;
                        let drawn = frames.get(key).copied().unwrap_or(0);
                        self.fail(format!(
                            "{what}: drew {drawn} frames in {STEP_TIMEOUT:?}, needed {min}"
                        ));
                        // Carry on: later windows are still worth checking.
                        self.wait = Wait::Nothing;
                    }
                    return None;
                }
            }
        }
        let step = self.steps.pop_front()?;
        self.wait_deadline = now + STEP_TIMEOUT;
        let (wait, what) = match step {
            Step::OpenPage(p) => (Wait::Frames(page_key(p), MIN_FRAMES), p.title()),
            Step::OpenAddDevice => (
                Wait::Frames("add-device".into(), MIN_FRAMES),
                "Add a device",
            ),
            Step::ShowToast => (Wait::Frames("toast".into(), 2), "toast"),
            Step::OpenFlyout => (Wait::Frames("flyout".into(), 2), "tray flyout"),
            Step::NativeNotice => (Wait::Until(now + Duration::from_millis(80)), "notification"),
            Step::TrayState(_) => (Wait::Until(now + Duration::from_millis(120)), "tray"),
            Step::ClearToasts => (Wait::Until(now + Duration::from_millis(80)), "toasts"),
            Step::CloseAddDevice => (Wait::Until(now + Duration::from_millis(120)), "close"),
            Step::CloseAll => (Wait::TrayOnly, "close all"),
            Step::Reopen => (
                Wait::Frames(format!("main@{}", sessions + 1), MIN_FRAMES),
                "Gander (second session)",
            ),
            Step::Quit => (Wait::Nothing, "quit"),
        };
        self.wait = wait;
        self.waiting_for = what;
        if step == Step::Quit {
            self.quitting = true;
        }
        Some(step)
    }
}

/// Checks a finished report against what the pairing window did on the controller.
pub fn evaluate(report: &SmokeReport, calls: &[Call]) -> Vec<String> {
    let mut problems = report.failures.clone();
    if !report.finished {
        problems.push("the application did not shut down cleanly".to_owned());
    }
    for p in Page::ALL {
        let key = page_key(p);
        let n = report.frames.get(&key).copied().unwrap_or(0);
        if n < MIN_FRAMES {
            problems.push(format!(
                "window {} drew {n} frames (needed {MIN_FRAMES})",
                p.title()
            ));
        }
    }
    for (key, name) in [
        ("add-device", "Add a device"),
        ("toast", "toast"),
        ("flyout", "tray flyout"),
    ] {
        let n = report.frames.get(key).copied().unwrap_or(0);
        let need = if key == "add-device" { MIN_FRAMES } else { 2 };
        if n < need {
            problems.push(format!("window {name} drew {n} frames (needed {need})"));
        }
    }
    if report.sessions < 2 {
        problems.push(format!(
            "{} window session(s) ran, needed 2: the window did not open again after closing",
            report.sessions
        ));
    }
    if report.root_updates < 20 {
        problems.push(format!(
            "the session roots only updated {} times: the event loop is not staying alive",
            report.root_updates
        ));
    }
    if report.tray_unavailable.is_none() && report.tray_states.len() != TrayState::ALL.len() {
        problems.push(format!(
            "the tray showed {} of {} states",
            report.tray_states.len(),
            TrayState::ALL.len()
        ));
    }
    let open = calls.iter().position(|c| *c == Call::OpenPairing);
    let close = calls.iter().rposition(|c| *c == Call::ClosePairing);
    match (open, close) {
        (Some(o), Some(c)) if o < c => {}
        _ => problems.push(
            "the pairing window was not opened and then closed with the Add a device window"
                .to_owned(),
        ),
    }
    if calls.contains(&Call::KillSwitch) || calls.iter().any(|c| matches!(c, Call::Approve(_))) {
        problems.push("the smoke run performed an action it must not".to_owned());
    }
    problems
}

/// Options of a smoke run.
#[derive(Debug, Clone)]
pub struct Options {
    /// Where the screenshots go.
    pub out_dir: PathBuf,
    /// Whether to run the native pass.
    pub native: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            out_dir: PathBuf::from(DEFAULT_OUT),
            native: true,
        }
    }
}

/// Runs the smoke test; prints a report; an error means a step failed.
pub fn run(options: &Options) -> anyhow::Result<()> {
    let mut failures: Vec<String> = Vec::new();

    println!(
        "ui-smoke: offscreen pass, writing screenshots to {}",
        options.out_dir.display()
    );
    match super::shot::render_all(&options.out_dir) {
        Ok(shots) => {
            for s in &shots {
                println!(
                    "ui-smoke: wrote {} ({}x{}, {} bytes, {} drawn pixels)",
                    s.path.display(),
                    s.width,
                    s.height,
                    s.bytes,
                    s.drawn_pixels
                );
                if s.drawn_pixels < super::shot::MIN_DRAWN_PIXELS {
                    failures.push(format!("{} is blank", s.name));
                }
            }
        }
        Err(e) => failures.push(format!("offscreen render failed: {e:#}")),
    }

    if options.native {
        println!("ui-smoke: native pass (real windows and tray icon; nothing is captured)");
        let demo = Arc::new(DemoController::sample());
        let report = Arc::new(Mutex::new(SmokeReport::default()));
        let driver = SmokeDriver::new(Arc::clone(&report));
        let ctrl: Arc<dyn HostController> = demo.clone();
        let picker: Arc<dyn FolderPicker> = Arc::new(ScriptedPicker::new(Vec::new()));
        match super::run_app(ctrl, picker, Some(driver)) {
            Ok(()) => {
                let report = report.lock().unwrap_or_else(|e| e.into_inner()).clone();
                for (key, n) in &report.frames {
                    println!("ui-smoke: window {key}: {n} frames");
                }
                println!(
                    "ui-smoke: {} window sessions, roots updated {} times",
                    report.sessions, report.root_updates
                );
                if let Some(n) = &report.native_notice {
                    println!("ui-smoke: native notification: {n}");
                }
                match &report.tray_unavailable {
                    Some(why) => println!("ui-smoke: WARNING tray icon unavailable: {why}"),
                    None => println!(
                        "ui-smoke: tray states shown: {}",
                        report.tray_states.join(", ")
                    ),
                }
                failures.extend(evaluate(&report, &demo.calls()));
            }
            Err(e) => failures.push(format!("the native window could not run: {e}")),
        }
    } else {
        println!("ui-smoke: native pass skipped (--ui-no-native)");
    }

    if failures.is_empty() {
        println!("ui-smoke: OK");
        Ok(())
    } else {
        for f in &failures {
            println!("ui-smoke: FAIL {f}");
        }
        bail!("ui-smoke failed: {} problem(s)", failures.len())
    }
}

/// Parses `--ui-out <dir>` and `--ui-no-native` out of the argument list (the binary's own parser
/// hands the rest of its arguments here).
pub fn parse_options(args: &mut dyn Iterator<Item = String>) -> anyhow::Result<Options> {
    let mut opts = Options::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--ui-out" => {
                opts.out_dir = args.next().context("--ui-out needs a directory")?.into();
            }
            "--ui-no-native" => opts.native = false,
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn driver() -> (SmokeDriver, Arc<Mutex<SmokeReport>>) {
        let report = Arc::new(Mutex::new(SmokeReport::default()));
        (SmokeDriver::new(Arc::clone(&report)), report)
    }

    fn all_frames(n: u32) -> HashMap<String, u32> {
        let mut f: HashMap<String, u32> = Page::ALL.iter().map(|p| (page_key(*p), n)).collect();
        f.insert("add-device".into(), n);
        f.insert("toast".into(), n);
        f.insert("flyout".into(), n);
        f
    }

    #[test]
    fn the_script_opens_every_window_once_and_ends_by_quitting() {
        let steps = script();
        for p in Page::ALL {
            assert_eq!(steps.iter().filter(|s| **s == Step::OpenPage(p)).count(), 1);
        }
        assert_eq!(
            steps.iter().filter(|s| **s == Step::OpenAddDevice).count(),
            1
        );
        assert_eq!(steps.last(), Some(&Step::Quit));
        let tray: Vec<_> = steps
            .iter()
            .filter(|s| matches!(s, Step::TrayState(_)))
            .collect();
        assert_eq!(tray.len(), 4, "all four tray states");
        let open = steps
            .iter()
            .position(|s| *s == Step::OpenAddDevice)
            .unwrap();
        let close = steps
            .iter()
            .position(|s| *s == Step::CloseAddDevice)
            .unwrap();
        assert!(open < close);
    }

    #[test]
    fn the_driver_waits_for_each_window_to_draw_before_moving_on() {
        let (mut d, _) = driver();
        let t0 = Instant::now();
        let mut frames = HashMap::new();
        assert_eq!(
            d.poll(&frames, 0, true, t0),
            Some(Step::OpenPage(Page::Shares))
        );
        assert_eq!(
            d.poll(&frames, 0, true, t0),
            None,
            "shares has not drawn yet"
        );
        frames.insert(page_key(Page::Shares), MIN_FRAMES - 1);
        assert_eq!(d.poll(&frames, 0, true, t0), None);
        frames.insert(page_key(Page::Shares), MIN_FRAMES);
        assert_eq!(
            d.poll(&frames, 0, true, t0),
            Some(Step::OpenPage(Page::Devices))
        );
    }

    #[test]
    fn a_window_that_never_draws_is_a_failure_but_the_run_continues() {
        let (mut d, report) = driver();
        let t0 = Instant::now();
        let frames = HashMap::new();
        assert_eq!(
            d.poll(&frames, 0, true, t0),
            Some(Step::OpenPage(Page::Shares))
        );
        assert_eq!(
            d.poll(&frames, 0, true, t0 + STEP_TIMEOUT + Duration::from_secs(1)),
            None
        );
        assert_eq!(report.lock().unwrap().failures.len(), 1);
        assert!(report.lock().unwrap().failures[0].contains("Shares"));
        assert_eq!(
            d.poll(&frames, 0, true, t0 + STEP_TIMEOUT + Duration::from_secs(2)),
            Some(Step::OpenPage(Page::Devices))
        );
    }

    #[test]
    fn the_overall_timeout_quits() {
        let (mut d, report) = driver();
        let t0 = Instant::now();
        let step = d.poll(
            &HashMap::new(),
            0,
            true,
            t0 + OVERALL_TIMEOUT + Duration::from_secs(1),
        );
        assert_eq!(step, Some(Step::Quit));
        assert!(report.lock().unwrap().failures[0].contains("took longer"));
        assert_eq!(
            d.poll(&HashMap::new(), 0, true, t0 + OVERALL_TIMEOUT * 2),
            None
        );
    }

    fn good_report() -> SmokeReport {
        SmokeReport {
            frames: all_frames(5).into_iter().collect(),
            tray_states: TrayState::ALL.iter().map(|s| s.name()).collect(),
            tray_unavailable: None,
            failures: Vec::new(),
            native_notice: None,
            root_updates: 100,
            sessions: 2,
            finished: true,
        }
    }

    #[test]
    fn a_good_run_has_no_problems() {
        let calls = vec![Call::OpenPairing, Call::ClosePairing];
        assert_eq!(evaluate(&good_report(), &calls), Vec::<String>::new());
    }

    #[test]
    fn each_kind_of_problem_is_reported() {
        let calls = vec![Call::OpenPairing, Call::ClosePairing];
        let mut r = good_report();
        r.frames.insert(page_key(Page::Network), 1);
        r.root_updates = 3;
        r.finished = false;
        r.tray_states.pop();
        let problems = evaluate(&r, &calls).join(" | ");
        assert!(
            problems.contains("window Network drew 1 frames"),
            "{problems}"
        );
        assert!(problems.contains("not staying alive"), "{problems}");
        assert!(problems.contains("did not shut down cleanly"), "{problems}");
        assert!(problems.contains("3 of 4 states"), "{problems}");

        let problems = evaluate(&good_report(), &[Call::OpenPairing]).join(" | ");
        assert!(problems.contains("opened and then closed"), "{problems}");
        let problems = evaluate(
            &good_report(),
            &[Call::OpenPairing, Call::ClosePairing, Call::KillSwitch],
        );
        assert!(problems.iter().any(|p| p.contains("must not")));
    }

    #[test]
    fn a_missing_tray_is_a_warning_not_a_failure() {
        let calls = vec![Call::OpenPairing, Call::ClosePairing];
        let mut r = good_report();
        r.tray_states.clear();
        r.tray_unavailable = Some("no notification area".into());
        assert!(evaluate(&r, &calls).is_empty());
    }

    #[test]
    fn options_parse() {
        let mut it = ["--ui-out", "x/y", "--ui-no-native"]
            .iter()
            .map(|s| (*s).to_owned());
        let o = parse_options(&mut it).unwrap();
        assert_eq!(o.out_dir, PathBuf::from("x/y"));
        assert!(!o.native);
        let d = parse_options(&mut std::iter::empty()).unwrap();
        assert_eq!(d.out_dir, PathBuf::from(DEFAULT_OUT));
        assert!(d.native);
        assert!(parse_options(&mut ["--ui-out"].iter().map(|s| (*s).to_owned())).is_err());
        assert!(parse_options(&mut ["--nope"].iter().map(|s| (*s).to_owned())).is_err());
    }
}
