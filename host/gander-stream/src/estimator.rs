// SPDX-License-Identifier: Apache-2.0
//! A delay-based bandwidth estimator behind webrtc-rs's [`BandwidthEstimator`] interface
//! (condition (a) of the D12 acceptance, `docs/specs/2026-11-01-stream-spike-findings.md`).
//!
//! # Why not GCC
//!
//! webrtc-rs 0.21's `Gcc` reacted to loss only in the spike: with a 2 s bottleneck buffer its
//! delay half stayed at the ceiling while the queue filled, and the encoder output fell under the
//! cap after 5.5 s. This estimator watches the queueing delay directly.
//!
//! # Algorithm
//!
//! Every TWCC report gives, per packet, the instant it left this endpoint (the pacer's release
//! instant) and the instant it arrived on the receiver's clock. Their difference is the one-way
//! delay plus an unknown, constant clock offset. Its minimum over the last [`Config::base_window`]
//! is the path's base delay; anything above it is queueing delay.
//!
//! - **Queue estimate**: per report batch, the smallest queueing delay of the packets that arrived
//!   (a standing queue delays every packet; one late packet is jitter, not a queue).
//! - **Delivered rate**: bytes that arrived over the last [`Config::rate_window`] of receiver time.
//! - **Overuse** (queue above [`Config::overuse`] in two consecutive batches): the target becomes
//!   `min(target, factor x delivered rate)`, with `factor` 0.85, or 0.6 while the queue is above
//!   [`Config::drain`] so it empties instead of standing. The rule is idempotent: repeating it
//!   while the queue drains does not compound.
//! - **Loss** above 10 % over the last second: `target x (1 - loss / 2)`, at most every 300 ms
//!   (the loss half, as in GCC; a shallow buffer overflows before it builds much delay).
//! - **Increase**: while the queue is under [`Config::underuse`] and loss under 2 %, the target
//!   grows by [`Config::increase_per_sec`] (multiplicative, per second of report time), but never
//!   above `1.5 x delivered rate + 300 kbit/s` (an application-limited sender cannot prove more).
//!
//! Limits: a base-delay increase (route change) reads as queueing for up to `base_window`, which
//! lowers the rate until the window forgets the old minimum. Clock drift between sender and
//! receiver is ignored (it is ppm-level over a 10 s window).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rtc::interceptor::{BandwidthEstimator, EstimatorStats, PacketReport};

/// Estimator settings.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Start estimate, bit/s.
    pub initial_bps: f64,
    /// Floor, bit/s.
    pub min_bps: f64,
    /// Ceiling, bit/s.
    pub max_bps: f64,
    /// Queueing delay that counts as overuse.
    pub overuse: Duration,
    /// Queueing delay above which the cut drains the queue (factor 0.6 instead of 0.85).
    pub drain: Duration,
    /// Queueing delay under which the target may grow.
    pub underuse: Duration,
    /// Multiplicative growth per second while under-used (0.15 = +15 %/s).
    pub increase_per_sec: f64,
    /// Window for the delivered rate (receiver time).
    pub rate_window: Duration,
    /// Window over which the minimum one-way delay is the base delay.
    pub base_window: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            initial_bps: 3_000_000.0,
            min_bps: 300_000.0,
            max_bps: 8_000_000.0,
            overuse: Duration::from_millis(50),
            drain: Duration::from_millis(250),
            underuse: Duration::from_millis(20),
            increase_per_sec: 0.15,
            rate_window: Duration::from_millis(500),
            base_window: Duration::from_secs(10),
        }
    }
}

/// The estimator's numbers, shared with the session (lock-free reads).
#[derive(Debug, Default)]
pub struct Published {
    target_bps: AtomicU64,
    queue_us: AtomicU64,
    delivered_bps: AtomicU64,
    loss_permille: AtomicU64,
    overuse_events: AtomicU64,
}

