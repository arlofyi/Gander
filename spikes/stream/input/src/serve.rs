// SPDX-License-Identifier: Apache-2.0
//! `spike-input serve`: stream a test window to the Android spike app and inject its taps.

use std::fmt::Write as _;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use spike_webrtc::metrics::{Metrics, now_100ns, pct};
use spike_webrtc::peer::{self, GccConfig, HostOptions};
use spike_webrtc::signal;
use spike_webrtc::source::{self, SourceConfig};
use tokio::sync::mpsc;
use windows::Win32::Foundation::{HWND, POINT};

use crate::driver;
use crate::inject::{self, Guard, Policy, map_tap, post_click, read_colour, send_click};

/// How a tap is injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `PostMessage` to the window.
    PostMessage,
    /// `SendInput` on the real mouse (refused when another window covers the point).
    SendInput,
    /// Odd taps `SendInput`, even taps `PostMessage` (a side-by-side comparison in one run).
    Alternate,
    /// Echo only, no injection.
    None,
}

impl Mode {
    fn for_tap(self, seq: u64) -> Mode {
        match self {
            Mode::Alternate if seq % 2 == 1 => Mode::SendInput,
            Mode::Alternate => Mode::PostMessage,
            m => m,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Mode::PostMessage => "postmessage",
            Mode::SendInput => "sendinput",
            Mode::Alternate => "alternate",
            Mode::None => "none",
        }
    }
}

/// One tap as the host handled it (times are QPC 100 ns).
#[derive(Debug, Clone)]
pub struct TapRec {
    /// The phone's sequence number.
    pub seq: u64,
    /// Normalised point as received.
    pub nx: f64,
    /// Normalised point as received.
    pub ny: f64,
    /// Injection method used for this tap.
    pub mode: &'static str,
    /// Mapped client point (physical pixels), `None` if the mapping failed.
    pub client: Option<(i32, i32)>,
    /// Data channel message received.
    pub rx: i64,
    /// Just before the injection call.
    pub inject_start: i64,
    /// Just after the injection call.
    pub inject_end: i64,
    /// The box colour change was seen on the control pipe (`None`: no change within the wait).
    pub effect: Option<i64>,
    /// `hits` went up (the click was on the box).
    pub hit: bool,
    /// `clicks` went up (the click reached the window at all).
    pub clicked: bool,
    /// What the injector reported.
    pub outcome: String,
    /// Session locked when the tap arrived.
    pub locked: Option<bool>,
}

/// Shared with the tap hook and the adb driver.
#[derive(Debug)]
pub struct TapCtx {
    /// Measurement state (window handle and pipe live here).
    pub m: Arc<Metrics>,
    /// Injection mode.
    pub mode: Mode,
    /// Every tap handled so far.
    pub taps: Mutex<Vec<TapRec>>,
}

impl TapCtx {
    /// The captured window and its pipe, once the source has started.
    pub fn window(&self) -> Option<(HWND, String)> {
        self.m
            .window
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map(|(h, p)| (HWND(h as *mut std::ffi::c_void), p))
    }
}

/// Wall clock, for lining the host log up with the driver script's own timestamps.
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn ms(a: i64, b: i64) -> f64 {
    (b - a) as f64 / 10_000.0
}

