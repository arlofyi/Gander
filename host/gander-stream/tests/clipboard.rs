// SPDX-License-Identifier: Apache-2.0
//! The clipboard text bridge (WP-56) over a real WebRTC session on 127.0.0.1: the loopback
//! viewer sends `clipboard` and `clipboard.get` on the `control` channel, an in-memory clipboard
//! stands in for the Windows one (the real one is covered by
//! `host/gander-win/tests/clipboard.rs`), and the answers, the refusals and the observer's events
//! are checked. The input target is a handle that is no window, so nothing is ever injected.

#![cfg(windows)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use gander_core::clipboard::{
    ClipboardMode, ClipboardRefusal, ClipboardService, ClipboardSetting, Direction, MAX_TEXT_BYTES,
    MemoryClipboard, RecordingObserver,
};
use gander_core::input::{GateState, PauseCause};
use gander_stream::input::{ClipboardLink, attach_with_clipboard};
use gander_stream::testing::{SyntheticSource, Viewer, ViewerOptions};
use gander_stream::{EndReason, Session, WindowTarget};
use gander_win::input::{InputOptions, InputTarget};

fn window() -> WindowTarget {
    WindowTarget {
        hwnd: 0,
        title: "synthetic".into(),
    }
}

/// A handle that is never a window.
fn no_window() -> InputTarget {
    InputTarget::new(0x7FFF_FFF0, 1)
}

struct Rig {
    session: Session,
    viewer: Viewer,
    backend: Arc<MemoryClipboard>,
    observer: Arc<RecordingObserver>,
    setting: ClipboardSetting,
    _link: gander_stream::input::InputLink,
    _h: Harness,
}

/// The next `control` message containing `needle`, from the ones received since the last
/// [`clear`].
async fn control_text(viewer: &Viewer, needle: &str) -> Option<String> {
    until(Duration::from_secs(5), || {
        viewer
            .stats
            .control_text_rx
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.contains(needle))
    })
    .await?;
    viewer
        .stats
        .control_text_rx
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.contains(needle))
        .cloned()
}

fn clear(viewer: &Viewer) {
    viewer.stats.control_text_rx.lock().unwrap().clear();
}

/// A session with the clipboard link attached. `gate` replaces the real input gate (None: the
/// real one, over `lock`).
async fn rig(
    mode: ClipboardMode,
    initial: Option<&str>,
    gate: Option<GateState>,
    locked: bool,
) -> Rig {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    // The engine's answers go out only once `control` is open: wait for a pong.
    assert!(control_text(&viewer, "pong").await.is_some());
    let backend = Arc::new(MemoryClipboard::with_text(initial));
    let observer = Arc::new(RecordingObserver::default());
    let setting = ClipboardSetting::new(mode);
    let service = ClipboardService::new(setting.clone(), backend.clone(), observer.clone());
    let mut link = ClipboardLink::new(service);
    if let Some(g) = gate {
        link = link.with_gate_probe(Arc::new(move || g));
    }
    let input = attach_with_clipboard(
        &session,
        no_window(),
        Arc::new(move || locked),
        InputOptions::default(),
        Some(link),
        false,
    )
    .expect("attach");
    clear(&viewer);
    Rig {
        session,
        viewer,
        backend,
        observer,
        setting,
        _link: input,
        _h: h,
    }
}