impl Published {
    /// Current target, bit/s.
    pub fn target_bps(&self) -> f64 {
        f64::from_bits(self.target_bps.load(Ordering::Relaxed))
    }
    /// Last queueing-delay estimate.
    pub fn queue_delay(&self) -> Duration {
        Duration::from_micros(self.queue_us.load(Ordering::Relaxed))
    }
    /// Delivered rate over the rate window, bit/s.
    pub fn delivered_bps(&self) -> f64 {
        f64::from_bits(self.delivered_bps.load(Ordering::Relaxed))
    }
    /// Loss over the last second, 0..1.
    pub fn loss(&self) -> f64 {
        self.loss_permille.load(Ordering::Relaxed) as f64 / 1000.0
    }
    /// Times the delay rule cut the target.
    pub fn overuse_events(&self) -> u64 {
        self.overuse_events.load(Ordering::Relaxed)
    }
}

/// The delay-based estimator. Plug it into `configure_congestion_control`.
#[derive(Debug)]
pub struct DelayEstimator {
    cfg: Config,
    target: f64,
    /// First departure seen: the sender-side epoch.
    epoch: Option<Instant>,
    /// (departure, one-way delay in ns incl. offset), a monotonic deque for the windowed minimum.
    owd_min: VecDeque<(Instant, i64)>,
    /// (arrival on receiver clock, bytes).
    arrivals: VecDeque<(Duration, usize)>,
    /// (report time, arrived?) for the loss window.
    fates: VecDeque<(Instant, bool)>,
    /// The pacer has been told the target at least once (see [`DelayEstimator::update`]).
    announced: bool,
    overuse_streak: u32,
    last_loss_cut: Option<Instant>,
    last_increase: Option<Instant>,
    queue: Duration,
    delivered: f64,
    loss: f64,
    published: Arc<Published>,
}

impl DelayEstimator {
    /// A new estimator; its numbers are mirrored into [`DelayEstimator::published`].
    pub fn new(cfg: Config) -> Self {
        let published = Arc::new(Published::default());
        let me = Self {
            target: cfg.initial_bps.clamp(cfg.min_bps, cfg.max_bps),
            cfg,
            epoch: None,
            owd_min: VecDeque::new(),
            arrivals: VecDeque::new(),
            fates: VecDeque::new(),
            announced: false,
            overuse_streak: 0,
            last_loss_cut: None,
            last_increase: None,
            queue: Duration::ZERO,
            delivered: 0.0,
            loss: 0.0,
            published,
        };
        me.publish();
        me
    }

    /// The shared view of the numbers.
    pub fn published(&self) -> Arc<Published> {
        Arc::clone(&self.published)
    }

    fn publish(&self) {
        let p = &self.published;
        p.target_bps.store(self.target.to_bits(), Ordering::Relaxed);
        p.queue_us
            .store(self.queue.as_micros() as u64, Ordering::Relaxed);
        p.delivered_bps
            .store(self.delivered.to_bits(), Ordering::Relaxed);
        p.loss_permille
            .store((self.loss * 1000.0) as u64, Ordering::Relaxed);
    }

