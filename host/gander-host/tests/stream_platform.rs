// SPDX-License-Identifier: Apache-2.0
//! WP-53: the Windows platform of the session lifecycle against a real `gander-testwin` window
//! (no capture, no session lock involved): the test-host guard refuses any other executable,
//! the launch finds the window it started, the window is the only one of its process, and the
//! end restores it and closes it with `WM_CLOSE`. The test window is the only app started; it
//! also quits by itself (`--self-test-secs`), and is terminated as a last resort.
//!
//! Needs `target/debug/gander-testwin.exe` (built by `cargo test --workspace` / `cargo build`);
//! skips with a note when it is missing.

#![cfg(windows)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gander_core::config::{AppCatalogue, AppEntry, CloseOnEnd, LaunchSpec, WindowMatch};
use gander_core::launch::plan::Authorisation;
use gander_core::policy::{
    HandlerInfo, PolicyEnvironment, PolicyInput, Signature, StaticAssociations, StreamPolicy,
};
use gander_core::protocol::AppId;
use gander_core::protocol::files::StreamDecision;
use gander_core::protocol::stream::Viewport;
use gander_host::lifecycle::windows::WindowsPlatform;
use gander_host::lifecycle::{LaunchJob, LaunchOutcome, WindowPlatform};

fn testwin() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let debug = exe.parent()?.parent()?;
    let p = debug.join("gander-testwin.exe");
    let p = std::fs::canonicalize(&p).ok()?;
    let s = p.to_string_lossy();
    Some(PathBuf::from(
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned(),
    ))
}

fn job(exe: &std::path::Path, stem: &str) -> (LaunchJob, PathBuf) {
    let catalogue = AppCatalogue {
        apps: vec![AppEntry {
            id: AppId::from("testwin".to_owned()),
            name: "Gander test window".into(),
            exe: exe.to_path_buf(),
            publisher: None,
            extensions: vec!["gtw".into()],
            approved_by_owner: Some(time::OffsetDateTime::UNIX_EPOCH),
            launch: LaunchSpec::Template(
                "{exe} --self-test-secs 60 --no-focus --title {file}".into(),
            ),
            window_match: WindowMatch::NewWindowOfProcess,
            close_on_end: CloseOnEnd::IfLaunchedByUs,
            slow_start: false,
            package_family_name: None,
        }],
        ..AppCatalogue::default()
    };
    let dir = std::env::temp_dir().join(format!("gander-wp53-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{stem}.gtw"));
    std::fs::write(&file, b"").unwrap();
    let assoc = StaticAssociations::new().with("gtw", HandlerInfo::new(exe, Signature::Unsigned));
    let env = PolicyEnvironment::default();
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let decision = StreamPolicy::new(&catalogue, &assoc, &env).evaluate(&PolicyInput {
        file_name: &name,
        stream_permitted: true,
        share_default: StreamDecision::Ask,
        requested_app: None,
    });
    assert_eq!(decision.outcome, StreamDecision::Allowed, "{decision:?}");
    (
        LaunchJob {
            decision,
            catalogue: Arc::new(catalogue),
            file: file.clone(),
            authorisation: Authorisation::Policy,
            viewport: Viewport {
                width: 1080,
                height: 2280,
                dpr: 2.625,
            },
        },
        file,
    )
}

fn platform(only: PathBuf) -> WindowsPlatform {
    WindowsPlatform {
        data_dir: std::env::temp_dir(),
        lock_probe: Arc::new(|| true),
        only_exe: Some(only),
        clipboard: None,
    }
}

/// Terminates `pid` if it is still running (last resort; the test closes it with WM_CLOSE).
fn reap(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}

#[test]
fn test_host_guard_refuses_any_other_executable() {
    let Some(exe) = testwin() else {
        eprintln!("skipped: gander-testwin.exe not built");
        return;
    };
    let (job, _) = job(&exe, "guard");
    let p = platform(PathBuf::from(r"C:\Windows\notepad.exe"));
    let e = p.launch(&job).expect_err("refused before anything runs");
    assert!(e.contains("only gander-testwin"), "{e}");
}

#[test]
fn launch_find_restore_and_close_the_test_window() {
    let Some(exe) = testwin() else {
        eprintln!("skipped: gander-testwin.exe not built");
        return;
    };
    let stem = format!("wp53 platform {}", std::process::id());
    let (job, _file) = job(&exe, &stem);
    let p = platform(exe);
    let launched = p.launch(&job).expect("launch");
    let LaunchOutcome::Found(window) = launched.outcome else {
        reap(launched.pid);
        panic!("expected the window to be found");
    };
    let pid = window.pid;
    assert_eq!(launched.close_on_end, CloseOnEnd::IfLaunchedByUs);
    assert!(window.launched_by_us);
    assert!(window.title.contains(&stem), "{}", window.title);
    assert!(window.size.is_some(), "fitted to the phone");
    assert!(p.is_only_window(&window));
    let report = p.finish(&window, true);
    if !report.closed {
        reap(pid);
    }
    assert!(report.closed, "{report:?}");
    assert!(!report.left_open_dialog);
    // Gone: a second finish has nothing to close.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!p.finish(&window, true).closed);
}

