// SPDX-License-Identifier: Apache-2.0
//! Test support: the loopback receiver from the spike (a webrtc-rs viewer that depacketises and
//! timestamps every access unit), the in-process impairment relay, a signalling sink that
//! collects events, and a synthetic source that follows the [`crate::source`] rules without a
//! GPU. Not used in production.

pub mod relay;
pub mod viewer;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use gander_core::protocol::DeviceId;
use gander_core::protocol::events::ServerEvent;
use tokio::sync::mpsc;

use crate::deps::SignalSink;
use crate::error::EngineError;
use crate::source::{
    EncodedFrame, Keepalive, SourceControl, SourceInfo, SourceSpec, VideoSource, fit_size,
};

pub use relay::Relay;
pub use viewer::{AudioRx, FrameRx, Viewer, ViewerOptions, ViewerStats};

/// Collects every event the engine sends, in order.
#[derive(Debug)]
pub struct CollectingSink {
    tx: mpsc::UnboundedSender<(DeviceId, ServerEvent)>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<(DeviceId, ServerEvent)>>>,
}

impl Default for CollectingSink {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
        }
    }
}

impl CollectingSink {
    /// The receiving end (once).
    pub fn take_receiver(&self) -> mpsc::UnboundedReceiver<(DeviceId, ServerEvent)> {
        self.rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .expect("receiver already taken")
    }
}

impl SignalSink for CollectingSink {
    fn send(&self, device: &DeviceId, event: ServerEvent) {
        let _ = self.tx.send((device.clone(), event));
    }
}

/// A source without capture or GPU: produces access units of `target_bps / fps` bytes at `fps`
/// (IDR when asked and every `gop` frames), or, while [`SyntheticSource::set_static`] is on, no
/// new content at all, so only the keepalive rule produces frames.
#[derive(Debug)]
pub struct SyntheticSource {
    static_content: Arc<AtomicBool>,
    /// Frames this source produced over its life (all sessions).
    pub produced: Arc<AtomicU64>,
    /// Refuse to start with this error text.
    pub fail: Mutex<Option<String>>,
    /// IDR period in frames.
    pub gop: u64,
}

impl Default for SyntheticSource {
    fn default() -> Self {
        Self {
            static_content: Arc::new(AtomicBool::new(false)),
            produced: Arc::new(AtomicU64::new(0)),
            fail: Mutex::new(None),
            gop: 240,
        }
    }
}

impl SyntheticSource {
    /// Static window (no new content) or animated.
    pub fn set_static(&self, v: bool) {
        self.static_content.store(v, Ordering::Relaxed);
    }
}

/// One fake H.264 access unit of about `size` bytes (no zero bytes after the start codes, so
/// the packetiser sees exactly the NAL units written here).
pub fn fake_au(size: usize, key: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(size + 32);
    if key {
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0xE0, 0x32, 0xAA]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80]);
        v.extend_from_slice(&[0, 0, 0, 1, 0x65]);
    } else {
        v.extend_from_slice(&[0, 0, 0, 1, 0x41]);
    }
    let fill = size.saturating_sub(v.len()).max(8);
    v.extend(std::iter::repeat_n(0xAAu8, fill));
    v
}

impl VideoSource for SyntheticSource {
    fn start(
        &self,
        spec: &SourceSpec,
        ctl: Arc<SourceControl>,
        out: mpsc::Sender<EncodedFrame>,
    ) -> Result<SourceInfo, EngineError> {
        if let Some(e) = self.fail.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Err(EngineError::Source(e));
        }
        let (w, h) = fit_size((960, 1600), spec.viewport);
        let fps = spec.fps.max(1);
        let keep = spec.keepalive;
        let st = Arc::clone(&self.static_content);
        let produced = Arc::clone(&self.produced);
        let gop = self.gop.max(1);
        std::thread::Builder::new()
            .name("synthetic-source".into())
            .spawn(move || {
                let period = Duration::from_secs(1) / fps;
                let mut keepalive = Keepalive::new(keep);
                let mut n = 0u64;
                let mut next = Instant::now();
                while !ctl.is_stopping() {
                    next += period;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        next = now;
                    }
                    let now = Instant::now();
                    if ctl.is_paused() {
                        keepalive.reset();
                        continue;
                    }
                    let fresh = !st.load(Ordering::Relaxed);
                    let repeat = !fresh && (keepalive.due(now) || ctl.keyframe_pending());
                    if !fresh && !repeat && n > 0 {
                        continue;
                    }
                    let key = ctl.take_keyframe_request() || n.is_multiple_of(gop);
                    let bps = ctl.target_bps.load(Ordering::Acquire);
                    ctl.stats.applied_bps.store(bps, Ordering::Relaxed);
                    let size = if repeat && !key {
                        40
                    } else {
                        (bps as usize / 8 / fps as usize).max(200)
                    };
                    let frame = EncodedFrame {
                        data: Bytes::from(fake_au(size, key)),
                        keyframe: key,
                        keepalive: repeat,
                        captured: now,
                    };
                    n += 1;
                    keepalive.fed(now);
                    let s = &ctl.stats;
                    if repeat {
                        s.frames_keepalive.fetch_add(1, Ordering::Relaxed);
                    } else {
                        s.frames_new.fetch_add(1, Ordering::Relaxed);
                    }
                    if key {
                        s.keyframes_out.fetch_add(1, Ordering::Relaxed);
                    }
                    s.frames_out.fetch_add(1, Ordering::Relaxed);
                    produced.fetch_add(1, Ordering::Relaxed);
                    match out.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            s.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                    }
                }
            })
            .map_err(|e| EngineError::Source(e.to_string()))?;
        Ok(SourceInfo {
            adapter: "synthetic".into(),
            vendor_id: 0,
            encoder: "synthetic".into(),
            width: w,
            height: h,
        })
    }
}

