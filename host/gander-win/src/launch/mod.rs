// SPDX-License-Identifier: Apache-2.0
//! Stream launch and window discovery on Windows (WP-51; stream-launch-policy spec, "Launch and
//! window discovery"). The decisions are in [`gander_core::launch`]; this module does the calls:
//!
//! 1. [`launch_and_discover`] plans the launch from the policy decision (refused or unanswered
//!    `ask` decisions launch nothing), snapshots the top-level windows (S0), starts the app with
//!    `CreateProcessW` (or activates a packaged app with `IApplicationActivationManager`), and
//!    polls snapshots and the process tree until discovery answers: [`DiscoveryOutcome::Found`]
//!    (fitted to the phone when a viewport is given), [`DiscoveryOutcome::Pick`] or
//!    [`DiscoveryOutcome::TimedOut`]. It blocks for up to the timeout (15 s, 30 s for
//!    `slow_start`), so call it from a worker thread.
//! 2. After a pick, [`target_from_candidate`] turns the chosen candidate into a [`WindowTarget`].
//! 3. [`fit_to_phone`] saves the original placement once and resizes; [`restore`] puts the
//!    window back at the end of the session.
//! 4. At the end of a session (WP-53): [`is_only_window`] and [`close_window`] (`WM_CLOSE`,
//!    never `TerminateProcess`) for `close_on_end`.
//! 5. [`secondary_windows`] lists the target's owned windows (modal dialogs are not part of the
//!    window capture even with `IncludeSecondaryWindows`, stream-spike findings C5).
//!
//! All coordinates are physical pixels: every call runs with the thread switched to per-monitor
//! v2 DPI awareness.

mod close;
pub(crate) mod dpi;
mod place;
mod snapshot;
mod spawn;

use std::collections::BTreeSet;
use std::path::Path;
use std::thread;
use std::time::Instant;

use gander_core::config::{AppCatalogue, CloseOnEnd};
use gander_core::launch::discovery::{
    Candidate, Discovery, DiscoveryTiming, Hwnd, Rect, Step, WindowInfo, descendants, owned_windows,
};
use gander_core::launch::fit::{FitError, FitPlan, Size};
use gander_core::launch::plan::{
    Authorisation, LaunchMethod, LaunchPlan, PlanError, PlanInput, plan_launch,
};
use gander_core::policy::{IsolatedBrowserLaunch, PolicyDecision};
use gander_core::protocol::stream::Viewport;

pub use close::{CloseOutcome, close_window, is_only_window, other_windows};
pub use place::{fit_to_phone, placement, restore};
pub use snapshot::{process_parents, snapshot, window};

/// One launch request.
#[derive(Debug, Clone, Copy)]
pub struct LaunchRequest<'a> {
    /// The policy decision for this file and device ([`gander_core::policy::StreamPolicy`]).
    pub decision: &'a PolicyDecision,
    /// The catalogue the decision was made with (launch template, window match, timing).
    pub catalogue: &'a AppCatalogue,
    /// The resolved absolute path of the file.
    pub file: &'a Path,
    /// Policy alone, or with the owner's "Allow once" for an `ask` decision.
    pub authorisation: Authorisation,
    /// The isolated browser profile directory
    /// ([`gander_core::launch::plan::browser_profile_dir`]); required for browsers.
    pub browser_profile_dir: Option<&'a Path>,
    /// Fit the found window to this viewport. `None` leaves the window as it is.
    pub viewport: Option<Viewport>,
    /// Overrides the catalogue timing (tests).
    pub timing: Option<DiscoveryTiming>,
}

/// The original placement of a window (`GetWindowPlacement`), physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedPlacement {
    /// `WINDOWPLACEMENT::flags`.
    pub flags: u32,
    /// `WINDOWPLACEMENT::showCmd` (normal, minimised, maximised).
    pub show_cmd: u32,
    /// Minimised position.
    pub min_position: (i32, i32),
    /// Maximised position.
    pub max_position: (i32, i32),
    /// The restored ("normal") rectangle, workspace coordinates.
    pub normal: Rect,
}

