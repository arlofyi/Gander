// SPDX-License-Identifier: Apache-2.0
//! Stream audio (WP-55) over real WebRTC on 127.0.0.1.
//!
//! - With the synthetic audio source (no sound device, run by default): the Opus track reaches
//!   the loopback viewer and decodes to the tone that went in; the capture runs only while the
//!   session streams and is not muted; a capture that cannot start leaves the session without
//!   sound and says why; an engine without an audio source offers no audio track at all.
//! - With the real WASAPI process loopback on a `gander-testwin` window (ignored by default: it
//!   needs an audio endpoint and plays a quiet 1 kHz tone for 0.8 s):
//!
//! ```text
//! cargo test -p gander-stream --test audio -- --ignored --nocapture --test-threads=1
//! ```

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use gander_core::protocol::events::ServerEvent;
use gander_stream::audio::tone;
use gander_stream::control::ControlMessage;
use gander_stream::testing::{SyntheticAudio, SyntheticSource, Viewer, ViewerOptions};
use gander_stream::{Session, StreamEngine, WindowTarget};

fn window() -> WindowTarget {
    WindowTarget {
        hwnd: 0,
        title: "synthetic".into(),
    }
}

fn with_audio(audio: Arc<dyn gander_stream::audio::AudioSource>) -> Harness {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    h.deps.audio = Some(audio);
    h.engine = StreamEngine::new(h.engine.config().clone(), h.deps.clone());
    h
}

/// The `audio` messages the viewer got, in order, as `(on, reason)`.
fn audio_msgs(v: &Viewer) -> Vec<(bool, Option<String>)> {
    v.stats
        .control_rx
        .lock()
        .unwrap()
        .iter()
        .filter_map(|m| match m {
            ControlMessage::Audio { on, reason } => Some((*on, reason.clone())),
            _ => None,
        })
        .collect()
}

async fn wait_audio_msg(v: &Viewer, on: bool, reason: Option<&str>) -> bool {
    until(Duration::from_secs(3), || {
        audio_msgs(v).last() == Some(&(on, reason.map(str::to_owned)))
    })
    .await
    .is_some()
}

/// Waits for at least `secs` of decoded audio, then detects the strongest tone in its loudest
/// quarter second.
async fn received_tone(v: &Viewer, secs: f64) -> Option<tone::Detected> {
    until(Duration::from_secs(10), || {
        v.stats.audio().pcm.len() as f64 >= secs * 48_000.0
    })
    .await?;
    let pcm = v.stats.audio().pcm;
    tone::detect(tone::loudest(&pcm, 12_000), 48_000.0, 100.0, 4000.0)
}

