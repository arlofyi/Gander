// SPDX-License-Identifier: Apache-2.0
//! `gander-e2e stream` (WP-57): the whole Stream path against a running test host, with the
//! loopback viewer from `gander_stream::testing` playing the phone and `gander-testwin` as the
//! only application. Normally run by `cargo xtask e2e-stream`, which starts the host with
//! `--test-stream-app <gander-testwin>` and wires this program's stdout to the host's stdin
//! (`lock`, `unlock`, `kill`). Everything human-readable goes to stderr.
//!
//! ```text
//! gander-e2e stream --addr 127.0.0.1:<port> --host-id <id> --share <share-id> --path <file.gtw>
//!                   --app <app-id> --testwin <exe> --report <json> --work <dir>
//!                   [--ffmpeg <exe>] [--box-out <json>] [--skip-engine]
//! ```
//!
//! Two stages:
//!
//! - **host**: pair, `/v1/events`, `POST /v1/stream` (201), `window_found`, offer/answer,
//!   `streaming`, frames decoded (ffmpeg), the parked box found in the decoded pixels in the colour
//!   the test window reports, a tap on it changes the colour (pipe and decoded pixels), keepalive
//!   while static, pause/resume on the `control` channel, a simulated lock ends the session,
//!   `DELETE ?close_app=true` closes the test window, the kill switch drops everything.
//! - **engine**: the same capture -> encode -> WebRTC -> decode path in process (a
//!   `StreamEngine` with the real WGC source on a test window started here, the lock guard faked),
//!   which also runs while the session is locked: WGC captures a locked desktop. Then
//!   `engine.audio_tone` (WP-55): the test window plays a quiet 1 kHz tone (`tone 1000 800`,
//!   -30 dBFS), the engine captures its process tree by WASAPI process loopback, Opus-encodes it
//!   on the audio track, and the viewer decodes it and finds 1000 Hz within 5 Hz (measured to
//!   work while locked too).
//!
//! Session state: the host refuses Stream while the computer is really locked (fail closed; the
//! simulated lock can only add one), and input is never injected into a locked session. When the
//! session is locked, the host stage checks that refusal and the kill switch and reports every
//! other step `deferred: session locked`; the engine stage runs in full. Nothing is ever posted
//! to a window as a substitute for injected input.
//!
//! Exit status: 0 when every step that ran passed (deferrals allowed), 1 otherwise, 2 on a usage
//! error.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use serde_json::{Value, json};

/// Command-line options.
#[derive(Debug, Clone)]
pub struct StreamOptions {
    pub addr: SocketAddr,
    pub host_id: String,
    pub share: String,
    pub path: String,
    pub app: String,
    pub testwin: PathBuf,
    pub report: PathBuf,
    pub work: PathBuf,
    pub ffmpeg: Option<PathBuf>,
    pub box_out: Option<PathBuf>,
    pub skip_engine: bool,
}

pub fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<StreamOptions> {
    let mut addr = None;
    let mut host_id = None;
    let mut share = None;
    let mut path = None;
    let mut app = None;
    let mut testwin = None;
    let mut report = None;
    let mut work = None;
    let mut ffmpeg = None;
    let mut box_out = None;
    let mut skip_engine = false;
    let mut it = args;
    while let Some(a) = it.next() {
        let mut value = || it.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--addr" => addr = Some(value()?.parse::<SocketAddr>()?),
            "--host-id" => host_id = Some(value()?),
            "--share" => share = Some(value()?),
            "--path" => path = Some(value()?),
            "--app" => app = Some(value()?),
            "--testwin" => testwin = Some(PathBuf::from(value()?)),
            "--report" => report = Some(PathBuf::from(value()?)),
            "--work" => work = Some(PathBuf::from(value()?)),
            "--ffmpeg" => ffmpeg = Some(PathBuf::from(value()?)),
            "--box-out" => box_out = Some(PathBuf::from(value()?)),
            "--skip-engine" => skip_engine = true,
            other => bail!("unknown stream argument {other}"),
        }
    }
    let addr = addr.context("--addr is required")?;
    if !addr.ip().is_loopback() {
        bail!("--addr must be a loopback address");
    }
    Ok(StreamOptions {
        addr,
        host_id: host_id.context("--host-id is required")?,
        share: share.context("--share is required")?,
        path: path.context("--path is required")?,
        app: app.context("--app is required")?,
        testwin: testwin.context("--testwin is required")?,
        report: report.context("--report is required")?,
        work: work.context("--work is required")?,
        ffmpeg,
        box_out,
        skip_engine,
    })
}

// ---------------------------------------------------------------------------------- report

/// Why a step did not pass.
#[derive(Debug, Clone)]
pub enum Halt {
    Fail(String),
    Deferred(String),
}

impl From<anyhow::Error> for Halt {
    fn from(e: anyhow::Error) -> Self {
        Halt::Fail(format!("{e:#}"))
    }
}

impl From<String> for Halt {
    fn from(e: String) -> Self {
        Halt::Fail(e)
    }
}

/// A step's result: a detail line, or why not.
pub type Res = Result<String, Halt>;

/// Returns `Err(Halt::Fail(msg))` unless `cond`.
pub fn ensure(cond: bool, msg: impl FnOnce() -> String) -> Result<(), Halt> {
    if cond { Ok(()) } else { Err(Halt::Fail(msg())) }
}

/// The steps in order, plus measurements.
#[derive(Debug, Default)]
pub struct Report {
    steps: Vec<Value>,
    pub measurements: serde_json::Map<String, Value>,
    pub session: serde_json::Map<String, Value>,
}

impl Report {
    pub fn record(&mut self, id: &str, started: Instant, r: &Res) {
        let ms = started.elapsed().as_millis() as u64;
        let (outcome, detail) = match r {
            Ok(d) => ("pass", d.clone()),
            Err(Halt::Fail(e)) => ("fail", e.clone()),
            Err(Halt::Deferred(e)) => ("deferred", e.clone()),
        };
        eprintln!("[{outcome:>8}] {id} ({ms} ms): {detail}");
        self.steps
            .push(json!({ "id": id, "outcome": outcome, "detail": detail, "ms": ms }));
    }

    pub fn defer(&mut self, id: &str, why: &str) {
        self.record(id, Instant::now(), &Err(Halt::Deferred(why.to_owned())));
    }

    fn count(&self, outcome: &str) -> usize {
        self.steps
            .iter()
            .filter(|s| s["outcome"] == outcome)
            .count()
    }