    /// Feeds one batch; also the body of [`BandwidthEstimator::on_reports`].
    pub fn update(&mut self, now: Instant, reports: &[PacketReport]) {
        if reports.is_empty() {
            return;
        }
        let epoch = *self.epoch.get_or_insert(reports[0].departure);
        let mut batch_queue: Option<i64> = None;
        for r in reports {
            self.fates.push_back((now, r.arrived));
            let Some(arrival) = r.arrival.filter(|_| r.arrived) else {
                continue;
            };
            self.arrivals.push_back((arrival, r.size));
            let dep_ns = r.departure.saturating_duration_since(epoch).as_nanos() as i64
                - epoch.saturating_duration_since(r.departure).as_nanos() as i64;
            let owd = arrival.as_nanos() as i64 - dep_ns;
            // Monotonic deque: the front is the minimum of the window.
            while self.owd_min.back().is_some_and(|&(_, v)| v >= owd) {
                self.owd_min.pop_back();
            }
            self.owd_min.push_back((r.departure, owd));
            while self.owd_min.front().is_some_and(|&(t, _)| {
                r.departure.saturating_duration_since(t) > self.cfg.base_window
            }) {
                self.owd_min.pop_front();
            }
            let base = self.owd_min.front().map_or(owd, |&(_, v)| v);
            let q = (owd - base).max(0);
            batch_queue = Some(batch_queue.map_or(q, |b: i64| b.min(q)));
        }
        // Windows.
        if let Some(&(last, _)) = self.arrivals.back() {
            while self
                .arrivals
                .front()
                .is_some_and(|&(t, _)| last.saturating_sub(t) > self.cfg.rate_window)
            {
                self.arrivals.pop_front();
            }
            let span = last
                .saturating_sub(self.arrivals.front().map_or(last, |a| a.0))
                .max(Duration::from_millis(100));
            let bytes: usize = self.arrivals.iter().map(|a| a.1).sum();
            self.delivered = bytes as f64 * 8.0 / span.as_secs_f64();
        }
        while self
            .fates
            .front()
            .is_some_and(|&(t, _)| now.saturating_duration_since(t) > Duration::from_secs(1))
        {
            self.fates.pop_front();
        }
        let lost = self.fates.iter().filter(|f| !f.1).count();
        self.loss = if self.fates.is_empty() {
            0.0
        } else {
            lost as f64 / self.fates.len() as f64
        };

        let before = self.target;
        if let Some(q) = batch_queue {
            self.queue = Duration::from_nanos(q as u64);
            if self.queue > self.cfg.overuse {
                self.overuse_streak += 1;
            } else {
                self.overuse_streak = 0;
            }
        }
        if self.overuse_streak >= 2 && self.delivered > 0.0 {
            let factor = if self.queue > self.cfg.drain {
                0.6
            } else {
                0.85
            };
            let cut = factor * self.delivered;
            if cut < self.target {
                self.target = cut;
                self.published
                    .overuse_events
                    .fetch_add(1, Ordering::Relaxed);
            }
            self.last_increase = Some(now);
        } else if self.loss > 0.10 {
            if self
                .last_loss_cut
                .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(300))
            {
                self.target *= 1.0 - self.loss / 2.0;
                self.last_loss_cut = Some(now);
            }
            self.last_increase = Some(now);
        } else if self.queue < self.cfg.underuse && self.loss < 0.02 {
            let dt = self
                .last_increase
                .map_or(Duration::ZERO, |t| now.saturating_duration_since(t))
                .min(Duration::from_millis(500));
            self.last_increase = Some(now);
            let grown = self.target * (1.0 + self.cfg.increase_per_sec * dt.as_secs_f64());
            let limit = (1.5 * self.delivered + 300_000.0).max(before);
            self.target = grown.min(limit);
        } else {
            // Hold between the thresholds.
            self.last_increase = Some(now);
        }
        self.target = self.target.clamp(self.cfg.min_bps, self.cfg.max_bps);
        // rtc 0.21 starts its pacer at 1 Mbit/s and tells it the estimate only when the
        // estimate CHANGES (`TargetBitrateChanged`). An estimate that still equals the initial
        // value after the first report would leave the pacer at 1 Mbit/s, so the first report
        // always moves the target by 1 bit/s.
        if !self.announced {
            self.announced = true;
            if self.target == before {
                self.target = if self.target < self.cfg.max_bps {
                    self.target + 1.0
                } else {
                    self.target - 1.0
                };
            }
        }
        self.publish();
    }
}

impl BandwidthEstimator for DelayEstimator {
    fn on_reports(&mut self, now: Instant, reports: &[PacketReport]) {
        self.update(now, reports);
    }

    fn target_bitrate(&self) -> f64 {
        self.target
    }