async fn stop(rig: Rig) {
    rig.session.stop(EndReason::ClientClosed);
    assert_eq!(rig.session.ended().await, EndReason::ClientClosed);
    let link = rig._link;
    tokio::task::spawn_blocking(move || drop(link))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_then_get_round_trips_over_the_control_channel() {
    let r = rig(ClipboardMode::Both, None, Some(GateState::Active), false).await;

    let text = "first line\nsecond \"quoted\" line \u{e9}\u{1F600}";
    let msg = serde_json::json!({ "t": "clipboard", "s": text }).to_string();
    r.viewer.control_send_text(&msg).await.unwrap();
    let ack = control_text(&r.viewer, "clipboard.ack").await.expect("ack");
    let bytes = text.len();
    assert_eq!(ack, format!(r#"{{"t":"clipboard.ack","bytes":{bytes}}}"#));
    assert_eq!(r.backend.text().as_deref(), Some(text));

    clear(&r.viewer);
    r.viewer
        .control_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    let got = control_text(&r.viewer, r#""t":"clipboard","#)
        .await
        .expect("clipboard text");
    let value: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(value["s"], text);

    // Audit-relevant facts: both transfers, direction and size, never the text.
    let events = r.observer.events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0].direction, Direction::FromPhone);
    assert_eq!(events[1].direction, Direction::ToPhone);
    assert!(events.iter().all(|e| e.outcome == Ok(bytes)));
    assert!(events.iter().all(|e| e.device == device()));
    assert!(format!("{events:?}").find("first line").is_none());
    stop(r).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_text_at_the_cap_fits_one_message_and_one_byte_more_is_refused() {
    let r = rig(ClipboardMode::Both, None, Some(GateState::Active), false).await;
    // 64 KiB with a newline every 16 bytes: the JSON is about 70 KiB, over SCTP's 64 KiB default.
    let line = "0123456789abcde\n";
    let text = line.repeat(MAX_TEXT_BYTES / line.len());
    assert_eq!(text.len(), MAX_TEXT_BYTES);
    let msg = serde_json::json!({ "t": "clipboard", "s": text }).to_string();
    assert!(msg.len() > 64 * 1024);
    r.viewer.control_send_text(&msg).await.unwrap();
    let ack = control_text(&r.viewer, "clipboard.ack").await.expect("ack");
    assert!(
        ack.contains(&format!(r#""bytes":{MAX_TEXT_BYTES}"#)),
        "{ack}"
    );
    assert_eq!(r.backend.text().as_deref(), Some(text.as_str()));

    clear(&r.viewer);
    r.viewer
        .control_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    let got = control_text(&r.viewer, r#""t":"clipboard","#)
        .await
        .expect("the full text comes back");
    let value: serde_json::Value = serde_json::from_str(&got).unwrap();
    assert_eq!(value["s"].as_str(), Some(text.as_str()));

    // One byte more is refused whole; the clipboard keeps what it had.
    clear(&r.viewer);
    let over = format!("{text}x");
    let msg = serde_json::json!({ "t": "clipboard", "s": over }).to_string();
    r.viewer.control_send_text(&msg).await.unwrap();
    let refused = control_text(&r.viewer, "clipboard.refused")
        .await
        .expect("refused");
    assert_eq!(
        refused,
        r#"{"t":"clipboard.refused","op":"set","reason":"too_large"}"#
    );
    assert_eq!(r.backend.text().as_deref(), Some(text.as_str()));
    assert_eq!(
        r.observer.events().last().unwrap().outcome,
        Err(ClipboardRefusal::TooLarge)
    );
    stop(r).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_setting_decides_the_direction_and_applies_at_once() {
    let r = rig(
        ClipboardMode::ToPhone,
        Some("host text"),
        Some(GateState::Active),
        false,
    )
    .await;

    // To phone only: reading works, writing is refused.
    r.viewer
        .control_send_text(r#"{"t":"clipboard","s":"from the phone"}"#)
        .await
        .unwrap();
    let refused = control_text(&r.viewer, "clipboard.refused")
        .await
        .expect("refused");
    assert_eq!(
        refused,
        r#"{"t":"clipboard.refused","op":"set","reason":"disabled"}"#
    );
    assert_eq!(r.backend.text().as_deref(), Some("host text"));
    clear(&r.viewer);
    r.viewer
        .control_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    assert!(
        control_text(&r.viewer, r#"{"t":"clipboard","s":"host text"}"#)
            .await
            .is_some()
    );

    // Turned off while the session runs: the next request is refused.
    r.setting.set(ClipboardMode::Off);
    clear(&r.viewer);
    r.viewer
        .control_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    let refused = control_text(&r.viewer, "clipboard.refused")
        .await
        .expect("refused");
    assert_eq!(
        refused,
        r#"{"t":"clipboard.refused","op":"get","reason":"disabled"}"#
    );
    assert_eq!(r.backend.reads(), 1, "the clipboard was not read again");
    stop(r).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_inactive_stream_gets_no_clipboard_access() {
    for cause in [
        PauseCause::PhoneBackground,
        PauseCause::FocusLost,
        PauseCause::LocalUserActive,
        PauseCause::Locked,
    ] {
        let r = rig(
            ClipboardMode::Both,
            Some("secret"),
            Some(GateState::Paused(cause)),
            false,
        )
        .await;
        r.viewer
            .control_send_text(r#"{"t":"clipboard.get"}"#)
            .await
            .unwrap();
        let refused = control_text(&r.viewer, "clipboard.refused")
            .await
            .expect("refused");
        let code = ClipboardRefusal::Paused(cause).code();
        assert_eq!(
            refused,
            format!(r#"{{"t":"clipboard.refused","op":"get","reason":"{code}"}}"#)
        );
        assert!(!refused.contains("secret"));
        assert_eq!(r.backend.reads(), 0, "{cause:?}");
        stop(r).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_real_input_gate_and_its_lock_probe_guard_the_clipboard() {
    // No gate override: the controller's own gate over the lock probe. The target is not a
    // window, so it is never in front, but the lock is checked first and says why.
    let locked = rig(ClipboardMode::Both, Some("secret"), None, true).await;
    locked
        .viewer
        .control_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    let refused = control_text(&locked.viewer, "clipboard.refused")
        .await
        .expect("refused");
    assert!(refused.contains(r#""reason":"host_locked""#), "{refused}");
    assert_eq!(locked.backend.reads(), 0);
    stop(locked).await;

    let unlocked = rig(ClipboardMode::Both, Some("secret"), None, false).await;
    unlocked
        .viewer
        .control_send_text(r#"{"t":"clipboard","s":"x"}"#)
        .await
        .unwrap();
    let refused = control_text(&unlocked.viewer, "clipboard.refused")
        .await
        .expect("refused");
    assert!(refused.contains(r#""reason":"focus_lost""#), "{refused}");
    assert_eq!(unlocked.backend.writes(), 0);
    stop(unlocked).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clipboard_messages_on_the_lossy_input_channel_are_refused() {
    let r = rig(
        ClipboardMode::Both,
        Some("secret"),
        Some(GateState::Active),
        false,
    )
    .await;
    r.viewer
        .input_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    let refused = control_text(&r.viewer, "input.refused")
        .await
        .expect("refused");
    assert!(refused.contains(r#""reason":"wrong_channel""#), "{refused}");
    assert_eq!(r.backend.reads(), 0);
    stop(r).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_link_the_bridge_does_not_exist() {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    assert!(control_text(&viewer, "pong").await.is_some());
    let link = gander_stream::input::attach(
        &session,
        no_window(),
        Arc::new(|| false),
        InputOptions::default(),
        false,
    )
    .expect("attach");
    clear(&viewer);
    viewer
        .control_send_text(r#"{"t":"clipboard.get"}"#)
        .await
        .unwrap();
    let refused = control_text(&viewer, "input.refused")
        .await
        .expect("refused");
    assert!(refused.contains(r#""reason":"unsupported""#), "{refused}");
    session.stop(EndReason::ClientClosed);
    assert_eq!(session.ended().await, EndReason::ClientClosed);
    tokio::task::spawn_blocking(move || drop(link))
        .await
        .unwrap();
}
