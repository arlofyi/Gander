// SPDX-License-Identifier: Apache-2.0
//! Window discovery after a launch (stream-launch-policy spec, "Launch and window discovery",
//! steps 1, 3 and 4).
//!
//! The platform side feeds snapshots of the top-level windows (`EnumWindows`, in z-order) and the
//! launched process tree; [`Discovery::step`] decides. The rules:
//!
//! - **New** means: a handle not in the baseline snapshot S0, or in S0 but then hidden or cloaked,
//!   or now owned by another process (handle reuse), or whose title now contains the file name
//!   and did not before (a single-instance app that re-titles an existing window).
//! - **Eligible** as the stream window: visible, not cloaked, not a tool window, a non-empty
//!   rectangle, unowned, and not on the splash skip list ([`default_splash_rules`]).
//! - **Strategies** ([`Strategy`]): the catalogue's `window_match`, or the packaged-app mapping.
//!   `new_window_of_process` matches the launched process and its descendants (Office and
//!   browsers re-exec); `title_contains_filename` matches the file name without extension,
//!   case-insensitively, in any process (single-instance apps reuse a running process);
//!   packaged apps match the `ApplicationFrameWindow` whose hosted `CoreWindow` belongs to the
//!   activated process or package family.
//! - **Settle**: once a match appears, discovery waits [`DiscoveryTiming::settle`] for others
//!   (an app that opens two windows), then answers [`Step::Found`] for one match and
//!   [`Step::Pick`] for several. A match that disappears restarts the wait.
//! - **Timeout**: no settled match within [`DiscoveryTiming::timeout`] -> [`Step::Pick`] with
//!   every new window that matches by any rule (including dialogs whose owner is hidden, such as
//!   a "file is locked" prompt before the main window exists); none -> [`Step::TimedOut`].
//! - Owned windows (modal dialogs, which window capture does not include even with
//!   `IncludeSecondaryWindows`, stream-spike findings C5) are reported with each candidate in
//!   [`Candidate::owned`] so the stream engine can show them.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use crate::config::WindowMatch;

/// A window handle as an integer (the platform's `HWND` value).
pub type Hwnd = u64;

/// The class of the frame window that hosts packaged (UWP) apps, owned by
/// `ApplicationFrameHost.exe`.
pub const APPLICATION_FRAME_CLASS: &str = "ApplicationFrameWindow";
/// The class of the packaged app's own window, a child of the frame while the app is shown.
pub const CORE_WINDOW_CLASS: &str = "Windows.UI.Core.CoreWindow";

/// A screen rectangle in physical pixels (right and bottom exclusive).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge (exclusive).
    pub right: i32,
    /// Bottom edge (exclusive).
    pub bottom: i32,
}

impl Rect {
    /// A rectangle from position and size.
    pub const fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        }
    }

    /// Width (0 when inverted).
    pub const fn width(&self) -> i32 {
        if self.right > self.left {
            self.right - self.left
        } else {
            0
        }
    }

    /// Height (0 when inverted).
    pub const fn height(&self) -> i32 {
        if self.bottom > self.top {
            self.bottom - self.top
        } else {
            0
        }
    }

    /// No area.
    pub const fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }
}

/// The app a packaged frame window hosts (its `Windows.UI.Core.CoreWindow` child).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostedApp {
    /// Process id of the core window.
    pub pid: u32,
    /// `GetPackageFamilyName` of that process, if it is packaged and readable.
    pub package_family: Option<String>,
}

/// One top-level window in a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowInfo {
    /// Handle.
    pub hwnd: Hwnd,
    /// Owning process id.
    pub pid: u32,
    /// Owner window (`GW_OWNER`), for dialogs and other owned windows.
    pub owner: Option<Hwnd>,
    /// Title text.
    pub title: String,
    /// Window class name.
    pub class: String,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// DWM cloaked (another virtual desktop, a suspended packaged app, ...).
    pub cloaked: bool,
    /// Minimised (`IsIconic`).
    pub minimised: bool,
    /// `WS_EX_TOOLWINDOW` (not in Alt+Tab; floating palettes, many splash screens).
    pub tool_window: bool,
    /// Outer window rectangle.
    pub rect: Rect,
    /// For an [`APPLICATION_FRAME_CLASS`] window: the hosted app.
    pub hosted: Option<HostedApp>,
}

