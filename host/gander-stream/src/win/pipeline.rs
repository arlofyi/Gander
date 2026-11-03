// SPDX-License-Identifier: Apache-2.0
//! The production video source: one window captured with WGC into a D3D11 device on the
//! encoder's adapter (chosen by LUID), converted to NV12 by the video processor and encoded by
//! that adapter's hardware H.264 MFT. Runs on its own thread (COM MTA, Media Foundation started
//! there) with the 1 ms timer resolution held while it runs.
//!
//! Adapter choice: the caller's LUID, else the caller's vendor, else NVIDIA, AMD, Intel, others.
//! On each adapter with a hardware H.264 MFT the encoder is activated for real; the first that
//! works wins (an NVIDIA MFT refuses to activate without the Optimus export, for example). The
//! discrete GPU is requested ([`GpuDecision`]) only while the encoder is set up or re-created:
//! once activated, the NVIDIA MFT keeps encoding with the request dropped, so a settings window
//! opened during a stream still lands on the integrated GPU.
//!
//! Rate: NVIDIA (and others) take `MeanBitRate` at run time; the Intel Quick Sync MFT accepts it
//! and ignores it (WP-42/43), so on Intel the encoder is re-created at the new rate (about
//! 100-160 ms and one IDR): at once on a cut over 15 %, at most every 1.5 s on a rise over 25 %.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::sync::mpsc;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::UI::WindowsAndMessaging::IsWindow;

use super::TimerResolution;
use super::clock;
use super::convert::Converter;
use super::d3d::{self, Adapter};
use super::encoder::{Config, Encoder, Event};
use super::mft::{self, Codec, EncoderInfo};
use super::optimus::GpuDecision;
use super::wgc::{Capture, CaptureOptions, Frame};
use crate::error::EngineError;
use crate::h264;
use crate::source::{
    EncodedFrame, Keepalive, SourceControl, SourceEnd, SourceInfo, SourceSpec, VideoSource,
    fit_size,
};

const RING: usize = 8;
const VENDOR_INTEL: u32 = 0x8086;
const VENDOR_NVIDIA: u32 = 0x10DE;
const VENDOR_AMD: u32 = 0x1002;

/// The WGC + Media Foundation source.
#[derive(Debug, Clone, Default)]
pub struct WgcSource {
    /// Use this adapter (LUID packed `(HighPart << 32) | LowPart`).
    pub prefer_luid: Option<i64>,
    /// Else prefer this PCI vendor (0x8086 Intel, 0x10DE NVIDIA, 0x1002 AMD).
    pub prefer_vendor: Option<u32>,
    /// `IsBorderRequired` (`[stream] capture_border`); `None` keeps the OS default (border on).
    /// The border is not Gander's "being viewed" indicator, only an extra cue.
    pub border_required: Option<bool>,
}

impl VideoSource for WgcSource {
    fn start(
        &self,
        spec: &SourceSpec,
        ctl: Arc<SourceControl>,
        out: mpsc::Sender<EncodedFrame>,
    ) -> Result<SourceInfo, EngineError> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let spec = spec.clone();
        let prefs = self.clone();
        std::thread::Builder::new()
            .name("gander-stream-source".into())
            .spawn(move || run(&spec, &prefs, &ctl, &out, &ready_tx))
            .map_err(|e| EngineError::Source(e.to_string()))?;
        match ready_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(r) => r.map_err(EngineError::Source),
            Err(_) => Err(EngineError::Source("source setup timed out".into())),
        }
    }
}

fn adapter_order(adapters: Vec<Adapter>, prefs: &WgcSource) -> Vec<Adapter> {
    let rank = |a: &Adapter| -> u32 {
        if prefs.prefer_luid == Some(a.info.luid) {
            return 0;
        }
        if prefs.prefer_vendor == Some(a.info.vendor_id) {
            return 1;
        }
        match a.info.vendor_id {
            VENDOR_NVIDIA => 2,
            VENDOR_AMD => 3,
            VENDOR_INTEL => 4,
            _ => 5,
        }
    };
    let mut v = adapters;
    v.sort_by_key(|a| rank(a));
    v
}

