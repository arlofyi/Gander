// SPDX-License-Identifier: Apache-2.0
//! The input link (WP-52) over a real WebRTC session on 127.0.0.1 with the synthetic source:
//! data-channel messages reach the injector, its pauses become `stream.state` (and the mirrored
//! `control` state), and its refusals reach the phone as `input.refused`. The target is a handle
//! that is no window, so nothing is ever injected: the clicks themselves are covered by
//! `host/gander-testwin/tests/input.rs`.

#![cfg(windows)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use gander_core::protocol::events::ServerEvent;
use gander_core::protocol::stream::{StreamReason, StreamState};
use gander_stream::input::StartFront;
use gander_stream::testing::{SyntheticSource, ViewerOptions};
use gander_stream::{EndReason, PauseReason, SessionState, WindowTarget};
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

async fn control_text(viewer: &gander_stream::testing::Viewer, needle: &str) -> Option<String> {
    let found = until(Duration::from_secs(3), || {
        viewer
            .stats
            .control_text_rx
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.contains(needle))
    })
    .await;
    found?;
    viewer
        .stats
        .control_text_rx
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.contains(needle))
        .cloned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn focus_lost_is_reported_and_refusals_reach_the_phone() {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    // The engine's mirrored `state` goes out only once `control` is open: wait for a pong.
    assert!(control_text(&viewer, "pong").await.is_some());
    let link = gander_stream::input::attach(
        &session,
        no_window(),
        Arc::new(|| false),
        InputOptions::default(),
        false,
    )
    .expect("attach");

    // The target is not in front: input pauses with focus_lost, video keeps flowing.
    assert!(
        wait_state(&session, Duration::from_secs(3), |s| {
            *s == SessionState::Paused(PauseReason::FocusLost)
        })
        .await
    );
    let id = session.id().clone();
    let reason = next_event(&mut h.events, Duration::from_secs(3), |e| match e {
        ServerEvent::StreamState(s) if s.session == id && s.state == StreamState::Paused => {
            Some(s.reason)
        }
        _ => None,
    })
    .await;
    assert_eq!(reason, Some(Some(StreamReason::FocusLost)));
    let mirrored = control_text(&viewer, "focus_lost").await;
    assert!(mirrored.is_some_and(|t| t.contains(r#""t":"state""#)));
    let frames = session.stats().frames_new;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        session.stats().frames_new > frames,
        "video keeps flowing during an input pause"
    );

    // A key on the lossy channel is refused before the gate and the phone is told.
    viewer
        .input_send_text(r#"{"t":"key","code":"KeyA","down":true}"#)
        .await
        .unwrap();
    let refused = control_text(&viewer, "input.refused")
        .await
        .expect("refused");
    assert!(refused.contains(r#""reason":"wrong_channel""#), "{refused}");
    // A pointer message while paused is not reported again (stream.state already says why).
    viewer
        .input_send_text(r#"{"t":"down","b":"left","x":0.5,"y":0.5}"#)
        .await
        .unwrap();
    // Bring to front: the window does not exist, so it fails and says so.
    viewer.control_send_text(r#"{"t":"front"}"#).await.unwrap();
    let front = control_text(&viewer, "front.result").await.expect("front");
    assert!(front.contains(r#""acquired":false"#), "{front}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let texts = viewer.stats.control_text_rx.lock().unwrap().clone();
    assert_eq!(
        texts.iter().filter(|t| t.contains("input.refused")).count(),
        1,
        "{texts:?}"
    );
    assert!(
        texts.iter().all(|t| !t.contains("pid")),
        "no other app's details"
    );

    drop(link);
    session.stop(EndReason::ClientClosed);
    assert_eq!(session.ended().await, EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_gate_adds_no_input_pause_and_the_link_ends_with_the_session() {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    let link = gander_stream::input::attach(
        &session,
        no_window(),
        Arc::new(|| true),
        InputOptions::default(),
        true,
    )
    .expect("attach");
    viewer
        .control_send_text(r#"{"t":"text","s":"nope"}"#)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Locked is the engine's lock guard's business (it ends the session); the input gate adds
    // no pause reason of its own and sends no refusal for a paused gate.
    assert_eq!(session.state(), SessionState::Streaming);
    let texts = viewer.stats.control_text_rx.lock().unwrap().clone();
    assert!(
        texts.iter().all(|t| !t.contains("input.refused")),
        "{texts:?}"
    );
    // The connect-time foreground attempt is refused while locked, and says nothing to the phone.
    assert_eq!(link.start_front(), Some(StartFront::Locked));
    assert!(
        texts.iter().all(|t| !t.contains("front.result")),
        "{texts:?}"
    );
    session.stop(EndReason::ClientClosed);
    assert_eq!(session.ended().await, EndReason::ClientClosed);
    // The worker notices the end on its next poll; dropping the link must not hang.
    tokio::task::spawn_blocking(move || drop(link))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn front_on_connect_runs_once_and_falls_back_to_focus_lost() {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    let link = gander_stream::input::attach(
        &session,
        no_window(),
        Arc::new(|| false),
        InputOptions::default(),
        true,
    )
    .expect("attach");
    // The window does not exist: the one attempt fails, and input pauses for focus_lost so the
    // phone offers "Bring to front".
    assert!(
        until(Duration::from_secs(3), || link.start_front().is_some())
            .await
            .is_some()
    );
    assert_eq!(link.start_front(), Some(StartFront::NotAcquired));
    assert!(
        wait_state(&session, Duration::from_secs(3), |s| {
            *s == SessionState::Paused(PauseReason::FocusLost)
        })
        .await
    );
    // Not announced on control (the phone did not ask) and not repeated.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let texts = viewer.stats.control_text_rx.lock().unwrap().clone();
    assert!(
        texts.iter().all(|t| !t.contains("front.result")),
        "{texts:?}"
    );
    // The phone's explicit request still runs the procedure and is answered.
    viewer.control_send_text(r#"{"t":"front"}"#).await.unwrap();
    let front = control_text(&viewer, "front.result").await.expect("front");
    assert!(front.contains(r#""acquired":false"#), "{front}");

    drop(link);
    session.stop(EndReason::ClientClosed);
    assert_eq!(session.ended().await, EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_front_on_connect_leaves_the_window_alone() {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let (session, _viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    let link = gander_stream::input::attach(
        &session,
        no_window(),
        Arc::new(|| false),
        InputOptions::default(),
        false,
    )
    .expect("attach");
    assert!(
        wait_state(&session, Duration::from_secs(3), |s| {
            *s == SessionState::Paused(PauseReason::FocusLost)
        })
        .await
    );
    assert_eq!(link.start_front(), None);
    drop(link);
    session.stop(EndReason::ClientClosed);
    assert_eq!(session.ended().await, EndReason::ClientClosed);
}
