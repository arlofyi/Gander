// SPDX-License-Identifier: Apache-2.0
//! From a policy decision to a launch plan: how to start the app, how to find its window and how
//! long to wait. The policy ([`crate::policy::StreamPolicy::evaluate`]) is the gate: nothing is
//! planned for a refused decision, and an `ask` decision needs the owner's "Allow once" (an
//! "Always" answer rewrites the catalogue, after which the policy answers `allowed`).

use std::path::{Path, PathBuf};

use super::command::{self, CommandError, LaunchCommand};
use super::discovery::{
    DiscoveryTiming, MatchTarget, SplashRule, Strategy, default_splash_rules, file_stem,
};
use crate::config::{AppCatalogue, CloseOnEnd, LaunchSpec, WindowMatch};
use crate::policy::{
    Browser, DecisionReason, IsolatedBrowserLaunch, PolicyDecision, Signature, entry_matches, rules,
};
use crate::protocol::files::StreamDecision;

/// What authorises the launch besides the policy outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Authorisation {
    /// Only the policy: the decision must be `allowed`.
    Policy,
    /// The owner answered "Allow once" on the host for this request: an `ask` decision may
    /// launch. A `refused` decision never does.
    OwnerApprovedOnce,
}

/// How the app is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMethod {
    /// `CreateProcessW` with this command.
    Process(LaunchCommand),
    /// A packaged app: `IApplicationActivationManager::ActivateForFile` with this
    /// `AppUserModelID` and the file.
    Packaged {
        /// The app's `AppUserModelID`.
        app_user_model_id: String,
    },
}

/// Everything the platform side needs for one launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// How to start.
    pub method: LaunchMethod,
    /// What discovery looks for.
    pub target: MatchTarget,
    /// Poll, settle and timeout.
    pub timing: DiscoveryTiming,
    /// Splash classes to skip.
    pub splash: Vec<SplashRule>,
    /// Set for a browser handler (the phone is told "opens in X (isolated window)").
    pub browser: Option<IsolatedBrowserLaunch>,
    /// What to do with the app at the end of the session (catalogue `close_on_end`).
    pub close_on_end: CloseOnEnd,
}

/// The inputs of [`plan_launch`].
#[derive(Debug, Clone, Copy)]
pub struct PlanInput<'a> {
    /// The policy's decision for this file and device.
    pub decision: &'a PolicyDecision,
    /// The catalogue the decision was made with.
    pub catalogue: &'a AppCatalogue,
    /// The resolved absolute path of the file (inside the share).
    pub file: &'a Path,
    /// Policy alone, or with the owner's "Allow once".
    pub authorisation: Authorisation,
    /// The dedicated browser profile directory (needed for Chromium browsers).
    pub browser_profile_dir: Option<&'a Path>,
}

/// Why nothing may be launched.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// The policy refused.
    #[error("the stream policy refused this file ({})", .0.as_str())]
    Refused(DecisionReason),
    /// The policy asks and the owner has not answered.
    #[error("the owner has not approved this app")]
    NeedsOwnerApproval,
    /// The decision carries no handler.
    #[error("the decision has no handler")]
    NoHandler,
    /// The decision is for a different file type than the file.
    #[error("the decision does not belong to this file")]
    DecisionMismatch,
    /// The decision names a catalogue entry that is missing or does not describe the handler.
    #[error("the catalogue entry does not match the handler")]
    EntryMismatch,
    /// A browser handler without a profile directory.
    #[error("the isolated browser profile directory is missing")]
    NoBrowserProfile,
    /// A packaged handler without an `AppUserModelID`.
    #[error("the packaged app has no AppUserModelID")]
    NoAppUserModelId,
    /// The command could not be built.
    #[error(transparent)]
    Command(#[from] CommandError),
}

