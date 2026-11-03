// SPDX-License-Identifier: Apache-2.0
//! The video source thread: `gander-testwin` captured with WGC (WP-41) into a device on the
//! chosen adapter, converted to NV12 and encoded by that adapter's hardware H.264 MFT (WP-42).
//! Each output access unit gets the frame-id SEI and goes to the host task.
//!
//! Rate control: the GCC target published in [`Metrics`] times `headroom` becomes the encoder's
//! `MeanBitRate` (changed at runtime with `Encoder::set_bitrate`) whenever it moves by more than
//! 5 %, at most every 100 ms. A PLI from the viewer sets `force_key`, which forces an IDR.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use spike_capture::{d3d, wgc};
use spike_encode::convert::Converter;
use spike_encode::encoder::{Config, Encoder, Event};
use spike_encode::mft::{self, Codec};
use tokio::sync::mpsc;

use crate::au;
use crate::metrics::{Metrics, now_100ns};
use crate::peer::EncodedFrame;
use crate::testwin::{self, Testwin};

const RING: usize = 8;

/// Source settings.
#[derive(Debug, Clone)]
pub struct SourceConfig {
    /// Encoded size.
    pub width: u32,
    /// Encoded size.
    pub height: u32,
    /// Frames per second (every n-th frame of the 60 Hz test window).
    pub fps: u32,
    /// PCI vendor of the adapter to capture and encode on (0x8086 Intel, 0x10DE NVIDIA).
    pub vendor: u32,
    /// GOP length in frames.
    pub gop: u32,
    /// First set point, bps.
    pub initial_bps: u32,
    /// Set point floor and ceiling, bps.
    pub min_bps: u32,
    /// Set point ceiling, bps.
    pub max_bps: u32,
    /// Encoder set point = GCC target * headroom (the pacer sends at exactly the target, so the
    /// encoder must stay below it or the pacer queue grows).
    pub headroom: f64,
    /// `gander-testwin` self-test deadline (backstop).
    pub testwin_secs: u32,
    /// Title of the test window.
    pub title: String,
}

/// What the source reports at the end.
#[derive(Debug, Default, Clone)]
pub struct SourceReport {
    /// Adapter and encoder names.
    pub adapter: String,
    /// Encoder MFT friendly name.
    pub encoder: String,
    /// Profile accepted.
    pub profile: String,
    /// WGC frame size.
    pub source_size: (u32, u32),
    /// Frames fed.
    pub frames_in: u64,
    /// Access units out.
    pub frames_out: u64,
    /// IDRs out.
    pub idrs: u64,
    /// `set_bitrate` calls that failed (text of the first error).
    pub set_bitrate_errors: Vec<String>,
    /// How the set point was applied (see `SPIKE_RATE_API`).
    pub rate_api: String,
    /// Encoder re-creations: time each took, ms.
    pub recreate_ms: Vec<f64>,
    /// Frames in flight lost to re-creations.
    pub recreated_dropped: u64,
}

