// SPDX-License-Identifier: Apache-2.0
//! Engine tests over real WebRTC on 127.0.0.1 with the synthetic source (no GPU, no capture):
//! the lock guard, keyframes, liveness and pause, candidates, ports, the battery cap, the
//! playout-delay extension, and adaptation through the impairment relay at 200 ms and 2 s
//! bottleneck buffers. The same checks with the real WGC + Media Foundation source are in
//! `tests/pipeline.rs`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use gander_core::error::ErrorCode;
use gander_core::protocol::SessionId;
use gander_core::protocol::events::{ClientEvent, ServerEvent};
use gander_core::protocol::stream::{StreamReason, StreamState};
use gander_stream::control::ControlMessage;
use gander_stream::testing::viewer::max_gap;
use gander_stream::testing::{Relay, SyntheticSource, ViewerOptions};
use gander_stream::{
    EndReason, EngineConfig, EngineDeps, EngineError, LockSource, LockStatus, PauseReason,
    SessionState, StreamEngine, WindowTarget,
};

fn window() -> WindowTarget {
    WindowTarget {
        hwnd: 0,
        title: "synthetic".into(),
    }
}

fn synthetic() -> Arc<SyntheticSource> {
    Arc::new(SyntheticSource::default())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refuses_to_start_while_locked() {
    let h = harness(test_config(), synthetic());
    h.lock.set_locked(true);
    let r = h.engine.start(window(), viewport(), device()).await;
    assert!(matches!(r, Err(EngineError::HostLocked)), "{r:?}");
    assert_eq!(EngineError::HostLocked.code(), ErrorCode::HostLocked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lock_event_ends_the_session_within_one_second() {
    let mut h = harness(test_config(), synthetic());
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let frames_before = viewer.stats.frames().len();
    assert!(frames_before > 5, "media flowing before the lock");

    let t = Instant::now();
    h.lock.set_locked(true);
    let ended = wait_state(&session, Duration::from_secs(1), |s| {
        matches!(s, SessionState::Ended(_))
    })
    .await;
    let took = t.elapsed();
    println!("lock -> Ended took {took:?}");
    assert!(ended, "not ended within 1 s: {:?}", session.state());
    assert_eq!(session.state(), SessionState::Ended(EndReason::HostLocked));
    assert!(took < Duration::from_secs(1));

    // The phone is told: error host_locked, then stream.state ended.
    let id = session.id().clone();
    let err = next_event(&mut h.events, Duration::from_secs(2), |e| match e {
        ServerEvent::Error { code, session, .. } if session.as_ref() == Some(&id) => Some(*code),
        _ => None,
    })
    .await;
    assert_eq!(err, Some(ErrorCode::HostLocked));
    let end = next_event(&mut h.events, Duration::from_secs(2), |e| match e {
        ServerEvent::StreamState(s) if s.session == id && s.state == StreamState::Ended => {
            Some(s.reason)
        }
        _ => None,
    })
    .await;
    assert_eq!(end, Some(Some(StreamReason::Error)));
    // Nothing more is encoded.
    let produced = session.stats().frames_new + session.stats().frames_keepalive;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        session.stats().frames_new + session.stats().frames_keepalive,
        produced
    );
    assert!(h.engine.session(&id).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_lock_source_fails_closed() {
    /// A lock source whose sender can be dropped (the watcher thread died).
    #[derive(Debug)]
    struct Closing(std::sync::Mutex<Option<tokio::sync::watch::Sender<LockStatus>>>);
    impl LockSource for Closing {
        fn subscribe(&self) -> tokio::sync::watch::Receiver<LockStatus> {
            match &*self.0.lock().unwrap() {
                Some(tx) => tx.subscribe(),
                None => tokio::sync::watch::channel(LockStatus::default()).1,
            }
        }
    }
    let lock = Arc::new(Closing(std::sync::Mutex::new(Some(
        tokio::sync::watch::Sender::new(LockStatus::default()),
    ))));
    let mut h = harness(test_config(), synthetic());
    h.engine = StreamEngine::new(
        h.engine.config().clone(),
        EngineDeps {
            lock: lock.clone(),
            ..h.deps.clone()
        },
    );
    let (session, _viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    lock.0.lock().unwrap().take();
    assert!(
        wait_state(&session, Duration::from_secs(1), |s| matches!(
            s,
            SessionState::Ended(EndReason::HostLocked)
        ))
        .await,
        "a dead lock source ends the session: {:?}",
        session.state()
    );
    // And a new session is refused while the source is dead.
    let r = h.engine.start(window(), viewport(), device()).await;
    assert!(matches!(r, Err(EngineError::HostLocked)), "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_brief_lock_still_ends_the_session() {
    // The watch channel keeps only the newest value; the lock counter makes a lock that was
    // undone before the session task looked still count.
    let mut h = harness(test_config(), synthetic());
    let (session, _viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    h.lock.set_locked(true);
    h.lock.set_locked(false);
    assert!(
        wait_state(&session, Duration::from_secs(1), |s| matches!(
            s,
            SessionState::Ended(EndReason::HostLocked)
        ))
        .await,
        "a brief lock still ends the session: {:?}",
        session.state()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn static_window_gets_a_keepalive_at_least_every_500_ms() {
    let src = synthetic();
    let mut h = harness(test_config(), src.clone());
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    src.set_static(true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let from = Instant::now();
    tokio::time::sleep(Duration::from_secs(4)).await;
    let frames = viewer.stats.frames();
    let n = frames.iter().filter(|f| f.at >= from).count();
    let gap = max_gap(&frames, from).expect("frames");
    println!(
        "static synthetic window: {n} frames in 4 s, max gap {gap:?}, keepalive frames {}",
        session.stats().frames_keepalive
    );
    assert!(gap <= Duration::from_millis(500), "gap {gap:?}");
    assert!(n >= 8, "{n} frames in 4 s");
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pli_forces_an_idr() {
    let mut h = harness(test_config(), synthetic());
    let opts = ViewerOptions {
        auto_pli: false,
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, window(), opts).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    for _ in 0..3 {
        let t = Instant::now();
        viewer.send_pli().await.expect("pli");
        let got = until(Duration::from_secs(2), || {
            viewer.stats.frames().iter().any(|f| f.at > t && f.idr)
        })
        .await;
        println!("PLI -> IDR received after {got:?}");
        assert!(got.is_some(), "no IDR after PLI");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(session.stats().keyframe_requests.pli >= 3);
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_sends_an_idr() {
    let relay = Arc::new(
        Relay::start(Duration::from_millis(200))
            .await
            .expect("relay"),
    );
    let mut h = harness(test_config(), synthetic());
    let opts = ViewerOptions {
        relay: Some(relay.clone()),
        auto_pli: false,
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, window(), opts).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    relay.set_blackhole(true);
    let t = Instant::now();
    assert!(
        wait_state(&session, Duration::from_secs(20), |s| {
            *s == SessionState::Paused(PauseReason::Network)
        })
        .await,
        "never noticed the dead link: {:?}",
        session.state()
    );
    println!("dead link -> paused(network) after {:?}", t.elapsed());
    let t = Instant::now();
    relay.set_blackhole(false);
    assert!(
        wait_state(&session, Duration::from_secs(20), |s| *s
            == SessionState::Streaming)
        .await,
        "never reconnected: {:?}",
        session.state()
    );
    let idr = until(Duration::from_secs(2), || {
        viewer.stats.frames().iter().any(|f| f.at > t && f.idr)
    })
    .await;
    let st = session.stats();
    println!(
        "link back -> streaming after {:?}; IDR received after {idr:?}; keyframe requests {:?}",
        t.elapsed(),
        st.keyframe_requests
    );
    assert!(idr.is_some(), "no IDR after the reconnect");
    assert_eq!(st.keyframe_requests.reconnect, 1);
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answer_without_candidates_connects_peer_reflexively() {
    let mut h = harness(test_config(), synthetic());
    let opts = ViewerOptions {
        strip_candidates: true,
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, window(), opts).await;
    assert!(
        until(Duration::from_secs(3), || viewer.stats.frames().len() > 5)
            .await
            .is_some()
    );
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trickled_answer_candidates_connect() {
    let mut h = harness(test_config(), synthetic());
    let opts = ViewerOptions {
        trickle: true,
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, window(), opts).await;
    assert!(
        until(Duration::from_secs(3), || viewer.stats.frames().len() > 5)
            .await
            .is_some()
    );
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn liveness_silence_pauses_and_traffic_resumes_with_an_idr() {
    let cfg = EngineConfig {
        liveness_timeout: Duration::from_millis(1500),
        ..test_config()
    };
    let mut h = harness(cfg, synthetic());
    let opts = ViewerOptions {
        ping_interval: Some(Duration::from_millis(300)),
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, window(), opts).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        viewer
            .stats
            .control_rx
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, ControlMessage::Pong { .. })),
        "pings answered"
    );
    viewer.set_pinging(false);
    let t = Instant::now();
    assert!(
        wait_state(&session, Duration::from_secs(4), |s| {
            *s == SessionState::Paused(PauseReason::Network)
        })
        .await
    );
    println!("silence -> paused(network) after {:?}", t.elapsed());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let n = viewer.stats.frames().len();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        viewer.stats.frames().len() <= n + 1,
        "frames kept flowing while paused"
    );
    let t = Instant::now();
    viewer.set_pinging(true);
    assert!(
        wait_state(&session, Duration::from_secs(3), |s| *s
            == SessionState::Streaming)
        .await
    );
    assert!(
        until(Duration::from_secs(2), || viewer
            .stats
            .frames()
            .iter()
            .any(|f| f.at > t && f.idr))
        .await
        .is_some()
    );
    assert_eq!(session.stats().liveness_pauses, 1);
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_pause_over_control_and_over_events() {
    let mut h = harness(test_config(), synthetic());
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    viewer
        .control_send(&ControlMessage::Pause)
        .await
        .expect("send");
    assert!(
        wait_state(&session, Duration::from_secs(2), |s| {
            *s == SessionState::Paused(PauseReason::PhoneBackground)
        })
        .await
    );
    let id = session.id().clone();
    let paused = next_event(&mut h.events, Duration::from_secs(2), |e| match e {
        ServerEvent::StreamState(s) if s.session == id && s.state == StreamState::Paused => {
            Some(s.reason)
        }
        _ => None,
    })
    .await;
    assert_eq!(paused, Some(Some(StreamReason::PhoneBackground)));
    let produced = session.stats().frames_new;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        session.stats().frames_new,
        produced,
        "no encoding while paused"
    );

    // Resume through /v1/events this time.
    assert!(
        h.engine
            .handle_client_event(
                &device(),
                ClientEvent::StreamResume {
                    session: id.clone()
                }
            )
            .is_none()
    );
    assert!(
        wait_state(&session, Duration::from_secs(2), |s| *s
            == SessionState::Streaming)
        .await
    );
    assert_eq!(session.stats().keyframe_requests.resume, 1);
    assert!(
        h.engine
            .handle_client_event(
                &device(),
                ClientEvent::StreamPause {
                    session: id.clone()
                }
            )
            .is_none()
    );
    assert!(
        wait_state(&session, Duration::from_secs(2), |s| {
            *s == SessionState::Paused(PauseReason::PhoneBackground)
        })
        .await
    );
    session.stop(EndReason::ClientClosed);
    assert_eq!(session.ended().await, EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_sessions_and_other_devices_are_refused() {
    let mut h = harness(test_config(), synthetic());
    let (session, _viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    let other = gander_core::protocol::DeviceId::from("dev_other");
    for (dev, sid) in [
        (other.clone(), session.id().clone()),
        (device(), SessionId::from("ses_nope")),
    ] {
        match h.engine.handle_client_event(
            &dev,
            ClientEvent::StreamPause {
                session: sid.clone(),
            },
        ) {
            Some(ServerEvent::Error { code, session, .. }) => {
                assert_eq!(code, ErrorCode::NotFound);
                assert_eq!(session, Some(sid));
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }
    assert_eq!(
        session.state(),
        SessionState::Streaming,
        "other device ignored"
    );
    assert!(
        h.engine
            .handle_client_event(&device(), ClientEvent::Ping {})
            .is_none()
    );
    h.engine.stop_all(EndReason::HostKillSwitch);
    assert_eq!(session.ended().await, EndReason::HostKillSwitch);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ice_binds_only_inside_the_firewall_range() {
    let mut h = harness(test_config(), synthetic());
    let range = h.range;
    let (session, _viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    let addrs = session.stats().local_addrs;
    println!("ICE sockets {addrs:?} inside {range}");
    assert!(!addrs.is_empty());
    for a in &addrs {
        assert!(a.ip().is_loopback(), "{a}");
        assert!((range.start()..=range.end()).contains(&a.port()), "{a}");
    }
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exhausted_port_range_refuses_the_session() {
    let cfg = EngineConfig {
        udp_ports: gander_core::config::UdpPortRange::new(52_990, 52_991).unwrap(),
        ..EngineConfig::default()
    };
    let _a = std::net::UdpSocket::bind("127.0.0.1:52990").expect("bind");
    let _b = std::net::UdpSocket::bind("127.0.0.1:52991").expect("bind");
    let h = harness(cfg, synthetic());
    let r = h.engine.start(window(), viewport(), device()).await;
    println!("exhausted range: {r:?}");
    match r {
        Err(e @ EngineError::PortsExhausted { .. }) => {
            assert_eq!(e.code(), ErrorCode::StreamUnavailable);
        }
        other => panic!("expected PortsExhausted, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_packet_carries_playout_delay_zero() {
    let mut h = harness(test_config(), synthetic());
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let frames = viewer.stats.frames();
    assert!(frames.len() > 5);
    assert!(
        frames.iter().all(|f| f.playout == Some((0, 0))),
        "{:?}",
        frames.iter().map(|f| f.playout).collect::<Vec<_>>()
    );
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn battery_caps_the_set_point() {
    let mut h = harness(test_config(), synthetic());
    h.power.set_on_battery(true);
    let (session, _viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let st = session.stats();
    println!(
        "on battery: estimate {:.0} kbps, set point {} kbps, ceiling {} kbps",
        st.estimate_bps / 1000.0,
        st.setpoint_bps / 1000,
        st.ceiling_bps / 1000
    );
    assert!(st.on_battery);
    assert!(st.setpoint_bps <= 2_500_000);
    assert_eq!(st.ceiling_bps, 2_500_000);
    session.stop(EndReason::ClientClosed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn source_failure_refuses_the_session() {
    let src = synthetic();
    *src.fail.lock().unwrap() = Some("no encoder".into());
    let h = harness(test_config(), src);
    let r = h.engine.start(window(), viewport(), device()).await;
    assert!(matches!(r, Err(EngineError::Source(_))), "{r:?}");
}

async fn adaptation(buffer_ms: u64) -> common::Adaptation {
    let relay = Arc::new(
        Relay::start(Duration::from_millis(buffer_ms))
            .await
            .expect("relay"),
    );
    let cfg = EngineConfig {
        estimator: gander_stream::estimator::Config {
            initial_bps: 6_000_000.0,
            ..Default::default()
        },
        ..test_config()
    };
    let mut h = harness(cfg, synthetic());
    let opts = ViewerOptions {
        relay: Some(relay.clone()),
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, window(), opts).await;
    let a = measure_adaptation(
        &session,
        &viewer,
        &relay,
        buffer_ms,
        1_500_000,
        Duration::from_secs(4),
        Duration::from_secs(8),
    )
    .await;
    session.stop(EndReason::ClientClosed);
    a
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adapts_within_5_s_with_a_200_ms_buffer() {
    let a = adaptation(200).await;
    println!("synthetic source, 200 ms buffer\n{a}");
    assert!(
        a.output_under_cap_s.is_some_and(|s| s <= 5.0),
        "output under the cap after {:?}",
        a.output_under_cap_s
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adapts_within_5_s_with_a_2_s_buffer() {
    let a = adaptation(2000).await;
    println!("synthetic source, 2 s buffer\n{a}");
    assert!(
        a.output_under_cap_s.is_some_and(|s| s <= 5.0),
        "output under the cap after {:?}",
        a.output_under_cap_s
    );
}
