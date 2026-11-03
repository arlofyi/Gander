// SPDX-License-Identifier: Apache-2.0
//! The video source contract: something that captures one window and produces encoded access
//! units, steered by a shared [`SourceControl`].
//!
//! The production source is [`crate::win::WgcSource`] (WGC -> D3D11 video processor -> MF
//! hardware H.264). Tests use `testing::SyntheticSource`, which follows the same control rules
//! without a GPU.
//!
//! # Rules every source follows
//!
//! - Encode at most `fps` frames per second from new captured content.
//! - **Keepalive**: when no new content was encoded for [`SourceSpec::keepalive`], encode the last
//!   picture again ([`Keepalive`]). A static window produces no WGC frames at all, and a phone
//!   decoder that sees nothing for seconds stalls its jitter buffer; the repeat costs a few bytes
//!   (an all-skip P frame).
//! - `force_key`: the next encoded picture is an IDR; the flag is cleared when taken.
//! - `paused`: capture nothing, encode nothing, no keepalive (the phone is not looking).
//! - `target_bps`: follow it (the engine computes it from the estimate and the battery cap).
//! - `stop`: end soon; the source drops its sender, which tells the engine it ended. A source that
//!   ends on its own (window closed, device lost) records why in [`SourceControl::end`] first.

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::error::EngineError;

/// The window to stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTarget {
    /// Top-level window handle (`HWND` as an integer).
    pub hwnd: isize,
    /// Title, for `stream.state` and logs.
    pub title: String,
}

/// What a source is asked to produce.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    /// The window.
    pub window: WindowTarget,
    /// The phone's viewport in physical pixels: the encoded picture fits inside it.
    pub viewport: (u32, u32),
    /// Frame rate cap.
    pub fps: u32,
    /// First bitrate set point, bit/s.
    pub initial_bps: u32,
    /// Repeat the last picture after this long without new content.
    pub keepalive: Duration,
    /// Insert the frame-id SEI (latency probe).
    pub frame_id_sei: bool,
}

/// Why a source ended by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEnd {
    /// The captured window was closed.
    WindowClosed,
    /// Capture or encode failed.
    Error(String),
}

/// Counters a source maintains.
#[derive(Debug, Default)]
pub struct SourceStats {
    /// New captured frames encoded.
    pub frames_new: AtomicU64,
    /// Keepalive repeats encoded.
    pub frames_keepalive: AtomicU64,
    /// Access units produced.
    pub frames_out: AtomicU64,
    /// IDR access units produced.
    pub keyframes_out: AtomicU64,
    /// Access units dropped because the engine's queue was full.
    pub dropped: AtomicU64,
    /// Encoder re-creations (Intel rate changes, resizes).
    pub recreations: AtomicU64,
    /// The set point the encoder runs at now, bit/s.
    pub applied_bps: AtomicU32,
}

/// Shared steering between the engine and a running source.
#[derive(Debug)]
pub struct SourceControl {
    /// Bitrate set point, bit/s.
    pub target_bps: AtomicU32,
    force_key: AtomicBool,
    /// Capture and encode nothing.
    pub paused: AtomicBool,
    /// End now.
    pub stop: AtomicBool,
    /// Counters.
    pub stats: SourceStats,
    end: Mutex<Option<SourceEnd>>,
}

impl SourceControl {
    /// Starts at `initial_bps`, paused (the engine unpauses once the peer is connected).
    pub fn new(initial_bps: u32) -> Self {
        Self {
            target_bps: AtomicU32::new(initial_bps),
            force_key: AtomicBool::new(false),
            paused: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            stats: SourceStats::default(),
            end: Mutex::new(None),
        }
    }

    /// Asks for an IDR.
    pub fn request_keyframe(&self) {
        self.force_key.store(true, Ordering::Release);
    }

    /// A keyframe request is waiting (a static window repeats its picture at once to carry it).
    pub fn keyframe_pending(&self) -> bool {
        self.force_key.load(Ordering::Acquire)
    }

    /// Takes the keyframe request (source side).
    pub fn take_keyframe_request(&self) -> bool {
        self.force_key.swap(false, Ordering::AcqRel)
    }

