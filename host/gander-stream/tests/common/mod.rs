// SPDX-License-Identifier: Apache-2.0
//! Shared harness: an engine on 127.0.0.1 with a collecting sink, and the loopback viewer
//! answering through `handle_client_event` exactly as `/v1/events` would deliver it.
#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use gander_core::config::UdpPortRange;
use gander_core::protocol::DeviceId;
use gander_core::protocol::events::{ClientEvent, ServerEvent};
use gander_core::protocol::stream::Viewport;
use gander_stream::source::VideoSource;
use gander_stream::testing::{CollectingSink, Viewer, ViewerOptions};
use gander_stream::{
    EngineConfig, EngineDeps, FakeLock, FixedPower, Session, SessionState, StaticAddrs,
    StreamEngine, WindowTarget,
};
use tokio::sync::mpsc;

pub type Events = mpsc::UnboundedReceiver<(DeviceId, ServerEvent)>;

/// Every test gets its own 20-port slice of 53000-55999, so parallel tests do not fight over
/// ports (each still probes, so a busy port only shifts the pick).
pub fn port_range() -> UdpPortRange {
    static NEXT: AtomicU16 = AtomicU16::new(0);
    let i = NEXT.fetch_add(1, Ordering::Relaxed) % 150;
    let start = 53_000 + i * 20;
    UdpPortRange::new(start, start + 19).expect("range")
}

pub fn device() -> DeviceId {
    DeviceId::from("dev_test")
}

pub fn viewport() -> Viewport {
    Viewport {
        width: 1080,
        height: 2280,
        dpr: 2.625,
    }
}

pub struct Harness {
    pub engine: StreamEngine,
    pub deps: EngineDeps,
    pub events: Events,
    pub lock: Arc<FakeLock>,
    pub power: Arc<FixedPower>,
    pub range: UdpPortRange,
}

pub fn harness(cfg: EngineConfig, source: Arc<dyn VideoSource>) -> Harness {
    let sink = Arc::new(CollectingSink::default());
    let events = sink.take_receiver();
    let lock = Arc::new(FakeLock::new(false));
    let power = Arc::new(FixedPower::new(false));
    let range = cfg.udp_ports;
    let deps = EngineDeps {
        lock: lock.clone(),
        power: power.clone(),
        addrs: Arc::new(StaticAddrs(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])),
        signal: sink,
        source,
        audio: None,
    };
    let engine = StreamEngine::new(cfg, deps.clone());
    Harness {
        engine,
        deps,
        events,
        lock,
        power,
        range,
    }
}

pub fn test_config() -> EngineConfig {
    EngineConfig {
        udp_ports: port_range(),
        ..EngineConfig::default()
    }
}

/// The next event matching `f`, within `timeout`.
pub async fn next_event<T>(
    events: &mut Events,
    timeout: Duration,
    mut f: impl FnMut(&ServerEvent) -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let ev = tokio::time::timeout_at(deadline, events.recv())
            .await
            .ok()??;
        if let Some(t) = f(&ev.1) {
            return Some(t);
        }
    }
}

/// Starts a session, answers it with a viewer and waits for `Streaming`.
pub async fn connect(
    h: &mut Harness,
    window: WindowTarget,
    opts: ViewerOptions,
) -> (Session, Viewer) {
    let session = h
        .engine
        .start(window, viewport(), device())
        .await
        .expect("start");
    let id = session.id().clone();
    let offer = next_event(&mut h.events, Duration::from_secs(5), |e| match e {
        ServerEvent::StreamOffer { session, sdp } if *session == id => Some(sdp.clone()),
        _ => None,
    })
    .await
    .expect("stream.offer");
    let (viewer, answer) = Viewer::answer(&offer, opts).await.expect("viewer answer");
    assert!(
        h.engine
            .handle_client_event(
                &device(),
                ClientEvent::StreamAnswer {
                    session: id.clone(),
                    sdp: answer.sdp,
                },
            )
            .is_none()
    );
    for c in answer.candidates {
        assert!(
            h.engine
                .handle_client_event(
                    &device(),
                    ClientEvent::StreamIce {
                        session: id.clone(),
                        candidate: c,
                    },
                )
                .is_none()
        );
    }
    assert!(
        wait_state(&session, Duration::from_secs(15), |s| *s
            == SessionState::Streaming)
        .await,
        "session never streamed: {:?}",
        session.state()
    );
    (session, viewer)
}