/// A splash-window rule: the class, and whether only an empty title counts as a splash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SplashRule {
    /// Window class (compared case-insensitively, as Windows does).
    pub class: String,
    /// Only a window with an empty title is a splash (Chromium's `Chrome_WidgetWin_0`).
    pub empty_title_only: bool,
}

impl SplashRule {
    /// A rule for `class`.
    pub fn new(class: &str, empty_title_only: bool) -> Self {
        Self {
            class: class.to_owned(),
            empty_title_only,
        }
    }

    /// Whether `w` is a splash window under this rule.
    pub fn matches(&self, w: &WindowInfo) -> bool {
        w.class.eq_ignore_ascii_case(&self.class)
            && (!self.empty_title_only || w.title.trim().is_empty())
    }
}

/// The spec's splash skip list: `MsoSplash` (Office) and `Chrome_WidgetWin_0` with an empty title
/// (Chromium start-up windows).
pub fn default_splash_rules() -> Vec<SplashRule> {
    vec![
        SplashRule::new("MsoSplash", false),
        SplashRule::new("Chrome_WidgetWin_0", true),
    ]
}

/// How the stream window is recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Strategy {
    /// A new window whose title contains the file name without extension.
    TitleContainsFilename,
    /// A new window of the launched process or one of its descendants.
    NewWindowOfProcess,
    /// The `ApplicationFrameWindow` hosting the activated packaged app.
    PackagedFrame,
    /// Always let the user pick among the new windows that match by any rule.
    Pick,
}

impl Strategy {
    /// The strategy for a catalogue `window_match`, or [`Strategy::PackagedFrame`] for a packaged
    /// app (whose window is never the launched process's own).
    pub fn for_entry(window_match: WindowMatch, packaged: bool) -> Self {
        match (packaged, window_match) {
            (_, WindowMatch::Pick) => Strategy::Pick,
            (true, _) => Strategy::PackagedFrame,
            (false, WindowMatch::TitleContainsFilename) => Strategy::TitleContainsFilename,
            (false, WindowMatch::NewWindowOfProcess) => Strategy::NewWindowOfProcess,
        }
    }
}

/// Why a window is a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MatchReason {
    /// Its packaged frame hosts the activated app.
    PackagedApp,
    /// It belongs to the launched process tree.
    Process,
    /// Its title contains the file name.
    Title,
    /// It is a dialog of the launched tree or titled with the file, whose owner is hidden
    /// (timeout fallback only).
    OrphanDialog,
}

impl MatchReason {
    /// Snake-case name for logs and the audit trail.
    pub const fn as_str(self) -> &'static str {
        match self {
            MatchReason::PackagedApp => "packaged_app",
            MatchReason::Process => "process",
            MatchReason::Title => "title",
            MatchReason::OrphanDialog => "orphan_dialog",
        }
    }
}

/// A window discovery proposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The window.
    pub window: WindowInfo,
    /// The strongest rule it matched.
    pub reason: MatchReason,
    /// Visible windows it owns (dialogs, owned palettes), nearest first. Window capture does not
    /// include modal dialogs, so the stream engine shows these separately.
    pub owned: Vec<WindowInfo>,
}

/// Timing of one discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryTiming {
    /// Snapshot interval.
    pub poll: Duration,
    /// Give up (pick or timed out) after this long.
    pub timeout: Duration,
    /// After the first match, wait this long for more before answering.
    pub settle: Duration,
}

impl DiscoveryTiming {
    /// Spec default: poll 100 ms, 15 s.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
    /// Spec: 30 s for catalogue entries with `slow_start = true`.
    pub const SLOW_START_TIMEOUT: Duration = Duration::from_secs(30);