    fn stats(&self) -> EstimatorStats {
        let mut s = EstimatorStats::default();
        s.delay_based_bitrate = Some(self.target);
        s.packet_loss = Some(self.loss);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtc::rtcp::transport_feedbacks::cc_feedback_report::Ecn;

    /// A clean bottleneck: sender paced at 0.85 x target, a token-bucket link with a drop-tail
    /// buffer, TWCC-style reports every 100 ms. Returns the seconds after the cap until the
    /// target and the sender rate are both at or under the cap.
    fn simulate(cap_bps: f64, buffer: Duration) -> (f64, f64) {
        let t0 = Instant::now();
        let mut est = DelayEstimator::new(Config {
            initial_bps: 8_000_000.0,
            ..Config::default()
        });
        let pkt = 1200usize;
        let mut link_free = Duration::ZERO; // when the link finishes the queued packets
        let mut pending: Vec<PacketReport> = Vec::new();
        let mut next_report = Duration::from_millis(100);
        let mut t = Duration::ZERO;
        let cap_at = Duration::from_secs(3);
        let mut target_ok = None;
        let mut id = 0u64;
        while t < Duration::from_secs(12) {
            let send_bps = est.target_bitrate() * 0.85;
            let gap = Duration::from_secs_f64(pkt as f64 * 8.0 / send_bps);
            let now = t0 + t;
            let capped = t >= cap_at;
            let (arrived, arrival) = if capped {
                let start = link_free.max(t);
                if start - t > buffer {
                    (false, None)
                } else {
                    link_free = start + Duration::from_secs_f64(pkt as f64 * 8.0 / cap_bps);
                    (true, Some(link_free + Duration::from_millis(5)))
                }
            } else {
                (true, Some(t + Duration::from_millis(5)))
            };
            id += 1;
            pending.push(PacketReport {
                ssrc: 1,
                id,
                rtp_sequence_number: id as u16,
                is_twcc: true,
                twcc_sequence_number: id as u16,
                size: pkt,
                arrived,
                departure: now,
                arrival,
                ecn: Ecn::default(),
            });
            t += gap;
            if t >= next_report {
                // Report packets whose arrival (or drop) is known by now.
                let (ready, rest): (Vec<_>, Vec<_>) = pending
                    .into_iter()
                    .partition(|r| r.arrival.is_none_or(|a| a <= t));
                pending = rest;
                est.update(t0 + t, &ready);
                next_report += Duration::from_millis(100);
                if capped && target_ok.is_none() && est.target_bitrate() * 0.85 <= cap_bps {
                    target_ok = Some((t - cap_at).as_secs_f64());
                }
            }
        }
        (
            target_ok.unwrap_or(f64::INFINITY),
            est.published().queue_delay().as_secs_f64(),
        )
    }

    #[test]
    fn adapts_within_a_second_with_a_shallow_buffer() {
        let (s, _) = simulate(1_500_000.0, Duration::from_millis(200));
        assert!(s < 1.0, "sender under the cap after {s} s");
    }

    #[test]
    fn adapts_within_a_second_with_a_deep_buffer_and_drains_it() {
        let (s, q) = simulate(1_500_000.0, Duration::from_secs(2));
        assert!(s < 1.0, "sender under the cap after {s} s");
        assert!(q < 0.3, "queue still {q} s at the end");
    }

    #[test]
    fn grows_when_the_path_is_free_but_not_past_the_delivered_rate() {
        let t0 = Instant::now();
        let mut est = DelayEstimator::new(Config::default());
        for i in 0..50u64 {
            let now = t0 + Duration::from_millis(100 * i);
            // 10 packets of 1200 B per 100 ms = 960 kbit/s delivered, no queue.
            let reports: Vec<_> = (0..10)
                .map(|k| PacketReport {
                    ssrc: 1,
                    id: i * 10 + k,
                    rtp_sequence_number: 0,
                    is_twcc: true,
                    twcc_sequence_number: 0,
                    size: 1200,
                    arrived: true,
                    departure: now + Duration::from_millis(10 * k),
                    arrival: Some(Duration::from_millis(100 * i + 10 * k + 3)),
                    ecn: Ecn::default(),
                })
                .collect();
            est.update(now + Duration::from_millis(100), &reports);
        }
        let t = est.target_bitrate();
        assert!(t >= 3_000_000.0, "never cut on a free path: {t}");
        assert!(t <= 1.5 * 960_000.0 + 300_000.0 + 3_000_000.0, "{t}");
    }

    #[test]
    fn heavy_loss_cuts() {
        let t0 = Instant::now();
        let mut est = DelayEstimator::new(Config::default());
        let reports: Vec<_> = (0..20u64)
            .map(|k| PacketReport {
                ssrc: 1,
                id: k,
                rtp_sequence_number: 0,
                is_twcc: true,
                twcc_sequence_number: 0,
                size: 1200,
                arrived: k % 2 == 0,
                departure: t0 + Duration::from_millis(5 * k),
                arrival: (k % 2 == 0).then(|| Duration::from_millis(5 * k + 1)),
                ecn: Ecn::default(),
            })
            .collect();
        est.update(t0 + Duration::from_millis(100), &reports);
        assert!(est.target_bitrate() < 3_000_000.0 * 0.8);
        assert!((est.published().loss() - 0.5).abs() < 0.01);
    }
}
