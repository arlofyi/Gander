// SPDX-License-Identifier: Apache-2.0
//! The engine with the REAL source (WGC capture -> D3D11 video processor -> Media Foundation
//! hardware H.264) on a `gander-testwin` window, received by the loopback viewer on 127.0.0.1.
//! Only `gander-testwin` windows are ever captured.
//!
//! Ignored by default: they need a GPU with a hardware H.264 MFT and an interactive desktop
//! session (locked is fine: WGC captures while locked, and the lock guard is faked here), and
//! take about two minutes. Run them one at a time:
//!
//! ```text
//! cargo test -p gander-stream --test pipeline -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! `GANDER_TEST_VENDOR=intel|nvidia` picks the encoder's adapter (default: the engine's own
//! order, NVIDIA first; the adaptation tests default to Intel, see `real_adaptation`).
#![cfg(windows)]

mod common;

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use gander_stream::testing::viewer::max_gap;
use gander_stream::testing::{Relay, ViewerOptions};
use gander_stream::win::WgcSource;
use gander_stream::{EndReason, EngineConfig, PauseReason, SessionState, WindowTarget};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOACTIVATE, SetWindowPos};

/// The test window process; killed on drop.
struct Testwin {
    child: Child,
    pipe: String,
    hwnd: isize,
}

impl Testwin {
    fn exe() -> PathBuf {
        let me = std::env::current_exe().expect("exe");
        // target/<profile>/deps/pipeline-*.exe -> target/<profile>/gander-testwin.exe
        let dir = me.parent().and_then(|d| d.parent()).expect("target dir");
        let exe = dir.join("gander-testwin.exe");
        if !exe.exists() {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let mut cmd = Command::new(cargo);
            cmd.args(["build", "--quiet", "-p", "gander-testwin"]);
            if dir.ends_with("release") {
                cmd.arg("--release");
            }
            assert!(cmd.status().expect("cargo build").success());
        }
        exe
    }

    fn spawn(title: &str) -> Testwin {
        let mut child = Command::new(Self::exe())
            .args(["--self-test-secs", "300", "--no-focus", "--title", title])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gander-testwin");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut line)
            .expect("ready line");
        assert!(line.starts_with("gander-testwin ready "), "{line:?}");
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .map(str::to_owned)
                .expect("field")
        };
        let pipe = field("pipe=");
        let hwnd = isize::from_str_radix(&field("hwnd=0x"), 16).expect("hwnd");
        // A phone-shaped window (logical 1:2), topmost, not activated.
        // SAFETY: plain call on the test window's handle.
        unsafe {
            let _ = SetWindowPos(
                HWND(hwnd as *mut c_void),
                Some(HWND_TOPMOST),
                40,
                40,
                540,
                1080,
                SWP_NOACTIVATE,
            );
        }
        std::thread::sleep(Duration::from_millis(400));
        Testwin { child, pipe, hwnd }
    }

    fn send(&self, cmd: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        let f = loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.pipe)
            {
                Ok(f) => break f,
                Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => panic!("pipe: {e}"),
            }
        };
        let mut io = BufReader::new(f);
        io.get_mut()
            .write_all(format!("{cmd}\n").as_bytes())
            .expect("write");
        let mut reply = String::new();
        io.read_line(&mut reply).expect("read");
        reply.trim().to_owned()
    }

    fn target(&self) -> WindowTarget {
        WindowTarget {
            hwnd: self.hwnd,
            title: "gander-testwin".into(),
        }
    }
}