/// What [`fit_to_phone`] did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitResult {
    /// The plan.
    pub plan: FitPlan,
    /// The dpi it was planned for.
    pub dpi: u32,
    /// The client size read back after `SetWindowPos` (differs from the plan when the app
    /// limits its size through `WM_GETMINMAXINFO`).
    pub client: Size,
    /// The outer rectangle read back.
    pub outer: Rect,
}

/// The window a stream session captures.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowTarget {
    /// Handle.
    pub hwnd: Hwnd,
    /// Owning process (checked before every call: handles are reused).
    pub pid: u32,
    /// Title at discovery.
    pub title: String,
    /// Class.
    pub class: String,
    /// Owned windows at discovery (dialogs); refresh with [`secondary_windows`].
    pub owned: Vec<WindowInfo>,
    /// The window belongs to the process tree Gander started (input to `close_on_end =
    /// "if_launched_by_us"`).
    pub launched_by_us: bool,
    /// The last fit, if any.
    pub fit: Option<FitResult>,
    saved: Option<SavedPlacement>,
}

impl WindowTarget {
    /// The placement saved before the first fit (what [`restore`] goes back to).
    pub fn saved_placement(&self) -> Option<SavedPlacement> {
        self.saved
    }
}

/// The answer of [`launch_and_discover`].
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryOutcome {
    /// One window; fitted when the request had a viewport.
    Found(WindowTarget),
    /// The user picks (`stream.state: pick` with the candidate titles).
    Pick(Vec<Candidate>),
    /// Nothing appeared (`app_unavailable`).
    TimedOut,
}

/// A launch and its outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct Launched {
    /// What discovery found.
    pub outcome: DiscoveryOutcome,
    /// The process started or activated.
    pub pid: u32,
    /// Set for a browser (the phone is told "opens in X (isolated window)").
    pub browser: Option<IsolatedBrowserLaunch>,
    /// The catalogue's `close_on_end`.
    pub close_on_end: CloseOnEnd,
    /// The fit failed after the window was found (the session may continue unfitted).
    pub fit_error: Option<String>,
}

/// Why nothing was launched.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// The policy or the plan refused (see [`PlanError`]).
    #[error(transparent)]
    Plan(#[from] PlanError),
    /// `CreateProcessW` failed.
    #[error("the app could not be started: {0}")]
    Spawn(std::io::Error),
    /// The app's manifest requires administrator rights; Gander does not elevate (and could not
    /// send input to an elevated window).
    #[error("the app needs administrator rights")]
    NeedsElevation,
    /// Packaged-app activation failed (HRESULT).
    #[error("the packaged app could not be activated (0x{0:08X})")]
    Activation(i32),
}

/// Why a window operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WindowError {
    /// The window is closed, or its handle now belongs to another process.
    #[error("the window is gone")]
    Gone,
    /// The app is not responding (`IsHungAppWindow`); nothing is sent to it.
    #[error("the app is not responding")]
    NotResponding,
    /// No fit plan.
    #[error(transparent)]
    Fit(FitError),
    /// A Win32 call failed.
    #[error("window call failed: {0}")]
    Os(std::io::Error),
}

