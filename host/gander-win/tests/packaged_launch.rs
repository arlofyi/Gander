// SPDX-License-Identifier: Apache-2.0
//! Stream launch of the REAL default handler of `.png` on this computer (Microsoft Photos on
//! Windows 11), through the same policy gate and `launch_and_discover` as production.
//!
//! Found on the owner's laptop (2026-11-02): Photos is a packaged *desktop* app.
//! `IApplicationActivationManager::ActivateForFile` refuses it (`0x80270254`, "This app does not
//! support the contract specified") and its window is a `WinUIDesktopWin32WindowClass` of its
//! own process, not an `ApplicationFrameWindow`; every Stream of a `.png` ended with
//! `app_unavailable`. This test opens a generated PNG with the handler, expects the window to
//! be found, and closes it with `WM_CLOSE`.
//!
//! Ignored by default: it opens a real app on the desktop. Run by hand on an unlocked, attended
//! session: `cargo test -p gander-win --test packaged_launch -- --ignored --nocapture`.

#![cfg(windows)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gander_core::config::AppCatalogue;
use gander_core::launch::discovery::DiscoveryTiming;
use gander_core::launch::plan::Authorisation;
use gander_core::policy::{
    AssociationResolver as _, PolicyEnvironment, PolicyInput, Signature, StreamPolicy,
    approve_handler,
};
use gander_core::protocol::files::StreamDecision;
use gander_win::assoc::WindowsAssociations;
use gander_win::launch::{
    CloseOutcome, DiscoveryOutcome, LaunchRequest, close_window, launch_and_discover,
};

/// A real PNG (a UI screenshot from the docs).
const PNG: &[u8] = include_bytes!("../../../docs/specs/assets/ui/devices.png");

#[test]
#[ignore = "opens the real .png handler on the desktop; run by hand on an unlocked session"]
fn the_default_png_handler_is_launched_and_its_window_found() {
    launch_default_handler("png", PNG);
}

/// The classic case: Notepad for `.txt` (on Windows 11 Notepad is itself a packaged app).
#[test]
#[ignore = "opens the real .txt handler on the desktop; run by hand on an unlocked session"]
fn the_default_txt_handler_is_launched_and_its_window_found() {
    launch_default_handler("txt", b"Gander stream launch test\r\n");
}

fn launch_default_handler(ext: &str, bytes: &[u8]) {
    let Some(handler) = WindowsAssociations
        .resolve(ext)
        .expect("association lookup")
    else {
        eprintln!("no .{ext} handler on this machine; nothing to test");
        return;
    };
    eprintln!("handler: {handler:?}");
    let dir = std::env::temp_dir().join(format!("gander-launch-{ext}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file: PathBuf = dir.join(format!("gander launch probe.{ext}"));
    std::fs::write(&file, bytes).unwrap();

    let env = PolicyEnvironment::default();
    let mut catalogue = AppCatalogue::default();
    approve_handler(
        &mut catalogue,
        &env,
        &handler,
        "default viewer",
        ext,
        time::OffsetDateTime::now_utc(),
    )
    .expect("approvable");
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let decision =
        StreamPolicy::new(&catalogue, &WindowsAssociations, &env).evaluate(&PolicyInput {
            file_name: &name,
            stream_permitted: true,
            share_default: StreamDecision::Ask,
            requested_app: None,
        });
    assert_eq!(decision.outcome, StreamDecision::Allowed, "{decision:?}");

    let started = Instant::now();
    let launched = launch_and_discover(&LaunchRequest {
        decision: &decision,
        catalogue: &catalogue,
        file: &file,
        authorisation: Authorisation::Policy,
        browser_profile_dir: None,
        viewport: None,
        timing: Some(DiscoveryTiming {
            poll: Duration::from_millis(200),
            timeout: Duration::from_secs(20),
            settle: Duration::from_millis(800),
        }),
    })
    .expect("launch");
    eprintln!("after {:?}: {:?}", started.elapsed(), launched.outcome);
    let target = match &launched.outcome {
        DiscoveryOutcome::Found(t) => t.clone(),
        other => panic!("expected the handler window, got {other:?}"),
    };
    if handler.signature == Signature::Package {
        assert!(
            target.launched_by_us,
            "the window belongs to the activated package"
        );
    }
    assert!(
        target.title.contains("gander launch probe"),
        "the window shows the file: {target:?}"
    );
    // WM_CLOSE. A tabbed app (Notepad) closes only the file's tab and keeps the window when
    // other tabs are open; no "save changes?" dialog may appear for an unchanged file.
    let closed = close_window(&target, Duration::from_secs(5));
    eprintln!("close: {closed:?}");
    assert!(
        matches!(
            closed,
            Ok(CloseOutcome::Closed | CloseOutcome::LeftOpen { dialog: false })
        ),
        "{closed:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
