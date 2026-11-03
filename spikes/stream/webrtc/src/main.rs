// SPDX-License-Identifier: Apache-2.0
//! `spike-webrtc` (WP-43): the WP-42 hardware H.264 stream of a `gander-testwin` window sent as
//! a WebRTC video track with webrtc-rs 0.21, received and decoded by a loopback viewer in the
//! same process, with GCC driving the encoder bitrate and an in-process UDP relay throttling the
//! link on a schedule.
//!
//! ```text
//! cargo run --release -p spike-webrtc                      # 50 s, throttle 1.5 Mbps at 20-35 s
//!   [--secs N] [--steps T:KBPS,...] (KBPS 0 = no cap) [--no-relay] [--vendor intel|nvidia]
//!   [--size WxH] [--fps N] [--gop N] [--gcc-initial-kbps N] [--gcc-min-kbps N]
//!   [--gcc-max-kbps N] [--headroom F] [--queue-ms N] [--nack] [--stack webrtc|str0m]
//!   [--out DIR] [--results DIR] [--label S]
//!
//! `--stack str0m` needs the str0m host from `spikes/stream/webrtc/str0m/` compiled in (it is
//! left out of the default build; see that file).
//! ```
//!
//! Network: ICE binds 127.0.0.1 only (host candidates, no STUN/TURN, no mDNS); signalling is a
//! plain WebSocket on 127.0.0.1; the relay binds two 127.0.0.1 sockets. Nothing listens on a
//! wildcard address and nothing leaves the machine.
//!
//! Exit code: 0 when frames were decoded and, for every throttle step, the encoder set point was
//! cut and the encoded bitrate fell under the cap within 5 s; 1 otherwise; 2 usage error.
#![cfg_attr(not(windows), allow(dead_code))]

/// NVIDIA Optimus opt-in (exported by build.rs); needed for the NVIDIA encoder MFT (WP-42).
#[cfg(windows)]
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static NvOptimusEnablement: u32 = 1;