/// Plans, launches and discovers (see the module documentation). Blocks up to the discovery
/// timeout.
pub fn launch_and_discover(req: &LaunchRequest<'_>) -> Result<Launched, LaunchError> {
    let plan: LaunchPlan = plan_launch(&PlanInput {
        decision: req.decision,
        catalogue: req.catalogue,
        file: req.file,
        authorisation: req.authorisation,
        browser_profile_dir: req.browser_profile_dir,
    })?;
    let timing = req.timing.unwrap_or(plan.timing);
    let baseline = snapshot();
    let spawned = match &plan.method {
        LaunchMethod::Process(cmd) => spawn::spawn(cmd)?,
        LaunchMethod::Packaged { app_user_model_id } => {
            spawn::activate_packaged(app_user_model_id, req.file)?
        }
    };
    // The discovery timeout counts from when the app was started, not from before a slow
    // activation call.
    let start = Instant::now();
    let family = match &plan.method {
        LaunchMethod::Packaged { .. } => plan.target.package_family.clone(),
        LaunchMethod::Process(_) => None,
    };
    let mut discovery = Discovery::new(plan.target.clone(), timing, plan.splash.clone(), &baseline);
    let (outcome, tree) = loop {
        let now = snapshot();
        let tree = launched_tree(spawned.pid, family.as_deref(), &now);
        match discovery.step(&now, &tree, start.elapsed()) {
            Step::Pending => thread::sleep(timing.poll),
            Step::Found(c) => break (Step::Found(c), tree),
            other => break (other, tree),
        }
    };
    let pid = spawned.pid;
    drop(spawned);

    let mut fit_error = None;
    let outcome = match outcome {
        Step::Found(c) => {
            let mut target = target_of(&c, tree.contains(&c.window.pid));
            if let Some(viewport) = req.viewport
                && let Err(e) = fit_to_phone(&mut target, viewport, None)
            {
                fit_error = Some(e.to_string());
            }
            DiscoveryOutcome::Found(target)
        }
        Step::Pick(c) => DiscoveryOutcome::Pick(c),
        Step::TimedOut | Step::Pending => DiscoveryOutcome::TimedOut,
    };
    Ok(Launched {
        outcome,
        pid,
        browser: plan.browser,
        close_on_end: plan.close_on_end,
        fit_error,
    })
}

/// The processes that count as launched: the started process and its descendants, plus, for a
/// packaged app, every process of its package family that owns a visible window. A packaged desktop app opened through its association handler reports no process
/// id (0), so its package family is the only link from the launch to its window.
fn launched_tree(pid: u32, family: Option<&str>, now: &[WindowInfo]) -> BTreeSet<u32> {
    let mut tree = if pid == 0 {
        BTreeSet::new()
    } else {
        descendants(pid, &process_parents())
    };
    if let Some(family) = family {
        // Every process with a visible window, not only new windows: an already running
        // instance (Photos keeps one in the background) shows a window it created hidden before
        // the launch. Which windows are new is the discovery's decision.
        let mut checked = BTreeSet::new();
        for w in now.iter().filter(|w| w.visible) {
            if checked.insert(w.pid)
                && snapshot::package_family(w.pid).is_some_and(|f| f.eq_ignore_ascii_case(family))
            {
                tree.insert(w.pid);
            }
        }
    }
    tree
}

fn target_of(c: &Candidate, launched_by_us: bool) -> WindowTarget {
    WindowTarget {
        hwnd: c.window.hwnd,
        pid: c.window.pid,
        title: c.window.title.clone(),
        class: c.window.class.clone(),
        owned: c.owned.clone(),
        launched_by_us,
        fit: None,
        saved: None,
    }
}

/// The target for a candidate the user picked, after checking the window still exists and
/// still belongs to the same process. `launched_by_us`: the candidate's process is
/// [`Launched::pid`] or one of its descendants ([`process_parents`], [`descendants`]).
pub fn target_from_candidate(
    c: &Candidate,
    launched_by_us: bool,
) -> Result<WindowTarget, WindowError> {
    let now = window(c.window.hwnd).ok_or(WindowError::Gone)?;
    if now.pid != c.window.pid {
        return Err(WindowError::Gone);
    }
    let mut t = target_of(c, launched_by_us);
    t.title = now.title;
    Ok(t)
}

/// The visible windows `target` owns now (dialogs, owned palettes), nearest first.
pub fn secondary_windows(target: &WindowTarget) -> Vec<WindowInfo> {
    owned_windows(&snapshot(), target.hwnd)
}