    /// True when stopping or paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// True when the engine asked the source to end.
    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Records why the source ended by itself.
    pub fn set_end(&self, why: SourceEnd) {
        *self.end.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
    }

    /// Why the source ended by itself, if it did.
    pub fn end(&self) -> Option<SourceEnd> {
        self.end.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// One encoded access unit.
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    /// Annex-B bytes.
    pub data: Bytes,
    /// Contains an IDR.
    pub keyframe: bool,
    /// A keepalive repeat of the previous picture.
    pub keepalive: bool,
    /// When the picture was captured (or repeated).
    pub captured: Instant,
}

/// Facts about a started source.
#[derive(Debug, Clone, Default)]
pub struct SourceInfo {
    /// Adapter name.
    pub adapter: String,
    /// Adapter PCI vendor id.
    pub vendor_id: u32,
    /// Encoder name.
    pub encoder: String,
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
}

/// A window capture + encoder.
pub trait VideoSource: Send + Sync + fmt::Debug + 'static {
    /// Starts capturing `spec.window`. Returns once the first stage is set up (window found,
    /// encoder configured), or the reason it could not be. Frames go to `out` until
    /// `ctl.stop`, the source's own end, or `out` closing.
    fn start(
        &self,
        spec: &SourceSpec,
        ctl: std::sync::Arc<SourceControl>,
        out: mpsc::Sender<EncodedFrame>,
    ) -> Result<SourceInfo, EngineError>;
}

/// The keepalive rule: repeat the last picture after `interval` without new content.
#[derive(Debug, Clone, Copy)]
pub struct Keepalive {
    interval: Duration,
    last: Option<Instant>,
}

impl Keepalive {
    /// A rule with the given interval.
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }

    /// A picture (new or repeated) was fed to the encoder at `now`.
    pub fn fed(&mut self, now: Instant) {
        self.last = Some(now);
    }

    /// Forget the last picture (pause, resize): no repeat until something new is fed.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// True when a repeat is due at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.last
            .is_some_and(|l| now.saturating_duration_since(l) >= self.interval)
    }
}

/// The encoded size for a window of `src` pixels shown in a `viewport`: the window's aspect,
/// fitted inside the viewport, never larger than the window, even, at least 64x64.
pub fn fit_size(src: (u32, u32), viewport: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (src.0.max(1) as f64, src.1.max(1) as f64);
    let (vw, vh) = (viewport.0.max(64) as f64, viewport.1.max(64) as f64);
    let s = (vw / sw).min(vh / sh).min(1.0);
    let even = |v: f64| ((v.round() as u32).max(64)) & !1;
    (even(sw * s), even(sh * s))
}

impl fmt::Display for SourceEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceEnd::WindowClosed => f.write_str("window closed"),
            SourceEnd::Error(e) => write!(f, "{e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_fires_after_the_interval_only() {
        let t = Instant::now();
        let mut k = Keepalive::new(Duration::from_millis(450));
        assert!(!k.due(t), "nothing fed yet");
        k.fed(t);
        assert!(!k.due(t + Duration::from_millis(449)));
        assert!(k.due(t + Duration::from_millis(450)));
        k.reset();
        assert!(!k.due(t + Duration::from_secs(5)));
    }

    #[test]
    fn fit_keeps_aspect_and_parity() {
        assert_eq!(fit_size((964, 767), (1080, 2280)), (964, 766));
        assert_eq!(fit_size((2000, 1000), (1080, 2280)), (1080, 540));
        assert_eq!(fit_size((1081, 2281), (1080, 2280)), (1080, 2278));
        assert_eq!(fit_size((10, 10), (1080, 2280)), (64, 64));
    }

    #[test]
    fn keyframe_request_is_taken_once() {
        let c = SourceControl::new(1);
        assert!(!c.take_keyframe_request());
        c.request_keyframe();
        assert!(c.take_keyframe_request());
        assert!(!c.take_keyframe_request());
        assert!(c.is_paused(), "starts paused until connected");
    }
}
