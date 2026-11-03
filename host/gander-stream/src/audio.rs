// SPDX-License-Identifier: Apache-2.0
//! Audio for Stream mode (WP-55): the streamed application's own sound, Opus-encoded, on a
//! WebRTC audio track next to the video.
//!
//! # What is captured
//!
//! Only the process tree that owns the streamed window, through WASAPI process loopback
//! (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, include-tree mode; Windows 10 2004 or
//! later), in [`crate::win::audio::ProcessLoopback`]. Never the whole system: when process
//! loopback is not available the session has **no audio** and says why (`{"t":"audio","on":false,
//! "reason":"unavailable"}` on `control`); there is no fallback to the endpoint loopback, which
//! would send every other application's sound (calls, notifications) to the phone.
//!
//! # When it runs
//!
//! [`AudioControl::active`] gates the capture: the source captures and encodes only while it is
//! set. The engine sets it while the session is streaming (connected, the phone looking, the
//! control channel alive) and the phone has not muted it (`{"t":"mute","on":true}`). Paused,
//! backgrounded, muted, not yet connected or ended: the WASAPI stream is stopped, nothing is
//! captured or encoded (battery and privacy both).
//!
//! # Format
//!
//! 48 kHz stereo float from WASAPI (the audio engine converts the mix), 20 ms Opus frames,
//! `Application::Audio`, VBR at [`OPUS_BITRATE`] (64 kbit/s) with in-band FEC, payload type
//! [`OPUS_PT`]. The encoder is the reference libopus (BSD-3-Clause) through `opusic-c`, which
//! builds the libopus 1.6.1 source it bundles with CMake and MSVC (CMake is a build requirement,
//! see `docs/release/reproducible-builds.md`; no NASM or clang).
//!
//! While active, a source keeps the packet clock tied to the wall clock: when the application
//! is silent, process loopback may deliver nothing, so the source pads with silence (see
//! [`SilencePad`]) and the RTP timestamps keep pace with real time.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use opusic_c::{
    Application, Bitrate, Channels, Encoder, ErrorCode, InbandFec, SampleRate as OpusRate,
};
use tokio::sync::mpsc;

use crate::source::WindowTarget;

/// Sample rate of the capture and the Opus track.
pub const SAMPLE_RATE: u32 = 48_000;
/// Channels (interleaved stereo).
pub const CHANNELS: usize = 2;
/// Frames (per channel) in one 20 ms Opus packet.
pub const FRAME: usize = 960;
/// Duration of one packet.
pub const PACKET_DURATION: Duration = Duration::from_millis(20);
/// Opus target bitrate, bit/s.
pub const OPUS_BITRATE: u32 = 64_000;
/// RTP payload type of the Opus track (the libwebrtc default).
pub const OPUS_PT: u8 = 111;
/// The audio track's SSRC ("GNDA").
pub const AUDIO_SSRC: u32 = 0x474E_4441;

/// One encoded 20 ms Opus packet.
#[derive(Debug, Clone)]
pub struct AudioPacket {
    /// The Opus packet.
    pub data: Bytes,
    /// When its last sample was captured (or padded).
    pub captured: Instant,
}

/// Counters a source maintains.
#[derive(Debug, Default)]
pub struct AudioStats {
    /// Opus packets produced.
    pub packets_out: AtomicU64,
    /// Bytes of Opus produced.
    pub bytes_out: AtomicU64,
    /// Frames (per channel) captured from the device.
    pub frames_captured: AtomicU64,
    /// Frames of silence padded while the application played nothing.
    pub frames_padded: AtomicU64,
    /// Packets dropped because the engine's queue was full.
    pub dropped: AtomicU64,
    /// Times the capture stream was started (each activation).
    pub starts: AtomicU64,
}

/// Shared steering between the engine and a running audio source.
#[derive(Debug, Default)]
pub struct AudioControl {
    active: AtomicBool,
    stop: AtomicBool,
    /// Counters.
    pub stats: AudioStats,
    wake: (Mutex<()>, Condvar),
}

