// SPDX-License-Identifier: Apache-2.0
//! The Windows [`WindowPlatform`]: WP-51 launch, discovery, fit and restore
//! (`gander_win::launch`), WP-52 input (`gander_stream::input`), and keep-awake
//! (`gander_win::session::power::KeepAwake`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gander_core::clipboard::ClipboardService;
use gander_core::launch::discovery::{Candidate, descendants};
use gander_core::launch::plan::browser_profile_dir;
use gander_core::policy::rules::same_windows_path;
use gander_core::protocol::stream::Viewport;
use gander_stream::Session;
use gander_stream::input::ClipboardLink;
use gander_win::input::{InputOptions, InputTarget, LockProbe};
use gander_win::launch::{
    CloseOutcome, DiscoveryOutcome, LaunchRequest, WindowTarget, close_window, fit_to_phone,
    is_only_window, launch_and_discover, process_parents, restore, target_from_candidate,
};

use super::{
    Awake, FinishReport, FoundWindow, LaunchJob, LaunchOutcome, LaunchResult, PickCandidate,
    WindowPlatform,
};

/// How long `WM_CLOSE` may take before the app counts as "left open".
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// The production platform.
#[derive(Clone)]
pub struct WindowsPlatform {
    /// Where isolated browser profiles live (`%LOCALAPPDATA%\Gander`).
    pub data_dir: PathBuf,
    /// The lock check for input (fail closed).
    pub lock_probe: LockProbe,
    /// Test hosts only: the one executable a launch may start (`gander-testwin`). Any other
    /// handler is refused before anything runs.
    pub only_exe: Option<PathBuf>,
    /// The clipboard text bridge (WP-56), served on every session's `control` channel behind the
    /// input gate. `None`: the phone's clipboard messages are refused as unsupported.
    pub clipboard: Option<ClipboardService>,
}

impl std::fmt::Debug for WindowsPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowsPlatform")
            .field("only_exe", &self.only_exe)
            .finish_non_exhaustive()
    }
}

fn found(target: WindowTarget) -> FoundWindow {
    FoundWindow {
        hwnd: target.hwnd,
        pid: target.pid,
        title: target.title.clone(),
        launched_by_us: target.launched_by_us,
        size: target.fit.map(|f| (f.client.width, f.client.height, f.dpi)),
        native: Some(Arc::new(Mutex::new(target))),
    }
}

fn native(window: &FoundWindow) -> Option<&Mutex<WindowTarget>> {
    window
        .native
        .as_ref()?
        .downcast_ref::<Mutex<WindowTarget>>()
}

impl WindowPlatform for WindowsPlatform {
    fn launch(&self, job: &LaunchJob) -> Result<LaunchResult, String> {
        if let Some(only) = &self.only_exe {
            let ok = job
                .decision
                .handler
                .as_ref()
                .is_some_and(|h| same_windows_path(&h.exe, only));
            if !ok {
                return Err("this test host launches only gander-testwin".to_owned());
            }
        }
        let profile = job
            .decision
            .browser
            .map(|b| browser_profile_dir(&self.data_dir, b.browser));
        let launched = launch_and_discover(&LaunchRequest {
            decision: &job.decision,
            catalogue: &job.catalogue,
            file: &job.file,
            authorisation: job.authorisation,
            browser_profile_dir: profile.as_deref(),
            viewport: Some(job.viewport),
            timing: None,
        })
        .map_err(|e| e.to_string())?;
        if let Some(e) = &launched.fit_error {
            tracing::warn!(error = %e, "fit to phone failed; streaming the window as it is");
        }
        let outcome = match launched.outcome {
            DiscoveryOutcome::Found(t) => LaunchOutcome::Found(found(t)),
            DiscoveryOutcome::Pick(candidates) => LaunchOutcome::Pick(
                candidates
                    .into_iter()
                    .map(|c| PickCandidate {
                        title: c.window.title.clone(),
                        native: Arc::new(c),
                    })
                    .collect(),
            ),
            DiscoveryOutcome::TimedOut => LaunchOutcome::TimedOut,
        };
        Ok(LaunchResult {
            outcome,
            pid: launched.pid,
            close_on_end: launched.close_on_end,
        })
    }

    fn pick(
        &self,
        launched_pid: u32,
        candidate: &PickCandidate,
        viewport: Viewport,
    ) -> Result<FoundWindow, String> {
        let c = candidate
            .native
            .downcast_ref::<Candidate>()
            .ok_or("not a Windows candidate")?;
        // pid 0: a packaged app opened through its association handler (no process id); only
        // its package match makes the window ours. Never walk the tree of pid 0 (every process).
        let ours = c.reason == gander_core::launch::discovery::MatchReason::PackagedApp
            || (launched_pid != 0
                && (c.window.pid == launched_pid
                    || descendants(launched_pid, &process_parents()).contains(&c.window.pid)));
        let mut target = target_from_candidate(c, ours).map_err(|e| e.to_string())?;
        if let Err(e) = fit_to_phone(&mut target, viewport, None) {
            tracing::warn!(error = %e, "fit to phone failed after the pick");
        }
        Ok(found(target))
    }

    fn attach_input(
        &self,
        session: &Session,
        window: &FoundWindow,
        front_on_connect: bool,
    ) -> Option<Box<dyn Send>> {
        // The default foreground policy: `AttachThreadInput` fallback on, the Alt-key trick off.
        match gander_stream::input::attach_with_clipboard(
            session,
            InputTarget::new(window.hwnd, window.pid),
            Arc::clone(&self.lock_probe),
            InputOptions::default(),
            self.clipboard.clone().map(ClipboardLink::new),
            front_on_connect,
        ) {
            Ok(link) => Some(Box::new(link)),
            Err(e) => {
                tracing::warn!(error = %e, "input could not be attached; the stream is view-only");
                None
            }
        }
    }

    fn is_only_window(&self, window: &FoundWindow) -> bool {
        native(window).is_some_and(|t| is_only_window(&t.lock().unwrap_or_else(|e| e.into_inner())))
    }

    fn finish(&self, window: &FoundWindow, close: bool) -> FinishReport {
        let Some(t) = native(window) else {
            return FinishReport::default();
        };
        let mut t = t.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = restore(&mut t) {
            tracing::debug!(error = %e, "window placement not restored");
        }
        if !close {
            return FinishReport::default();
        }
        match close_window(&t, CLOSE_WAIT) {
            Ok(CloseOutcome::Closed) => FinishReport {
                closed: true,
                left_open_dialog: false,
            },
            Ok(CloseOutcome::LeftOpen { dialog }) => FinishReport {
                closed: false,
                left_open_dialog: dialog,
            },
            Err(e) => {
                tracing::debug!(error = %e, "close on end skipped");
                FinishReport::default()
            }
        }
    }
}

/// Keep-awake through `SetThreadExecutionState` while a session streams.
#[derive(Debug, Default)]
pub struct SystemAwake;

impl Awake for SystemAwake {
    fn hold(&self) -> Option<Box<dyn Send>> {
        match gander_win::session::power::KeepAwake::start() {
            Ok(g) => Some(Box::new(g)),
            Err(e) => {
                tracing::warn!(error = %e, "keep-awake unavailable");
                None
            }
        }
    }
}
