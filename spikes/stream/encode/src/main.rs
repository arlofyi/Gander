// SPDX-License-Identifier: Apache-2.0
//! `spike-encode`: captures a `gander-testwin` window with WGC, converts each frame to NV12 with
//! the D3D11 video processor and encodes it with the Media Foundation hardware encoder of each
//! adapter (H.264 and HEVC), at 1080x2280 and 1920x1080. Prints a ms/frame and bitrate table and
//! writes Annex-B dumps.
//!
//! ```text
//! cargo run --release -p spike-encode                 # full matrix, about 2 minutes
//! cargo run --release -p spike-encode -- --wp43       # only the WP-43 dump (iGPU, H.264, 1080x2280, 5 s, 4 Mbps)
//! cargo run --release -p spike-encode -- --list       # adapters and encoder MFTs only
//!   [--secs N] [--fps N] [--mbps N] [--gop N] [--burst N] [--sizes 1080x2280,1920x1080]
//!   [--adapters 0,1] [--codecs h264,hevc] [--no-cross] [--no-timer] [--out DIR] [--testwin PATH]
//! ```
//!
//! Exit code: 0 when every run produced output that ffmpeg decodes (or ffmpeg is absent), 1 on
//! any failed run, 2 usage error.

#[cfg(windows)]
mod testwin;

/// NVIDIA Optimus: asks the driver to treat this process as a "high performance" (dGPU) app.
/// Exported by `build.rs`. See the findings doc (WP-42) for why the NVIDIA encoder MFT needs it.
#[cfg(windows)]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static NvOptimusEnablement: u32 = 1;