/// The whole lifecycle with the real platform and a real test window, the engine on loopback
/// with the synthetic source (nothing is captured) and a simulated lock source: 201-equivalent
/// start, `window_found` with the fitted size, the engine's offer, the one-session rule, then a
/// simulated lock ends it and the window Gander started is closed. Input is attached with a lock
/// probe that always says locked, so nothing is injected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifecycle_over_the_real_platform_ends_on_simulated_lock() {
    use gander_core::config::IdlePromptDefault;
    use gander_core::error::ErrorCode;
    use gander_core::protocol::events::ServerEvent;
    use gander_core::protocol::stream::StreamState;
    use gander_core::protocol::{DeviceId, ShareId};
    use gander_host::audit::AuditLog;
    use gander_host::lifecycle::{
        NewSession, NoAwake, SessionDeps, SessionsConfig, StreamSessions,
    };
    use gander_stream::testing::{CollectingSink, SyntheticSource};
    use gander_stream::{
        EngineConfig, EngineDeps, FakeLock, FixedPower, StaticAddrs, StreamEngine,
    };

    let Some(exe) = testwin() else {
        eprintln!("skipped: gander-testwin.exe not built");
        return;
    };
    let stem = format!("wp53 lifecycle {}", std::process::id());
    let (job, file) = job(&exe, &stem);
    let sink = Arc::new(CollectingSink::default());
    let mut events = sink.take_receiver();
    let lock = Arc::new(FakeLock::new(false));
    let engine = StreamEngine::new(
        EngineConfig {
            udp_ports: gander_core::config::UdpPortRange::new(57_400, 57_419).unwrap(),
            owner_reports_end: true,
            ..EngineConfig::default()
        },
        EngineDeps {
            lock: lock.clone(),
            power: Arc::new(FixedPower::new(false)),
            addrs: Arc::new(StaticAddrs(vec!["127.0.0.1".parse().unwrap()])),
            signal: sink.clone(),
            source: Arc::new(SyntheticSource::default()),
            audio: None,
        },
    );
    let sessions = StreamSessions::new(
        SessionsConfig {
            idle_default: IdlePromptDefault::Allow,
            ..SessionsConfig::default()
        },
        SessionDeps {
            engine,
            platform: Arc::new(platform(exe)),
            lock: lock.clone(),
            attended: Arc::new(|| false),
            signal: sink,
            audit: Arc::new(AuditLog::memory()),
            awake: Arc::new(NoAwake),
            names: Arc::new(|_| "Pixel 9".to_owned()),
            approve_always: None,
        },
    );
    let device = DeviceId::from("dev_phone");
    let new = NewSession {
        device: device.clone(),
        source: None,
        decision: job.decision.clone(),
        catalogue: Arc::clone(&job.catalogue),
        file,
        file_name: format!("{stem}.gtw"),
        share: ShareId::from("shr_a"),
        path: format!("{stem}.gtw"),
        viewport: job.viewport,
        max_kbps: None,
    };
    let id = sessions.start(new.clone()).expect("201").session;
    let mut found = None;
    let mut offered = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while found.is_none() || !offered {
        let (_, ev) = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("window_found and offer in time")
            .expect("sink open");
        match ev {
            ServerEvent::StreamState(s) if s.session == id => {
                assert_ne!(s.state, StreamState::Ended, "ended early: {s:?}");
                if s.state == StreamState::WindowFound {
                    found = s.window;
                }
            }
            ServerEvent::StreamOffer { session, .. } if session == id => offered = true,
            _ => {}
        }
    }
    let window = found.expect("window info");
    assert!(window.title.contains(&stem), "{window:?}");
    assert!(window.width >= 640 && window.height >= 480, "{window:?}");
    assert_eq!(
        sessions.start(new).unwrap_err().error.code,
        ErrorCode::StreamBusy
    );

    lock.set_locked(true);
    let mut saw_error = false;
    loop {
        let (_, ev) = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("end in time")
            .expect("sink open");
        match ev {
            ServerEvent::Error { code, session, .. } if session.as_ref() == Some(&id) => {
                assert_eq!(code, ErrorCode::HostLocked);
                saw_error = true;
            }
            ServerEvent::StreamState(s) if s.session == id && s.state == StreamState::Ended => {
                break;
            }
            _ => {}
        }
    }
    assert!(saw_error, "error host_locked before ended");
    // The test window Gander started is gone (closed with WM_CLOSE).
    let mut gone = false;
    for _ in 0..50 {
        let out = std::process::Command::new("tasklist")
            .args([
                "/V",
                "/FI",
                "IMAGENAME eq gander-testwin.exe",
                "/FO",
                "CSV",
                "/NH",
            ])
            .output()
            .expect("tasklist");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if !text.contains(&format!("\"{}\"", window.title)) && sessions.active().is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        gone,
        "the test window is still open, or the session was not freed"
    );
}