    pub fn ok(&self) -> bool {
        self.count("fail") == 0 && self.count("pass") > 0
    }

    pub fn to_json(&self) -> Value {
        json!({
            "tool": "gander-e2e stream (WP-57)",
            "session": self.session,
            "summary": {
                "passed": self.count("pass"),
                "failed": self.count("fail"),
                "deferred": self.count("deferred"),
                "total": self.steps.len(),
                "ok": self.ok(),
            },
            "measurements": self.measurements,
            "steps": self.steps,
        })
    }
}

/// Runs `fut` as step `id` and records it; returns the detail on a pass.
#[macro_export]
macro_rules! step {
    ($report:expr, $id:expr, $fut:expr) => {{
        let started = std::time::Instant::now();
        let r: $crate::stream::Res = $fut.await;
        $report.record($id, started, &r);
        r
    }};
}

/// The session the harness runs in can take injected input (unlocked, attached).
#[cfg(windows)]
pub fn interactive() -> (bool, String) {
    match gander_win::session::query_session_state() {
        Ok(s) => (s.is_interactive(), format!("{s:?}")),
        Err(e) => (false, format!("query failed: {e}")),
    }
}

#[cfg(not(windows))]
pub fn interactive() -> (bool, String) {
    (false, "not Windows".to_owned())
}

pub const LOCKED: &str = "deferred: session locked (the host refuses Stream and input is never injected while the session is really locked; run again unlocked and attended)";

pub async fn run(o: &StreamOptions) -> anyhow::Result<bool> {
    std::fs::create_dir_all(&o.work)?;
    let mut report = Report::default();
    let (unlocked, state) = interactive();
    eprintln!("session at start: {state} (interactive: {unlocked})");
    report.session.insert(
        "at_start".into(),
        json!({ "interactive": unlocked, "state": state }),
    );
    report.session.insert(
        "ffmpeg".into(),
        json!(o.ffmpeg.as_ref().map(|p| p.display().to_string())),
    );
    #[cfg(windows)]
    {
        win::host_stage(o, &mut report, unlocked).await;
        if o.skip_engine {
            report.defer("engine", "skipped (--skip-engine)");
        } else {
            win::engine_stage(o, &mut report).await;
        }
    }
    #[cfg(not(windows))]
    report.defer("host", "Stream mode is Windows only");
    let (unlocked_end, state_end) = interactive();
    report.session.insert(
        "at_end".into(),
        json!({ "interactive": unlocked_end, "state": state_end }),
    );
    let v = report.to_json();
    std::fs::write(&o.report, serde_json::to_vec_pretty(&v)?)
        .with_context(|| format!("write {}", o.report.display()))?;
    eprintln!("summary: {}", v["summary"]);
    Ok(report.ok())
}

/// `gander-e2e park-watch --seconds <n> [--front]`: for the Android stage. Every test window that
/// starts while it runs is parked (`park`: the box at its start position, where the harness told
/// the phone to tap). `--front` (optional, off in `cargo xtask e2e-stream`) also brings it to the
/// foreground once from outside the product; the host does that itself when the phone connects
/// and the client asks on `focus_lost`. Prints `parked <pid> ok=<bool> front=<bool>` per window on
/// stdout.
#[cfg(windows)]
pub fn park_watch(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    use crate::testwin;
    let mut seconds = 600u64;
    let mut front = false;
    let mut it = args;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--seconds" => seconds = it.next().context("--seconds needs a value")?.parse()?,
            "--front" => front = true,
            other => bail!("unknown park-watch argument {other}"),
        }
    }
    let mut seen = testwin::pids();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        for pid in testwin::pids() {
            if !seen.insert(pid) {
                continue;
            }
            // Give the window a moment to be created and shown.
            std::thread::sleep(Duration::from_millis(150));
            let parked = testwin::request(pid, "park").is_ok();
            let fronted = front && win::bring_to_front(pid);
            use std::io::Write as _;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "parked {pid} ok={parked} front={fronted}");
            let _ = out.flush();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

#[cfg(windows)]
mod win {
    use std::collections::BTreeSet;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::net::{IpAddr, Ipv4Addr};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use futures_util::{SinkExt as _, StreamExt as _};
    use gander_core::config::UdpPortRange;
    use gander_core::protocol::DeviceId;
    use gander_core::protocol::events::{ClientEvent, ServerEvent};
    use gander_core::protocol::stream::Viewport;
    use gander_stream::control::ControlMessage;
    use gander_stream::testing::viewer::{FrameRx, max_gap};
    use gander_stream::testing::{CollectingSink, Viewer, ViewerOptions};
    use gander_stream::win::WgcSource;
    use gander_stream::{
        EndReason, EngineConfig, EngineDeps, FakeLock, FixedPower, SessionState, StaticAddrs,
        StreamEngine, WindowTarget,
    };
    use serde_json::{Value, json};
    use tokio::sync::mpsc;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

    use super::{Halt, LOCKED, Report, Res, StreamOptions, ensure, interactive};
    use crate::client::{Client, Req};
    use crate::decode::{self, Decoded, Rgb};
    use crate::testwin;

    /// The phone's viewport, the same as `StreamViewerFixtureTest` asks for, so the fitted window
    /// (and the box position measured here) is the one the Android stage sees.
    const VIEWPORT: (u32, u32, f64) = (1080, 2280, 2.625);
    const STREAM_WAIT: Duration = Duration::from_secs(40);
    /// Largest per-channel difference that still counts as "the same colour" after H.264.
    const COLOUR_TOL: u8 = 48;

    // ------------------------------------------------------------------------------ events

    /// `/v1/events`: a reader task feeding `rx`, a writer task draining `out`.
    struct Events {
        rx: mpsc::UnboundedReceiver<Value>,
        out: mpsc::UnboundedSender<String>,
        tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl Drop for Events {
        fn drop(&mut self) {
            for t in &self.tasks {
                t.abort();
            }
        }
    }