/// The dedicated profile directory of `browser` under the host's data directory
/// (`%LOCALAPPDATA%\Gander`): `<data_dir>\stream-browser\<browser>`. One per browser, because
/// Chromium browsers must not share a profile and the owner's own profile must never be used.
pub fn browser_profile_dir(data_dir: &Path, browser: Browser) -> PathBuf {
    let name = match browser {
        Browser::Edge => "edge",
        Browser::Chrome => "chrome",
        Browser::Firefox => "firefox",
        Browser::Brave => "brave",
        Browser::Opera => "opera",
        Browser::Vivaldi => "vivaldi",
        Browser::Arc => "arc",
    };
    data_dir.join("stream-browser").join(name)
}

/// The last path segment (either separator).
fn file_name(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.rsplit(['\\', '/']).next().unwrap_or_default().to_owned()
}

/// Plans the launch of `input.file` under `input.decision`.
pub fn plan_launch(input: &PlanInput<'_>) -> Result<LaunchPlan, PlanError> {
    let decision = input.decision;
    match (decision.outcome, input.authorisation) {
        (StreamDecision::Allowed, _) => {}
        (StreamDecision::Ask, Authorisation::OwnerApprovedOnce) => {}
        (StreamDecision::Ask, Authorisation::Policy) => return Err(PlanError::NeedsOwnerApproval),
        (StreamDecision::Refused, _) => return Err(PlanError::Refused(decision.reason)),
    }
    let name = file_name(input.file);
    if rules::final_extension(&name) != decision.extension || decision.extension.is_none() {
        return Err(PlanError::DecisionMismatch);
    }
    let handler = decision.handler.as_ref().ok_or(PlanError::NoHandler)?;
    let entry = match &decision.app {
        Some(app) => {
            let e = input
                .catalogue
                .apps
                .iter()
                .find(|e| e.id == app.id)
                .ok_or(PlanError::EntryMismatch)?;
            if !entry_matches(e, handler) {
                return Err(PlanError::EntryMismatch);
            }
            Some(e)
        }
        None => None,
    };

    let packaged = is_packaged(handler);
    let window_match = entry.map_or(WindowMatch::default(), |e| e.window_match);
    let (method, strategy) = if let Some(browser) = decision.browser {
        // Step 6: a browser is launched only through the isolated template, whatever the
        // catalogue says. Its window belongs to the new browser process.
        let profile = input
            .browser_profile_dir
            .ok_or(PlanError::NoBrowserProfile)?;
        let cmd = command::for_browser(&browser, &handler.exe, input.file, profile)?;
        let strategy = match window_match {
            WindowMatch::Pick => Strategy::Pick,
            _ => Strategy::NewWindowOfProcess,
        };
        (LaunchMethod::Process(cmd), strategy)
    } else if packaged {
        let aumid = handler
            .app_user_model_id
            .clone()
            .ok_or(PlanError::NoAppUserModelId)?;
        (
            LaunchMethod::Packaged {
                app_user_model_id: aumid,
            },
            Strategy::for_entry(window_match, true),
        )
    } else {
        let cmd = match entry.map(|e| &e.launch) {
            Some(LaunchSpec::Template(t)) => command::from_template(t, &handler.exe, input.file)?,
            Some(LaunchSpec::Default) | None => {
                command::from_registered(handler.command.as_deref(), &handler.exe, input.file)?
            }
        };
        (
            LaunchMethod::Process(cmd),
            Strategy::for_entry(window_match, false),
        )
    };

    Ok(LaunchPlan {
        method,
        target: MatchTarget {
            strategy,
            file_stem: file_stem(&name),
            package_family: handler.package_family_name.clone(),
        },
        timing: DiscoveryTiming::for_entry(entry.is_some_and(|e| e.slow_start)),
        splash: default_splash_rules(),
        browser: decision.browser,
        close_on_end: entry.map_or(CloseOnEnd::default(), |e| e.close_on_end),
    })
}