#[cfg(not(windows))]
fn main() {
    eprintln!("spike-webrtc needs Windows (WGC capture, Media Foundation encoder)");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    match win::run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("spike-webrtc: {e:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
mod win {
    use std::collections::VecDeque;
    use std::fmt::Write as _;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use tokio::sync::mpsc;

    use spike_webrtc::decode::Decoder;
    use spike_webrtc::metrics::{Metrics, Row, now_100ns, pct};
    use spike_webrtc::peer::{self, GccConfig, HostOptions, HostReport};
    use spike_webrtc::relay::Relay;
    use spike_webrtc::signal;
    use spike_webrtc::source::{self, SourceConfig, SourceReport};
    use spike_webrtc::testwin;

    #[derive(Debug, Clone)]
    struct Args {
        secs: f64,
        steps: Vec<(f64, u64)>,
        relay: bool,
        vendor: u32,
        width: u32,
        height: u32,
        fps: u32,
        gop: Option<u32>,
        gcc: GccConfig,
        headroom: f64,
        queue_ms: u64,
        nack: bool,
        out: PathBuf,
        results: Option<PathBuf>,
        label: String,
    }

    fn usage() -> ! {
        eprintln!(
            "usage: spike-webrtc [--secs N] [--steps T:KBPS,...] [--no-relay] [--vendor intel|nvidia] \
             [--size WxH] [--fps N] [--gop N] [--gcc-initial-kbps N] [--gcc-min-kbps N] \
             [--gcc-max-kbps N] [--headroom F] [--queue-ms N] [--nack] [--stack webrtc|str0m] [--out DIR] [--results DIR] [--label S]"
        );
        std::process::exit(2);
    }

    fn parse_args() -> Args {
        let mut a = Args {
            secs: 50.0,
            steps: vec![(20.0, 1500), (35.0, 0)],
            relay: true,
            vendor: 0x8086,
            width: 1080,
            height: 2280,
            fps: 30,
            gop: None,
            gcc: GccConfig {
                initial: 3_000_000.0,
                min: 300_000.0,
                max: 8_000_000.0,
            },
            headroom: 0.85,
            queue_ms: 200,
            nack: false,
            out: PathBuf::from("target/spike-webrtc"),
            results: None,
            label: "run".into(),
        };
        let mut it = std::env::args().skip(1);
        while let Some(k) = it.next() {
            let mut v = || it.next().unwrap_or_else(|| usage());
            let num = |s: String| s.parse::<f64>().unwrap_or_else(|_| usage());
            match k.as_str() {
                "--secs" => a.secs = num(v()),
                "--steps" => {
                    a.steps = v()
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| {
                            let (t, k) = s.split_once(':').unwrap_or_else(|| usage());
                            (num(t.into()), num(k.into()) as u64)
                        })
                        .collect();
                }
                "--no-relay" => a.relay = false,
                "--nack" => a.nack = true,
                "--stack" => match v().as_str() {
                    "webrtc" => {}
                    "str0m" => {
                        eprintln!(
                            "the str0m host is not built by default (str0m 0.24.1 pulls \
                             RUSTSEC-2026-0387 through dimpl, so cargo deny fails); see \
                             spikes/stream/webrtc/str0m/peer_str0m.rs to enable it"
                        );
                        std::process::exit(2);
                    }
                    _ => usage(),
                },
                "--vendor" => {
                    a.vendor = match v().as_str() {
                        "intel" => 0x8086,
                        "nvidia" => 0x10DE,
                        "amd" => 0x1002,
                        _ => usage(),
                    }
                }
                "--size" => {
                    let s = v();
                    let (w, h) = s.split_once('x').unwrap_or_else(|| usage());
                    a.width = num(w.into()) as u32;
                    a.height = num(h.into()) as u32;
                }
                "--fps" => a.fps = num(v()) as u32,
                "--gop" => a.gop = Some(num(v()) as u32),
                "--gcc-initial-kbps" => a.gcc.initial = num(v()) * 1000.0,
                "--gcc-min-kbps" => a.gcc.min = num(v()) * 1000.0,
                "--gcc-max-kbps" => a.gcc.max = num(v()) * 1000.0,
                "--headroom" => a.headroom = num(v()),
                "--queue-ms" => a.queue_ms = num(v()) as u64,
                "--out" => a.out = PathBuf::from(v()),
                "--results" => a.results = Some(PathBuf::from(v())),
                "--label" => a.label = v(),
                _ => usage(),
            }
        }
        if !a.relay && a.steps.iter().any(|s| s.1 > 0) {
            eprintln!("--no-relay: throttle steps ignored");
            a.steps.clear();
        }
        a
    }

    /// Prints webrtc-rs log records at or above the level in `SPIKE_LOG` (error, warn, info,
    /// debug, trace) to stderr.
    struct StderrLog(log::LevelFilter);

    impl log::Log for StderrLog {
        fn enabled(&self, md: &log::Metadata<'_>) -> bool {
            md.level() <= self.0
        }
        fn log(&self, r: &log::Record<'_>) {
            if self.enabled(r.metadata()) {
                eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }

    pub fn run() -> Result<i32> {
        let args = parse_args();
        if let Ok(level) = std::env::var("SPIKE_LOG")
            && let Ok(f) = level.parse::<log::LevelFilter>()
            && log::set_logger(Box::leak(Box::new(StderrLog(f)))).is_ok()
        {
            log::set_max_level(f);
        }
        testwin::become_dpi_aware();
        // Per process since Windows 10 2004 (WP-42 finding 2: the Intel MFT needs it).
        // SAFETY: plain call; undone below.
        unsafe { windows::Win32::Media::timeBeginPeriod(1) };
        let r = run_inner(&args);
        // SAFETY: matches the call above.
        unsafe { windows::Win32::Media::timeEndPeriod(1) };
        r
    }

    struct Outcome {
        host: HostReport,
        source: SourceReport,
        decoded: u64,
        wall_s: f64,
    }

    fn run_inner(a: &Args) -> Result<i32> {
        let m = Arc::new(Metrics::default());
        let (ftx, frx) = mpsc::channel(8);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let scfg = SourceConfig {
            width: a.width,
            height: a.height,
            fps: a.fps,
            vendor: a.vendor,
            gop: a.gop.unwrap_or(a.fps * 2),
            initial_bps: (a.gcc.initial * a.headroom) as u32,
            min_bps: (a.gcc.min * a.headroom) as u32,
            max_bps: (a.gcc.max * a.headroom) as u32,
            headroom: a.headroom,
            testwin_secs: a.secs as u32 + 120,
            title: "spike-webrtc".into(),
        };
        let src = {
            let m = m.clone();
            std::thread::Builder::new()
                .name("source".into())
                .spawn(move || source::run(&scfg, &m, &ftx, ready_tx))?
        };
        match ready_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = src.join();
                bail!("source setup: {e}");
            }
            Err(_) => {
                m.stop.store(true, Ordering::Relaxed);
                bail!("source setup timed out");
            }
        }

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let session = rt.block_on(session(a, m.clone(), frx));
        m.stop.store(true, Ordering::Relaxed);
        let source = match src.join() {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                eprintln!("source: {e:#}");
                SourceReport::default()
            }
            Err(_) => SourceReport::default(),
        };
        rt.shutdown_timeout(Duration::from_secs(2));
        let (host, decoder, wall_s) = session?;
        let decoded = decoder.finish(Duration::from_secs(5));
        let out = Outcome {
            host,
            source,
            decoded,
            wall_s,
        };
        report(a, &m, &out)
    }

    async fn session(
        a: &Args,
        m: Arc<Metrics>,
        frx: mpsc::Receiver<peer::EncodedFrame>,
    ) -> Result<(HostReport, Decoder, f64)> {
        let relay = if a.relay {
            Some(Arc::new(
                Relay::start(Duration::from_millis(a.queue_ms)).await?,
            ))
        } else {
            None
        };
        let listener = signal::listen().await?;
        let addr = listener.local_addr()?;
        let decoder = Decoder::start(m.clone())?;
        let (ctx, crx) = tokio::sync::oneshot::channel();
        let host = tokio::spawn(peer::run_host(
            listener,
            frx,
            m.clone(),
            relay.clone(),
            a.gcc,
            a.nack,
            HostOptions::default(),
            ctx,
        ));
        let viewer = tokio::spawn(peer::run_viewer(addr, m.clone(), decoder.feeder(), a.nack));
        let hr = match tokio::time::timeout(Duration::from_secs(30), crx).await {
            Ok(Ok(r)) => r,
            _ => {
                m.stop.store(true, Ordering::Relaxed);
                viewer.abort();
                let e = match tokio::time::timeout(Duration::from_secs(2), host).await {
                    Ok(Ok(Err(e))) => format!("{e:#}"),
                    _ => "no connection".into(),
                };
                bail!("connect: {e}");
            }
        };
        println!(
            "connected in {:.0} ms (signalling ws://{addr}/, relay {})",
            hr.connect_ms,
            relay
                .as_ref()
                .map_or("off".into(), |r| format!("A={} B={}", r.a, r.b))
        );
        m.t0.store(now_100ns() as u64, Ordering::Relaxed);
        m.go.store(true, Ordering::Relaxed);
        let start = Instant::now();

        let mut steps: VecDeque<(f64, u64)> = a.steps.iter().copied().collect();
        let mut rx_hist: VecDeque<(i64, u64)> = VecDeque::new();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tick.tick().await;
            let t = start.elapsed().as_secs_f64();
            while let Some(&(at, kbps)) = steps.front() {
                if t < at {
                    break;
                }
                steps.pop_front();
                if let Some(r) = &relay {
                    r.set_cap(kbps * 1000);
                    println!("t={t:5.2}s relay cap -> {}", cap_text(kbps));
                }
            }
            let row = sample(&m, relay.as_deref(), &mut rx_hist);
            println!(
                "t={:5.2}s gcc {:6.0} (delay {:6.0} loss {:6.0}) enc set {:6.0} out {:6.0} rx {:6.0} cap {:5} drops {:4} dec {:4.1} fps cap->recv p50 {:6.1} ms",
                row.t,
                row.gcc_kbps,
                row.gcc_delay_kbps,
                row.gcc_loss_kbps,
                row.enc_set_kbps,
                row.enc_out_kbps,
                row.rx_kbps,
                cap_text(row.cap_kbps as u64),
                row.relay_drops,
                row.dec_fps,
                row.cap_recv_p50_ms
            );
            m.timeline
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(row);
            if t >= a.secs {
                break;
            }
        }
        let wall_s = start.elapsed().as_secs_f64();
        m.stop.store(true, Ordering::Relaxed);
        // Let in-flight frames arrive, then the source thread ends and closes the frame channel,
        // the host sends bye and closes, and the viewer sees Closed.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        match tokio::time::timeout(Duration::from_secs(5), host).await {
            Ok(Ok(Err(e))) => eprintln!("host: {e:#}"),
            Ok(_) => {}
            Err(_) => eprintln!("host did not stop within 5 s"),
        }
        if let Ok(Ok(Err(e))) = tokio::time::timeout(Duration::from_secs(5), viewer).await {
            eprintln!("viewer: {e:#}");
        }
        Ok((hr, decoder, wall_s))
    }

    fn redact_sdp(sdp: &str) -> String {
        sdp.split_inclusive('\n')
            .map(|l| {
                if l.starts_with("a=ice-pwd:") {
                    let end = &l[l.trim_end_matches(['\r', '\n']).len()..];
                    format!("a=ice-pwd:<redacted>{end}")
                } else {
                    l.to_owned()
                }
            })
            .collect()
    }

    fn cap_text(kbps: u64) -> String {
        if kbps == 0 {
            "none".into()
        } else {
            kbps.to_string()
        }
    }

    fn sample(m: &Metrics, relay: Option<&Relay>, rx_hist: &mut VecDeque<(i64, u64)>) -> Row {
        let now = now_100ns();
        let win = now - 10_000_000;
        let rx = m.rx_bytes.load(Ordering::Relaxed);
        rx_hist.push_back((now, rx));
        while rx_hist.len() > 1 && rx_hist[1].0 <= win {
            rx_hist.pop_front();
        }
        let (t_old, rx_old) = rx_hist[0];
        let rx_kbps = if now > t_old {
            (rx - rx_old) as f64 * 8.0 / ((now - t_old) as f64 / 1e7) / 1000.0
        } else {
            0.0
        };
        let (mut enc_bytes, mut dec, mut lat) = (0usize, 0u32, Vec::new());
        {
            let g = m.frames.lock().unwrap_or_else(|e| e.into_inner());
            for f in g.values().rev() {
                if f.cap < win - 20_000_000 {
                    break;
                }
                if f.enc.is_some_and(|e| e > win) {
                    enc_bytes += f.bytes;
                }
                if f.dec.is_some_and(|d| d > win) {
                    dec += 1;
                }
                if let Some(r) = f.recv
                    && r > win
                {
                    lat.push((r - f.cap) as f64 / 10_000.0);
                }
            }
        }
        Row {
            t: m.secs(now),
            gcc_kbps: m.gcc_target.get() / 1000.0,
            gcc_delay_kbps: m.gcc_delay.get() / 1000.0,
            gcc_loss_kbps: m.gcc_loss.get() / 1000.0,
            enc_set_kbps: m.enc_setpoint.load(Ordering::Relaxed) as f64 / 1000.0,
            enc_out_kbps: enc_bytes as f64 * 8.0 / 1000.0,
            rx_kbps,
            cap_kbps: relay.map_or(0.0, |r| {
                r.state.cap_bps.load(Ordering::Relaxed) as f64 / 1000.0
            }),
            relay_drops: relay.map_or(0, |r| r.state.dropped.load(Ordering::Relaxed)),
            dec_fps: f64::from(dec),
            cap_recv_p50_ms: pct(&lat, 50.0),
        }
    }

    fn stats(v: &[f64]) -> String {
        if v.is_empty() {
            return "n=0".into();
        }
        format!(
            "p50 {:.1} / p95 {:.1} / max {:.1} ms (n={})",
            pct(v, 50.0),
            pct(v, 95.0),
            pct(v, 100.0),
            v.len()
        )
    }

    fn report(a: &Args, m: &Metrics, o: &Outcome) -> Result<i32> {
        let mut s = String::new();
        let frames = m.frames.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let timeline = m.timeline.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let changes = m
            .enc_changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rtts = m
            .dc_rtt_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let t0 = m.t0.load(Ordering::Relaxed) as i64;
        let first_cap = a
            .steps
            .iter()
            .find(|s| s.1 > 0)
            .map_or(f64::INFINITY, |s| s.0);

        writeln!(s, "# spike-webrtc (WP-43) {}", a.label)?;
        writeln!(
            s,
            "host webrtc-rs 0.21.1 (GCC + pacer); viewer webrtc-rs 0.21.1 (rtc 0.21.1, crypto ring); BWE initial {:.0} min {:.0} max {:.0} kbps; headroom {}; relay {} (queue {} ms); NACK {}; steps {:?}; {} s",
            a.gcc.initial / 1000.0,
            a.gcc.min / 1000.0,
            a.gcc.max / 1000.0,
            a.headroom,
            if a.relay { "on" } else { "off" },
            a.queue_ms,
            if a.nack { "on" } else { "off" },
            a.steps,
            a.secs
        )?;
        writeln!(
            s,
            "source: {} / {} ({}), {}x{} @ {} fps from a {}x{} window, GOP {}",
            o.source.adapter,
            o.source.encoder,
            o.source.profile,
            a.width,
            a.height,
            a.fps,
            o.source.source_size.0,
            o.source.source_size.1,
            a.gop.unwrap_or(a.fps * 2)
        )?;
        writeln!(s, "offer candidates (host): {:?}", o.host.offer_candidates)?;
        writeln!(
            s,
            "answer candidates (viewer): {:?}",
            o.host.answer_candidates
        )?;
        let fmtp: Vec<&str> = o
            .host
            .answer_sdp
            .lines()
            .filter(|l| {
                l.starts_with("a=fmtp:")
                    || l.starts_with("a=rtcp-fb:")
                    || l.starts_with("a=extmap:")
            })
            .collect();
        writeln!(s, "answer fmtp/rtcp-fb/extmap: {fmtp:?}")?;
        writeln!(
            s,
            "connect (ws accept -> Connected): {:.0} ms",
            o.host.connect_ms
        )?;

        let recvd = frames.values().filter(|f| f.recv.is_some()).count();
        let sent = frames.values().filter(|f| f.sent.is_some()).count();
        writeln!(
            s,
            "frames: captured+fed {} encoded {} (IDR {}) sent {} received-clean {} decoded {} (ffmpeg lines {}) | lost pkts {} late/dup pkts {} lost AUs {} skipped-until-IDR {} PLI sent {} recv {} | sender queue full {} | wall {:.1} s",
            o.source.frames_in,
            o.source.frames_out,
            o.source.idrs,
            sent,
            recvd,
            frames.values().filter(|f| f.dec.is_some()).count(),
            o.decoded,
            m.rx_pkts_lost.load(Ordering::Relaxed),
            m.rx_late.load(Ordering::Relaxed),
            m.rx_aus_lost.load(Ordering::Relaxed),
            m.rx_aus_skipped.load(Ordering::Relaxed),
            m.plis_sent.load(Ordering::Relaxed),
            m.plis_recv.load(Ordering::Relaxed),
            m.tx_dropped.load(Ordering::Relaxed),
            o.wall_s
        )?;
        {
            let kinds = m.rtcp_kinds.lock().unwrap_or_else(|e| e.into_inner());
            let mut counts = std::collections::BTreeMap::<&str, usize>::new();
            for k in kinds.iter() {
                *counts.entry(k.as_str()).or_default() += 1;
            }
            writeln!(s, "RTCP seen by the host track: {counts:?}")?;
        }
        writeln!(
            s,
            "encoder rate control: {} ({} re-creations, {} ms each p50, max {:.0} ms; {} in-flight frames lost)",
            o.source.rate_api,
            o.source.recreate_ms.len(),
            if o.source.recreate_ms.is_empty() {
                "-".to_owned()
            } else {
                format!("{:.0}", pct(&o.source.recreate_ms, 50.0))
            },
            pct(&o.source.recreate_ms, 100.0),
            o.source.recreated_dropped
        )?;
        if !o.source.set_bitrate_errors.is_empty() {
            writeln!(s, "set_bitrate errors: {:?}", o.source.set_bitrate_errors)?;
        }

        // Latency per segment, per phase.
        let ms = |a: i64, b: i64| (b - a) as f64 / 10_000.0;
        let phases: [(&str, f64, f64); 3] = [
            ("steady (3 s .. first cap)", 3.0, first_cap),
            (
                "throttled",
                first_cap,
                a.steps
                    .iter()
                    .find(|s| s.0 > first_cap && s.1 == 0)
                    .map_or(f64::INFINITY, |s| s.0),
            ),
            ("whole run", 0.0, f64::INFINITY),
        ];
        for (name, from, to) in phases {
            let sel: Vec<_> = frames
                .values()
                .filter(|f| {
                    let t = (f.cap - t0) as f64 / 1e7;
                    t >= from && t < to
                })
                .collect();
            if sel.is_empty() {
                continue;
            }
            let seg = |g: &dyn Fn(&spike_webrtc::metrics::FrameRec) -> Option<f64>| -> Vec<f64> {
                sel.iter().filter_map(|f| g(f)).collect()
            };
            writeln!(s, "latency [{name}]:")?;
            writeln!(
                s,
                "  capture->encoded  {}",
                stats(&seg(&|f| f.enc.map(|e| ms(f.cap, e))))
            )?;
            writeln!(
                s,
                "  encoded->sent     {}",
                stats(&seg(&|f| Some(ms(f.enc?, f.sent?))))
            )?;
            writeln!(
                s,
                "  sent->received    {}",
                stats(&seg(&|f| Some(ms(f.sent?, f.recv?))))
            )?;
            writeln!(
                s,
                "  capture->received {}",
                stats(&seg(&|f| f.recv.map(|r| ms(f.cap, r))))
            )?;
            writeln!(
                s,
                "  received->decoded {}",
                stats(&seg(&|f| Some(ms(f.recv?, f.dec?))))
            )?;
            writeln!(
                s,
                "  capture->decoded  {}",
                stats(&seg(&|f| f.dec.map(|d| ms(f.cap, d))))
            )?;
        }
        writeln!(
            s,
            "data channel echo RTT: {} (echoed {})",
            stats(&rtts),
            m.dc_echoed.load(Ordering::Relaxed)
        )?;

        // Adaptation.
        let mut pass = o.decoded > 0;
        let mut prev_gcc = None::<f64>;
        for &(at, kbps) in &a.steps {
            let after: Vec<&Row> = timeline.iter().filter(|r| r.t >= at).collect();
            let before_gcc = timeline.iter().rev().find(|r| r.t < at).map(|r| r.gcc_kbps);
            if kbps > 0 {
                let cap = kbps as f64;
                let gcc_s = after.iter().find(|r| r.gcc_kbps <= cap).map(|r| r.t - at);
                let out_s = after
                    .iter()
                    .find(|r| r.enc_out_kbps <= cap)
                    .map(|r| r.t - at);
                let set_before = changes
                    .iter()
                    .rev()
                    .find(|c| m.secs(c.0) < at)
                    .map_or(a.gcc.initial * a.headroom, |c| f64::from(c.1));
                let set_s = if set_before <= cap * 1000.0 {
                    Some(0.0)
                } else {
                    changes
                        .iter()
                        .find(|c| m.secs(c.0) >= at && f64::from(c.1) <= cap * 1000.0)
                        .map(|c| m.secs(c.0) - at)
                };
                let cut_s = changes
                    .iter()
                    .find(|c| m.secs(c.0) >= at && f64::from(c.1) < set_before * 0.85)
                    .map(|c| m.secs(c.0) - at);
                let half_s = before_gcc.and_then(|b| {
                    after
                        .iter()
                        .find(|r| r.gcc_kbps <= b * 0.5)
                        .map(|r| r.t - at)
                });
                let f = |v: Option<f64>| v.map_or("never".into(), |x| format!("{x:.2} s"));
                writeln!(
                    s,
                    "adaptation: cap {kbps} kbps at t={at:.1}s (GCC before {:.0} kbps, set point {:.0}): first set-point cut after {}, GCC <= 50 % after {}, encoded output (1 s window) <= cap after {}, set point <= cap after {}, GCC <= cap after {}",
                    before_gcc.unwrap_or(f64::NAN),
                    set_before / 1000.0,
                    f(cut_s),
                    f(half_s),
                    f(out_s),
                    f(set_s),
                    f(gcc_s)
                )?;
                // Settled behaviour: the last 5 s of the capped period.
                let end = a
                    .steps
                    .iter()
                    .find(|st| st.0 > at)
                    .map_or(a.secs, |st| st.0);
                let tail: Vec<&Row> = timeline
                    .iter()
                    .filter(|r| r.t >= (end - 5.0).max(at) && r.t < end)
                    .collect();
                let mean = |g: &dyn Fn(&Row) -> f64| {
                    tail.iter().map(|r| g(r)).sum::<f64>() / tail.len().max(1) as f64
                };
                writeln!(
                    s,
                    "  settled (last 5 s under the cap, means): GCC {:.0} kbps, set point {:.0}, encoded {:.0}, received {:.0}, decoded {:.1} fps, capture->received p50 {:.0} ms",
                    mean(&|r| r.gcc_kbps),
                    mean(&|r| r.enc_set_kbps),
                    mean(&|r| r.enc_out_kbps),
                    mean(&|r| r.rx_kbps),
                    mean(&|r| r.dec_fps),
                    mean(&|r| if r.cap_recv_p50_ms.is_nan() {
                        0.0
                    } else {
                        r.cap_recv_p50_ms
                    })
                )?;
                // The plan's criterion: the bitrate adapts within 5 s.
                pass &= out_s.is_some_and(|x| x <= 5.0) && cut_s.is_some_and(|x| x <= 5.0);
                prev_gcc = before_gcc;
            } else {
                let target = prev_gcc.unwrap_or(a.gcc.initial / 1000.0) * 0.9;
                let rec = after
                    .iter()
                    .find(|r| r.gcc_kbps >= target)
                    .map(|r| r.t - at);
                writeln!(
                    s,
                    "recovery: cap removed at t={at:.1}s: GCC back to >= {target:.0} kbps (90 % of pre-cap) after {}",
                    rec.map_or("not within the run".into(), |x| format!("{x:.2} s"))
                )?;
            }
        }
        writeln!(s, "encoder set point changes: {}", changes.len())?;
        writeln!(s, "RESULT: {}", if pass { "PASS" } else { "FAIL" })?;
        print!("\n{s}");

        // Files.
        std::fs::create_dir_all(&a.out)?;
        let mut csv = String::from(
            "t_s,gcc_kbps,gcc_delay_kbps,gcc_loss_kbps,enc_set_kbps,enc_out_kbps,rx_kbps,cap_kbps,relay_drops,dec_fps,cap_recv_p50_ms\n",
        );
        for r in &timeline {
            writeln!(
                csv,
                "{:.2},{:.0},{:.0},{:.0},{:.0},{:.0},{:.0},{:.0},{},{:.0},{:.1}",
                r.t,
                r.gcc_kbps,
                r.gcc_delay_kbps,
                r.gcc_loss_kbps,
                r.enc_set_kbps,
                r.enc_out_kbps,
                r.rx_kbps,
                r.cap_kbps,
                r.relay_drops,
                r.dec_fps,
                r.cap_recv_p50_ms
            )?;
        }
        let mut dirs = vec![a.out.clone()];
        dirs.extend(a.results.clone());
        for d in dirs {
            std::fs::create_dir_all(&d)?;
            std::fs::write(d.join(format!("{}-timeline.csv", a.label)), &csv)
                .with_context(|| d.display().to_string())?;
            std::fs::write(d.join(format!("{}-summary.txt", a.label)), &s)?;
            // The ICE password is a (short-lived) credential: never written out.
            std::fs::write(
                d.join(format!("{}-offer.sdp", a.label)),
                redact_sdp(&o.host.offer_sdp),
            )?;
            std::fs::write(
                d.join(format!("{}-answer.sdp", a.label)),
                redact_sdp(&o.host.answer_sdp),
            )?;
        }
        Ok(if pass { 0 } else { 1 })
    }
}