struct Chosen {
    adapter: Adapter,
    encoder: EncoderInfo,
    device: ID3D11Device,
    capture: Capture,
    conv: Converter,
    enc: Encoder,
    cfg: Config,
}

fn encoder_config(w: u32, h: u32, fps: u32, bps: u32) -> Config {
    let mut c = Config::new(Codec::H264, w, h, fps, bps);
    c.gop = fps * 2;
    c
}

fn setup(spec: &SourceSpec, prefs: &WgcSource) -> Result<Chosen> {
    let hwnd = HWND(spec.window.hwnd as *mut c_void);
    anyhow::ensure!(
        // SAFETY: IsWindow accepts any handle value.
        unsafe { IsWindow(Some(hwnd)) }.as_bool(),
        "the window does not exist"
    );
    let adapters = adapter_order(d3d::adapters()?, prefs);
    let mut errors = Vec::new();
    for adapter in adapters {
        let Ok(encoders) = mft::hardware_encoders(Codec::H264, Some(adapter.info.luid)) else {
            continue;
        };
        for e in encoders {
            let attempt = (|| -> Result<Chosen> {
                let device = d3d::create_device(Some(&adapter.adapter))?;
                let opts = CaptureOptions {
                    cursor: Some(false),
                    include_secondary_windows: Some(true),
                    border_required: prefs.border_required,
                    ..CaptureOptions::default()
                };
                let capture = Capture::start(hwnd, &device, &opts)?;
                let (iw, ih) = capture.item_size().context("window has no size")?;
                let (w, h) = fit_size((iw.max(1) as u32, ih.max(1) as u32), spec.viewport);
                let conv = Converter::new(&device, w, h, spec.fps, RING)?;
                let cfg = encoder_config(w, h, spec.fps, spec.initial_bps);
                let enc = Encoder::new(&e.activate, &device, &cfg)?;
                Ok(Chosen {
                    adapter: adapter.clone(),
                    encoder: e.clone(),
                    device,
                    capture,
                    conv,
                    enc,
                    cfg,
                })
            })();
            match attempt {
                Ok(c) => return Ok(c),
                Err(err) => errors.push(format!("{} / {}: {err:#}", adapter.info.name, e.name)),
            }
        }
    }
    anyhow::bail!("no usable hardware H.264 encoder: {}", errors.join("; "))
}

fn run(
    spec: &SourceSpec,
    prefs: &WgcSource,
    ctl: &Arc<SourceControl>,
    out: &mpsc::Sender<EncodedFrame>,
    ready: &std::sync::mpsc::Sender<std::result::Result<SourceInfo, String>>,
) {
    let _timer = TimerResolution::one_ms();
    if let Err(e) = mft::startup() {
        let _ = ready.send(Err(format!("{e:#}")));
        return;
    }
    let gpu = GpuDecision::take(true);
    let setup = setup(spec, prefs);
    drop(gpu);
    match setup {
        Ok(chosen) => {
            let _ = ready.send(Ok(SourceInfo {
                adapter: chosen.adapter.info.name.clone(),
                vendor_id: chosen.adapter.info.vendor_id,
                encoder: chosen.encoder.name.clone(),
                width: chosen.cfg.width,
                height: chosen.cfg.height,
            }));
            if let Err(e) = pump(spec, ctl, out, chosen) {
                ctl.set_end(SourceEnd::Error(format!("{e:#}")));
            }
        }
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
        }
    }
    mft::shutdown();
}

