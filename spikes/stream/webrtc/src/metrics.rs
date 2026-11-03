// SPDX-License-Identifier: Apache-2.0
//! Shared measurement state: per-frame timestamps (QPC, 100 ns) from capture to decode, the
//! congestion controller's estimate, the encoder set point and a sampled timeline.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::Serialize;

/// Now on the QPC clock in 100 ns units (the time base of WGC `SystemRelativeTime`).
#[cfg(windows)]
pub fn now_100ns() -> i64 {
    spike_capture::clock::now_100ns()
}

/// Now on a process-local monotonic clock in 100 ns units (non-Windows builds).
#[cfg(not(windows))]
pub fn now_100ns() -> i64 {
    use std::sync::OnceLock;
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    (T0.get_or_init(std::time::Instant::now).elapsed().as_nanos() / 100) as i64
}

/// An `f64` in an atomic.
#[derive(Debug, Default)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    /// Stores.
    pub fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
    /// Loads.
    pub fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
}

/// Timestamps of one frame (QPC 100 ns).
#[derive(Debug, Default, Clone, Serialize)]
pub struct FrameRec {
    /// DWM composition time of the captured frame.
    pub cap: i64,
    /// Encoder `HaveOutput`.
    pub enc: Option<i64>,
    /// `write_sample` returned (packets handed to the pacer).
    pub sent: Option<i64>,
    /// Last packet of the access unit arrived at the receiver (complete, clean AU).
    pub recv: Option<i64>,
    /// ffmpeg emitted the decoded picture.
    pub dec: Option<i64>,
    /// Encoded size in bytes.
    pub bytes: usize,
    /// IDR.
    pub key: bool,
}

/// One timeline row.
#[derive(Debug, Clone, Serialize)]
pub struct Row {
    /// Seconds since the stream started.
    pub t: f64,
    /// GCC target, kbps.
    pub gcc_kbps: f64,
    /// GCC delay-based half, kbps.
    pub gcc_delay_kbps: f64,
    /// GCC loss-based half, kbps.
    pub gcc_loss_kbps: f64,
    /// Encoder `MeanBitRate` set point, kbps.
    pub enc_set_kbps: f64,
    /// Encoded output over the last second, kbps.
    pub enc_out_kbps: f64,
    /// RTP payload received over the last second, kbps.
    pub rx_kbps: f64,
    /// Relay cap, kbps (0 = none).
    pub cap_kbps: f64,
    /// Relay drops so far.
    pub relay_drops: u64,
    /// Decoded frames over the last second.
    pub dec_fps: f64,
    /// Median capture -> receive latency over the last second, ms.
    pub cap_recv_p50_ms: f64,
}

/// Everything the threads and tasks share.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Frames by id.
    pub frames: Mutex<BTreeMap<u64, FrameRec>>,
    /// GCC target (bps).
    pub gcc_target: AtomicF64,
    /// GCC delay-based half (bps).
    pub gcc_delay: AtomicF64,
    /// GCC loss-based half (bps).
    pub gcc_loss: AtomicF64,
    /// Encoder set point (bps).
    pub enc_setpoint: AtomicU64,
    /// Encoder set point changes: (QPC, bps).
    pub enc_changes: Mutex<Vec<(i64, u32)>>,
    /// Set once the peer connection is up: the source starts feeding the encoder.
    pub go: AtomicBool,
    /// QPC at which `go` was set.
    pub t0: AtomicU64,
    /// Stop everything.
    pub stop: AtomicBool,
    /// Next picture must be an IDR (PLI received).
    pub force_key: AtomicBool,
    /// PLIs the receiver sent.
    pub plis_sent: AtomicU64,
    /// RTCP packet types the host's track saw (diagnostic).
    pub rtcp_kinds: Mutex<Vec<String>>,
    /// PLIs the sender received.
    pub plis_recv: AtomicU64,
    /// RTP payload bytes received.
    pub rx_bytes: AtomicU64,
    /// RTP packets missing (sequence gaps).
    pub rx_pkts_lost: AtomicU64,
    /// RTP packets older than expected (retransmissions, duplicates), ignored.
    pub rx_late: AtomicU64,
    /// Access units dropped because a packet was missing.
    pub rx_aus_lost: AtomicU64,
    /// Clean access units dropped while waiting for an IDR.
    pub rx_aus_skipped: AtomicU64,
    /// Frames the sender could not queue (channel full).
    pub tx_dropped: AtomicU64,
    /// Data channel echo round trips, ms.
    pub dc_rtt_ms: Mutex<Vec<f64>>,
    /// Data channel messages echoed by the host.
    pub dc_echoed: AtomicU64,
    /// Timeline.
    pub timeline: Mutex<Vec<Row>>,
    /// The captured test window once the source has started it: (HWND value, control pipe name).
    pub window: Mutex<Option<(isize, String)>>,
}

impl Metrics {
    /// Updates one frame record (created if missing).
    pub fn with_frame(&self, id: u64, f: impl FnOnce(&mut FrameRec)) {
        let mut g = self.frames.lock().unwrap_or_else(|e| e.into_inner());
        f(g.entry(id).or_default());
    }

    /// Seconds since `go`, for a QPC time.
    pub fn secs(&self, qpc: i64) -> f64 {
        (qpc - self.t0.load(Ordering::Relaxed) as i64) as f64 / 1e7
    }
}

/// Percentile (nearest rank) of unsorted values; NaN when empty.
pub fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    s[rank.min(s.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(pct(&v, 50.0), 3.0);
        assert_eq!(pct(&v, 95.0), 5.0);
        assert_eq!(pct(&v, 0.0), 1.0);
        assert!(pct(&[], 50.0).is_nan());
    }
}