#[cfg(not(windows))]
fn main() {
    eprintln!("spike-encode needs Windows (Media Foundation, Windows Graphics Capture)");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    match win::run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("spike-encode: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use spike_capture::{d3d, wgc};
    use spike_encode::annexb;
    use spike_encode::convert::Converter;
    use spike_encode::encoder::{Config, Encoder, Event};
    use spike_encode::mft::{self, Codec, EncoderInfo};
    use spike_encode::xadapter::{self, CrossCopy};
    use windows::Win32::Graphics::Direct3D11::ID3D11Device;

    use crate::testwin::{self, Testwin};

    const RING: usize = 8;

    struct Args {
        secs: f64,
        fps: u32,
        mbps: f64,
        gop: Option<u32>,
        burst: u32,
        sizes: Vec<(u32, u32)>,
        adapters: Option<Vec<usize>>,
        codecs: Vec<Codec>,
        cross: bool,
        out: PathBuf,
        testwin: Option<PathBuf>,
        list: bool,
        wp43: bool,
        timer_1ms: bool,
        low_latency: bool,
    }

    fn usage() -> ! {
        eprintln!(
            "usage: spike-encode [--list | --wp43] [--secs N] [--fps N] [--mbps N] [--gop N] \
             [--burst N] [--sizes WxH,..] [--adapters I,..] [--codecs h264,hevc] [--no-cross] \
             [--out DIR] [--testwin PATH]"
        );
        std::process::exit(2);
    }

    fn default_out() -> PathBuf {
        // The workspace target directory: <repo>/target/spike-encode.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/spike-encode")
    }

    fn parse_args() -> Args {
        let mut a = Args {
            secs: 6.0,
            fps: 30,
            mbps: 6.0,
            gop: None,
            burst: 90,
            sizes: vec![(1080, 2280), (1920, 1080)],
            adapters: None,
            codecs: vec![Codec::H264, Codec::Hevc],
            cross: true,
            out: default_out(),
            testwin: None,
            list: false,
            wp43: false,
            timer_1ms: true,
            low_latency: true,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            let mut val = || it.next().unwrap_or_else(|| usage());
            match arg.as_str() {
                "--secs" => a.secs = val().parse().unwrap_or_else(|_| usage()),
                "--fps" => a.fps = val().parse().unwrap_or_else(|_| usage()),
                "--mbps" => a.mbps = val().parse().unwrap_or_else(|_| usage()),
                "--gop" => a.gop = Some(val().parse().unwrap_or_else(|_| usage())),
                "--burst" => a.burst = val().parse().unwrap_or_else(|_| usage()),
                "--sizes" => {
                    a.sizes = val()
                        .split(',')
                        .map(|s| {
                            let (w, h) = s.split_once('x').unwrap_or_else(|| usage());
                            let w: u32 = w.parse().unwrap_or_else(|_| usage());
                            let h: u32 = h.parse().unwrap_or_else(|_| usage());
                            if !w.is_multiple_of(2) || !h.is_multiple_of(2) {
                                usage();
                            }
                            (w, h)
                        })
                        .collect()
                }
                "--adapters" => {
                    a.adapters = Some(
                        val()
                            .split(',')
                            .map(|s| s.parse().unwrap_or_else(|_| usage()))
                            .collect(),
                    )
                }
                "--codecs" => {
                    a.codecs = val()
                        .split(',')
                        .map(|s| Codec::parse(s).unwrap_or_else(|| usage()))
                        .collect()
                }
                "--no-cross" => a.cross = false,
                "--no-timer" => a.timer_1ms = false,
                "--no-low-latency" => a.low_latency = false,
                "--out" => a.out = PathBuf::from(val()),
                "--testwin" => a.testwin = Some(PathBuf::from(val())),
                "--list" => a.list = true,
                "--wp43" => a.wp43 = true,
                _ => usage(),
            }
        }
        if a.wp43 {
            a.secs = 5.0;
            a.mbps = 4.0;
            a.sizes = vec![(1080, 2280)];
            a.adapters = None; // the Intel iGPU, selected by vendor in run_inner
            a.codecs = vec![Codec::H264];
            a.cross = false;
            a.burst = 0;
        }
        a
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct Stats {
        p50: f64,
        p95: f64,
        max: f64,
    }

    fn stats(v: &[f64]) -> Stats {
        if v.is_empty() {
            return Stats::default();
        }
        let mut s = v.to_vec();
        s.sort_by(f64::total_cmp);
        let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
        Stats {
            p50: at(0.5),
            p95: at(0.95),
            max: s[s.len() - 1],
        }
    }

    #[derive(Debug, Default)]
    struct Row {
        item: &'static str,
        capture_adapter: String,
        encode_adapter: String,
        codec: &'static str,
        encoder: String,
        size: (u32, u32),
        source: (u32, u32),
        profile: String,
        frames_in: usize,
        frames_out: usize,
        skipped_busy: usize,
        fps: f64,
        convert: Stats,
        copy: Option<Stats>,
        latency: Stats,
        burst_ms: Option<f64>,
        burst_latency: Stats,
        mbps: f64,
        idrs: usize,
        param_sets_first: bool,
        forced_idr: Option<bool>,
        applied: String,
        dump: Option<PathBuf>,
        dump_bytes: u64,
        ffmpeg: Option<String>,
        ffprobe: Option<String>,
        error: Option<String>,
    }

    struct Session<'a> {
        args: &'a Args,
        capture: &'a wgc::Capture,
        cap_dev: &'a ID3D11Device,
        enc_dev: &'a ID3D11Device,
        cross: bool,
        info: &'a EncoderInfo,
        codec: Codec,
        size: (u32, u32),
        dump: PathBuf,
    }

    #[derive(PartialEq, Eq, Clone, Copy, Debug)]
    enum Phase {
        Paced,
        Burst,
        Draining,
    }

    fn ms(a: i64, b: i64) -> f64 {
        (b - a) as f64 / 10_000.0
    }

    fn run_session(s: &Session<'_>, row: &mut Row) -> Result<()> {
        let a = s.args;
        let (w, h) = s.size;
        let period = 10_000_000i64 / i64::from(a.fps);
        let mut cfg = Config::new(s.codec, w, h, a.fps, (a.mbps * 1_000_000.0) as u32);
        if let Some(g) = a.gop {
            cfg.gop = g;
        }
        cfg.low_latency = a.low_latency;
        let mut conv = Converter::new(s.enc_dev, w, h, a.fps, RING)?;
        let mut xcopy = if s.cross {
            Some(CrossCopy::new(s.cap_dev, s.enc_dev)?)
        } else {
            None
        };
        // A fresh activation object per session: an IMFActivate whose activation failed (or that
        // was shut down) cannot be activated again.
        let fresh = mft::hardware_encoders(s.codec, Some(d3d::device_luid(s.enc_dev)?))?;
        let activate = fresh
            .iter()
            .find(|e| e.clsid == s.info.clsid)
            .map_or(&s.info.activate, |e| &e.activate);
        let mut enc = Encoder::new(activate, s.enc_dev, &cfg)?;
        row.profile = enc.profile.clone();
        row.applied = enc
            .applied
            .iter()
            .map(|(k, r)| match r {
                Ok(()) => format!("{k}=ok"),
                Err(e) => format!("{k}=ERR {e}"),
            })
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(parent) = s.dump.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut dump = File::create(&s.dump).with_context(|| s.dump.display().to_string())?;
        row.dump = Some(s.dump.clone());

        s.capture.drain();
        let mut need = 0u32;
        let mut pending: Option<(usize, i64, i64)> = None; // ring index, sample time, arrival
        let mut inflight: HashMap<i64, i64> = HashMap::new(); // sample time -> input QPC
        let mut paced_times: Vec<i64> = Vec::new();
        let (mut convert_ms, mut copy_ms, mut lat, mut burst_lat) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut t0: Option<i64> = None;
        let mut last_accepted: Option<i64> = None;
        let mut last_time = 0i64;
        let force_at = (a.fps * 3 / 2) as usize; // between GOP boundaries (0, 2 s, 4 s ...)
        let mut forced_time: Option<i64> = None;
        let mut burst_start: Option<i64> = None;
        let mut burst_first_in: Option<i64> = None;
        let mut burst_last_out: Option<i64> = None;
        let mut burst_fed = 0u32;
        let (mut paced_bytes, mut paced_first, mut paced_last) = (0u64, None::<i64>, 0i64);
        let mut first_packet = true;
        let start = Instant::now();
        let mut phase = Phase::Paced;
        let mut drain_deadline = Instant::now();
        let mut drained = false;

        loop {
            // 1. Events (block only when there is nothing else to do).
            let wait = match phase {
                Phase::Paced => Duration::ZERO,
                Phase::Burst if need > 0 && burst_fed < a.burst => Duration::ZERO,
                _ => Duration::from_millis(100),
            };
            let mut first = true;
            while let Some(ev) = enc.next_event(if first { wait } else { Duration::ZERO }) {
                first = false;
                match ev {
                    Event::NeedInput(_) => need += 1,
                    Event::HaveOutput(t) => {
                        let Some(pkt) = enc.take_output()? else {
                            continue;
                        };
                        row.frames_out += 1;
                        let sum = annexb::summarize(s.codec.bitstream(), &pkt.data);
                        if first_packet {
                            row.param_sets_first = sum.sps && sum.pps && sum.idr;
                            anyhow::ensure!(
                                annexb::starts_with_start_code(&pkt.data),
                                "first packet is not Annex-B"
                            );
                            first_packet = false;
                        }
                        if sum.idr {
                            row.idrs += 1;
                        }
                        if Some(pkt.time) == forced_time {
                            row.forced_idr = Some(sum.idr);
                        }
                        let fed = inflight.remove(&pkt.time);
                        let is_burst = burst_start.is_some_and(|b| pkt.time >= b);
                        if is_burst {
                            burst_last_out = Some(t);
                            if let Some(i) = fed {
                                burst_lat.push(ms(i, t));
                            }
                        } else {
                            if let Some(i) = fed {
                                lat.push(ms(i, t));
                            }
                            dump.write_all(&pkt.data)?;
                            row.dump_bytes += pkt.data.len() as u64;
                            paced_bytes += pkt.data.len() as u64;
                            paced_first.get_or_insert(pkt.time);
                            paced_last = pkt.time;
                        }
                    }
                    Event::DrainComplete(_) => {
                        anyhow::ensure!(phase == Phase::Draining, "unexpected drain complete");
                        // The event thread ends after this event: stop reading the channel.
                        drained = true;
                        break;
                    }
                    Event::Error(e) => bail!("encoder event: {e}"),
                    Event::Other(_) => {}
                }
            }
            match phase {
                Phase::Paced => {
                    if let Some(frame) = s.capture.recv_timeout(Duration::from_millis(2)) {
                        let srt = frame.system_relative_time_100ns();
                        let due = last_accepted.is_none_or(|l| srt - l >= period * 8 / 10);
                        if due {
                            last_accepted = Some(srt);
                            if inflight.len() + 2 >= RING {
                                row.skipped_busy += 1;
                            } else {
                                let (fw, fh) = frame.size();
                                row.source = (fw, fh);
                                let c = if let Some(x) = xcopy.as_mut() {
                                    let (tex, cms) = x.copy(frame.texture(), fw, fh)?;
                                    drop(frame);
                                    copy_ms.push(cms);
                                    conv.convert(&tex, fw, fh)?
                                } else {
                                    let c = conv.convert(frame.texture(), fw, fh)?;
                                    drop(frame);
                                    c
                                };
                                convert_ms.push(c.ms);
                                let base = *t0.get_or_insert(srt);
                                if pending.is_some() {
                                    row.skipped_busy += 1;
                                }
                                pending = Some((c.index, srt - base, clock_now()));
                            }
                        }
                    }
                    if need > 0
                        && let Some((idx, time, _arrival)) = pending.take()
                    {
                        if row.frames_in == force_at {
                            enc.force_keyframe()?;
                            forced_time = Some(time);
                        }
                        let tex = conv.ring()[idx].clone();
                        inflight.insert(time, clock_now());
                        enc.feed(&tex, time, period)?;
                        need -= 1;
                        row.frames_in += 1;
                        paced_times.push(time);
                        last_time = time;
                    }
                    if start.elapsed().as_secs_f64() >= a.secs {
                        if a.burst > 0 && row.frames_in >= RING {
                            phase = Phase::Burst;
                            burst_start = Some(last_time + period);
                        } else {
                            enc.drain()?;
                            phase = Phase::Draining;
                            drain_deadline = Instant::now() + Duration::from_secs(5);
                        }
                    }
                }
                Phase::Burst => {
                    if need > 0 && burst_fed < a.burst {
                        // Re-feed the last RING converted frames as fast as the encoder takes
                        // them: throughput, independent of the 60 Hz source.
                        let tex = conv.ring()[burst_fed as usize % RING].clone();
                        last_time += period;
                        let now = clock_now();
                        burst_first_in.get_or_insert(now);
                        inflight.insert(last_time, now);
                        enc.feed(&tex, last_time, period)?;
                        need -= 1;
                        burst_fed += 1;
                    } else if burst_fed >= a.burst {
                        enc.drain()?;
                        phase = Phase::Draining;
                        drain_deadline = Instant::now() + Duration::from_secs(5);
                    }
                }
                Phase::Draining => {
                    if drained {
                        break;
                    }
                    anyhow::ensure!(
                        Instant::now() < drain_deadline,
                        "no METransformDrainComplete within 5 s"
                    );
                }
            }
            anyhow::ensure!(
                start.elapsed().as_secs_f64() < a.secs + 30.0,
                "session did not finish (encoder stalled?)"
            );
        }
        drop(dump);
        drop(enc);
        if let Some(f) = paced_first {
            let secs = (paced_last - f + period) as f64 / 1e7;
            row.mbps = paced_bytes as f64 * 8.0 / secs / 1e6;
        }
        if paced_times.len() > 1 {
            let span = (paced_times[paced_times.len() - 1] - paced_times[0]) as f64 / 1e7;
            row.fps = (paced_times.len() - 1) as f64 / span;
        }
        if let (Some(i), Some(o)) = (burst_first_in, burst_last_out)
            && burst_fed > 0
        {
            row.burst_ms = Some(ms(i, o) / f64::from(burst_fed));
        }
        row.convert = stats(&convert_ms);
        row.copy = xcopy.map(|_| stats(&copy_ms));
        row.latency = stats(&lat);
        row.burst_latency = stats(&burst_lat);
        Ok(())
    }

    fn clock_now() -> i64 {
        spike_capture::clock::now_100ns()
    }

    fn ffmpeg_check(path: &Path) -> (Option<String>, Option<String>) {
        let ff = Command::new("ffmpeg")
            .args(["-hide_banner", "-v", "error", "-i"])
            .arg(path)
            .args(["-f", "null", "-"])
            .output();
        let ff = ff.ok().map(|o| {
            let err = String::from_utf8_lossy(&o.stderr);
            let first = err.lines().next().unwrap_or("").trim().to_owned();
            format!(
                "exit {}{}",
                o.status.code().unwrap_or(-1),
                if first.is_empty() {
                    String::new()
                } else {
                    format!(" ({first})")
                }
            )
        });
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,profile,level,width,height,nb_read_frames",
                "-of",
                "compact=p=0",
            ])
            .arg(path)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        (ff, probe)
    }

    fn adapter_short(name: &str) -> String {
        if name.contains("Intel") {
            "iGPU".into()
        } else if name.contains("NVIDIA") {
            "dGPU".into()
        } else {
            name.chars().take(10).collect()
        }
    }

    pub fn run() -> Result<i32> {
        let args = parse_args();
        testwin::become_dpi_aware();
        mft::startup()?;
        // The hardware MFTs wait for the GPU with short sleeps; at the default 15.6 ms timer
        // resolution every frame costs a whole tick. timeBeginPeriod is per process on
        // Windows 10 2004 and later, so this does not change machine state.
        let timer = args.timer_1ms;
        if timer {
            // SAFETY: plain call; undone by timeEndPeriod before exit.
            unsafe { windows::Win32::Media::timeBeginPeriod(1) };
        }
        let r = run_inner(&args);
        if timer {
            // SAFETY: matches the timeBeginPeriod above.
            unsafe { windows::Win32::Media::timeEndPeriod(1) };
        }
        r
    }

    fn run_inner(args: &Args) -> Result<i32> {
        println!(
            "timer resolution: {}",
            if args.timer_1ms {
                "1 ms (timeBeginPeriod)"
            } else {
                "default (15.6 ms)"
            }
        );
        let adapters = d3d::adapters()?;
        println!("# spike-encode (WP-42)");
        for a in &adapters {
            println!(
                "adapter {}: {} (VEN {:04X} DEV {:04X}) LUID 0x{:X} driver {} outputs [{}]",
                a.info.index,
                a.info.name,
                a.info.vendor_id,
                a.info.device_id,
                a.info.luid,
                a.info.driver_version.as_deref().unwrap_or("?"),
                a.info
                    .outputs
                    .iter()
                    .map(|o| o.device_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        // Encoder MFTs: unfiltered, then per adapter.
        let mut per_adapter: Vec<HashMap<&'static str, Vec<EncoderInfo>>> = Vec::new();
        for codec in [Codec::H264, Codec::Hevc] {
            let all = mft::hardware_encoders(codec, None).unwrap_or_default();
            println!(
                "hardware {} encoder MFTs (no adapter filter): {}",
                codec.name(),
                all.len()
            );
            for e in &all {
                println!(
                    "  - {} | clsid {} | vendor {} | luid {} | url {}",
                    e.name,
                    e.clsid.map_or("-".into(), |g| format!("{g:?}")),
                    e.vendor.as_deref().unwrap_or("-"),
                    e.luid.map_or("-".into(), |l| format!("0x{l:X}")),
                    e.url.as_deref().unwrap_or("-")
                );
            }
        }
        for a in &adapters {
            let mut m = HashMap::new();
            for codec in [Codec::H264, Codec::Hevc] {
                let list = match mft::hardware_encoders(codec, Some(a.info.luid)) {
                    Ok(l) => l,
                    Err(e) => {
                        println!(
                            "adapter {} {}: MFTEnum2 failed: {e:#}",
                            a.info.index,
                            codec.name()
                        );
                        Vec::new()
                    }
                };
                println!(
                    "adapter {} ({}) {}: {}",
                    a.info.index,
                    adapter_short(&a.info.name),
                    codec.name(),
                    if list.is_empty() {
                        "none".to_owned()
                    } else {
                        list.iter()
                            .map(|e| format!("{} [{}]", e.name, e.vendor.as_deref().unwrap_or("-")))
                            .collect::<Vec<_>>()
                            .join("; ")
                    }
                );
                m.insert(codec.name(), list);
            }
            per_adapter.push(m);
        }
        if args.list {
            return Ok(0);
        }

        let use_adapters: Vec<usize> = match (&args.adapters, args.wp43) {
            (_, true) => adapters
                .iter()
                .position(|a| a.info.vendor_id == 0x8086)
                .into_iter()
                .collect(),
            (Some(v), false) => v.clone(),
            (None, false) => (0..adapters.len()).collect(),
        };
        let devices: Vec<ID3D11Device> = adapters
            .iter()
            .map(|a| d3d::create_device(Some(&a.adapter)))
            .collect::<Result<_>>()?;
        for &i in &use_adapters {
            anyhow::ensure!(i < adapters.len(), "no adapter {i}");
            anyhow::ensure!(
                d3d::has_video_support(&devices[i]),
                "adapter {i} device has no VIDEO_SUPPORT"
            );
        }

        // C12: zero-copy sharing between the two adapters?
        let mut shared_notes = Vec::new();
        if args.cross && adapters.len() > 1 {
            for (s, d) in [(0usize, 1usize), (1, 0)] {
                let note = xadapter::try_shared_handle(&devices[s], &devices[d]);
                println!("shared texture adapter {s} -> {d}: {note}");
                shared_notes.push(note);
            }
        }

        let exe = testwin::exe(args.testwin.clone())?;
        let win = Testwin::spawn(&exe, "spike-encode", 900)?;
        let mut rows: Vec<Row> = Vec::new();
        let result = (|| -> Result<()> {
            for &(w, h) in &args.sizes {
                let work = testwin::work_area(win.hwnd);
                let (ow, oh) = testwin::fit(work, w, h, 16);
                testwin::place(win.hwnd, work.left + 16, work.top + 16, ow, oh)?;
                std::thread::sleep(Duration::from_millis(500));
                println!(
                    "\n## {w}x{h}: test window outer {ow}x{oh} (work area {}x{}), scaled by the video processor",
                    work.right - work.left,
                    work.bottom - work.top
                );
                // Same-adapter runs (capture device = encode device).
                for &ai in &use_adapters {
                    let cap = wgc::Capture::start(
                        win.hwnd,
                        &devices[ai],
                        &wgc::CaptureOptions::default(),
                    )?;
                    for &codec in &args.codecs {
                        let Some(info) = per_adapter[ai][codec.name()].first() else {
                            rows.push(Row {
                                item: "C1",
                                capture_adapter: adapter_short(&adapters[ai].info.name),
                                encode_adapter: adapter_short(&adapters[ai].info.name),
                                codec: codec.name(),
                                size: (w, h),
                                error: Some("no hardware MFT on this adapter".into()),
                                ..Default::default()
                            });
                            continue;
                        };
                        let short = adapter_short(&adapters[ai].info.name);
                        let dump = args.out.join(format!(
                            "{}-{}-{w}x{h}.{}",
                            short.to_lowercase(),
                            codec.name(),
                            codec.name()
                        ));
                        let mut row = Row {
                            item: "C1",
                            capture_adapter: short.clone(),
                            encode_adapter: short.clone(),
                            codec: codec.name(),
                            encoder: info.name.clone(),
                            size: (w, h),
                            ..Default::default()
                        };
                        let s = Session {
                            args,
                            capture: &cap,
                            cap_dev: &devices[ai],
                            enc_dev: &devices[ai],
                            cross: false,
                            info,
                            codec,
                            size: (w, h),
                            dump,
                        };
                        if let Err(e) = run_session(&s, &mut row) {
                            row.error = Some(format!("{e:#}"));
                        }
                        print_row(&row);
                        rows.push(row);
                    }
                    cap.stop();
                }
                // C12: capture on one adapter, encode (H.264) on the other, CPU copy between.
                if args.cross && use_adapters.len() > 1 {
                    for (ci, ei) in [
                        (use_adapters[0], use_adapters[1]),
                        (use_adapters[1], use_adapters[0]),
                    ] {
                        let Some(info) = per_adapter[ei]["h264"].first() else {
                            continue;
                        };
                        let cap = wgc::Capture::start(
                            win.hwnd,
                            &devices[ci],
                            &wgc::CaptureOptions::default(),
                        )?;
                        let (cs, es) = (
                            adapter_short(&adapters[ci].info.name),
                            adapter_short(&adapters[ei].info.name),
                        );
                        let mut row = Row {
                            item: "C12",
                            capture_adapter: cs.clone(),
                            encode_adapter: es.clone(),
                            codec: "h264",
                            encoder: info.name.clone(),
                            size: (w, h),
                            ..Default::default()
                        };
                        let s = Session {
                            args,
                            capture: &cap,
                            cap_dev: &devices[ci],
                            enc_dev: &devices[ei],
                            cross: true,
                            info,
                            codec: Codec::H264,
                            size: (w, h),
                            dump: args.out.join(format!(
                                "cross-{}-to-{}-h264-{w}x{h}.h264",
                                cs.to_lowercase(),
                                es.to_lowercase()
                            )),
                        };
                        if let Err(e) = run_session(&s, &mut row) {
                            row.error = Some(format!("{e:#}"));
                        }
                        print_row(&row);
                        rows.push(row);
                        cap.stop();
                    }
                }
            }
            Ok(())
        })();
        win.quit();
        result?;

        // Decode check with ffmpeg (Verify) and stream facts with ffprobe.
        for r in rows.iter_mut() {
            if let (Some(d), None) = (&r.dump, &r.error) {
                let (ff, probe) = ffmpeg_check(d);
                r.ffmpeg = ff;
                r.ffprobe = probe;
            }
        }
        print_table(args, &rows);
        if args.wp43
            && let Some(r) = rows.first()
            && let Some(d) = &r.dump
        {
            anyhow::ensure!(
                r.ffmpeg.as_deref().is_some_and(|f| f.starts_with("exit 0")),
                "WP-43 dump does not decode: {:?}",
                r.ffmpeg
            );
            let canonical = args.out.join("wp43.h264");
            std::fs::copy(d, &canonical)?;
            println!(
                "\nWP-43 dump: {} ({} bytes)",
                canonical.display(),
                r.dump_bytes
            );
            // A committed copy for WP-43, only if it is small.
            if r.dump_bytes < 5 * 1024 * 1024 {
                let results = Path::new(env!("CARGO_MANIFEST_DIR")).join("results");
                std::fs::create_dir_all(&results)?;
                let committed = results.join("wp43-h264-1080x2280.h264");
                std::fs::copy(d, &committed)?;
                println!("WP-43 dump (committed copy): {}", committed.display());
            }
        }
        let failed = rows.iter().any(|r| {
            r.error.is_some()
                && !r
                    .error
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("no hardware MFT")
                || r.ffmpeg
                    .as_deref()
                    .is_some_and(|f| !f.starts_with("exit 0"))
        });
        Ok(i32::from(failed))
    }

    fn print_row(r: &Row) {
        match &r.error {
            Some(e) => println!(
                "{} {}->{} {} {}x{}: ERROR {e}",
                r.item, r.capture_adapter, r.encode_adapter, r.codec, r.size.0, r.size.1
            ),
            None => println!(
                "{} {}->{} {} {}x{} [{}; profile {}]: in {} out {} fps {:.1} | latency p50 {:.2} p95 {:.2} max {:.2} ms | convert p50 {:.2} ms{} | burst {} | {:.2} Mbps | IDR {} forced {:?} | {}",
                r.item,
                r.capture_adapter,
                r.encode_adapter,
                r.codec,
                r.size.0,
                r.size.1,
                r.encoder,
                r.profile,
                r.frames_in,
                r.frames_out,
                r.fps,
                r.latency.p50,
                r.latency.p95,
                r.latency.max,
                r.convert.p50,
                r.copy.map_or(String::new(), |c| format!(
                    " | x-copy p50 {:.2} p95 {:.2} ms",
                    c.p50, c.p95
                )),
                r.burst_ms
                    .map_or("-".into(), |b| format!("{b:.2} ms/frame")),
                r.mbps,
                r.idrs,
                r.forced_idr,
                r.applied
            ),
        }
    }

    fn print_table(a: &Args, rows: &[Row]) {
        println!(
            "\n## ms/frame and bitrate ({} fps target, {} Mbps CBR, GOP {}, {} s paced + {} burst frames)\n",
            a.fps,
            a.mbps,
            a.gop.unwrap_or(a.fps * 2),
            a.secs,
            a.burst
        );
        println!(
            "| Item | Capture -> encode | Codec | Size | Source | Encoder MFT | Profile | fps | Encode latency p50 / p95 / max ms | Burst ms/frame | Convert p50 ms | Cross copy p50 / p95 ms | Mbps | IDRs (forced ok) | ffmpeg | ffprobe |"
        );
        println!(
            "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
        );
        for r in rows {
            if let Some(e) = &r.error {
                println!(
                    "| {} | {} -> {} | {} | {}x{} | | | | | ERROR: {} | | | | | | | |",
                    r.item, r.capture_adapter, r.encode_adapter, r.codec, r.size.0, r.size.1, e
                );
                continue;
            }
            println!(
                "| {} | {} -> {} | {} | {}x{} | {}x{} | {} | {} | {:.1} | {:.2} / {:.2} / {:.2} | {} | {:.2} | {} | {:.2} | {} ({}) | {} | {} |",
                r.item,
                r.capture_adapter,
                r.encode_adapter,
                r.codec,
                r.size.0,
                r.size.1,
                r.source.0,
                r.source.1,
                r.encoder,
                r.profile,
                r.fps,
                r.latency.p50,
                r.latency.p95,
                r.latency.max,
                r.burst_ms.map_or("-".into(), |b| format!(
                    "{b:.2} (lat p50 {:.1})",
                    r.burst_latency.p50
                )),
                r.convert.p50,
                r.copy
                    .map_or("-".into(), |c| format!("{:.2} / {:.2}", c.p50, c.p95)),
                r.mbps,
                r.idrs,
                r.forced_idr.map_or("-", |f| if f { "yes" } else { "NO" }),
                r.ffmpeg.as_deref().unwrap_or("-"),
                r.ffprobe.as_deref().unwrap_or("-"),
            );
        }
        let skipped: usize = rows.iter().map(|r| r.skipped_busy).sum();
        let ps = rows
            .iter()
            .filter(|r| r.error.is_none())
            .all(|r| r.param_sets_first);
        println!(
            "\nframes skipped because the encoder was busy: {skipped}; first packet carries parameter sets + IDR in every run: {ps}"
        );
        for r in rows.iter().filter(|r| r.error.is_none()) {
            if let Some(d) = &r.dump {
                println!("dump {} ({} bytes)", d.display(), r.dump_bytes);
            }
        }
    }
}