/// An audio source without WASAPI (WP-55 tests): while active, a sine of `hz` at `amplitude`
/// (the same Opus framing and pacing rules as [`crate::win::audio::ProcessLoopback`]), counting
/// every activation; [`SyntheticAudio::fail`] makes `start` refuse (no process loopback).
#[derive(Debug)]
pub struct SyntheticAudio {
    /// Tone frequency, Hz.
    pub hz: f64,
    /// Tone amplitude (1.0 = full scale).
    pub amplitude: f32,
    /// Refuse to start with this text.
    pub fail: Mutex<Option<String>>,
    /// The control of the last session started (tests read its counters and gate).
    pub last: Mutex<Option<Arc<crate::audio::AudioControl>>>,
}

impl Default for SyntheticAudio {
    fn default() -> Self {
        Self {
            hz: 1000.0,
            amplitude: 0.0316,
            fail: Mutex::new(None),
            last: Mutex::new(None),
        }
    }
}

impl crate::audio::AudioSource for SyntheticAudio {
    fn start(
        &self,
        _window: &crate::source::WindowTarget,
        ctl: Arc<crate::audio::AudioControl>,
        tx: mpsc::Sender<crate::audio::AudioPacket>,
    ) -> Result<crate::audio::AudioInfo, crate::audio::AudioUnavailable> {
        use crate::audio::{CHANNELS, OpusFramer, SAMPLE_RATE, deliver};
        if let Some(e) = self.fail.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Err(crate::audio::AudioUnavailable(e));
        }
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&ctl));
        let mut framer = OpusFramer::new()?;
        let (hz, amp) = (self.hz, self.amplitude);
        std::thread::Builder::new()
            .name("synthetic-audio".into())
            .spawn(move || {
                let mut phase = 0u64;
                let mut running: Option<(Instant, u64)> = None;
                while !ctl.is_stopped() && !tx.is_closed() {
                    if !ctl.is_active() {
                        running = None;
                        framer.reset();
                        ctl.wait(Duration::from_millis(200));
                        continue;
                    }
                    let (t0, done) = running.get_or_insert_with(|| {
                        ctl.stats.starts.fetch_add(1, Ordering::Relaxed);
                        (Instant::now(), 0)
                    });
                    std::thread::sleep(Duration::from_millis(10));
                    let due = (t0.elapsed().as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
                    let n = due.saturating_sub(*done) as usize;
                    *done += n as u64;
                    let mut pcm = Vec::with_capacity(n * CHANNELS);
                    for _ in 0..n {
                        let v = amp
                            * (2.0 * std::f64::consts::PI * hz * phase as f64
                                / f64::from(SAMPLE_RATE))
                            .sin() as f32;
                        phase += 1;
                        pcm.extend([v, v]);
                    }
                    ctl.stats
                        .frames_captured
                        .fetch_add(n as u64, Ordering::Relaxed);
                    if !deliver(&ctl, &tx, framer.push(&pcm)) {
                        return;
                    }
                }
            })
            .map_err(|e| crate::audio::AudioUnavailable(e.to_string()))?;
        Ok(crate::audio::AudioInfo {
            process_id: 0,
            description: format!("synthetic {hz} Hz tone"),
        })
    }
}