fn handle_tap(ctx: &TapCtx, v: &Value, host_rx: i64) {
    if v.get("t").and_then(Value::as_str) != Some("tap") {
        return;
    }
    let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let nx = v.get("x").and_then(Value::as_f64).unwrap_or(-1.0);
    let ny = v.get("y").and_then(Value::as_f64).unwrap_or(-1.0);
    let mode = ctx.mode.for_tap(seq);
    let mut rec = TapRec {
        seq,
        nx,
        ny,
        mode: mode.name(),
        client: None,
        rx: host_rx,
        inject_start: 0,
        inject_end: 0,
        effect: None,
        hit: false,
        clicked: false,
        outcome: String::new(),
        locked: inject::session_locked(),
    };
    let Some((hwnd, pipe)) = ctx.window() else {
        rec.outcome = "no window".into();
        ctx.taps.lock().unwrap_or_else(|e| e.into_inner()).push(rec);
        return;
    };
    if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
        rec.outcome = "refused: coordinates outside 0..1".into();
        ctx.taps.lock().unwrap_or_else(|e| e.into_inner()).push(rec);
        return;
    }
    let mapped = match map_tap(hwnd, nx, ny) {
        Ok(m) => m,
        Err(e) => {
            rec.outcome = format!("mapping failed: {e:#}");
            ctx.taps.lock().unwrap_or_else(|e| e.into_inner()).push(rec);
            return;
        }
    };
    rec.client = Some((mapped.client.x, mapped.client.y));
    let before = read_colour(&pipe).ok();
    rec.inject_start = now_100ns();
    rec.outcome = match mode {
        Mode::PostMessage => {
            let o = post_click(hwnd, mapped.client);
            format!("PostMessage accepted {}/3 error {:?}", o.posted, o.error)
        }
        Mode::SendInput => {
            let o = send_click(hwnd, POINT { ..mapped.screen }, Policy::TargetOnly);
            match &o.guard {
                Guard::Ok => format!(
                    "SendInput accepted {:?}/3 error {:?}; cursor {:?} -> {:?}",
                    o.accepted,
                    o.error,
                    o.cursor_before.map(|p| (p.x, p.y)),
                    o.cursor_after.map(|p| (p.x, p.y))
                ),
                g => format!("SendInput refused: {g:?}"),
            }
        }
        Mode::Alternate | Mode::None => "no injection".into(),
    };
    rec.inject_end = now_100ns();
    if let Some(b) = before {
        // The colour change is the effect; poll the pipe until it shows (2 ms steps, 400 ms max).
        let deadline = Instant::now() + Duration::from_millis(400);
        loop {
            if let Ok(c) = read_colour(&pipe) {
                rec.clicked = c.clicks > b.clicks;
                rec.hit = c.hits > b.hits;
                if c.index != b.index {
                    rec.effect = Some(now_100ns());
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    println!(
        "tap seq {seq} ({nx:.3}, {ny:.3}) {} client {:?}: rx->inject {:.1} ms, inject {:.2} ms, effect {}; {}{}",
        rec.mode,
        rec.client,
        ms(rec.rx, rec.inject_start),
        ms(rec.inject_start, rec.inject_end),
        rec.effect.map_or("none".to_owned(), |e| format!(
            "{:.1} ms after inject",
            ms(rec.inject_end, e)
        )),
        rec.outcome,
        if rec.locked == Some(true) {
            " [session locked]"
        } else {
            ""
        }
    );
    ctx.taps.lock().unwrap_or_else(|e| e.into_inner()).push(rec);
}

#[derive(Debug)]
struct Args {
    port: u16,
    advertise: Option<IpAddr>,
    mode: Mode,
    width: u32,
    height: u32,
    fps: u32,
    vendor: u32,
    secs: f64,
    gcc: GccConfig,
    headroom: f64,
    adb: Option<PathBuf>,
    serial: Option<String>,
    drive: u32,
    results: Option<PathBuf>,
    label: String,
    strip: bool,
}

fn parse(args: &[String]) -> Result<Args> {
    let mut a = Args {
        port: 47440,
        advertise: None,
        mode: Mode::PostMessage,
        width: 1080,
        height: 2280,
        fps: 30,
        vendor: 0x8086,
        secs: 120.0,
        gcc: GccConfig {
            initial: 4_000_000.0,
            min: 300_000.0,
            max: 6_000_000.0,
        },
        headroom: 0.85,
        adb: None,
        serial: None,
        drive: 0,
        results: None,
        label: "run".into(),
        strip: false,
    };
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let mut v = || it.next().with_context(|| format!("{k} needs a value"));
        match k.as_str() {
            "--port" => a.port = v()?.parse()?,
            "--advertise" => a.advertise = Some(v()?.parse()?),
            "--inject" => {
                a.mode = match v()?.as_str() {
                    "postmessage" => Mode::PostMessage,
                    "sendinput" => Mode::SendInput,
                    "alternate" => Mode::Alternate,
                    "none" => Mode::None,
                    o => bail!("unknown --inject {o}"),
                }
            }
            "--size" => {
                let s = v()?;
                let (w, h) = s.split_once('x').context("--size WxH")?;
                a.width = w.parse()?;
                a.height = h.parse()?;
            }
            "--fps" => a.fps = v()?.parse()?,
            "--vendor" => {
                a.vendor = match v()?.as_str() {
                    "intel" => 0x8086,
                    "nvidia" => 0x10DE,
                    o => bail!("unknown --vendor {o}"),
                }
            }
            "--secs" => a.secs = v()?.parse()?,
            "--gcc-initial-kbps" => a.gcc.initial = v()?.parse::<f64>()? * 1000.0,
            "--gcc-max-kbps" => a.gcc.max = v()?.parse::<f64>()? * 1000.0,
            "--headroom" => a.headroom = v()?.parse()?,
            "--adb" => a.adb = Some(PathBuf::from(v()?)),
            "--serial" => a.serial = Some(v()?.clone()),
            "--drive" => a.drive = v()?.parse()?,
            "--results" => a.results = Some(PathBuf::from(v()?)),
            "--label" => a.label = v()?.clone(),
            "--strip-answer-candidates" => a.strip = true,
            o => bail!("unknown option {o}"),
        }
    }
    Ok(a)
}

/// Runs the host until the viewer leaves, the deadline passes or the driver finishes.
pub fn run(args: Vec<String>) -> Result<i32> {
    let a = parse(&args)?;
    let m = Arc::new(Metrics::default());
    let (ftx, frx) = mpsc::channel(8);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let scfg = SourceConfig {
        width: a.width,
        height: a.height,
        fps: a.fps,
        vendor: a.vendor,
        gop: a.fps * 2,
        initial_bps: (a.gcc.initial * a.headroom) as u32,
        min_bps: (a.gcc.min * a.headroom) as u32,
        max_bps: (a.gcc.max * a.headroom) as u32,
        headroom: a.headroom,
        testwin_secs: a.secs as u32 + 600,
        title: "spike-input".into(),
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
    let ctx = Arc::new(TapCtx {
        m: m.clone(),
        mode: a.mode,
        taps: Mutex::new(Vec::new()),
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome = rt.block_on(session(&a, &m, &ctx, frx));
    m.stop.store(true, Ordering::Relaxed);
    let source_report = match src.join() {
        Ok(Ok(r)) => Some(r),
        Ok(Err(e)) => {
            eprintln!("source: {e:#}");
            None
        }
        Err(_) => None,
    };
    rt.shutdown_timeout(Duration::from_secs(2));
    let summary = summarise(
        &a,
        &ctx,
        source_report
            .as_ref()
            .map(|r| (r.adapter.clone(), r.encoder.clone())),
    );
    print!("\n{summary}");
    if let Some(dir) = &a.results {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(format!("{}-host-summary.txt", a.label)), &summary)?;
        let taps = ctx.taps.lock().unwrap_or_else(|e| e.into_inner());
        let mut lines = String::new();
        for t in taps.iter() {
            writeln!(
                lines,
                "{}",
                json!({
                    "seq": t.seq, "x": t.nx, "y": t.ny, "mode": t.mode, "client": t.client,
                    "rx": t.rx, "inject_start": t.inject_start, "inject_end": t.inject_end,
                    "effect": t.effect, "hit": t.hit, "clicked": t.clicked,
                    "outcome": t.outcome, "locked": t.locked,
                })
            )?;
        }
        std::fs::write(dir.join(format!("{}-host-taps.jsonl", a.label)), lines)?;
    }
    outcome?;
    Ok(0)
}

async fn session(
    a: &Args,
    m: &Arc<Metrics>,
    ctx: &Arc<TapCtx>,
    frx: mpsc::Receiver<peer::EncodedFrame>,
) -> Result<()> {
    let listener = signal::listen_on(a.port).await?;
    let addr = listener.local_addr()?;
    let hook_ctx = ctx.clone();
    let opts = HostOptions {
        advertise_ip: a.advertise,
        strip_answer_candidates: a.strip,
        on_input: Some(Arc::new(move |v, rx| handle_tap(&hook_ctx, v, rx))),
    };
    let (ctx_tx, crx) = tokio::sync::oneshot::channel();
    let host = tokio::spawn(peer::run_host(
        listener,
        frx,
        m.clone(),
        None,
        a.gcc,
        false,
        opts,
        ctx_tx,
    ));
    println!(
        "spike-input ready signalling=ws://{addr}/ port={} advertise={:?} inject={}",
        addr.port(),
        a.advertise,
        a.mode.name()
    );
    // Wait for a phone (the driver script starts the app after it sees the line above).
    let hr = match tokio::time::timeout(Duration::from_secs(600), crx).await {
        Ok(Ok(r)) => r,
        _ => {
            m.stop.store(true, Ordering::Relaxed);
            let e = match tokio::time::timeout(Duration::from_secs(2), host).await {
                Ok(Ok(Err(e))) => format!("{e:#}"),
                _ => "no viewer connected".into(),
            };
            bail!("connect: {e}");
        }
    };
    println!(
        "connected in {:.0} ms; offer candidates {:?}; answer candidates {:?}",
        hr.connect_ms, hr.offer_candidates, hr.answer_candidates
    );
    m.t0.store(now_100ns() as u64, Ordering::Relaxed);
    m.go.store(true, Ordering::Relaxed);
    if let Some(dir) = &a.results {
        // The ICE password is a short-lived credential: never written out.
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            dir.join(format!("{}-offer.sdp", a.label)),
            redact_sdp(&hr.offer_sdp),
        )?;
        std::fs::write(
            dir.join(format!("{}-answer.sdp", a.label)),
            redact_sdp(&hr.answer_sdp),
        )?;
    }

    // Optional adb-driven taps on a std thread (it blocks on child processes).
    let driven = a.adb.clone().filter(|_| a.drive > 0).map(|adb| {
        let ctx = ctx.clone();
        let n = a.drive;
        let serial = a.serial.clone();
        let results = a.results.clone();
        let label = a.label.clone();
        std::thread::spawn(move || {
            driver::run(&adb, serial.as_deref(), n, &ctx, results.as_deref(), &label)
        })
    });

    let deadline = Instant::now() + Duration::from_secs_f64(a.secs);
    let mut host = host;
    let mut last_status = Instant::now();
    loop {
        if last_status.elapsed() >= Duration::from_secs(5) {
            last_status = Instant::now();
            let sent = m
                .frames
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|f| f.sent.is_some())
                .count();
            println!(
                "status epoch_ms={} frames_sent={sent} dc_echoed={}",
                epoch_ms(),
                m.dc_echoed.load(Ordering::Relaxed)
            );
        }
        if let Some(d) = &driven
            && d.is_finished()
        {
            break;
        }
        if Instant::now() >= deadline {
            println!("deadline reached");
            break;
        }
        if host.is_finished() {
            println!("viewer left epoch_ms={}", epoch_ms());
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Some(d) = driven {
        match d.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("driver: {e:#}"),
            Err(_) => eprintln!("driver panicked"),
        }
    }
    m.stop.store(true, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    match tokio::time::timeout(Duration::from_secs(5), &mut host).await {
        Ok(Ok(Err(e))) => eprintln!("host: {e:#}"),
        Ok(_) => {}
        Err(_) => eprintln!("host did not stop within 5 s"),
    }
    Ok(())
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

fn summarise(a: &Args, ctx: &TapCtx, src: Option<(String, String)>) -> String {
    let taps = ctx.taps.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = String::new();
    let _ = writeln!(s, "# spike-input serve ({})", a.label);
    if let Some((adapter, encoder)) = src {
        let _ = writeln!(
            s,
            "source: {adapter} / {encoder}, {}x{} @ {} fps",
            a.width, a.height, a.fps
        );
    }
    let _ = writeln!(
        s,
        "inject mode {}, advertise {:?}, taps handled {}",
        a.mode.name(),
        a.advertise,
        taps.len()
    );
    for mode in ["postmessage", "sendinput"] {
        let sel: Vec<&TapRec> = taps.iter().filter(|t| t.mode == mode).collect();
        if sel.is_empty() {
            continue;
        }
        let hits = sel.iter().filter(|t| t.hit).count();
        let clicked = sel.iter().filter(|t| t.clicked).count();
        let effects: Vec<f64> = sel
            .iter()
            .filter_map(|t| t.effect.map(|e| ms(t.inject_end, e)))
            .collect();
        let rx_inject: Vec<f64> = sel.iter().map(|t| ms(t.rx, t.inject_start)).collect();
        let inject: Vec<f64> = sel
            .iter()
            .map(|t| ms(t.inject_start, t.inject_end))
            .collect();
        let _ = writeln!(
            s,
            "{mode}: {} taps, {} reached the window (clicks +1), {} hit the box; rx->inject p50 {:.1} ms; inject call p50 {:.2} ms; inject->colour change seen on the pipe p50 {:.1} / p95 {:.1} ms (n={})",
            sel.len(),
            clicked,
            hits,
            pct(&rx_inject, 50.0),
            pct(&inject, 50.0),
            pct(&effects, 50.0),
            pct(&effects, 95.0),
            effects.len()
        );
        if let Some(first) = sel.first() {
            let _ = writeln!(s, "  first outcome: {}", first.outcome);
        }
    }
    s
}