    /// The spec timing (`slow_start` -> 30 s), settle 400 ms.
    pub const fn for_entry(slow_start: bool) -> Self {
        Self {
            poll: Duration::from_millis(100),
            timeout: if slow_start {
                Self::SLOW_START_TIMEOUT
            } else {
                Self::DEFAULT_TIMEOUT
            },
            settle: Duration::from_millis(400),
        }
    }
}

impl Default for DiscoveryTiming {
    fn default() -> Self {
        Self::for_entry(false)
    }
}

/// What discovery is looking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchTarget {
    /// The rule that decides.
    pub strategy: Strategy,
    /// The file name without its final extension (title matching); empty disables title matches.
    pub file_stem: String,
    /// Package family of a packaged app.
    pub package_family: Option<String>,
}

/// One answer of [`Discovery::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Keep polling.
    Pending,
    /// Exactly one window, settled.
    Found(Candidate),
    /// Several windows, or the timeout fallback, or the `pick` strategy: the user chooses.
    Pick(Vec<Candidate>),
    /// Nothing appeared before the timeout.
    TimedOut,
}

/// `Deck.v2.pptx` -> `Deck.v2`; a name without a dot is returned whole; a leading-dot name
/// (`.bashrc`) has no stem.
pub fn file_stem(file_name: &str) -> String {
    match file_name.rfind('.') {
        Some(0) => String::new(),
        Some(i) => file_name[..i].to_owned(),
        None => file_name.to_owned(),
    }
}

/// The pids descending from `root` (inclusive) given `(pid, parent pid)` pairs. A pid that is its
/// own parent (pid 0, the idle process) does not loop.
pub fn descendants(root: u32, pairs: &[(u32, u32)]) -> BTreeSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in pairs {
        if pid != ppid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut out = BTreeSet::from([root]);
    let mut stack = vec![root];
    while let Some(p) = stack.pop() {
        for &c in children.get(&p).into_iter().flatten() {
            if out.insert(c) {
                stack.push(c);
            }
        }
    }
    out
}

/// Visible windows owned (directly or through an owner chain) by `hwnd`, in snapshot order.
pub fn owned_windows(snapshot: &[WindowInfo], hwnd: Hwnd) -> Vec<WindowInfo> {
    let owners: HashMap<Hwnd, Hwnd> = snapshot
        .iter()
        .filter_map(|w| w.owner.map(|o| (w.hwnd, o)))
        .collect();
    snapshot
        .iter()
        .filter(|w| w.visible && !w.cloaked && w.hwnd != hwnd)
        .filter(|w| {
            let mut cur = w.owner;
            for _ in 0..32 {
                match cur {
                    Some(o) if o == hwnd => return true,
                    Some(o) => cur = owners.get(&o).copied(),
                    None => return false,
                }
            }
            false
        })
        .cloned()
        .collect()
}

/// The discovery state machine for one launch.
#[derive(Debug, Clone)]
pub struct Discovery {
    target: MatchTarget,
    timing: DiscoveryTiming,
    splash: Vec<SplashRule>,
    baseline: HashMap<Hwnd, WindowInfo>,
    stem_lower: String,
    first_match_at: Option<Duration>,
}

impl Discovery {
    /// Starts discovery with the snapshot S0 taken before the launch.
    pub fn new(
        target: MatchTarget,
        timing: DiscoveryTiming,
        splash: Vec<SplashRule>,
        baseline: &[WindowInfo],
    ) -> Self {
        let stem_lower = target.file_stem.trim().to_lowercase();
        Self {
            target,
            timing,
            splash,
            baseline: baseline.iter().map(|w| (w.hwnd, w.clone())).collect(),
            stem_lower,
            first_match_at: None,
        }
    }

    /// The timing in force.
    pub fn timing(&self) -> DiscoveryTiming {
        self.timing
    }

    fn title_matches(&self, title: &str) -> bool {
        !self.stem_lower.is_empty() && title.to_lowercase().contains(&self.stem_lower)
    }

    fn is_new(&self, w: &WindowInfo) -> bool {
        match self.baseline.get(&w.hwnd) {
            None => true,
            Some(b) => {
                b.pid != w.pid
                    || !b.visible
                    || b.cloaked
                    || (self.title_matches(&w.title) && !self.title_matches(&b.title))
            }
        }
    }