/// Runs until `m.stop`. `ready` receives the setup result once the window is captured and the
/// encoder is configured; frames are only fed after `m.go`.
pub fn run(
    cfg: &SourceConfig,
    m: &Arc<Metrics>,
    tx: &mpsc::Sender<EncodedFrame>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<SourceReport> {
    let setup = (|| -> Result<_> {
        mft::startup()?;
        let adapters = d3d::adapters()?;
        let a = adapters
            .iter()
            .find(|a| a.info.vendor_id == cfg.vendor)
            .with_context(|| format!("no adapter with vendor 0x{:04X}", cfg.vendor))?;
        let device = d3d::create_device(Some(&a.adapter))?;
        let encs = mft::hardware_encoders(Codec::H264, Some(d3d::device_luid(&device)?))?;
        let info = encs
            .first()
            .context("no hardware H.264 encoder on that adapter")?;
        let exe = testwin::exe(None)?;
        let win = Testwin::spawn(&exe, &cfg.title, cfg.testwin_secs)?;
        *m.window.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((win.hwnd.0 as isize, win.pipe().to_owned()));
        let work = testwin::work_area(win.hwnd);
        let (ow, oh) = testwin::fit(work, cfg.width, cfg.height, 16);
        testwin::place(win.hwnd, work.left + 16, work.top + 16, ow, oh)?;
        std::thread::sleep(Duration::from_millis(500));
        let cap = wgc::Capture::start(win.hwnd, &device, &wgc::CaptureOptions::default())?;
        let conv = Converter::new(&device, cfg.width, cfg.height, cfg.fps, RING)?;
        let mut ecfg = Config::new(Codec::H264, cfg.width, cfg.height, cfg.fps, cfg.initial_bps);
        ecfg.gop = cfg.gop;
        let enc = Encoder::new(&info.activate, &device, &ecfg)?;
        let report = SourceReport {
            adapter: a.info.name.clone(),
            encoder: info.name.clone(),
            profile: enc.profile.clone(),
            ..Default::default()
        };
        Ok((win, cap, conv, enc, report, device, ecfg, info.clsid))
    })();
    let (win, cap, mut conv, mut enc, mut report, device, mut ecfg, clsid) = match setup {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return Err(e);
        }
    };
    m.enc_setpoint
        .store(u64::from(cfg.initial_bps), Ordering::Relaxed);

    // How the set point reaches the MFT (SPIKE_RATE_API): `codecapi` (ICodecAPI::SetValue of
    // MeanBitRate), `event` (MEEncodingParameters via ProcessEvent), `both`, or `recreate` (a new
    // encoder at the new rate; the next picture is an IDR). Default `auto`: `recreate` on Intel,
    // whose MFT accepts the runtime MeanBitRate without effect and refuses the event (E_NOTIMPL,
    // measured), `codecapi` elsewhere.
    let mut rate_api = std::env::var("SPIKE_RATE_API").unwrap_or_else(|_| "auto".into());
    if rate_api == "auto" {
        rate_api = if cfg.vendor == 0x8086 {
            "recreate"
        } else {
            "codecapi"
        }
        .into();
    }
    report.rate_api = rate_api.clone();
    let period = 10_000_000i64 / i64::from(cfg.fps);
    let mut need = 0u32;
    let mut pending: Option<(usize, i64)> = None; // ring index, capture time
    let mut inflight: HashMap<i64, (u64, i64)> = HashMap::new(); // sample time -> (id, cap)
    let mut last_accepted: Option<i64> = None;
    let mut t0: Option<i64> = None;
    let mut next_id = 0u64;
    let mut cur_bps = cfg.initial_bps;
    let mut last_change = Instant::now();

    let result = (|| -> Result<()> {
        while !m.stop.load(Ordering::Relaxed) {
            while let Some(ev) = enc.next_event(Duration::ZERO) {
                match ev {
                    Event::NeedInput(_) => need += 1,
                    Event::HaveOutput(t) => {
                        let Some(pkt) = enc.take_output()? else {
                            continue;
                        };
                        report.frames_out += 1;
                        let Some((id, cap_t)) = inflight.remove(&pkt.time) else {
                            continue;
                        };
                        let key = au::is_idr(&pkt.data);
                        if key {
                            report.idrs += 1;
                        }
                        let data = au::insert_sei(&pkt.data, &au::sei_nal(id, cap_t));
                        m.with_frame(id, |f| {
                            f.enc = Some(t);
                            f.bytes = pkt.data.len();
                            f.key = key;
                        });
                        if tx
                            .try_send(EncodedFrame {
                                id,
                                cap: cap_t,
                                data,
                            })
                            .is_err()
                        {
                            m.tx_dropped.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Event::DrainComplete(_) => {}
                    Event::Error(e) => bail!("encoder event: {e}"),
                    Event::Other(_) => {}
                }
            }

            // Follow the congestion controller.
            let want = (m.gcc_target.get() * cfg.headroom)
                .clamp(f64::from(cfg.min_bps), f64::from(cfg.max_bps))
                as u32;
            let moved = (f64::from(want) - f64::from(cur_bps)) / f64::from(cur_bps);
            let since = last_change.elapsed();
            let due = if rate_api == "recreate" {
                // Each re-creation costs an IDR: act at once on a cut, rarely on a rise.
                (moved < -0.15 && since >= Duration::from_millis(100))
                    || (moved > 0.25 && since >= Duration::from_millis(1500))
            } else {
                moved.abs() > 0.05 && since >= Duration::from_millis(100)
            };
            if due {
                let r = match rate_api.as_str() {
                    "event" => enc.set_bitrate_event(want),
                    "both" => enc
                        .set_bitrate(want)
                        .and_then(|()| enc.set_bitrate_event(want)),
                    "recreate" => {
                        let t = Instant::now();
                        ecfg.bitrate = want;
                        // A fresh activation object: a shut-down IMFActivate cannot be reused.
                        let fresh =
                            mft::hardware_encoders(Codec::H264, Some(d3d::device_luid(&device)?))?;
                        let act = fresh
                            .iter()
                            .find(|e| e.clsid == clsid)
                            .or(fresh.first())
                            .context("encoder MFT gone")?;
                        drop(std::mem::replace(
                            &mut enc,
                            Encoder::new(&act.activate, &device, &ecfg)?,
                        ));
                        report.recreated_dropped += inflight.len() as u64;
                        inflight.clear();
                        need = 0;
                        report.recreate_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                        Ok(())
                    }
                    _ => enc.set_bitrate(want),
                };
                match r {
                    Ok(()) => {
                        cur_bps = want;
                        m.enc_setpoint.store(u64::from(want), Ordering::Relaxed);
                        m.enc_changes
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push((now_100ns(), want));
                    }
                    Err(e) => {
                        if report.set_bitrate_errors.len() < 3 {
                            report.set_bitrate_errors.push(format!("{e:#}"));
                        }
                    }
                }
                last_change = Instant::now();
            }

            if let Some(frame) = cap.recv_timeout(Duration::from_millis(2)) {
                let srt = frame.system_relative_time_100ns();
                let due = last_accepted.is_none_or(|l| srt - l >= period * 8 / 10);
                if due && m.go.load(Ordering::Relaxed) && inflight.len() + 2 < RING {
                    last_accepted = Some(srt);
                    let (fw, fh) = frame.size();
                    report.source_size = (fw, fh);
                    let c = conv.convert(frame.texture(), fw, fh)?;
                    drop(frame);
                    pending = Some((c.index, srt));
                }
            }
            if need > 0
                && let Some((idx, srt)) = pending.take()
            {
                if m.force_key.swap(false, Ordering::Relaxed) {
                    let _ = enc.force_keyframe();
                }
                let time = srt - *t0.get_or_insert(srt);
                let tex = conv.ring()[idx].clone();
                let id = next_id;
                next_id += 1;
                inflight.insert(time, (id, srt));
                m.with_frame(id, |f| f.cap = srt);
                enc.feed(&tex, time, period)?;
                need -= 1;
                report.frames_in += 1;
            }
        }
        Ok(())
    })();
    drop(enc);
    cap.stop();
    win.quit();
    result.map(|()| report)
}