fn packets(s: &Session) -> u64 {
    s.stats().audio_packets_sent
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synthetic_tone_reaches_the_viewer_and_is_detected() {
    let audio = Arc::new(SyntheticAudio::default());
    let mut h = with_audio(audio.clone());
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    assert!(
        wait_audio_msg(&viewer, true, None).await,
        "{:?}",
        audio_msgs(&viewer)
    );
    let d = received_tone(&viewer, 1.0).await.expect("audio decoded");
    let rx = viewer.stats.audio();
    let st = session.stats();
    println!(
        "synthetic 1000 Hz at -30 dBFS: detected {:.1} Hz, rms {:.1} dBFS, purity {:.3}; {} packets ({} bytes, pt {:?}), {} lost, {} decode errors; host sent {} packets",
        d.hz,
        d.rms_dbfs,
        d.purity,
        rx.packets,
        rx.bytes,
        rx.payload_type,
        rx.lost,
        rx.decode_errors,
        st.audio_packets_sent
    );
    assert!((d.hz - 1000.0).abs() <= 5.0, "{d:?}");
    assert!(d.purity > 0.8, "{d:?}");
    assert_eq!(rx.payload_type, Some(gander_stream::audio::OPUS_PT));
    assert_eq!(rx.decode_errors, 0);
    assert!(session.audio_active());
    // About 50 packets a second, about 64 kbit/s at most.
    let kbps = rx.bytes as f64 * 8.0 / (rx.packets as f64 * 0.02) / 1000.0;
    assert!(kbps < 80.0, "{kbps} kbit/s");
    session.stop(gander_stream::EndReason::ClientClosed);
    session.ended().await;
    let ctl = audio.last.lock().unwrap().clone().unwrap();
    assert!(ctl.is_stopped(), "the capture stops with the session");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audio_runs_only_while_streaming_and_unmuted() {
    let audio = Arc::new(SyntheticAudio::default());
    let mut h = with_audio(audio.clone());
    // Negotiating: nothing captured.
    let session = h
        .engine
        .start(window(), viewport(), device())
        .await
        .expect("start");
    let ctl = audio.last.lock().unwrap().clone().expect("started");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!ctl.is_active(), "no capture before the phone connects");
    assert_eq!(
        ctl.stats.starts.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    session.stop(gander_stream::EndReason::ClientClosed);
    session.ended().await;
    while h.events.try_recv().is_ok() {}

    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    let ctl = audio.last.lock().unwrap().clone().expect("started");
    assert!(wait_audio_msg(&viewer, true, None).await);
    assert!(ctl.is_active());

    // Muted: the capture stops, nothing more is sent.
    viewer.send_mute(true).await.unwrap();
    assert!(wait_audio_msg(&viewer, false, Some("muted")).await);
    assert!(!ctl.is_active());
    assert!(!session.audio_active());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let n = packets(&session);
    let captured = ctl
        .stats
        .frames_captured
        .load(std::sync::atomic::Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(packets(&session), n, "no packets while muted");
    assert_eq!(
        ctl.stats
            .frames_captured
            .load(std::sync::atomic::Ordering::Relaxed),
        captured,
        "no capture while muted"
    );

    // Unmuted: sound again.
    viewer.send_mute(false).await.unwrap();
    assert!(wait_audio_msg(&viewer, true, None).await);
    let t = Instant::now();
    assert!(
        until(Duration::from_secs(2), || packets(&session) > n + 10)
            .await
            .is_some()
    );
    println!("unmute -> packets again within {:?}", t.elapsed());

    // Phone paused (backgrounded): no capture either.
    viewer.control_send(&ControlMessage::Pause).await.unwrap();
    assert!(wait_audio_msg(&viewer, false, Some("paused")).await);
    assert!(!ctl.is_active());
    viewer.control_send(&ControlMessage::Resume).await.unwrap();
    assert!(wait_audio_msg(&viewer, true, None).await);

    // Muted and paused, then resumed: still muted.
    viewer.send_mute(true).await.unwrap();
    assert!(wait_audio_msg(&viewer, false, Some("muted")).await);
    viewer.control_send(&ControlMessage::Pause).await.unwrap();
    viewer.control_send(&ControlMessage::Resume).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!ctl.is_active(), "resume does not unmute");
    assert_eq!(
        ctl.stats.starts.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "one start per activation: connect, unmute, resume"
    );

    session.stop(gander_stream::EndReason::ClientClosed);
    session.ended().await;
    assert!(ctl.is_stopped());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capture_that_cannot_start_means_no_sound_and_says_why() {
    let audio = Arc::new(SyntheticAudio::default());
    *audio.fail.lock().unwrap() = Some("process loopback activation failed".into());
    let mut h = with_audio(audio);
    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    assert!(wait_audio_msg(&viewer, false, Some("unavailable")).await);
    let st = session.audio_status();
    assert!(!st.on);
    assert_eq!(st.reason, Some("unavailable"));
    assert_eq!(
        st.detail.as_deref(),
        Some("process loopback activation failed")
    );
    // Video streams on.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(viewer.stats.frames().len() > 3);
    assert_eq!(viewer.stats.audio().packets, 0);
    // Muting changes nothing: there is no sound to mute.
    viewer.send_mute(true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(session.audio_status().reason, Some("unavailable"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_audio_source_offers_no_audio_track() {
    let mut h = harness(test_config(), Arc::new(SyntheticSource::default()));
    let session = h
        .engine
        .start(window(), viewport(), device())
        .await
        .expect("start");
    let id = session.id().clone();
    let offer = next_event(&mut h.events, Duration::from_secs(5), |e| match e {
        ServerEvent::StreamOffer { session, sdp } if *session == id => Some(sdp.clone()),
        _ => None,
    })
    .await
    .expect("offer");
    assert!(!offer.contains("m=audio"), "{offer}");
    session.stop(gander_stream::EndReason::ClientClosed);
    session.ended().await;
    while h.events.try_recv().is_ok() {}

    let (session, viewer) = connect(&mut h, window(), ViewerOptions::default()).await;
    assert!(wait_audio_msg(&viewer, false, Some("none")).await);
    assert_eq!(session.audio_status().reason, Some("none"));

    // With a source: one BUNDLE, video first, then audio (Opus 111), then the data channels.
    let mut h = with_audio(Arc::new(SyntheticAudio::default()));
    let session = h
        .engine
        .start(window(), viewport(), device())
        .await
        .expect("start");
    let id = session.id().clone();
    let offer = next_event(&mut h.events, Duration::from_secs(5), |e| match e {
        ServerEvent::StreamOffer { session, sdp } if *session == id => Some(sdp.clone()),
        _ => None,
    })
    .await
    .expect("offer");
    let m: Vec<&str> = offer.lines().filter(|l| l.starts_with("m=")).collect();
    assert_eq!(m.len(), 3, "{offer}");
    assert!(m[0].starts_with("m=video"));
    assert!(
        m[1].starts_with("m=audio") && m[1].ends_with(" 111"),
        "{}",
        m[1]
    );
    assert!(m[2].starts_with("m=application"));
    assert!(offer.contains("a=rtpmap:111 opus/48000/2"), "{offer}");
    assert!(offer.contains("a=group:BUNDLE 0 1 2"), "{offer}");
    let audio_section = offer.split("m=audio").nth(1).unwrap();
    let audio_section = audio_section.split("m=application").next().unwrap();
    assert!(audio_section.contains("a=sendonly"), "{audio_section}");
    session.stop(gander_stream::EndReason::ClientClosed);
    session.ended().await;
}

/// Ogg page (RFC 3533) CRC: polynomial 0x04C11DB7, no reflection, zero start and end.
fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Appends one Ogg page that holds exactly one packet.
fn ogg_page(out: &mut Vec<u8>, kind: u8, granule: u64, seq: u32, packet: &[u8]) {
    let start = out.len();
    out.extend_from_slice(b"OggS");
    out.extend_from_slice(&[0, kind]);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&0x474E_4452u32.to_le_bytes()); // serial "GNDR"
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&[0; 4]); // CRC, filled in below
    let segments = packet.len() / 255 + 1;
    out.push(segments as u8);
    out.extend(std::iter::repeat_n(255u8, segments - 1));
    out.push((packet.len() % 255) as u8);
    out.extend_from_slice(packet);
    let crc = ogg_crc(&out[start..]).to_le_bytes();
    out[start + 22..start + 26].copy_from_slice(&crc);
}

/// Cross-check against an independent decoder: the packets the host encodes (libopus), wrapped
/// in Ogg/Opus (RFC 7845) with no re-encoding, must decode in ffmpeg to the same 1 kHz tone.
/// Skipped (with a note) when `ffmpeg` is not on `PATH`.
#[test]
fn ffmpeg_decodes_the_encoded_stream_to_the_tone_that_went_in() {
    use std::process::Command;

    const PRE_SKIP: u16 = 312;
    if Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map_or(true, |o| !o.status.success())
    {
        eprintln!("ffmpeg is not on PATH: cross-check skipped");
        return;
    }
    // 1 s of 1 kHz at -30 dBFS peak (amplitude 0.0316), stereo.
    let pcm: Vec<f32> = (0..48_000)
        .flat_map(|i| {
            let v = 0.0316
                * (2.0 * std::f64::consts::PI * 1000.0 * f64::from(i) / 48_000.0).sin() as f32;
            [v, v]
        })
        .collect();
    let mut framer = gander_stream::audio::OpusFramer::new().unwrap();
    let packets = framer.push(&pcm);
    assert_eq!(packets.len(), 50);

    let mut ogg = Vec::new();
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, 2]); // version, channels
    head.extend_from_slice(&PRE_SKIP.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&[0, 0, 0]); // output gain, mapping family 0
    ogg_page(&mut ogg, 0x02, 0, 0, &head);
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&6u32.to_le_bytes());
    tags.extend_from_slice(b"gander");
    tags.extend_from_slice(&0u32.to_le_bytes());
    ogg_page(&mut ogg, 0x00, 0, 1, &tags);
    for (i, p) in packets.iter().enumerate() {
        let last = i + 1 == packets.len();
        let granule = u64::from(PRE_SKIP) + (i as u64 + 1) * gander_stream::audio::FRAME as u64;
        ogg_page(
            &mut ogg,
            if last { 0x04 } else { 0x00 },
            granule,
            i as u32 + 2,
            p,
        );
    }
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("libopus-1khz.opus");
    std::fs::write(&file, &ogg).unwrap();

    // Plain decode to the null muxer: no decode errors.
    let null = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&file)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        null.status.success() && null.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&null.stderr)
    );
    // Decode to stereo float PCM (no downmix gain), keep the left channel, detect the tone.
    let raw = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&file)
        .args(["-f", "f32le", "-ac", "2", "-ar", "48000", "-"])
        .output()
        .unwrap();
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    let mono: Vec<f32> = raw
        .stdout
        .chunks_exact(8)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let d = tone::detect(tone::loudest(&mono, 12_000), 48_000.0, 100.0, 4000.0).unwrap();
    println!(
        "ffmpeg decode of {} libopus packets ({} bytes of Ogg, {} samples): detected {:.1} Hz, rms {:.1} dBFS, purity {:.3}",
        packets.len(),
        ogg.len(),
        mono.len(),
        d.hz,
        d.rms_dbfs,
        d.purity
    );
    assert!(mono.len() >= 46_000, "{} samples", mono.len());
    assert!((d.hz - 1000.0).abs() <= 5.0, "{d:?}");
    assert!((d.rms_dbfs + 33.0).abs() < 3.0, "{d:?}");
    assert!(d.purity > 0.8, "{d:?}");
}