/// A packaged app: known only by its AppUserModelID, or an executable inside a package
/// (`...\WindowsApps\<package>\...`, such as Notepad on Windows 11). A package's executable
/// cannot be started with `CreateProcess` from outside the package (access denied, measured on
/// Windows 11 build 26200, 2026-11-02); it is activated by its AppUserModelID instead.
fn is_packaged(handler: &crate::policy::HandlerInfo) -> bool {
    handler.signature == Signature::Package
        || (handler.app_user_model_id.is_some()
            && handler
                .exe
                .components()
                .any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::config::AppEntry;
    use crate::policy::{
        HandlerInfo, PolicyEnvironment, PolicyInput, StaticAssociations, StreamPolicy,
    };
    use crate::protocol::AppId;

    const TESTWIN: &str = r"C:\build\gander-testwin.exe";

    fn entry(id: &str, exe: &str, ext: &str, launch: LaunchSpec, wm: WindowMatch) -> AppEntry {
        AppEntry {
            id: AppId::from(id.to_owned()),
            name: id.to_owned(),
            exe: PathBuf::from(exe),
            publisher: None,
            extensions: vec![ext.to_owned()],
            approved_by_owner: Some(time::OffsetDateTime::UNIX_EPOCH),
            launch,
            window_match: wm,
            close_on_end: CloseOnEnd::Never,
            slow_start: true,
            package_family_name: None,
        }
    }

    fn decide(cat: &AppCatalogue, assoc: &StaticAssociations, file: &str) -> PolicyDecision {
        let env = PolicyEnvironment::default();
        StreamPolicy::new(cat, assoc, &env).evaluate(&PolicyInput {
            file_name: file,
            stream_permitted: true,
            share_default: StreamDecision::Ask,
            requested_app: None,
        })
    }

    fn input<'a>(d: &'a PolicyDecision, cat: &'a AppCatalogue, file: &'a Path) -> PlanInput<'a> {
        PlanInput {
            decision: d,
            catalogue: cat,
            file,
            authorisation: Authorisation::Policy,
            browser_profile_dir: Some(Path::new(
                r"C:\Users\u\AppData\Local\Gander\stream-browser\edge",
            )),
        }
    }

    fn testwin_setup(launch: LaunchSpec, wm: WindowMatch) -> (AppCatalogue, StaticAssociations) {
        let cat = AppCatalogue {
            apps: vec![entry("testwin", TESTWIN, "gtw", launch, wm)],
            ..AppCatalogue::default()
        };
        let assoc =
            StaticAssociations::new().with("gtw", HandlerInfo::new(TESTWIN, Signature::Unsigned));
        (cat, assoc)
    }

    #[test]
    fn approved_template_entry() {
        let (cat, assoc) = testwin_setup(
            LaunchSpec::Template("{exe} --self-test-secs 60 --title {file}".into()),
            WindowMatch::NewWindowOfProcess,
        );
        let d = decide(&cat, &assoc, "Deck One.gtw");
        let file = Path::new(r"C:\s\Deck One.gtw");
        let plan = plan_launch(&input(&d, &cat, file)).unwrap();
        let LaunchMethod::Process(cmd) = &plan.method else {
            panic!("process launch expected")
        };
        assert_eq!(cmd.program, TESTWIN);
        assert_eq!(
            cmd.args,
            vec!["--self-test-secs", "60", "--title", r"C:\s\Deck One.gtw"]
        );
        assert_eq!(plan.target.strategy, Strategy::NewWindowOfProcess);
        assert_eq!(plan.target.file_stem, "Deck One");
        assert_eq!(plan.timing.timeout, Duration::from_secs(30));
        assert_eq!(plan.close_on_end, CloseOnEnd::Never);
        assert!(plan.splash.iter().any(|r| r.class == "MsoSplash"));
    }

    #[test]
    fn refused_and_ask_are_gated() {
        let (cat, assoc) = testwin_setup(LaunchSpec::Default, WindowMatch::default());
        let d = decide(&cat, &assoc, "evil.gtw.exe");
        assert!(matches!(
            plan_launch(&input(&d, &cat, Path::new(r"C:\s\evil.gtw.exe"))),
            Err(PlanError::Refused(DecisionReason::HardRefusedExtension))
        ));
        // Unknown handler -> ask.
        let empty = AppCatalogue::default();
        let d = decide(&empty, &assoc, "a.gtw");
        assert_eq!(d.outcome, StreamDecision::Ask);
        let file = Path::new(r"C:\s\a.gtw");
        assert_eq!(
            plan_launch(&input(&d, &empty, file)),
            Err(PlanError::NeedsOwnerApproval)
        );
        let once = PlanInput {
            authorisation: Authorisation::OwnerApprovedOnce,
            ..input(&d, &empty, file)
        };
        let plan = plan_launch(&once).unwrap();
        assert_eq!(plan.target.strategy, Strategy::TitleContainsFilename);
        assert_eq!(plan.timing.timeout, Duration::from_secs(15));
        // A refused decision stays refused even with an owner answer.
        let d = decide(&cat, &assoc, "noext");
        let r = plan_launch(&PlanInput {
            authorisation: Authorisation::OwnerApprovedOnce,
            ..input(&d, &cat, Path::new(r"C:\s\noext"))
        });
        assert!(matches!(r, Err(PlanError::Refused(_))));
    }

    #[test]
    fn decision_for_another_file_is_rejected() {
        let (cat, assoc) = testwin_setup(LaunchSpec::Default, WindowMatch::default());
        let d = decide(&cat, &assoc, "a.gtw");
        assert_eq!(
            plan_launch(&input(&d, &cat, Path::new(r"C:\s\a.txt"))),
            Err(PlanError::DecisionMismatch)
        );
    }

    #[test]
    fn entry_must_describe_the_handler() {
        let (cat, assoc) = testwin_setup(LaunchSpec::Default, WindowMatch::default());
        let mut d = decide(&cat, &assoc, "a.gtw");
        d.handler = Some(HandlerInfo::new(r"C:\other\x.exe", Signature::Unsigned));
        assert_eq!(
            plan_launch(&input(&d, &cat, Path::new(r"C:\s\a.gtw"))),
            Err(PlanError::EntryMismatch)
        );
    }

    #[test]
    fn default_launch_uses_registered_command() {
        let (cat, _) = testwin_setup(LaunchSpec::Default, WindowMatch::TitleContainsFilename);
        let mut h = HandlerInfo::new(TESTWIN, Signature::Unsigned);
        h.command = Some(format!("\"{TESTWIN}\" /open \"%1\""));
        let assoc = StaticAssociations::new().with("gtw", h);
        let d = decide(&cat, &assoc, "a.gtw");
        let plan = plan_launch(&input(&d, &cat, Path::new(r"C:\s\a.gtw"))).unwrap();
        let LaunchMethod::Process(cmd) = plan.method else {
            panic!()
        };
        assert_eq!(cmd.args, vec!["/open", r"C:\s\a.gtw"]);
    }

    #[test]
    fn browser_goes_through_the_isolated_template() {
        let edge = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe";
        let cat = AppCatalogue {
            apps: vec![entry(
                "edge",
                edge,
                "pdf",
                // A catalogue template is ignored for browsers.
                LaunchSpec::Template("{exe} {file}".into()),
                WindowMatch::TitleContainsFilename,
            )],
            ..AppCatalogue::default()
        };
        let assoc = StaticAssociations::new().with(
            "pdf",
            HandlerInfo::new(
                edge,
                Signature::Signed {
                    publisher: "Microsoft Corporation".into(),
                },
            ),
        );
        let d = decide(&cat, &assoc, "Manual.pdf");
        let file = Path::new(r"C:\s\Manual.pdf");
        let plan = plan_launch(&input(&d, &cat, file)).unwrap();
        let LaunchMethod::Process(cmd) = &plan.method else {
            panic!()
        };
        assert_eq!(cmd.args[0], "--new-window");
        assert_eq!(cmd.args[1], "--app=file:///C:/s/Manual.pdf");
        assert!(cmd.args[2].starts_with("--user-data-dir="));
        assert_eq!(plan.target.strategy, Strategy::NewWindowOfProcess);
        assert!(plan.browser.is_some());
        let no_profile = PlanInput {
            browser_profile_dir: None,
            ..input(&d, &cat, file)
        };
        assert_eq!(plan_launch(&no_profile), Err(PlanError::NoBrowserProfile));
    }

    #[test]
    fn browser_profiles_are_per_browser() {
        let d = Path::new("data");
        assert_eq!(
            browser_profile_dir(d, Browser::Edge),
            d.join("stream-browser").join("edge")
        );
        assert_ne!(
            browser_profile_dir(d, Browser::Edge),
            browser_profile_dir(d, Browser::Chrome)
        );
    }

    /// Notepad on Windows 11: a signed executable inside a package, with an AppUserModelID.
    #[test]
    fn an_executable_inside_a_package_is_activated_not_spawned() {
        let aumid = "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App";
        let exe = r"C:\Program Files\WindowsApps\Microsoft.WindowsNotepad_11.2607.14.0_x64__8wekyb3d8bbwe\Notepad\Notepad.exe";
        let h = HandlerInfo {
            exe: PathBuf::from(exe),
            command: Some(format!("\"{exe}\"")),
            signature: Signature::Signed {
                publisher: "Microsoft Corporation".into(),
            },
            app_user_model_id: Some(aumid.into()),
            package_family_name: Some("Microsoft.WindowsNotepad_8wekyb3d8bbwe".into()),
        };
        let mut e = entry(
            "notepad",
            exe,
            "txt",
            LaunchSpec::Default,
            WindowMatch::TitleContainsFilename,
        );
        e.package_family_name = h.package_family_name.clone();
        let cat = AppCatalogue {
            apps: vec![e],
            ..AppCatalogue::default()
        };
        let assoc = StaticAssociations::new().with("txt", h);
        let d = decide(&cat, &assoc, "notes.txt");
        assert_eq!(d.outcome, StreamDecision::Allowed);
        let plan = plan_launch(&input(&d, &cat, Path::new(r"C:\s\notes.txt"))).unwrap();
        assert_eq!(
            plan.method,
            LaunchMethod::Packaged {
                app_user_model_id: aumid.into()
            }
        );
        assert_eq!(plan.target.strategy, Strategy::PackagedFrame);
    }

    #[test]
    fn packaged_app_is_activated() {
        let aumid = "Microsoft.Windows.Photos_8wekyb3d8bbwe!App";
        let h = HandlerInfo {
            exe: PathBuf::from(format!("shell:AppsFolder\\{aumid}")),
            command: None,
            signature: Signature::Package,
            app_user_model_id: Some(aumid.into()),
            package_family_name: Some("Microsoft.Windows.Photos_8wekyb3d8bbwe".into()),
        };
        let mut e = entry(
            "photos",
            &h.exe.to_string_lossy(),
            "jpg",
            LaunchSpec::Default,
            WindowMatch::TitleContainsFilename,
        );
        e.package_family_name = h.package_family_name.clone();
        let cat = AppCatalogue {
            apps: vec![e],
            ..AppCatalogue::default()
        };
        let assoc = StaticAssociations::new().with("jpg", h);
        let d = decide(&cat, &assoc, "holiday.jpg");
        assert_eq!(d.outcome, StreamDecision::Allowed);
        let plan = plan_launch(&input(&d, &cat, Path::new(r"C:\s\holiday.jpg"))).unwrap();
        assert_eq!(
            plan.method,
            LaunchMethod::Packaged {
                app_user_model_id: aumid.into()
            }
        );
        assert_eq!(plan.target.strategy, Strategy::PackagedFrame);
        assert_eq!(
            plan.target.package_family.as_deref(),
            Some("Microsoft.Windows.Photos_8wekyb3d8bbwe")
        );
    }
}