/// A fresh encoder of the same MFT (a shut-down `IMFActivate` cannot be reused).
fn recreate(chosen: &Chosen, cfg: &Config) -> Result<Encoder> {
    // The Intel MFT needs no discrete-GPU request (and re-creates often, on rate changes).
    let _gpu = (chosen.adapter.info.vendor_id != VENDOR_INTEL).then(|| GpuDecision::take(true));
    let fresh = mft::hardware_encoders(Codec::H264, Some(d3d::device_luid(&chosen.device)?))?;
    let act = fresh
        .iter()
        .find(|e| e.clsid == chosen.encoder.clsid)
        .or(fresh.first())
        .context("encoder MFT gone")?;
    Encoder::new(&act.activate, &chosen.device, cfg)
}

struct InFlight {
    captured: Instant,
    cap_100ns: i64,
    keepalive: bool,
    id: u64,
}

fn pump(
    spec: &SourceSpec,
    ctl: &Arc<SourceControl>,
    out: &mpsc::Sender<EncodedFrame>,
    mut chosen: Chosen,
) -> Result<()> {
    let st = &ctl.stats;
    let fps = spec.fps.max(1);
    let period = 10_000_000i64 / i64::from(fps);
    let recreate_rate = chosen.adapter.info.vendor_id == VENDOR_INTEL;
    let hwnd = HWND(spec.window.hwnd as *mut c_void);
    let mut cfg = chosen.cfg.clone();
    let mut need = 0u32;
    // (ring index, capture time 100 ns, keepalive)
    let mut pending: Option<(usize, i64, bool)> = None;
    let mut last_index: Option<usize> = None;
    let mut inflight: HashMap<i64, InFlight> = HashMap::new();
    let mut last_accepted: Option<i64> = None;
    // The newest captured frame that came too soon after the previous one (frame pacing) or
    // while the ring was full. WGC delivers a frame only when the window changes, so dropping it
    // would leave the phone on a stale picture until the next change: the last repaint of a
    // burst (the box after a click, the last character typed) is kept and converted once it is
    // due. At most one is held, so the capture pool is never starved.
    let mut held: Option<Frame> = None;
    let mut last_time = -1i64;
    let t0 = clock::now_100ns();
    let mut next_id = 0u64;
    let mut cur_bps = cfg.bitrate;
    let mut last_change = Instant::now();
    let mut keepalive = Keepalive::new(spec.keepalive);
    let mut last_alive_check = Instant::now();
    st.applied_bps.store(cur_bps, Ordering::Relaxed);

    while !ctl.is_stopping() {
        // Encoder output.
        while let Some(ev) = chosen.enc.next_event(Duration::ZERO) {
            match ev {
                Event::NeedInput(_) => need += 1,
                Event::HaveOutput(_) => {
                    let Some(pkt) = chosen.enc.take_output()? else {
                        continue;
                    };
                    let Some(f) = inflight.remove(&pkt.time) else {
                        continue;
                    };
                    let key = h264::is_idr(&pkt.data);
                    let data = if spec.frame_id_sei {
                        h264::insert_sei(&pkt.data, &h264::sei_nal(f.id, f.cap_100ns))
                    } else {
                        pkt.data
                    };
                    st.frames_out.fetch_add(1, Ordering::Relaxed);
                    if key {
                        st.keyframes_out.fetch_add(1, Ordering::Relaxed);
                    }
                    let frame = EncodedFrame {
                        data: Bytes::from(data),
                        keyframe: key,
                        keepalive: f.keepalive,
                        captured: f.captured,
                    };
                    match out.try_send(frame) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            st.dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
                    }
                }
                Event::DrainComplete(_) | Event::Other(_) => {}
                Event::Error(e) => anyhow::bail!("encoder: {e}"),
            }
        }

        // The window.
        if chosen.capture.is_closed() {
            ctl.set_end(SourceEnd::WindowClosed);
            return Ok(());
        }
        if last_alive_check.elapsed() >= Duration::from_millis(500) {
            last_alive_check = Instant::now();
            // SAFETY: IsWindow accepts any handle value.
            if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                ctl.set_end(SourceEnd::WindowClosed);
                return Ok(());
            }
        }

        if ctl.is_paused() {
            // Nothing is captured or encoded; pool buffers are released at once.
            pending = None;
            held = None;
            keepalive.reset();
            if let Some(f) = chosen.capture.recv_timeout(Duration::from_millis(20)) {
                drop(f);
            }
            continue;
        }

        // Follow the set point.
        let want = ctl.target_bps.load(Ordering::Acquire).max(100_000);
        if want != cur_bps {
            let moved = (f64::from(want) - f64::from(cur_bps)) / f64::from(cur_bps);
            let since = last_change.elapsed();
            let due = if recreate_rate {
                (moved < -0.15 && since >= Duration::from_millis(100))
                    || (moved > 0.25 && since >= Duration::from_millis(1500))
            } else {
                moved.abs() > 0.05 && since >= Duration::from_millis(100)
            };
            if due {
                if recreate_rate {
                    cfg.bitrate = want;
                    let new = recreate(&chosen, &cfg)?;
                    drop(std::mem::replace(&mut chosen.enc, new));
                    inflight.clear();
                    need = 0;
                    st.recreations.fetch_add(1, Ordering::Relaxed);
                } else if chosen.enc.set_bitrate(want).is_err() {
                    // Leave the rate; try again on the next change.
                }
                cur_bps = want;
                cfg.bitrate = want;
                st.applied_bps.store(want, Ordering::Relaxed);
                last_change = Instant::now();
            }
        }

        // New content: a fresh frame replaces a held one (dropping the older returns its buffer).
        if let Some(fresh) = chosen.capture.recv_timeout(Duration::from_millis(2)) {
            held = Some(fresh);
        }
        if let Some(frame) = held.take() {
            let srt = frame.system_relative_time_100ns();
            let (fw, fh) = frame.size();
            let fit = fit_size((fw, fh), spec.viewport);
            if fit != (cfg.width, cfg.height) {
                // The window was resized: new converter and encoder at the new size, IDR first.
                drop(frame);
                cfg.width = fit.0;
                cfg.height = fit.1;
                chosen.conv = Converter::new(&chosen.device, fit.0, fit.1, fps, RING)?;
                let new = recreate(&chosen, &cfg)?;
                drop(std::mem::replace(&mut chosen.enc, new));
                inflight.clear();
                need = 0;
                pending = None;
                last_index = None;
                keepalive.reset();
                st.recreations.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            // Pacing against now, not the capture time, so a held frame becomes due.
            let at = clock::now_100ns().max(srt);
            let due = last_accepted.is_none_or(|l| at - l >= period * 8 / 10);
            if due && inflight.len() + 2 < RING {
                last_accepted = Some(at);
                let c = chosen.conv.convert(frame.texture(), fw, fh)?;
                drop(frame);
                pending = Some((c.index, srt, false));
                last_index = Some(c.index);
            } else {
                held = Some(frame);
            }
        }
        // Keepalive: nothing new for a while, repeat the last picture.
        let now = Instant::now();
        if pending.is_none()
            && (keepalive.due(now) || ctl.keyframe_pending())
            && let Some(idx) = last_index
            && inflight.len() + 2 < RING
        {
            pending = Some((idx, clock::now_100ns(), true));
        }
        if need > 0
            && let Some((idx, cap, repeat)) = pending.take()
        {
            if ctl.take_keyframe_request() {
                let _ = chosen.enc.force_keyframe();
            }
            // Sample times must be unique and increasing (they key the in-flight map).
            let time = (cap - t0).max(last_time + 1);
            last_time = time;
            let tex = chosen.conv.ring()[idx].clone();
            inflight.insert(
                time,
                InFlight {
                    captured: now,
                    cap_100ns: cap,
                    keepalive: repeat,
                    id: next_id,
                },
            );
            next_id += 1;
            chosen.enc.feed(&tex, time, period)?;
            need -= 1;
            keepalive.fed(now);
            if repeat {
                st.frames_keepalive.fetch_add(1, Ordering::Relaxed);
            } else {
                st.frames_new.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    Ok(())
}