    impl Events {
        async fn connect(c: &Client) -> anyhow::Result<Events> {
            let stream = c.connect().await?;
            let mut request =
                format!("ws://127.0.0.1:{}/v1/events", c.addr.port()).into_client_request()?;
            let token = c.token.clone().unwrap_or_default();
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse()?);
            let (ws, resp) = tokio_tungstenite::client_async(request, stream).await?;
            anyhow::ensure!(resp.status() == 101, "upgrade got {}", resp.status());
            let (mut sink, mut source) = ws.split();
            let (tx, rx) = mpsc::unbounded_channel();
            let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
            let reader = tokio::spawn(async move {
                while let Some(Ok(m)) = source.next().await {
                    match m {
                        Message::Text(t) => {
                            if let Ok(v) = serde_json::from_str::<Value>(t.as_str())
                                && tx.send(v).is_err()
                            {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            });
            let writer = tokio::spawn(async move {
                while let Some(text) = out_rx.recv().await {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            });
            Ok(Events {
                rx,
                out,
                tasks: vec![reader, writer],
            })
        }

        fn send(&self, ev: &ClientEvent) {
            if let Ok(t) = serde_json::to_string(ev) {
                let _ = self.out.send(t);
            }
        }

        /// The next event, `Err("socket closed")` once the socket is gone.
        async fn next(&mut self, wait: Duration) -> Result<Value, String> {
            match tokio::time::timeout(wait, self.rx.recv()).await {
                Err(_) => Err("no event in time".to_owned()),
                Ok(None) => Err("socket closed".to_owned()),
                Ok(Some(v)) => Ok(v),
            }
        }

        async fn wait_for(
            &mut self,
            what: &str,
            want: impl Fn(&Value) -> bool,
            wait: Duration,
        ) -> Result<Value, Halt> {
            let deadline = tokio::time::Instant::now() + wait;
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                match self.next(left).await {
                    Ok(v) if want(&v) => return Ok(v),
                    Ok(_) => {}
                    Err(e) => return Err(Halt::Fail(format!("waiting for {what}: {e}"))),
                }
            }
        }

        /// True when the socket closes within `wait` (events read meanwhile are dropped).
        async fn closes_within(&mut self, wait: Duration) -> bool {
            let deadline = tokio::time::Instant::now() + wait;
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                match self.next(left).await {
                    Ok(_) => {}
                    Err(e) => return e == "socket closed",
                }
            }
        }
    }

    /// Writes one command to the test host (stdout is the host's stdin).
    fn host_command(line: &str) -> Result<(), Halt> {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{line}")
            .and_then(|()| out.flush())
            .map_err(|e| Halt::Fail(format!("host command {line}: {e}")))
    }

    fn ses(v: &Value, id: &str) -> bool {
        v["session"] == id
    }

    // ------------------------------------------------------------------------------ shared checks

    struct Ctx<'a> {
        o: &'a StreamOptions,
    }

    impl Ctx<'_> {
        fn ffmpeg(&self) -> Result<&std::path::Path, Halt> {
            self.o.ffmpeg.as_deref().ok_or_else(|| {
                Halt::Deferred("deferred: ffmpeg not on PATH (needed to decode)".to_owned())
            })
        }

        /// Decodes the access units from index `from` on.
        fn decode(&self, frames: &[FrameRx], from: usize, tag: &str) -> Result<Decoded, Halt> {
            let ffmpeg = self.ffmpeg()?;
            let aus: Vec<bytes::Bytes> = frames[from..]
                .iter()
                .filter_map(|f| f.data.clone())
                .collect();
            Ok(decode::decode(ffmpeg, &aus, &self.o.work, tag)?)
        }

        /// The last picture (decoding from the last IDR).
        fn last_picture(&self, viewer: &Viewer, tag: &str) -> Result<Decoded, Halt> {
            let frames = viewer.stats.frames();
            let from = decode::from_last_idr(&frames, |f| f.idr)
                .ok_or_else(|| Halt::Fail("no IDR received".to_owned()))?;
            self.decode(&frames, from, tag)
        }
    }