/// Waits until the session state satisfies `f`.
pub async fn wait_state(
    session: &Session,
    timeout: Duration,
    f: impl Fn(&SessionState) -> bool,
) -> bool {
    let mut rx = session.watch_state();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if f(&rx.borrow_and_update()) {
            return true;
        }
        if tokio::time::timeout_at(deadline, rx.changed())
            .await
            .is_err()
        {
            return f(&rx.borrow());
        }
    }
}

/// Polls `f` every 50 ms until true or `timeout`; returns the elapsed time when it became true.
pub async fn until(timeout: Duration, mut f: impl FnMut() -> bool) -> Option<Duration> {
    let t = Instant::now();
    while t.elapsed() < timeout {
        if f() {
            return Some(t.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

/// One row of an adaptation timeline.
#[derive(Debug, Clone)]
pub struct Row {
    pub t: f64,
    pub estimate_kbps: f64,
    pub setpoint_kbps: f64,
    pub applied_kbps: f64,
    pub output_kbps: f64,
    pub received_kbps: f64,
    pub queue_ms: f64,
    pub est_queue_ms: f64,
    pub drops: u64,
}

/// What an adaptation run measured (seconds after the cap went on).
#[derive(Debug, Clone)]
pub struct Adaptation {
    pub buffer_ms: u64,
    pub cap_kbps: f64,
    pub before: Row,
    pub first_cut_s: Option<f64>,
    pub estimate_under_cap_s: Option<f64>,
    pub output_under_cap_s: Option<f64>,
    pub queue_under_300ms_s: Option<f64>,
    pub peak_queue_ms: f64,
    pub settled: Row,
    pub rows: Vec<Row>,
}

impl std::fmt::Display for Adaptation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = |v: Option<f64>| v.map_or("never".to_owned(), |x| format!("{x:.2} s"));
        writeln!(
            f,
            "cap {:.0} kbps, buffer {} ms: before: estimate {:.0} set {:.0} output {:.0} kbps | first set-point cut {} | estimate <= cap {} | encoded output (1 s window) <= cap {} | peak queue {:.0} ms, queue < 300 ms {} | settled (last 2 s): estimate {:.0} set {:.0} output {:.0} received {:.0} kbps, queue {:.0} ms, drops {}",
            self.cap_kbps,
            self.buffer_ms,
            self.before.estimate_kbps,
            self.before.setpoint_kbps,
            self.before.output_kbps,
            s(self.first_cut_s),
            s(self.estimate_under_cap_s),
            s(self.output_under_cap_s),
            self.peak_queue_ms,
            s(self.queue_under_300ms_s),
            self.settled.estimate_kbps,
            self.settled.setpoint_kbps,
            self.settled.output_kbps,
            self.settled.received_kbps,
            self.settled.queue_ms,
            self.settled.drops
        )?;
        writeln!(
            f,
            "t_s,estimate_kbps,setpoint_kbps,applied_kbps,output_kbps,received_kbps,relay_queue_ms,estimator_queue_ms,drops"
        )?;
        for r in &self.rows {
            writeln!(
                f,
                "{:.2},{:.0},{:.0},{:.0},{:.0},{:.0},{:.0},{:.0},{}",
                r.t,
                r.estimate_kbps,
                r.setpoint_kbps,
                r.applied_kbps,
                r.output_kbps,
                r.received_kbps,
                r.queue_ms,
                r.est_queue_ms,
                r.drops
            )?;
        }
        Ok(())
    }
}

/// Lets the stream ramp for `warm`, caps the relay at `cap_bps` and samples every 100 ms for
/// `capped`.
pub async fn measure_adaptation(
    session: &Session,
    viewer: &Viewer,
    relay: &gander_stream::testing::Relay,
    buffer_ms: u64,
    cap_bps: u64,
    warm: Duration,
    capped: Duration,
) -> Adaptation {
    use std::collections::VecDeque;
    let mut hist: VecDeque<(Instant, u64)> = VecDeque::new();
    let row = |t: f64, hist: &mut VecDeque<(Instant, u64)>| {
        let st = session.stats();
        let now = Instant::now();
        hist.push_back((now, st.bytes_sent));
        while hist.len() > 1 && now.duration_since(hist[1].0) >= Duration::from_secs(1) {
            hist.pop_front();
        }
        let (t0, b0) = hist[0];
        let span = now.duration_since(t0).as_secs_f64().max(0.1);
        let rs = &relay.state;
        Row {
            t,
            estimate_kbps: st.estimate_bps / 1000.0,
            setpoint_kbps: st.setpoint_bps as f64 / 1000.0,
            applied_kbps: st.applied_bps as f64 / 1000.0,
            output_kbps: (st.bytes_sent - b0) as f64 * 8.0 / span / 1000.0,
            received_kbps: viewer.stats.rate_bps(Duration::from_secs(1)) / 1000.0,
            queue_ms: rs.queue_us.load(Ordering::Relaxed) as f64 / 1000.0,
            est_queue_ms: st.queue_delay.as_secs_f64() * 1000.0,
            drops: rs.dropped.load(Ordering::Relaxed),
        }
    };
    let start = Instant::now();
    let mut before = row(0.0, &mut hist);
    while start.elapsed() < warm {
        tokio::time::sleep(Duration::from_millis(100)).await;
        before = row(0.0, &mut hist);
    }
    relay.set_cap(cap_bps);
    let cap_kbps = cap_bps as f64 / 1000.0;
    let t_cap = Instant::now();
    let mut rows = Vec::new();
    while t_cap.elapsed() < capped {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rows.push(row(t_cap.elapsed().as_secs_f64(), &mut hist));
    }
    relay.set_cap(0);
    let first = |p: &dyn Fn(&Row) -> bool| rows.iter().find(|r| p(r)).map(|r| r.t);
    let peak_queue_ms = rows.iter().map(|r| r.queue_ms).fold(0.0, f64::max);
    let peak_at = rows
        .iter()
        .find(|r| r.queue_ms >= peak_queue_ms)
        .map_or(0.0, |r| r.t);
    let tail: Vec<&Row> = rows
        .iter()
        .filter(|r| r.t >= capped.as_secs_f64() - 2.0)
        .collect();
    let mean =
        |g: &dyn Fn(&Row) -> f64| tail.iter().map(|r| g(r)).sum::<f64>() / tail.len().max(1) as f64;
    let settled = Row {
        t: capped.as_secs_f64(),
        estimate_kbps: mean(&|r| r.estimate_kbps),
        setpoint_kbps: mean(&|r| r.setpoint_kbps),
        applied_kbps: mean(&|r| r.applied_kbps),
        output_kbps: mean(&|r| r.output_kbps),
        received_kbps: mean(&|r| r.received_kbps),
        queue_ms: mean(&|r| r.queue_ms),
        est_queue_ms: mean(&|r| r.est_queue_ms),
        drops: tail.last().map_or(0, |r| r.drops),
    };
    Adaptation {
        buffer_ms,
        cap_kbps,
        first_cut_s: first(&|r| r.setpoint_kbps < before.setpoint_kbps * 0.85),
        estimate_under_cap_s: first(&|r| r.estimate_kbps <= cap_kbps),
        output_under_cap_s: first(&|r| r.output_kbps <= cap_kbps),
        queue_under_300ms_s: rows
            .iter()
            .find(|r| r.t >= peak_at && r.queue_ms < 300.0)
            .map(|r| r.t),
        peak_queue_ms,
        before,
        settled,
        rows,
    }
}