impl AudioControl {
    /// Inactive, not stopped.
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture and encode (`true`) or not; wakes a waiting source.
    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::Release);
        self.notify();
    }

    /// Capturing is wanted now.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire) && !self.is_stopped()
    }

    /// Ends the source soon.
    pub fn stop(&self) {
        self.active.store(false, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.notify();
    }

    /// The source must end.
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Blocks up to `timeout` or until [`AudioControl::set_active`] / [`AudioControl::stop`].
    pub fn wait(&self, timeout: Duration) {
        let guard = self.wake.0.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_active() || self.is_stopped() {
            return;
        }
        let _ = self.wake.1.wait_timeout(guard, timeout);
    }

    fn notify(&self) {
        let _guard = self.wake.0.lock().unwrap_or_else(|e| e.into_inner());
        self.wake.1.notify_all();
    }
}

/// What a started audio source reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInfo {
    /// The root of the captured process tree.
    pub process_id: u32,
    /// What captures (for logs and stats).
    pub description: String,
}

/// Why a session has no audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioUnavailable(pub String);

impl fmt::Display for AudioUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AudioUnavailable {}

/// Captures the window's application sound and produces Opus packets, steered by
/// [`AudioControl`]. `start` blocks only until the capture is set up (or refused); the source
/// then runs on its own thread, starts inactive, and drops `tx` when it ends.
pub trait AudioSource: Send + Sync + fmt::Debug + 'static {
    /// Sets up capture of `window`'s process tree.
    fn start(
        &self,
        window: &WindowTarget,
        ctl: std::sync::Arc<AudioControl>,
        tx: mpsc::Sender<AudioPacket>,
    ) -> Result<AudioInfo, AudioUnavailable>;
}

/// Interleaved stereo float in, 20 ms Opus packets out.
pub struct OpusFramer {
    enc: Encoder,
    pcm: Vec<f32>,
    out: Vec<u8>,
}

impl fmt::Debug for OpusFramer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpusFramer")
            .field("buffered", &(self.pcm.len() / CHANNELS))
            .finish_non_exhaustive()
    }
}

impl OpusFramer {
    /// A 48 kHz stereo encoder at [`OPUS_BITRATE`], VBR, in-band FEC on.
    pub fn new() -> Result<Self, AudioUnavailable> {
        let unavailable = |what: &str, e: ErrorCode| {
            AudioUnavailable(format!("opus encoder ({what}): {}", e.message()))
        };
        let mut enc = Encoder::new(Channels::Stereo, OpusRate::Hz48000, Application::Audio)
            .map_err(|e| unavailable("create", e))?;
        enc.set_bitrate(Bitrate::Value(OPUS_BITRATE))
            .map_err(|e| unavailable("bitrate", e))?;
        enc.set_vbr(true).map_err(|e| unavailable("vbr", e))?;
        enc.set_inband_fec(InbandFec::Mode1)
            .map_err(|e| unavailable("fec", e))?;
        enc.set_packet_loss(5)
            .map_err(|e| unavailable("packet loss", e))?;
        Ok(Self {
            enc,
            pcm: Vec::with_capacity(FRAME * CHANNELS * 4),
            out: vec![0; 1500],
        })
    }

    /// Appends interleaved stereo samples and returns every complete packet.
    pub fn push(&mut self, interleaved: &[f32]) -> Vec<Bytes> {
        self.pcm.extend_from_slice(interleaved);
        let mut packets = Vec::new();
        let n = FRAME * CHANNELS;
        while self.pcm.len() >= n {
            match self
                .enc
                .encode_float_to_slice(&self.pcm[..n], &mut self.out)
            {
                Ok(len) => packets.push(Bytes::copy_from_slice(&self.out[..len])),
                Err(e) => tracing::debug!("opus encode: {}", e.message()),
            }
            self.pcm.drain(..n);
        }
        packets
    }

    /// Appends `frames` frames of silence.
    pub fn push_silence(&mut self, frames: usize) -> Vec<Bytes> {
        self.push(&vec![0.0; frames * CHANNELS])
    }

    /// Drops a partial frame (the stream stopped; the next start is a new talk spurt).
    pub fn reset(&mut self) {
        self.pcm.clear();
    }
}