    async fn wait_frames(viewer: &Viewer, n: usize, within: Duration) -> usize {
        let deadline = Instant::now() + within;
        loop {
            let have = viewer.stats.frames().len();
            if have >= n || Instant::now() > deadline {
                return have;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn check_frames(ctx: &Ctx<'_>, viewer: &Viewer, tag: &str) -> Res {
        let have = wait_frames(viewer, 60, Duration::from_secs(15)).await;
        ensure(have >= 30, || format!("only {have} access units in 15 s"))?;
        let frames = viewer.stats.frames();
        let first = frames
            .iter()
            .position(|f| f.idr)
            .ok_or_else(|| Halt::Fail("no IDR among the access units".to_owned()))?;
        let aus = frames.len() - first;
        let damaged = frames.iter().filter(|f| f.damaged).count();
        let d = ctx.decode(&frames, first, &format!("{tag}-all"))?;
        ensure(d.frames * 10 >= aus * 9 && d.frames >= 25, || {
            format!("ffmpeg decoded {} of {aus} access units", d.frames)
        })?;
        Ok(format!(
            "{aus} access units from the first IDR ({damaged} damaged), {} decoded, {}x{}",
            d.frames, d.last.width, d.last.height
        ))
    }

    /// Where the parked box is in the decoded picture.
    #[derive(Debug, Clone, Copy)]
    struct BoxAt {
        centre: (f64, f64),
    }

    /// A rectangle (x, y, w, h) in client pixels.
    type Rect = (f64, f64, f64, f64);

    /// The box's client rectangle and the client size, from the pipe.
    fn box_geometry(pid: u32) -> Result<(Rect, (f64, f64)), Halt> {
        let st = testwin::request(pid, "state?")?;
        let b: Vec<f64> = testwin::field(&st, "box")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.parse().ok())
            .collect();
        let lay = testwin::request(pid, "layout?")?;
        let c: Vec<f64> = testwin::field(&lay, "client")
            .unwrap_or_default()
            .split('x')
            .filter_map(|v| v.parse().ok())
            .collect();
        match (b.as_slice(), c.as_slice()) {
            ([x, y, w, h], [cw, ch]) => Ok(((*x, *y, *w, *h), (*cw, *ch))),
            _ => Err(Halt::Fail(format!("unreadable geometry: {st} / {lay}"))),
        }
    }

    /// Where the box centre should be in a `pw` x `ph` picture of the whole window: the client
    /// area sits at the bottom, centred horizontally (equal side borders, title bar on top).
    fn expected_centre(bx: Rect, client: (f64, f64), pw: f64, ph: f64) -> Option<(f64, f64)> {
        let side = (pw - client.0) / 2.0;
        let top = ph - client.1 - side;
        (side >= 0.0 && top >= 0.0).then(|| {
            (
                (side + bx.0 + bx.2 / 2.0) / pw,
                (top + bx.1 + bx.3 / 2.0) / ph,
            )
        })
    }

    async fn check_box(
        ctx: &Ctx<'_>,
        viewer: &Viewer,
        pid: u32,
        tag: &str,
    ) -> Result<(String, BoxAt), Halt> {
        ctx.ffmpeg()?;
        let parked_at = Instant::now();
        testwin::request(pid, "park")?;
        let (hex, index, _) = testwin::colour(pid)?;
        let want = Rgb::from_hex(&hex).ok_or_else(|| Halt::Fail(format!("bad colour {hex}")))?;
        let (bx, client) = box_geometry(pid)?;
        // The repaint has to come through capture, encode, network and decode: poll the decoded
        // picture until the box is where the window says it is.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = viewer.control_send(&ControlMessage::Keyframe).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            let d = ctx.last_picture(viewer, tag)?;
            let (pw, ph) = (d.last.width as f64, d.last.height as f64);
            let expect = expected_centre(bx, client, pw, ph);
            let last = if let Some(blob) = d.last.find(want, COLOUR_TOL).filter(|b| b.is_box(8)) {
                let off = expect.map(|(ex, ey)| {
                    ((blob.centre.0 - ex) * pw)
                        .abs()
                        .max(((blob.centre.1 - ey) * ph).abs())
                });
                if off.is_none_or(|o| o <= 12.0) {
                    let seen = d.last.sample(blob.centre.0, blob.centre.1, 2);
                    return Ok((
                        format!(
                            "parked box {hex} (palette {index}) at ({:.3}, {:.3}) of {}x{} ({}x{} px, {:.0} px from where the window says), in the decoded picture {} ms after `park`; centre reads {} (max delta {})",
                            blob.centre.0,
                            blob.centre.1,
                            d.last.width,
                            d.last.height,
                            blob.bbox.2,
                            blob.bbox.3,
                            off.unwrap_or(0.0),
                            parked_at.elapsed().as_millis(),
                            seen.hex(),
                            seen.max_delta(want)
                        ),
                        BoxAt {
                            centre: blob.centre,
                        },
                    ));
                }
                format!(
                    "box {hex} at ({:.3}, {:.3}), expected {expect:?}",
                    blob.centre.0, blob.centre.1
                )
            } else {
                format!(
                    "no box of colour {hex} in the decoded {}x{} picture",
                    d.last.width, d.last.height
                )
            };
            if Instant::now() > deadline {
                return Err(Halt::Fail(format!("after 10 s: {last}")));
            }
        }
    }

    async fn check_keepalive(viewer: &Viewer) -> Res {
        // The window is parked (static): only keepalive repeats keep the picture alive.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let from = Instant::now();
        tokio::time::sleep(Duration::from_secs(3)).await;
        let frames = viewer.stats.frames();
        let n = frames.iter().filter(|f| f.at >= from).count();
        let gap = max_gap(&frames, from).unwrap_or(Duration::from_secs(3));
        ensure(n >= 4 && gap <= Duration::from_millis(700), || {
            format!("static window: {n} access units in 3 s, max gap {gap:?}")
        })?;
        Ok(format!(
            "static window: {n} access units in 3 s, max gap {} ms",
            gap.as_millis()
        ))
    }

    fn control_states(viewer: &Viewer) -> Vec<String> {
        viewer
            .stats
            .control_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|m| match m {
                ControlMessage::State { state, .. } => Some(state.clone()),
                _ => None,
            })
            .collect()
    }

    async fn wait_control_state(viewer: &Viewer, from: usize, state: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if control_states(viewer).iter().skip(from).any(|s| s == state) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        false
    }

    /// WP-55: the test window plays a quiet 1 kHz tone (0.8 s, -30 dBFS); the engine captures
    /// its process tree by WASAPI process loopback, Opus-encodes it on the audio track, and the
    /// loopback viewer decodes it and finds the frequency within 5 Hz.
    async fn check_audio_tone(viewer: &Viewer, session: &gander_stream::Session, pid: u32) -> Res {
        use gander_stream::audio::tone;
        let deadline = Instant::now() + Duration::from_secs(3);
        while !session.audio_active() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let st = session.audio_status();
        ensure(st.on, || {
            format!(
                "no sound: {:?} ({})",
                st.reason,
                st.detail.clone().unwrap_or_default()
            )
        })?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let from = viewer.stats.audio().pcm.len();
        let reply = tokio::task::spawn_blocking(move || testwin::request(pid, "tone 1000 800"))
            .await
            .map_err(|e| Halt::Fail(e.to_string()))??;
        ensure(reply == "ok", || format!("tone: {reply}"))?;
        tokio::time::sleep(Duration::from_millis(1600)).await;
        let rx = viewer.stats.audio();
        let pcm = rx.pcm.get(from..).unwrap_or_default();
        let d = tone::detect(tone::loudest(pcm, 12_000), 48_000.0, 100.0, 4000.0)
            .ok_or_else(|| Halt::Fail(format!("no audio decoded ({} packets)", rx.packets)))?;
        let s = session.stats();
        ensure((d.hz - 1000.0).abs() <= 5.0 && d.rms_dbfs > -60.0, || {
            format!("expected 1000 Hz, found {d:?}")
        })?;
        Ok(format!(
            "testwin tone 1000 Hz -> detected {:.1} Hz at {:.1} dBFS rms (purity {:.3}); {} Opus packets received, {} lost, {} decode errors; host captured {} frames, sent {} packets",
            d.hz,
            d.rms_dbfs,
            d.purity,
            rx.packets,
            rx.lost,
            rx.decode_errors,
            s.audio_frames_captured,
            s.audio_packets_sent
        ))
    }

    async fn check_pause_resume(viewer: &Viewer) -> Res {
        let s0 = control_states(viewer).len();
        viewer.control_send(&ControlMessage::Pause).await?;
        ensure(wait_control_state(viewer, s0, "paused").await, || {
            format!("no paused state on control: {:?}", control_states(viewer))
        })?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let n0 = viewer.stats.frames().len();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let during = viewer.stats.frames().len() - n0;
        ensure(during <= 1, || {
            format!("{during} access units arrived while paused")
        })?;
        let s1 = control_states(viewer).len();
        let resumed_at = Instant::now();
        viewer.control_send(&ControlMessage::Resume).await?;
        ensure(wait_control_state(viewer, s1, "streaming").await, || {
            format!(
                "no streaming state after resume: {:?}",
                control_states(viewer)
            )
        })?;
        let n1 = viewer.stats.frames().len();
        let deadline = Instant::now() + Duration::from_secs(3);
        while viewer.stats.frames().len() <= n1 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let after = viewer.stats.frames();
        ensure(after.len() > n1, || {
            "no access unit within 3 s of resume".to_owned()
        })?;
        Ok(format!(
            "paused: {during} access units in 1.5 s; first access unit {} ms after resume (IDR: {})",
            resumed_at.elapsed().as_millis().min(3000),
            after[n1].idr
        ))
    }