impl Drop for Testwin {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn source() -> Arc<WgcSource> {
    source_or(None)
}

/// The adapter from `GANDER_TEST_VENDOR`, else `default`.
fn source_or(default: Option<u32>) -> Arc<WgcSource> {
    let vendor = match std::env::var("GANDER_TEST_VENDOR").as_deref() {
        Ok("intel") => Some(0x8086),
        Ok("nvidia") => Some(0x10DE),
        _ => default,
    };
    Arc::new(WgcSource {
        prefer_luid: None,
        prefer_vendor: vendor,
        border_required: None,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a GPU encoder and a desktop session; see the module docs"]
async fn real_static_window_keepalive_cadence() {
    let win = Testwin::spawn("WP-50 keepalive");
    let mut h = harness(test_config(), source());
    let (session, viewer) = connect(&mut h, win.target(), ViewerOptions::default()).await;
    let info = session.stats().source;
    println!(
        "source: {} / {} at {}x{}",
        info.adapter, info.encoder, info.width, info.height
    );
    // Animated first.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let from = Instant::now();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let frames = viewer.stats.frames();
    let animated = frames.iter().filter(|f| f.at >= from).count();
    println!(
        "animated window: {animated} frames in 3 s ({:.1} fps), max gap {:?}",
        animated as f64 / 3.0,
        max_gap(&frames, from)
    );
    // Then static.
    assert_eq!(win.send("freeze"), "ok");
    tokio::time::sleep(Duration::from_millis(600)).await;
    let ka0 = session.stats().frames_keepalive;
    let new0 = session.stats().frames_new;
    let from = Instant::now();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let frames = viewer.stats.frames();
    let sel: Vec<_> = frames.iter().filter(|f| f.at >= from).collect();
    let gap = max_gap(&frames, from).expect("frames while static");
    let st = session.stats();
    println!(
        "static window: {} frames in 5 s, max gap {gap:?}, mean gap {:?}, keepalive frames +{}, new frames +{}, damaged {}",
        sel.len(),
        Duration::from_secs(5) / (sel.len().max(1) as u32),
        st.frames_keepalive - ka0,
        st.frames_new - new0,
        sel.iter().filter(|f| f.damaged).count()
    );
    assert!(gap <= Duration::from_millis(500), "max gap {gap:?}");
    assert!(st.frames_keepalive > ka0 + 8, "keepalive repeats");
    assert_eq!(win.send("animate"), "ok");
    session.stop(EndReason::ClientClosed);
    session.ended().await;
}

async fn real_adaptation(buffer_ms: u64) {
    let win = Testwin::spawn("WP-50 adaptation");
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
    // Intel by default: its CBR fills the set point on the test window, while NVIDIA's
    // undershoots far below a 1.5 Mbit/s cap (about 0.4 Mbit/s here), which measures nothing.
    let mut h = harness(cfg, source_or(Some(0x8086)));
    let opts = ViewerOptions {
        relay: Some(relay.clone()),
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, win.target(), opts).await;
    let info = session.stats().source;
    let a = measure_adaptation(
        &session,
        &viewer,
        &relay,
        buffer_ms,
        1_500_000,
        Duration::from_secs(8),
        Duration::from_secs(10),
    )
    .await;
    let st = session.stats();
    println!(
        "real source {} / {} at {}x{}, encoder re-creations {}, keyframes {:?}\n{a}",
        info.adapter, info.encoder, info.width, info.height, st.recreations, st.keyframe_requests
    );
    session.stop(EndReason::ClientClosed);
    session.ended().await;
    assert!(
        a.before.output_kbps > 2.0 * a.cap_kbps,
        "the encoder never exceeded the cap, so nothing was measured (output {:.0} kbps)",
        a.before.output_kbps
    );
    assert!(
        a.output_under_cap_s.is_some_and(|s| s <= 5.0),
        "encoded output under the cap after {:?}",
        a.output_under_cap_s
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a GPU encoder and a desktop session; see the module docs"]
async fn real_adapts_within_5_s_with_a_200_ms_buffer() {
    real_adaptation(200).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a GPU encoder and a desktop session; see the module docs"]
async fn real_adapts_within_5_s_with_a_2_s_buffer() {
    real_adaptation(2000).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a GPU encoder and a desktop session; see the module docs"]
async fn real_keyframes_on_pli_and_on_reconnect() {
    let win = Testwin::spawn("WP-50 keyframes");
    let relay = Arc::new(
        Relay::start(Duration::from_millis(200))
            .await
            .expect("relay"),
    );
    let mut h = harness(test_config(), source());
    let opts = ViewerOptions {
        relay: Some(relay.clone()),
        auto_pli: false,
        ..ViewerOptions::default()
    };
    let (session, viewer) = connect(&mut h, win.target(), opts).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    // PLI -> IDR in the real bitstream (nal_unit_type 5), animated and static.
    for phase in ["animated", "static"] {
        if phase == "static" {
            assert_eq!(win.send("freeze"), "ok");
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        for _ in 0..3 {
            let t = Instant::now();
            viewer.send_pli().await.expect("pli");
            let got = until(Duration::from_secs(2), || {
                viewer.stats.frames().iter().any(|f| f.at > t && f.idr)
            })
            .await;
            println!("{phase}: PLI -> IDR received after {got:?}");
            assert!(got.is_some(), "no IDR after PLI ({phase})");
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }
    assert_eq!(win.send("animate"), "ok");
    // Dead link, then back: an IDR follows the reconnect.
    relay.set_blackhole(true);
    let t = Instant::now();
    assert!(
        wait_state(&session, Duration::from_secs(20), |s| {
            *s == SessionState::Paused(PauseReason::Network)
        })
        .await
    );
    println!("dead link -> paused(network) after {:?}", t.elapsed());
    relay.set_blackhole(false);
    let t = Instant::now();
    assert!(
        wait_state(&session, Duration::from_secs(20), |s| *s
            == SessionState::Streaming)
        .await
    );
    let idr = until(Duration::from_secs(3), || {
        viewer.stats.frames().iter().any(|f| f.at > t && f.idr)
    })
    .await;
    let st = session.stats();
    println!(
        "reconnect: streaming after {:?}, IDR after {idr:?}, requests {:?}, IDRs out {}",
        t.elapsed(),
        st.keyframe_requests,
        st.keyframes_out
    );
    assert!(idr.is_some());
    assert!(st.keyframe_requests.pli >= 6);
    assert_eq!(st.keyframe_requests.reconnect, 1);
    session.stop(EndReason::ClientClosed);
    session.ended().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a GPU encoder and a desktop session; see the module docs"]
async fn real_lock_guard_and_window_close() {
    // Lock: ends within 1 s and the capture stops.
    let win = Testwin::spawn("WP-50 lock");
    let mut h = harness(test_config(), source());
    let (session, viewer) = connect(&mut h, win.target(), ViewerOptions::default()).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let t = Instant::now();
    h.lock.set_locked(true);
    let ended = wait_state(&session, Duration::from_secs(1), |s| {
        matches!(s, SessionState::Ended(_))
    })
    .await;
    println!("real source: lock -> Ended after {:?}", t.elapsed());
    assert!(ended);
    assert_eq!(session.state(), SessionState::Ended(EndReason::HostLocked));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let n = viewer.stats.frames().len();
    let produced = session.stats().frames_new + session.stats().frames_keepalive;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(viewer.stats.frames().len(), n, "no frames after the lock");
    assert_eq!(
        session.stats().frames_new + session.stats().frames_keepalive,
        produced,
        "the source stopped encoding"
    );
    // While locked, a new session is refused.
    assert!(matches!(
        h.engine.start(win.target(), viewport(), device()).await,
        Err(gander_stream::EngineError::HostLocked)
    ));
    h.lock.set_locked(false);

    // Window closed: the session ends with window_closed.
    let (session, _viewer) = connect(&mut h, win.target(), ViewerOptions::default()).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let t = Instant::now();
    let _ = win.send("quit");
    let reason = tokio::time::timeout(Duration::from_secs(5), session.ended())
        .await
        .expect("ended");
    println!("window closed -> Ended({reason:?}) after {:?}", t.elapsed());
    assert_eq!(reason, EndReason::WindowClosed);
}