/// Keeps an active capture's sample count level with the wall clock: process loopback delivers
/// nothing while the application is silent, so the source asks how much silence to add.
#[derive(Debug, Clone)]
pub struct SilencePad {
    since: Instant,
    frames: u64,
}

impl SilencePad {
    /// How far behind the clock a capture may fall before silence is added (device periods
    /// arrive in bursts of about 10 ms).
    pub const SLACK: Duration = Duration::from_millis(60);

    /// Starts counting now.
    pub fn new(now: Instant) -> Self {
        Self {
            since: now,
            frames: 0,
        }
    }

    /// `frames` frames arrived from the device.
    pub fn delivered(&mut self, frames: u64) {
        self.frames += frames;
    }

    /// Frames of silence to add at `now` (0 while within [`SilencePad::SLACK`]); counts them as
    /// delivered.
    pub fn pad(&mut self, now: Instant) -> u64 {
        let elapsed = now.saturating_duration_since(self.since);
        let expected = (elapsed.as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
        let slack = (Self::SLACK.as_secs_f64() * f64::from(SAMPLE_RATE)) as u64;
        if expected <= self.frames + slack {
            return 0;
        }
        // Catch up to 20 ms behind the clock, so the next device burst is not "early".
        let add = expected - self.frames - FRAME as u64;
        self.frames += add;
        add
    }
}

/// Hands encoded packets to the engine; counts and drops when the queue is full (the engine is
/// behind: old audio is worth less than new).
pub fn deliver(ctl: &AudioControl, tx: &mpsc::Sender<AudioPacket>, packets: Vec<Bytes>) -> bool {
    let now = Instant::now();
    for data in packets {
        let len = data.len() as u64;
        match tx.try_send(AudioPacket {
            data,
            captured: now,
        }) {
            Ok(()) => {
                ctl.stats.packets_out.fetch_add(1, Ordering::Relaxed);
                ctl.stats.bytes_out.fetch_add(len, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                ctl.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
    true
}

/// Tone detection for the tests (WP-55 verification): the strongest frequency in a mono signal
/// and its level.
pub mod tone {
    /// What [`detect`] found.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Detected {
        /// Frequency of the strongest component, Hz.
        pub hz: f64,
        /// Its amplitude (a full-scale sine is 1.0).
        pub amplitude: f64,
        /// RMS level of the analysed window, dBFS (a full-scale sine is -3 dBFS).
        pub rms_dbfs: f64,
        /// The strongest component's share of the window's power (1.0 = a pure tone).
        pub purity: f64,
    }

    /// Goertzel power of `hz` over `x` (generalised: `hz` need not fall on a DFT bin).
    pub fn goertzel(x: &[f32], rate: f64, hz: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * hz / rate;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s0 = f64::from(v) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
    }

    /// The loudest `window`-sample stretch of `x` (hop of a quarter window).
    pub fn loudest(x: &[f32], window: usize) -> &[f32] {
        if x.len() <= window {
            return x;
        }
        let hop = (window / 4).max(1);
        let mut best = (0usize, -1.0f64);
        let mut start = 0;
        while start + window <= x.len() {
            let e: f64 = x[start..start + window]
                .iter()
                .map(|&v| f64::from(v) * f64::from(v))
                .sum();
            if e > best.1 {
                best = (start, e);
            }
            start += hop;
        }
        &x[best.0..best.0 + window]
    }

    /// The strongest frequency between `lo` and `hi` Hz in `x` (a 10 Hz scan, then 0.1 Hz
    /// steps around the peak). `None` for an empty or silent signal.
    pub fn detect(x: &[f32], rate: f64, lo: f64, hi: f64) -> Option<Detected> {
        if x.is_empty() {
            return None;
        }
        let energy: f64 = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        if energy <= 0.0 {
            return None;
        }
        let n = x.len() as f64;
        let mut best = (lo, 0.0f64);
        let mut f = lo;
        while f <= hi {
            let p = goertzel(x, rate, f);
            if p > best.1 {
                best = (f, p);
            }
            f += 10.0;
        }
        let mut fine = best;
        let mut f = (best.0 - 10.0).max(lo);
        while f <= (best.0 + 10.0).min(hi) {
            let p = goertzel(x, rate, f);
            if p > fine.1 {
                fine = (f, p);
            }
            f += 0.1;
        }
        // |X(f)| = A n / 2 for a sine of amplitude A; the sine's power is A^2 n / 2.
        let amplitude = 2.0 * fine.1.sqrt() / n;
        let purity = (amplitude * amplitude * n / 2.0) / energy;
        let rms = (energy / n).sqrt();
        Some(Detected {
            hz: fine.0,
            amplitude,
            rms_dbfs: 20.0 * rms.max(1e-12).log10(),
            purity: purity.min(1.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f64, amp: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = amp
                    * (2.0 * std::f64::consts::PI * hz * i as f64 / f64::from(SAMPLE_RATE)).sin()
                        as f32;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn framer_emits_20_ms_packets_and_round_trips_a_tone() {
        let mut f = OpusFramer::new().unwrap();
        // 0.5 s of a 1 kHz tone at -30 dBFS (amplitude 0.0316), pushed in odd-sized chunks.
        let pcm = sine(1000.0, 0.0316, 24_000);
        let mut packets = Vec::new();
        for chunk in pcm.chunks(441 * 2) {
            packets.extend(f.push(chunk));
        }
        assert_eq!(packets.len(), 25);
        let mut dec = opusic_c::Decoder::new(Channels::Stereo, OpusRate::Hz48000).unwrap();
        let mut mono = Vec::new();
        let mut out = vec![0.0f32; FRAME * CHANNELS];
        for p in &packets {
            let n = dec.decode_float_to_slice(p, &mut out, false).unwrap();
            assert_eq!(n, FRAME);
            mono.extend(out[..n * CHANNELS].chunks(2).map(|s| (s[0] + s[1]) / 2.0));
        }
        let d = tone::detect(tone::loudest(&mono, 12_000), 48_000.0, 100.0, 4000.0).unwrap();
        assert!((d.hz - 1000.0).abs() < 5.0, "{d:?}");
        assert!((d.rms_dbfs + 33.0).abs() < 3.0, "{d:?}");
        assert!(d.purity > 0.8, "{d:?}");
        // Partial frames are dropped by reset; silence encodes too.
        assert!(f.push(&sine(1000.0, 0.1, 100)).is_empty());
        f.reset();
        assert_eq!(f.push_silence(FRAME * 2).len(), 2);
    }

    #[test]
    fn goertzel_finds_odd_frequencies() {
        for hz in [440.0, 997.0, 1234.5, 3000.0] {
            let pcm: Vec<f32> = sine(hz, 0.5, 9600).chunks(2).map(|s| s[0]).collect();
            let d = tone::detect(&pcm, 48_000.0, 50.0, 4000.0).unwrap();
            assert!((d.hz - hz).abs() < 1.0, "{hz}: {d:?}");
            assert!((d.amplitude - 0.5).abs() < 0.02, "{d:?}");
        }
        assert!(tone::detect(&[0.0; 100], 48_000.0, 50.0, 4000.0).is_none());
    }

    #[test]
    fn silence_pad_follows_the_clock() {
        let t0 = Instant::now();
        let mut pad = SilencePad::new(t0);
        assert_eq!(pad.pad(t0 + Duration::from_millis(50)), 0);
        pad.delivered(2400);
        assert_eq!(pad.pad(t0 + Duration::from_millis(100)), 0);
        // 500 ms, nothing more delivered: pad to 20 ms behind.
        let add = pad.pad(t0 + Duration::from_millis(500));
        assert_eq!(add, 24_000 - 2400 - 960);
        assert_eq!(pad.pad(t0 + Duration::from_millis(510)), 0);
    }

    #[test]
    fn control_gates_and_stops() {
        let c = AudioControl::new();
        assert!(!c.is_active());
        c.set_active(true);
        assert!(c.is_active());
        c.wait(Duration::from_secs(5)); // returns at once while active
        c.stop();
        assert!(!c.is_active());
        assert!(c.is_stopped());
    }
}