    // ------------------------------------------------------------------------------ host stage

    struct Live {
        id: String,
        viewer: Viewer,
        pid: Option<u32>,
    }

    struct Host<'a> {
        o: &'a StreamOptions,
        c: Client,
        events: Option<Events>,
        /// Test windows that were running before this run (never touched).
        foreign: BTreeSet<u32>,
    }

    fn body(o: &StreamOptions) -> Req {
        Req::new("POST", "/v1/stream").json(&json!({
            "share": o.share,
            "path": o.path,
            "app": o.app,
            "viewport": { "width": VIEWPORT.0, "height": VIEWPORT.1, "dpr": VIEWPORT.2 },
        }))
    }

    impl Host<'_> {
        fn events(&mut self) -> Result<&mut Events, Halt> {
            self.events
                .as_mut()
                .ok_or_else(|| Halt::Fail("no events socket".to_owned()))
        }

        /// POST, window_found, offer/answer, streaming. With `report`, each phase is a step.
        async fn start(&mut self, mut report: Option<&mut Report>) -> Result<Live, Halt> {
            let before = testwin::pids();
            // created
            let t = Instant::now();
            let r = async {
                let r = self.c.send(&body(self.o)).await?;
                let v = r.json().unwrap_or(Value::Null);
                ensure(r.status == 201, || {
                    format!("POST /v1/stream: {}", r.describe())
                })?;
                ensure(v["state"] == "requested", || format!("201 body {v}"))?;
                let id = v["session"]
                    .as_str()
                    .ok_or_else(|| Halt::Fail("no session id".to_owned()))?;
                Ok::<_, Halt>(id.to_owned())
            }
            .await;
            let id = phase(&mut report, "host.created", t, r, |id| {
                format!("201, session {id}")
            })?;
            // window_found
            let t = Instant::now();
            let r = async {
                let ev = self.events()?;
                let found = ev
                    .wait_for(
                        "window_found",
                        |e| {
                            e["type"] == "stream.state"
                                && ses(e, &id)
                                && (e["state"] == "window_found" || e["state"] == "ended")
                        },
                        STREAM_WAIT,
                    )
                    .await?;
                ensure(found["state"] == "window_found", || {
                    format!("ended before a window was found: {found}")
                })?;
                Ok::<_, Halt>(found["window"].clone())
            }
            .await;
            phase(&mut report, "host.window_found", t, r, |w| {
                format!("window {w}")
            })?;
            let pid = testwin::wait_new(&before, Duration::from_secs(5));
            // offer / answer
            let t = Instant::now();
            let mut early_ice = Vec::new();
            let r = async {
                let ev = self.events()?;
                let deadline = tokio::time::Instant::now() + STREAM_WAIT;
                let offer = loop {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    let e = ev.next(left).await?;
                    if !ses(&e, &id) {
                        continue;
                    }
                    match e["type"].as_str() {
                        Some("stream.offer") => {
                            break e["sdp"].as_str().unwrap_or_default().to_owned();
                        }
                        Some("stream.ice") => {
                            early_ice.push(e["candidate"].as_str().unwrap_or_default().to_owned())
                        }
                        Some("stream.state") if e["state"] == "ended" => {
                            return Err(Halt::Fail(format!("ended before the offer: {e}")));
                        }
                        _ => {}
                    }
                };
                ensure(offer.contains("H264"), || "offer without H.264".to_owned())?;
                let (viewer, answer) = Viewer::answer(
                    &offer,
                    ViewerOptions {
                        keep_data: true,
                        ..ViewerOptions::default()
                    },
                )
                .await?;
                let session = gander_core::protocol::SessionId::from(id.clone());
                ev.send(&ClientEvent::StreamAnswer {
                    session: session.clone(),
                    sdp: answer.sdp,
                });
                for c in answer.candidates {
                    ev.send(&ClientEvent::StreamIce {
                        session: session.clone(),
                        candidate: c,
                    });
                }
                for c in &early_ice {
                    let _ = viewer.add_host_candidate(c).await;
                }
                Ok::<_, Halt>(viewer)
            }
            .await;
            let viewer = phase(&mut report, "host.signalling", t, r, |_| {
                "stream.offer (H.264) answered with stream.answer on /v1/events".to_owned()
            })?;
            // streaming
            let t = Instant::now();
            let r = async {
                let ev = self.events()?;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                    let e = ev
                        .next(left)
                        .await
                        .map_err(|e| Halt::Fail(format!("waiting for streaming: {e}")))?;
                    if !ses(&e, &id) {
                        continue;
                    }
                    if e["type"] == "stream.ice" {
                        let _ = viewer
                            .add_host_candidate(e["candidate"].as_str().unwrap_or_default())
                            .await;
                    } else if e["type"] == "stream.state" && e["state"] == "streaming" {
                        break;
                    } else if e["type"] == "stream.state" && e["state"] == "ended" {
                        return Err(Halt::Fail(format!("ended: {e}")));
                    }
                }
                ensure(viewer.wait_connected(Duration::from_secs(10)).await, || {
                    "the viewer's peer connection never connected".to_owned()
                })?;
                Ok::<_, Halt>(())
            }
            .await;
            phase(&mut report, "host.streaming", t, r, |()| {
                "stream.state streaming, viewer connected".to_owned()
            })?;
            Ok(Live { id, viewer, pid })
        }

        async fn delete(&mut self, id: &str, close_app: bool) -> Result<u16, Halt> {
            let target = if close_app {
                format!("/v1/stream/{id}?close_app=true")
            } else {
                format!("/v1/stream/{id}")
            };
            Ok(self.c.send(&Req::new("DELETE", target)).await?.status)
        }
    }

    /// Records a phase as a step when `report` is given; maps the value to the result.
    fn phase<T>(
        report: &mut Option<&mut Report>,
        id: &str,
        started: Instant,
        r: Result<T, Halt>,
        detail: impl FnOnce(&T) -> String,
    ) -> Result<T, Halt> {
        if let Some(rep) = report.as_deref_mut() {
            let res: Res = match &r {
                Ok(v) => Ok(detail(v)),
                Err(e) => Err(e.clone()),
            };
            rep.record(id, started, &res);
        }
        r
    }

    const HOST_SESSION_STEPS: [&str; 11] = [
        "host.created",
        "host.window_found",
        "host.signalling",
        "host.streaming",
        "host.frames_decoded",
        "host.box_pixels",
        "host.tap",
        "host.keepalive",
        "host.pause_resume",
        "host.lock_ends_session",
        "host.delete_close_app",
    ];

    pub async fn host_stage(o: &StreamOptions, report: &mut Report, unlocked: bool) {
        let started = Instant::now();
        let paired = crate::pair::pair_client(
            o.addr,
            &o.host_id,
            "e2e-stream",
            &o.share,
            &["list".to_owned(), "stream".to_owned()],
        )
        .await;
        let c = match paired {
            Ok((c, device)) => {
                report.record("host.pair", started, &Ok(format!("device {device}")));
                c
            }
            Err(e) => {
                report.record("host.pair", started, &Err(e.into()));
                return;
            }
        };
        let mut h = Host {
            o,
            c,
            events: None,
            foreign: testwin::pids(),
        };
        let r = step!(report, "host.events", async {
            let mut ev = Events::connect(&h.c).await?;
            ev.wait_for("hello", |v| v["type"] == "hello", Duration::from_secs(5))
                .await?;
            let st = ev
                .wait_for(
                    "host.state",
                    |v| v["type"] == "host.state",
                    Duration::from_secs(5),
                )
                .await?;
            h.events = Some(ev);
            Ok(format!("hello, host.state {st}"))
        });
        if r.is_err() {
            return;
        }
        let ctx = Ctx { o };
        if !unlocked {
            let _ = step!(report, "host.locked_refused", async {
                let r = h.c.send(&body(o)).await?;
                ensure(
                    r.status == 412 && r.error_code().as_deref() == Some("host_locked"),
                    || format!("expected 412 host_locked, got {}", r.describe()),
                )?;
                Ok("412 host_locked while the session is really locked (fail closed)".to_owned())
            });
            for id in HOST_SESSION_STEPS {
                report.defer(id, LOCKED);
            }
        } else {
            host_session_steps(&mut h, &ctx, report).await;
        }
        let _ = step!(report, "host.kill_switch", kill_switch(&mut h, unlocked));
        // Leave nothing behind: every test window this run started.
        for pid in testwin::pids().difference(&h.foreign) {
            testwin::quit(*pid);
        }
    }

    async fn host_session_steps(h: &mut Host<'_>, ctx: &Ctx<'_>, report: &mut Report) {
        let live = match h.start(Some(report)).await {
            Ok(l) => l,
            Err(_) => {
                for id in &HOST_SESSION_STEPS[4..] {
                    report.record(
                        id,
                        Instant::now(),
                        &Err(Halt::Fail("not run: the session did not start".to_owned())),
                    );
                }
                return;
            }
        };
        let viewer = &live.viewer;
        let _ = step!(
            report,
            "host.frames_decoded",
            check_frames(ctx, viewer, "host")
        );
        let boxed = match live.pid {
            Some(pid) => {
                let t = Instant::now();
                let r = check_box(ctx, viewer, pid, "host-box").await;
                let res: Res = r.as_ref().map(|(d, _)| d.clone()).map_err(Halt::clone);
                report.record("host.box_pixels", t, &res);
                r.ok().map(|(_, b)| b)
            }
            None => {
                report.record(
                    "host.box_pixels",
                    Instant::now(),
                    &Err(Halt::Fail("no new test window appeared".to_owned())),
                );
                None
            }
        };
        if let Some(b) = boxed {
            report
                .measurements
                .insert("box_norm".into(), json!([b.centre.0, b.centre.1]));
        }
        let tap = step!(report, "host.tap", tap(ctx, viewer, live.pid, boxed));
        if let Some(path) = &ctx.o.box_out
            && let Some(b) = boxed
        {
            let v = json!({
                "box_x": b.centre.0,
                "box_y": b.centre.1,
                "tap": tap.is_ok(),
            });
            let _ = std::fs::write(path, v.to_string());
        }
        let _ = step!(report, "host.keepalive", check_keepalive(viewer));
        let _ = step!(report, "host.pause_resume", check_pause_resume(viewer));
        let _ = step!(report, "host.lock_ends_session", lock_ends(h, &live));
        drop(live);
        let _ = step!(report, "host.delete_close_app", delete_close_app(h));
    }

    /// What the client does on a `focus_lost` right after connect: `{"t":"front"}`, then waits for
    /// the `front.result` (control texts after index `t0`).
    async fn ask_front(viewer: &Viewer, t0: usize) -> Result<String, Halt> {
        viewer.control_send_text(r#"{"t":"front"}"#).await?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let texts = viewer
                .stats
                .control_text_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(t) = texts.iter().skip(t0).find(|t| t.contains("front.result")) {
                return Ok(t.clone());
            }
            if Instant::now() > deadline {
                return Err(Halt::Fail("no front.result within 5 s".to_owned()));
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    /// Checks the host brought the test window to the front when the viewer connected
    /// (launch-policy step 7; the test host is unattended, so it should), falling back to the
    /// phone's `{"t":"front"}` as the Android client does on `focus_lost`; then taps the box on
    /// `control` and checks the colour changed on the pipe and in the decoded pixels.
    async fn tap(ctx: &Ctx<'_>, viewer: &Viewer, pid: Option<u32>, at: Option<BoxAt>) -> Res {
        let (ok, state) = interactive();
        if !ok {
            return Err(Halt::Deferred(format!("{LOCKED} [{state}]")));
        }
        let pid = pid.ok_or_else(|| Halt::Fail("no test window".to_owned()))?;
        let at = at.ok_or_else(|| Halt::Fail("not run: the box was not found".to_owned()))?;
        let cursor = gander_win::input::cursor();
        let result = async {
            let t0 = viewer
                .stats
                .control_text_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len();
            let deadline = Instant::now() + Duration::from_secs(3);
            let at_connect = loop {
                if in_front(pid) {
                    break true;
                }
                if Instant::now() > deadline {
                    break false;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            let front = if at_connect {
                "in front since connect (host step 7)".to_owned()
            } else {
                let answer = ask_front(viewer, t0).await?;
                format!("not in front at connect; phone asked: {answer}")
            };
            tokio::time::sleep(Duration::from_millis(300)).await;
            let (_, index0, hits0) = testwin::colour(pid)?;
            let (x, y) = at.centre;
            let sent = Instant::now();
            viewer
                .control_send_text(&format!(r#"{{"t":"down","b":"left","x":{x:.5},"y":{y:.5}}}"#))
                .await?;
            viewer
                .control_send_text(&format!(r#"{{"t":"up","b":"left","x":{x:.5},"y":{y:.5}}}"#))
                .await?;
            let deadline = Instant::now() + Duration::from_secs(3);
            let (hex, index1) = loop {
                let (hex, index, hits) = testwin::colour(pid)?;
                if hits > hits0 {
                    break (hex, index);
                }
                if Instant::now() > deadline {
                    let texts = viewer
                        .stats
                        .control_text_rx
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .skip(t0)
                        .cloned()
                        .collect::<Vec<_>>();
                    return Err(Halt::Fail(format!(
                        "the box was not hit within 3 s ({front}; control: {texts:?})"
                    )));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            };
            let pipe_ms = sent.elapsed().as_millis();
            let want = Rgb::from_hex(&hex)
                .ok_or_else(|| Halt::Fail(format!("bad colour {hex}")))?;
            // The new colour has to come back through capture, encode, network and decode.
            let deadline = Instant::now() + Duration::from_secs(5);
            let seen = loop {
                tokio::time::sleep(Duration::from_millis(400)).await;
                let _ = viewer.control_send(&ControlMessage::Keyframe).await;
                tokio::time::sleep(Duration::from_millis(400)).await;
                let d = ctx.last_picture(viewer, "host-tap")?;
                let seen = d.last.sample(x, y, 3);
                if seen.max_delta(want) <= COLOUR_TOL || Instant::now() > deadline {
                    break seen;
                }
            };
            ensure(seen.max_delta(want) <= COLOUR_TOL, || {
                format!(
                    "the pipe says {hex}, the decoded picture shows {} at the box",
                    seen.hex()
                )
            })?;
            Ok(format!(
                "{front}; palette {index0} -> {index1} ({hex}) on the pipe {pipe_ms} ms after the tap; decoded picture shows {} (max delta {})",
                seen.hex(),
                seen.max_delta(want)
            ))
        }
        .await;
        if let Some(p) = cursor {
            gander_win::input::set_cursor(p);
        }
        result
    }

    async fn lock_ends(h: &mut Host<'_>, live: &Live) -> Res {
        let id = live.id.clone();
        host_command("lock")?;
        let ev = h.events()?;
        let res = async {
            let err = ev
                .wait_for(
                    "error host_locked",
                    |e| e["type"] == "error" && ses(e, &id),
                    Duration::from_secs(10),
                )
                .await?;
            ensure(err["code"] == "host_locked", || {
                format!("error event {err}")
            })?;
            let end = ev
                .wait_for(
                    "ended",
                    |e| e["type"] == "stream.state" && ses(e, &id) && e["state"] == "ended",
                    Duration::from_secs(10),
                )
                .await?;
            Ok::<_, Halt>(end)
        }
        .await;
        let started = Instant::now();
        let n0 = live.viewer.stats.frames().len();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let after = live.viewer.stats.frames().len() - n0;
        host_command("unlock")?;
        ev.wait_for(
            "host.state unlocked",
            |v| v["type"] == "host.state" && v["locked"] == false,
            Duration::from_secs(10),
        )
        .await?;
        let end = res?;
        ensure(after <= 1, || {
            format!("{after} access units arrived after the session ended")
        })?;
        let _ = started;
        Ok(format!(
            "error host_locked, then stream.state ended ({}); {after} access units afterwards",
            end["reason"]
        ))
    }

    async fn delete_close_app(h: &mut Host<'_>) -> Res {
        let live = h.start(None).await?;
        let pid = live
            .pid
            .ok_or_else(|| Halt::Fail("no new test window for session 2".to_owned()))?;
        let status = h.delete(&live.id, true).await?;
        ensure(status == 204, || {
            format!("DELETE ?close_app=true: HTTP {status}")
        })?;
        let id = live.id.clone();
        let end = h
            .events()?
            .wait_for(
                "ended",
                |e| e["type"] == "stream.state" && ses(e, &id) && e["state"] == "ended",
                Duration::from_secs(10),
            )
            .await?;
        ensure(end["reason"] == "client_closed", || format!("ended {end}"))?;
        let t = Instant::now();
        ensure(testwin::wait_gone(pid, Duration::from_secs(10)), || {
            format!("test window {pid} still running 10 s after DELETE ?close_app=true")
        })?;
        Ok(format!(
            "204, ended client_closed, test window {pid} closed {} ms later",
            t.elapsed().as_millis()
        ))
    }

    async fn kill_switch(h: &mut Host<'_>, unlocked: bool) -> Res {
        let live = if unlocked {
            Some(h.start(None).await?)
        } else {
            None
        };
        host_command("kill")?;
        let closed = h.events()?.closes_within(Duration::from_secs(5)).await;
        ensure(closed, || {
            "the events socket stayed open after the kill switch".to_owned()
        })?;
        let mut detail = "events socket dropped".to_owned();
        if let Some(live) = &live {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            let n0 = live.viewer.stats.frames().len();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let after = live.viewer.stats.frames().len() - n0;
            ensure(after <= 1, || {
                format!("{after} access units still arriving after the kill switch")
            })?;
            detail += &format!("; media stopped ({after} access units in 1.5 s)");
        }
        // The host still serves: a new socket works, the session is gone.
        let mut ev = Events::connect(&h.c).await?;
        ev.wait_for("hello", |v| v["type"] == "hello", Duration::from_secs(5))
            .await?;
        h.events = Some(ev);
        detail += "; a new events socket gets hello";
        if let Some(live) = live {
            let status = h.delete(&live.id, false).await?;
            ensure(status == 404, || {
                format!("DELETE after the kill switch: HTTP {status}, expected 404")
            })?;
            detail += "; the session is gone (DELETE 404)";
            if let Some(pid) = live.pid {
                testwin::quit(pid);
            }
        }
        Ok(detail)
    }

    // ------------------------------------------------------------------------------ engine stage

    /// A test window started by the engine stage; quit (or killed) on drop.
    struct OwnTestwin {
        child: Child,
        pid: u32,
        hwnd: isize,
    }

    impl OwnTestwin {
        fn spawn(exe: &std::path::Path) -> anyhow::Result<Self> {
            let mut child = Command::new(exe)
                .args([
                    "--self-test-secs",
                    "180",
                    "--no-focus",
                    "--title",
                    "WP-57 engine",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let mut line = String::new();
            BufReader::new(
                child
                    .stdout
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("stdout"))?,
            )
            .read_line(&mut line)?;
            let get = |k: &str| testwin::field(&line, k).map(str::to_owned);
            let pid = get("pid").and_then(|p| p.parse().ok());
            let hwnd = get("hwnd")
                .and_then(|h| isize::from_str_radix(h.trim_start_matches("0x"), 16).ok());
            match (pid, hwnd) {
                (Some(pid), Some(hwnd)) => Ok(Self { child, pid, hwnd }),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("unexpected ready line {line:?}")
                }
            }
        }
    }

    impl Drop for OwnTestwin {
        fn drop(&mut self) {
            testwin::quit(self.pid);
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = self.child.try_wait() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    pub async fn engine_stage(o: &StreamOptions, report: &mut Report) {
        let ctx = Ctx { o };
        let tw = match OwnTestwin::spawn(&o.testwin) {
            Ok(t) => t,
            Err(e) => {
                report.record("engine.start", Instant::now(), &Err(e.into()));
                return;
            }
        };
        let sink = Arc::new(CollectingSink::default());
        let mut events = sink.take_receiver();
        let lock = Arc::new(FakeLock::new(false));
        let deps = EngineDeps {
            lock: lock.clone(),
            power: Arc::new(FixedPower::new(false)),
            addrs: Arc::new(StaticAddrs(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])),
            signal: sink,
            source: Arc::new(WgcSource {
                prefer_luid: None,
                prefer_vendor: None,
                border_required: None,
            }),
            audio: Some(Arc::new(gander_stream::win::ProcessLoopback)),
        };
        let cfg = EngineConfig {
            udp_ports: UdpPortRange::new(54_100, 54_139).expect("a valid range"),
            ..EngineConfig::default()
        };
        let engine = StreamEngine::new(cfg, deps);
        let device = DeviceId::from("dev_e2e_stream");
        let mut slot = None;
        let _ = step!(report, "engine.start", async {
            let session = engine
                .start(
                    WindowTarget {
                        hwnd: tw.hwnd,
                        title: "gander-testwin".into(),
                    },
                    Viewport {
                        width: VIEWPORT.0,
                        height: VIEWPORT.1,
                        dpr: VIEWPORT.2,
                    },
                    device.clone(),
                )
                .await
                .map_err(|e| Halt::Fail(format!("start: {e}")))?;
            let id = session.id().clone();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let offer = loop {
                let ev = tokio::time::timeout_at(deadline, events.recv())
                    .await
                    .map_err(|_| Halt::Fail("no stream.offer within 10 s".to_owned()))?
                    .ok_or_else(|| Halt::Fail("signal sink closed".to_owned()))?;
                if let ServerEvent::StreamOffer { session: s, sdp } = ev.1
                    && s == id
                {
                    break sdp;
                }
            };
            let (viewer, answer) = Viewer::answer(
                &offer,
                ViewerOptions {
                    keep_data: true,
                    ..ViewerOptions::default()
                },
            )
            .await?;
            let _ = engine.handle_client_event(
                &device,
                ClientEvent::StreamAnswer {
                    session: id.clone(),
                    sdp: answer.sdp,
                },
            );
            for c in answer.candidates {
                let _ = engine.handle_client_event(
                    &device,
                    ClientEvent::StreamIce {
                        session: id.clone(),
                        candidate: c,
                    },
                );
            }
            let mut rx = session.watch_state();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
            while *rx.borrow_and_update() != SessionState::Streaming {
                if tokio::time::timeout_at(deadline, rx.changed())
                    .await
                    .is_err()
                {
                    return Err(Halt::Fail(format!("never streamed: {:?}", session.state())));
                }
            }
            let info = session.stats().source;
            slot = Some((session, viewer));
            Ok(format!(
                "streaming: {} / {} at {}x{}",
                info.adapter, info.encoder, info.width, info.height
            ))
        });
        let Some((session, viewer)) = slot else {
            for id in [
                "engine.frames_decoded",
                "engine.box_pixels",
                "engine.keepalive",
                "engine.pause_resume",
                "engine.audio_tone",
                "engine.lock_guard",
            ] {
                report.record(
                    id,
                    Instant::now(),
                    &Err(Halt::Fail("not run: the engine did not stream".to_owned())),
                );
            }
            return;
        };
        let _ = step!(
            report,
            "engine.frames_decoded",
            check_frames(&ctx, &viewer, "engine")
        );
        let t = Instant::now();
        let r = check_box(&ctx, &viewer, tw.pid, "engine-box").await;
        report.record(
            "engine.box_pixels",
            t,
            &r.as_ref().map(|(d, _)| d.clone()).map_err(Halt::clone),
        );
        let _ = step!(report, "engine.keepalive", check_keepalive(&viewer));
        let _ = step!(report, "engine.pause_resume", check_pause_resume(&viewer));
        let _ = step!(
            report,
            "engine.audio_tone",
            check_audio_tone(&viewer, &session, tw.pid)
        );
        let _ = step!(report, "engine.lock_guard", async {
            let t = Instant::now();
            lock.set_locked(true);
            let reason = tokio::time::timeout(Duration::from_secs(3), session.ended())
                .await
                .map_err(|_| Halt::Fail("the session kept running after the lock".to_owned()))?;
            ensure(reason == EndReason::HostLocked, || {
                format!("ended with {reason:?}")
            })?;
            Ok(format!(
                "a (faked) lock ended the session in {} ms with HostLocked",
                t.elapsed().as_millis()
            ))
        });
        viewer.close().await;
        engine.stop_all(EndReason::ClientClosed);
        drop(tw);
    }

    /// The test window `pid` (or a window it owns) is the foreground window.
    pub fn in_front(pid: u32) -> bool {
        testwin::main_hwnd(pid)
            .is_ok_and(|hwnd| gander_win::input::InputTarget::new(hwnd, pid).is_foreground())
    }

    /// The foreground procedure of the product (`gander_win::input::foreground`), Alt-key trick
    /// off, on the test window `pid`. Only for `park-watch --front` (optional; the host now
    /// brings the window forward itself).
    pub fn bring_to_front(pid: u32) -> bool {
        let Ok(hwnd) = testwin::main_hwnd(pid) else {
            return false;
        };
        let target = gander_win::input::InputTarget::new(hwnd, pid);
        let policy = gander_win::input::AcquirePolicy {
            alt_key: false,
            ..gander_win::input::AcquirePolicy::default()
        };
        let mut log = gander_core::input::activity::InjectionLog::new();
        gander_win::input::foreground::acquire(&target, &policy, &mut log).is_ok_and(|r| r.acquired)
    }
}