#[cfg(windows)]
mod real {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};

    use gander_stream::win::ProcessLoopback;

    use super::*;

    /// The test window process; killed on drop.
    struct Testwin {
        child: Child,
        pipe: String,
        hwnd: isize,
    }

    impl Testwin {
        fn exe() -> PathBuf {
            let me = std::env::current_exe().expect("exe");
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

        fn spawn() -> Testwin {
            let mut child = Command::new(Self::exe())
                .args([
                    "--self-test-secs",
                    "120",
                    "--no-focus",
                    "--title",
                    "WP-55 audio",
                ])
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
            Testwin { child, pipe, hwnd }
        }

        fn send(&self, cmd: &str) -> String {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.pipe)
                .expect("pipe");
            let mut io = BufReader::new(f);
            io.get_mut()
                .write_all(format!("{cmd}\n").as_bytes())
                .expect("write");
            let mut reply = String::new();
            io.read_line(&mut reply).expect("read");
            reply.trim().to_owned()
        }
    }

    impl Drop for Testwin {
        fn drop(&mut self) {
            let _ = self.send("quit");
            std::thread::sleep(Duration::from_millis(300));
            if let Ok(None) = self.child.try_wait() {
                let _ = self.child.kill();
            }
            let _ = self.child.wait();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs an audio endpoint and plays a quiet 0.8 s tone; see the module docs"]
    async fn real_testwin_tone_through_process_loopback() {
        let session_state = gander_win::session::query_session_state()
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|e| format!("unknown ({e})"));
        println!("desktop session: {session_state}");
        let tw = Testwin::spawn();
        let mut h = with_audio(Arc::new(ProcessLoopback));
        let target = WindowTarget {
            hwnd: tw.hwnd,
            title: "gander-testwin".into(),
        };
        let (session, viewer) = connect(&mut h, target, ViewerOptions::default()).await;
        assert!(
            wait_audio_msg(&viewer, true, None).await,
            "audio: {:?} / {:?}",
            audio_msgs(&viewer),
            session.audio_status()
        );
        // A pause and a resume first: the capture must come back after its stream was stopped.
        viewer.control_send(&ControlMessage::Pause).await.unwrap();
        assert!(wait_audio_msg(&viewer, false, Some("paused")).await);
        tokio::time::sleep(Duration::from_millis(300)).await;
        viewer.control_send(&ControlMessage::Resume).await.unwrap();
        assert!(wait_audio_msg(&viewer, true, None).await);
        // Silence first (padded), then the tone.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let before = viewer.stats.audio();
        assert_eq!(tw.send("tone 1000 800"), "ok");
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let rx = viewer.stats.audio();
        let st = session.stats();
        let pcm = &rx.pcm[before.pcm.len()..];
        let d = tone::detect(tone::loudest(pcm, 12_000), 48_000.0, 100.0, 4000.0);
        let quiet = tone::detect(&before.pcm, 48_000.0, 100.0, 4000.0);
        println!(
            "process loopback of testwin (pid {:?}): tone 1000 Hz at -30 dBFS peak -> detected {:?}; before the tone {:?}; {} packets, {} lost, {} decode errors; host: {} frames captured, {} packets encoded, {} sent, starts {}",
            gander_stream::win::audio::window_process(tw.hwnd),
            d,
            quiet.map(|q| q.rms_dbfs),
            rx.packets,
            rx.lost,
            rx.decode_errors,
            st.audio_frames_captured,
            st.audio_packets_encoded,
            st.audio_packets_sent,
            st.audio_starts
        );
        let d = d.expect("audio decoded");
        assert!((d.hz - 1000.0).abs() <= 5.0, "{d:?}");
        assert!(
            d.rms_dbfs > -60.0,
            "the tone is audible in the capture: {d:?}"
        );
        session.stop(gander_stream::EndReason::ClientClosed);
        session.ended().await;
    }
}