    fn is_splash(&self, w: &WindowInfo) -> bool {
        self.splash.iter().any(|r| r.matches(w))
    }

    fn packaged_match(&self, w: &WindowInfo, tree: &BTreeSet<u32>) -> bool {
        if !w.class.eq_ignore_ascii_case(APPLICATION_FRAME_CLASS) {
            // A packaged *desktop* app (WinUI 3 or Win32 in a package, such as Microsoft Photos
            // since 2023) owns its top-level window itself, outside ApplicationFrameHost. The
            // platform puts the activated process, or the package family's processes, in `tree`.
            // A UWP app's own CoreWindow is not it: that one is shown through its frame.
            return self.target.strategy == Strategy::PackagedFrame
                && !w.class.eq_ignore_ascii_case(CORE_WINDOW_CLASS)
                && tree.contains(&w.pid);
        }
        let Some(h) = &w.hosted else {
            return false;
        };
        let family = match (&self.target.package_family, &h.package_family) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        };
        family || tree.contains(&h.pid)
    }

    /// Every rule `w` matches, strongest first.
    fn reasons(&self, w: &WindowInfo, tree: &BTreeSet<u32>) -> Vec<MatchReason> {
        let mut r = Vec::new();
        if self.packaged_match(w, tree) {
            r.push(MatchReason::PackagedApp);
        }
        if tree.contains(&w.pid) {
            r.push(MatchReason::Process);
        }
        if self.title_matches(&w.title) {
            r.push(MatchReason::Title);
        }
        r
    }

    fn primary_reason(&self, reasons: &[MatchReason]) -> Option<MatchReason> {
        let wanted = match self.target.strategy {
            Strategy::TitleContainsFilename => MatchReason::Title,
            Strategy::NewWindowOfProcess => MatchReason::Process,
            Strategy::PackagedFrame => MatchReason::PackagedApp,
            Strategy::Pick => return reasons.first().copied(),
        };
        reasons.contains(&wanted).then_some(wanted)
    }

    fn base_ok(&self, w: &WindowInfo) -> bool {
        w.visible && !w.cloaked && !w.rect.is_empty() && !self.is_splash(w) && self.is_new(w)
    }

    fn candidate(w: &WindowInfo, reason: MatchReason, now: &[WindowInfo]) -> Candidate {
        Candidate {
            window: w.clone(),
            reason,
            owned: owned_windows(now, w.hwnd),
        }
    }

    /// The windows the strategy accepts in `now`.
    fn primary(&self, now: &[WindowInfo], tree: &BTreeSet<u32>) -> Vec<Candidate> {
        now.iter()
            .filter(|w| self.base_ok(w) && !w.tool_window && w.owner.is_none())
            .filter_map(|w| {
                let reason = self.primary_reason(&self.reasons(w, tree))?;
                Some(Self::candidate(w, reason, now))
            })
            .collect()
    }

    /// Everything plausible at the timeout: new eligible windows matching any rule, plus
    /// dialogs of the tree (or titled with the file) whose owner is not visible.
    fn fallback(&self, now: &[WindowInfo], tree: &BTreeSet<u32>) -> Vec<Candidate> {
        let visible: BTreeSet<Hwnd> = now.iter().filter(|w| w.visible).map(|w| w.hwnd).collect();
        now.iter()
            .filter(|w| self.base_ok(w))
            .filter_map(|w| {
                let reasons = self.reasons(w, tree);
                let first = *reasons.first()?;
                match w.owner {
                    None if !w.tool_window => Some(Self::candidate(w, first, now)),
                    Some(o) if !visible.contains(&o) => {
                        Some(Self::candidate(w, MatchReason::OrphanDialog, now))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// One poll: `now` is the current snapshot (z-order), `tree` the launched process and its
    /// descendants (empty when nothing was launched by pid), `elapsed` the time since launch.
    pub fn step(&mut self, now: &[WindowInfo], tree: &BTreeSet<u32>, elapsed: Duration) -> Step {
        let primary = self.primary(now, tree);
        if primary.is_empty() {
            self.first_match_at = None;
        } else {
            let first = *self.first_match_at.get_or_insert(elapsed);
            let settled = elapsed.saturating_sub(first) >= self.timing.settle;
            if settled || elapsed >= self.timing.timeout {
                // A packaged app may restore other windows as it starts (Notepad on Windows 11
                // reopens its previous session): the one titled with the file is the one.
                let primary = match self.target.strategy {
                    Strategy::PackagedFrame if primary.len() > 1 => {
                        let titled: Vec<Candidate> = primary
                            .iter()
                            .filter(|c| self.title_matches(&c.window.title))
                            .cloned()
                            .collect();
                        if titled.len() == 1 { titled } else { primary }
                    }
                    _ => primary,
                };
                return if primary.len() == 1 && self.target.strategy != Strategy::Pick {
                    let mut primary = primary;
                    Step::Found(primary.remove(0))
                } else {
                    Step::Pick(primary)
                };
            }
            return Step::Pending;
        }
        if elapsed >= self.timing.timeout {
            let fallback = self.fallback(now, tree);
            return if fallback.is_empty() {
                Step::TimedOut
            } else {
                Step::Pick(fallback)
            };
        }
        Step::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAUNCHED: u32 = 100;

    fn win(hwnd: Hwnd, pid: u32, title: &str, class: &str) -> WindowInfo {
        WindowInfo {
            hwnd,
            pid,
            owner: None,
            title: title.to_owned(),
            class: class.to_owned(),
            visible: true,
            cloaked: false,
            minimised: false,
            tool_window: false,
            rect: Rect::from_xywh(10, 10, 800, 600),
            hosted: None,
        }
    }

    fn timing() -> DiscoveryTiming {
        DiscoveryTiming {
            poll: Duration::from_millis(100),
            timeout: Duration::from_secs(15),
            settle: Duration::from_millis(400),
        }
    }

    fn disc(strategy: Strategy, baseline: &[WindowInfo]) -> Discovery {
        Discovery::new(
            MatchTarget {
                strategy,
                file_stem: "Deck".into(),
                package_family: None,
            },
            timing(),
            default_splash_rules(),
            baseline,
        )
    }

    fn tree() -> BTreeSet<u32> {
        descendants(LAUNCHED, &[(101, LAUNCHED), (102, 101), (7, 1)])
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn found_hwnd(s: &Step) -> Hwnd {
        match s {
            Step::Found(c) => c.window.hwnd,
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn stems() {
        assert_eq!(file_stem("Deck.v2.pptx"), "Deck.v2");
        assert_eq!(file_stem("Makefile"), "Makefile");
        assert_eq!(file_stem(".bashrc"), "");
    }

    #[test]
    fn process_tree() {
        let t = tree();
        assert_eq!(t, BTreeSet::from([100, 101, 102]));
        assert_eq!(descendants(0, &[(0, 0), (4, 0)]), BTreeSet::from([0, 4]));
    }

    #[test]
    fn new_process_window_found_after_settle() {
        let s0 = vec![win(1, 7, "Inbox", "Other")];
        let mut d = disc(Strategy::NewWindowOfProcess, &s0);
        let t = tree();
        assert_eq!(d.step(&s0, &t, ms(100)), Step::Pending);
        let now = vec![win(1, 7, "Inbox", "Other"), win(2, 102, "Something", "App")];
        assert_eq!(d.step(&now, &t, ms(200)), Step::Pending);
        assert_eq!(d.step(&now, &t, ms(500)), Step::Pending);
        assert_eq!(found_hwnd(&d.step(&now, &t, ms(600))), 2);
    }

    #[test]
    fn title_match_in_another_process_reused_instance() {
        let s0 = vec![win(1, 50, "Other - App", "App")];
        let mut d = disc(Strategy::TitleContainsFilename, &s0);
        let t = tree();
        // The launched process forwarded to pid 50 and exited; pid 50 opens a new window.
        let now = vec![
            win(1, 50, "Other - App", "App"),
            win(3, 50, "deck - App", "App"),
        ];
        d.step(&now, &t, ms(100));
        let step = d.step(&now, &t, ms(600));
        assert_eq!(found_hwnd(&step), 3);
        let Step::Found(c) = step else { unreachable!() };
        assert_eq!(c.reason, MatchReason::Title);
    }

    #[test]
    fn retitled_existing_window_counts_as_new() {
        let s0 = vec![win(1, 50, "Empty - App", "App")];
        let mut d = disc(Strategy::TitleContainsFilename, &s0);
        let now = vec![win(1, 50, "Deck.pptx - App", "App")];
        d.step(&now, &BTreeSet::new(), ms(0));
        assert_eq!(found_hwnd(&d.step(&now, &BTreeSet::new(), ms(400))), 1);
        // An existing window that already had the name is not new.
        let s0 = vec![win(1, 50, "Deck.pptx - App", "App")];
        let mut d = disc(Strategy::TitleContainsFilename, &s0);
        assert_eq!(d.step(&s0, &BTreeSet::new(), ms(400)), Step::Pending);
    }

    #[test]
    fn splash_then_main() {
        let mut d = disc(Strategy::TitleContainsFilename, &[]);
        let t = tree();
        let splash = win(5, LAUNCHED, "Deck", "MsoSplash");
        let chrome_splash = win(6, LAUNCHED, "", "Chrome_WidgetWin_0");
        assert_eq!(
            d.step(&[splash.clone(), chrome_splash.clone()], &t, ms(100)),
            Step::Pending
        );
        assert_eq!(
            d.step(std::slice::from_ref(&splash), &t, ms(900)),
            Step::Pending
        );
        let main = win(9, LAUNCHED, "Deck - App", "AppMain");
        d.step(std::slice::from_ref(&main), &t, ms(1000));
        assert_eq!(found_hwnd(&d.step(&[main], &t, ms(1400))), 9);
        // A titled Chrome_WidgetWin_0 is not a splash.
        let mut d = disc(Strategy::NewWindowOfProcess, &[]);
        let titled = win(6, LAUNCHED, "Deck", "Chrome_WidgetWin_0");
        d.step(std::slice::from_ref(&titled), &t, ms(0));
        assert_eq!(found_hwnd(&d.step(&[titled], &t, ms(400))), 6);
    }

    #[test]
    fn two_candidates_pick() {
        let mut d = disc(Strategy::NewWindowOfProcess, &[]);
        let t = tree();
        let a = win(2, LAUNCHED, "Deck", "App");
        d.step(std::slice::from_ref(&a), &t, ms(100));
        // The second window appears within the settle time.
        let b = win(3, 101, "Deck (2)", "App");
        assert_eq!(d.step(&[a.clone(), b.clone()], &t, ms(300)), Step::Pending);
        match d.step(&[a, b], &t, ms(500)) {
            Step::Pick(c) => {
                assert_eq!(
                    c.iter().map(|c| c.window.hwnd).collect::<Vec<_>>(),
                    vec![2, 3]
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_match_that_vanishes_restarts_settle() {
        let mut d = disc(Strategy::NewWindowOfProcess, &[]);
        let t = tree();
        let a = win(2, LAUNCHED, "x", "App");
        d.step(std::slice::from_ref(&a), &t, ms(0));
        assert_eq!(d.step(&[], &t, ms(300)), Step::Pending);
        assert_eq!(d.step(std::slice::from_ref(&a), &t, ms(500)), Step::Pending);
        assert_eq!(found_hwnd(&d.step(&[a], &t, ms(900))), 2);
    }

    #[test]
    fn ineligible_windows_are_ignored() {
        let t = tree();
        let mut hidden = win(2, LAUNCHED, "Deck", "App");
        hidden.visible = false;
        let mut cloaked = win(3, LAUNCHED, "Deck", "App");
        cloaked.cloaked = true;
        let mut tool = win(4, LAUNCHED, "Deck", "App");
        tool.tool_window = true;
        let mut empty = win(5, LAUNCHED, "Deck", "App");
        empty.rect = Rect::from_xywh(0, 0, 0, 0);
        let mut owned = win(6, LAUNCHED, "Deck", "#32770");
        owned.owner = Some(99);
        let all = [hidden, cloaked, tool, empty, owned];
        let mut d = disc(Strategy::NewWindowOfProcess, &[]);
        assert_eq!(d.step(&all, &t, ms(1000)), Step::Pending);
        // At the timeout, the dialog whose owner is hidden is offered; the tool window too is
        // not (it is unowned but a tool window).
        match d.step(&all, &t, ms(15_000)) {
            Step::Pick(c) => {
                assert_eq!(c.len(), 1);
                assert_eq!(c[0].window.hwnd, 6);
                assert_eq!(c[0].reason, MatchReason::OrphanDialog);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn timeout_without_anything() {
        let mut d = disc(Strategy::TitleContainsFilename, &[]);
        let unrelated = win(2, 7, "Mail", "App");
        assert_eq!(
            d.step(std::slice::from_ref(&unrelated), &tree(), ms(14_900)),
            Step::Pending
        );
        assert_eq!(d.step(&[unrelated], &tree(), ms(15_000)), Step::TimedOut);
    }

    #[test]
    fn timeout_falls_back_to_other_rules() {
        // Title strategy, but the app titles its window differently: at the timeout the process
        // match is offered for picking.
        let mut d = disc(Strategy::TitleContainsFilename, &[]);
        let w = win(2, LAUNCHED, "Untitled - App", "App");
        assert_eq!(
            d.step(std::slice::from_ref(&w), &tree(), ms(1000)),
            Step::Pending
        );
        match d.step(&[w], &tree(), ms(15_000)) {
            Step::Pick(c) => assert_eq!(c[0].reason, MatchReason::Process),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pick_strategy_always_picks() {
        let mut d = disc(Strategy::Pick, &[]);
        let w = win(2, LAUNCHED, "x", "App");
        d.step(std::slice::from_ref(&w), &tree(), ms(0));
        match d.step(&[w], &tree(), ms(400)) {
            Step::Pick(c) => assert_eq!(c.len(), 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn handle_reuse_by_another_process_is_new() {
        let s0 = vec![win(2, 7, "Deck", "Old")];
        let mut d = disc(Strategy::NewWindowOfProcess, &s0);
        let now = vec![win(2, LAUNCHED, "Deck", "App")];
        d.step(&now, &tree(), ms(0));
        assert_eq!(found_hwnd(&d.step(&now, &tree(), ms(400))), 2);
    }

    /// Found on Windows 11 26200 (2026-11-02): Microsoft Photos is a packaged desktop app whose
    /// top-level window is a `WinUIDesktopWin32WindowClass` of its own process, not an
    /// `ApplicationFrameWindow`. Before, discovery never matched it and the stream ended with
    /// `app_unavailable`.
    #[test]
    fn a_packaged_desktop_app_window_of_the_launched_family_matches() {
        let existing = win(1, 900, "Other app", "WinUIDesktopWin32WindowClass");
        let mut d = Discovery::new(
            MatchTarget {
                strategy: Strategy::PackagedFrame,
                file_stem: "shares".into(),
                package_family: Some("Microsoft.Windows.Photos_8wekyb3d8bbwe".into()),
            },
            timing(),
            default_splash_rules(),
            std::slice::from_ref(&existing),
        );
        let photos = win(2, 4242, "shares.png", "WinUIDesktopWin32WindowClass");
        let stranger = win(3, 31337, "Notepad", "Notepad");
        let now = vec![existing, photos, stranger];
        // The platform found pid 4242 in the Photos package family.
        let t = BTreeSet::from([4242]);
        d.step(&now, &t, ms(0));
        assert_eq!(found_hwnd(&d.step(&now, &t, ms(400))), 2);
    }

    /// Notepad on Windows 11 restores its previous session when it starts: two new windows of
    /// the package, one titled with the file.
    #[test]
    fn a_packaged_app_that_restores_other_windows_gives_the_one_with_the_file() {
        let mut d = Discovery::new(
            MatchTarget {
                strategy: Strategy::PackagedFrame,
                file_stem: "notes".into(),
                package_family: Some("Microsoft.WindowsNotepad_8wekyb3d8bbwe".into()),
            },
            timing(),
            default_splash_rules(),
            &[],
        );
        let restored = win(2, 4242, "*Draft - Notepad", "Notepad");
        let opened = win(3, 4242, "notes.txt - Notepad", "Notepad");
        let now = vec![restored, opened];
        let t = BTreeSet::from([4242]);
        d.step(&now, &t, ms(0));
        assert_eq!(found_hwnd(&d.step(&now, &t, ms(400))), 3);
    }

    #[test]
    fn packaged_frame_mapping() {
        let mut existing = win(1, 900, "Photos", APPLICATION_FRAME_CLASS);
        existing.hosted = Some(HostedApp {
            pid: 55,
            package_family: Some("Other_abc".into()),
        });
        let mut d = Discovery::new(
            MatchTarget {
                strategy: Strategy::PackagedFrame,
                file_stem: "holiday".into(),
                package_family: Some("Microsoft.Windows.Photos_8wekyb3d8bbwe".into()),
            },
            timing(),
            default_splash_rules(),
            std::slice::from_ref(&existing),
        );
        let mut frame = win(2, 900, "Photos", APPLICATION_FRAME_CLASS);
        frame.hosted = Some(HostedApp {
            pid: 4242,
            package_family: Some("microsoft.windows.photos_8wekyb3d8bbwe".into()),
        });
        // A frame hosting another app, and the core window itself, do not match.
        let mut other = win(3, 900, "Calculator", APPLICATION_FRAME_CLASS);
        other.hosted = Some(HostedApp {
            pid: 77,
            package_family: Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe".into()),
        });
        let core = win(4, 4242, "Photos", CORE_WINDOW_CLASS);
        let now = vec![existing, frame, other, core.clone()];
        let t = BTreeSet::from([4242]);
        d.step(&now, &t, ms(0));
        let step = d.step(&now, &t, ms(400));
        // The core window (pid 4242) is a process match but not the packaged frame.
        assert_eq!(found_hwnd(&step), 2);
        // Matching by pid alone works when the family is unreadable.
        let mut d = Discovery::new(
            MatchTarget {
                strategy: Strategy::PackagedFrame,
                file_stem: String::new(),
                package_family: Some("X_y".into()),
            },
            timing(),
            vec![],
            &[],
        );
        let mut f = win(8, 900, "", APPLICATION_FRAME_CLASS);
        f.hosted = Some(HostedApp {
            pid: 4242,
            package_family: None,
        });
        d.step(std::slice::from_ref(&f), &t, ms(0));
        assert_eq!(found_hwnd(&d.step(&[f], &t, ms(400))), 8);
    }

    #[test]
    fn owned_dialogs_are_reported() {
        let main = win(2, LAUNCHED, "Deck", "App");
        let mut dlg = win(3, LAUNCHED, "Save changes?", "#32770");
        dlg.owner = Some(2);
        let mut nested = win(4, LAUNCHED, "Details", "#32770");
        nested.owner = Some(3);
        let mut hidden = win(5, LAUNCHED, "x", "#32770");
        hidden.owner = Some(2);
        hidden.visible = false;
        let now = vec![dlg, nested, main, hidden];
        let mut d = disc(Strategy::NewWindowOfProcess, &[]);
        d.step(&now, &tree(), ms(0));
        let Step::Found(c) = d.step(&now, &tree(), ms(400)) else {
            panic!("expected found")
        };
        assert_eq!(c.window.hwnd, 2);
        assert_eq!(
            c.owned.iter().map(|w| w.hwnd).collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn strategy_from_catalogue() {
        assert_eq!(
            Strategy::for_entry(WindowMatch::TitleContainsFilename, false),
            Strategy::TitleContainsFilename
        );
        assert_eq!(
            Strategy::for_entry(WindowMatch::TitleContainsFilename, true),
            Strategy::PackagedFrame
        );
        assert_eq!(Strategy::for_entry(WindowMatch::Pick, true), Strategy::Pick);
        assert_eq!(
            Strategy::for_entry(WindowMatch::NewWindowOfProcess, false),
            Strategy::NewWindowOfProcess
        );
    }
}
