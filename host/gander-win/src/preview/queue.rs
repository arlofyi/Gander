// SPDX-License-Identifier: Apache-2.0
//! The preview queue: at most 2 previews render at once overall and at most 1 per device
//! (renderer spec, "Scheduling"; protocol spec, "previews run on their own queue").
//!
//! - Jobs are de-duplicated by key: asking again for a queued or running key adds nothing and
//!   returns where it stands (the caller answers `503 generating`).
//! - Fairness: the longest-waiting job whose device has nothing running goes first, so one chatty
//!   device cannot starve the others and cannot use both slots.
//! - Clients cancel by no longer asking: a job not requested again within `stale_after` is
//!   dropped when it reaches the front, and [`PreviewQueue::cancel`] drops one explicitly.
//! - Deep-zoom tiles (WP-67) are submitted with [`PreviewQueue::submit_low`]: they start only when
//!   no page, info or metadata job is waiting for a free slot (a running job is never
//!   interrupted), keep the same per-device and overall limits, and are dropped sooner when the
//!   client stops asking.
//! - [`PreviewQueue::set_paused`] holds every waiting job (battery below 15 %).

use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::thumbs::queue::Enqueued;

/// Queue tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewQueueConfig {
    /// Previews rendering at once, across all devices. Spec: 2.
    pub total: usize,
    /// Previews rendering at once for one device. Spec: 1.
    pub per_device: usize,
    /// Most jobs waiting; beyond it the oldest waiting job is dropped.
    pub max_queued: usize,
    /// A waiting job not re-requested for this long is dropped instead of run.
    pub stale_after: Duration,
}

impl Default for PreviewQueueConfig {
    fn default() -> Self {
        Self {
            total: 2,
            per_device: 1,
            max_queued: 256,
            stale_after: Duration::from_secs(60),
        }
    }
}

struct Entry<J> {
    key: String,
    device: String,
    payload: J,
    last_requested: Instant,
    /// Deep-zoom tiles go after everything else.
    low: bool,
    /// Dropped when not requested again within this long (`None`: the queue's default).
    stale_after: Option<Duration>,
}

struct State<J> {
    waiting: VecDeque<Entry<J>>,
    /// Running keys and the device each runs for.
    running: HashMap<String, String>,
    paused: bool,
    shutdown: bool,
}

type Handler<J> = dyn Fn(&str, J) + Send + Sync;

struct Inner<J> {
    state: Mutex<State<J>>,
    wake: Condvar,
    handler: Box<Handler<J>>,
    config: PreviewQueueConfig,
}

impl<J> Inner<J> {
    fn lock(&self) -> MutexGuard<'_, State<J>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn running_for(st: &State<J>, device: &str) -> usize {
        st.running.values().filter(|d| d.as_str() == device).count()
    }

    /// How many waiting jobs start before the one at `index`, plus one: a normal job waits only
    /// behind the normal jobs queued earlier, a low-priority one behind every normal job and the
    /// low-priority jobs queued earlier.
    fn position(st: &State<J>, index: usize) -> usize {
        let low = st.waiting[index].low;
        1 + st
            .waiting
            .iter()
            .enumerate()
            .filter(|(i, e)| {
                if low {
                    !e.low || *i < index
                } else {
                    !e.low && *i < index
                }
            })
            .count()
    }

    fn is_stale(&self, e: &Entry<J>) -> bool {
        e.last_requested.elapsed() > e.stale_after.unwrap_or(self.config.stale_after)
    }
}

/// A fixed pool of queue threads running `handler(key, payload)`, bounded per device and overall.
pub struct PreviewQueue<J: Send + 'static> {
    inner: Arc<Inner<J>>,
    threads: Vec<JoinHandle<()>>,
}

impl<J: Send + 'static> std::fmt::Debug for PreviewQueue<J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewQueue")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl<J: Send + 'static> PreviewQueue<J> {
    /// Starts `config.total` queue threads.
    pub fn new(
        config: PreviewQueueConfig,
        handler: impl Fn(&str, J) + Send + Sync + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                waiting: VecDeque::new(),
                running: HashMap::new(),
                paused: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
            handler: Box::new(handler),
            config,
        });
        let threads = (0..config.total.max(1))
            .map(|i| {
                let inner = Arc::clone(&inner);
                thread::Builder::new()
                    .name(format!("gander-preview-queue-{i}"))
                    .spawn(move || run(&inner))
                    .expect("spawn preview queue thread")
            })
            .collect();
        Self { inner, threads }
    }

    /// Submits `key` for `device`, creating the payload only if the key is not already queued or
    /// running.
    pub fn submit(&self, key: &str, device: &str, payload: impl FnOnce() -> J) -> Enqueued {
        self.submit_entry(key, device, false, None, payload)
    }

    /// [`submit`](Self::submit) for a job that may wait: it starts only when no normal job is
    /// waiting for a slot, and is dropped when not requested again within `stale_after`. When
    /// the queue is full, low-priority jobs are dropped before normal ones.
    pub fn submit_low(
        &self,
        key: &str,
        device: &str,
        stale_after: Duration,
        payload: impl FnOnce() -> J,
    ) -> Enqueued {
        self.submit_entry(key, device, true, Some(stale_after), payload)
    }

    fn submit_entry(
        &self,
        key: &str,
        device: &str,
        low: bool,
        stale_after: Option<Duration>,
        payload: impl FnOnce() -> J,
    ) -> Enqueued {
        let mut st = self.inner.lock();
        if st.running.contains_key(key) {
            return Enqueued::Running;
        }
        // Positions count live jobs only.
        st.waiting.retain(|e| !self.inner.is_stale(e));
        let now = Instant::now();
        if let Some(i) = st.waiting.iter().position(|e| e.key == key) {
            st.waiting[i].last_requested = now;
            return Enqueued::Queued {
                position: Inner::position(&st, i),
            };
        }
        if st.waiting.len() >= self.inner.config.max_queued {
            // Tiles first: a flood of them must not push out a page someone is waiting for.
            let victim = st.waiting.iter().position(|e| e.low).unwrap_or(0);
            st.waiting.remove(victim);
        }
        st.waiting.push_back(Entry {
            key: key.to_owned(),
            device: device.to_owned(),
            payload: payload(),
            last_requested: now,
            low,
            stale_after,
        });
        let position = Inner::position(&st, st.waiting.len() - 1);
        drop(st);
        self.inner.wake.notify_all();
        Enqueued::Queued { position }
    }

    /// Where `key` stands, if it is queued or running.
    pub fn status(&self, key: &str) -> Option<Enqueued> {
        let st = self.inner.lock();
        if st.running.contains_key(key) {
            return Some(Enqueued::Running);
        }
        st.waiting
            .iter()
            .position(|e| e.key == key)
            .map(|i| Enqueued::Queued {
                position: Inner::position(&st, i),
            })
    }

    /// Drops a waiting job (a running one finishes). Returns whether one was dropped.
    pub fn cancel(&self, key: &str) -> bool {
        let mut st = self.inner.lock();
        let before = st.waiting.len();
        st.waiting.retain(|e| e.key != key);
        before != st.waiting.len()
    }

    /// Pauses or resumes starting new jobs.
    pub fn set_paused(&self, paused: bool) {
        self.inner.lock().paused = paused;
        self.inner.wake.notify_all();
    }

    /// Jobs waiting.
    pub fn queued_len(&self) -> usize {
        self.inner.lock().waiting.len()
    }

    /// Jobs running.
    pub fn running_len(&self) -> usize {
        self.inner.lock().running.len()
    }
}

impl<J: Send + 'static> Drop for PreviewQueue<J> {
    fn drop(&mut self) {
        {
            let mut st = self.inner.lock();
            st.shutdown = true;
            st.waiting.clear();
        }
        self.inner.wake.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn run<J>(inner: &Inner<J>) {
    loop {
        let entry = {
            let mut st = inner.lock();
            loop {
                if st.shutdown {
                    return;
                }
                if !st.paused {
                    // Drop stale jobs, then take the first normal job whose device is free, else
                    // the first low-priority one.
                    st.waiting.retain(|e| !inner.is_stale(e));
                    if st.running.len() < inner.config.total {
                        let free = |i: &usize| {
                            Inner::running_for(&st, &st.waiting[*i].device)
                                < inner.config.per_device
                        };
                        let pick = (0..st.waiting.len())
                            .find(|i| !st.waiting[*i].low && free(i))
                            .or_else(|| (0..st.waiting.len()).find(|i| free(i)));
                        if let Some(e) = pick.and_then(|i| st.waiting.remove(i)) {
                            st.running.insert(e.key.clone(), e.device.clone());
                            break e;
                        }
                    }
                }
                st = inner.wake.wait(st).unwrap_or_else(|p| p.into_inner());
            }
        };
        let key = entry.key;
        let _ = catch_unwind(AssertUnwindSafe(|| (inner.handler)(&key, entry.payload)));
        inner.lock().running.remove(&key);
        // A finished job frees a device slot and a total slot for the other threads.
        inner.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// A handler that records when it starts, blocks until released, and tracks concurrency.
    struct Gate {
        started: mpsc::Receiver<String>,
        release: mpsc::Sender<()>,
        peak_total: Arc<AtomicUsize>,
    }

    fn gated(config: PreviewQueueConfig) -> (PreviewQueue<()>, Gate) {
        let (started_tx, started_rx) = mpsc::channel::<String>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let started_tx = Mutex::new(started_tx);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (a, p) = (Arc::clone(&active), Arc::clone(&peak));
        let queue = PreviewQueue::new(config, move |key: &str, ()| {
            let now = a.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            started_tx.lock().unwrap().send(key.to_owned()).unwrap();
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
            a.fetch_sub(1, Ordering::SeqCst);
        });
        (
            queue,
            Gate {
                started: started_rx,
                release: release_tx,
                peak_total: peak,
            },
        )
    }

    fn next(g: &Gate) -> String {
        g.started
            .recv_timeout(Duration::from_secs(5))
            .expect("a job started")
    }

    fn nothing_starts(g: &Gate) -> bool {
        g.started.recv_timeout(Duration::from_millis(200)).is_err()
    }

    #[test]
    fn one_per_device_two_overall() {
        let (q, g) = gated(PreviewQueueConfig::default());
        for k in ["a1", "a2", "a3"] {
            q.submit(k, "dev-a", || ());
        }
        q.submit("b1", "dev-b", || ());
        q.submit("c1", "dev-c", || ());
        // a1 starts; a2 and a3 may not (same device); b1 takes the second slot; c1 waits.
        let mut first = [next(&g), next(&g)];
        first.sort();
        assert_eq!(first, ["a1".to_owned(), "b1".to_owned()]);
        assert!(nothing_starts(&g), "two running, nothing else may start");
        assert_eq!(q.running_len(), 2);
        assert_eq!(q.status("a2"), Some(Enqueued::Queued { position: 1 }));
        assert_eq!(q.status("a1"), Some(Enqueued::Running));

        // Finishing b1 frees a slot: c1 (other device) goes before a2 (a's slot is still busy).
        g.release.send(()).unwrap();
        let after = next(&g);
        assert!(after == "c1" || after == "a2", "{after}");
        // Release everything and let the rest run in turn.
        for _ in 0..6 {
            let _ = g.release.send(());
        }
        let mut seen = vec![after];
        while let Ok(k) = g.started.recv_timeout(Duration::from_millis(500)) {
            seen.push(k);
            let _ = g.release.send(());
        }
        seen.sort();
        assert_eq!(seen, ["a2", "a3", "c1"]);
        assert!(
            g.peak_total.load(Ordering::SeqCst) <= 2,
            "never more than 2 at once"
        );
    }

    #[test]
    fn a_busy_device_does_not_block_others() {
        let (q, g) = gated(PreviewQueueConfig::default());
        q.submit("a1", "dev-a", || ());
        assert_eq!(next(&g), "a1");
        q.submit("a2", "dev-a", || ());
        q.submit("b1", "dev-b", || ());
        // b1 overtakes the earlier a2: a2 has to wait for a1, b1 has a free slot.
        assert_eq!(next(&g), "b1");
        assert!(nothing_starts(&g));
        let _ = g.release.send(());
        let _ = g.release.send(());
        assert_eq!(next(&g), "a2");
        let _ = g.release.send(());
    }

    #[test]
    fn duplicates_stale_and_cancelled_jobs() {
        let (q, g) = gated(PreviewQueueConfig {
            total: 1,
            stale_after: Duration::from_millis(80),
            ..PreviewQueueConfig::default()
        });
        q.submit("busy", "d", || ());
        assert_eq!(next(&g), "busy");
        let mut built = 0;
        q.submit("x", "d", || built += 1);
        assert_eq!(
            q.submit("x", "d", || built += 1),
            Enqueued::Queued { position: 1 }
        );
        assert_eq!(built, 1, "the payload is not rebuilt for a queued key");
        assert_eq!(q.submit("busy", "d", || ()), Enqueued::Running);
        q.submit("gone", "d", || ());
        assert!(q.cancel("gone"));
        assert!(!q.cancel("gone"));
        thread::sleep(Duration::from_millis(150));
        q.submit("fresh", "d", || ());
        let _ = g.release.send(());
        // `x` went stale while waiting and is dropped; `fresh` runs.
        assert_eq!(next(&g), "fresh");
        let _ = g.release.send(());
        assert_eq!(q.queued_len(), 0);
    }

    #[test]
    fn tiles_start_after_every_waiting_page() {
        let (q, g) = gated(PreviewQueueConfig {
            total: 1,
            ..PreviewQueueConfig::default()
        });
        let long = Duration::from_secs(30);
        q.submit("busy", "d", || ());
        assert_eq!(next(&g), "busy");
        q.submit_low("tile1", "d", long, || ());
        q.submit_low("tile2", "d", long, || ());
        q.submit("page", "d", || ());
        // The page was queued last and still goes first; the tiles keep their order.
        assert_eq!(q.status("page"), Some(Enqueued::Queued { position: 1 }));
        assert_eq!(q.status("tile1"), Some(Enqueued::Queued { position: 2 }));
        assert_eq!(q.status("tile2"), Some(Enqueued::Queued { position: 3 }));
        let _ = g.release.send(());
        assert_eq!(next(&g), "page");
        let _ = g.release.send(());
        assert_eq!(next(&g), "tile1");
        let _ = g.release.send(());
        assert_eq!(next(&g), "tile2");
        let _ = g.release.send(());
    }

    #[test]
    fn a_waiting_tile_does_not_block_a_page_of_another_device() {
        let (q, g) = gated(PreviewQueueConfig::default());
        q.submit("a-page", "dev-a", || ());
        assert_eq!(next(&g), "a-page");
        q.submit_low("a-tile", "dev-a", Duration::from_secs(30), || ());
        q.submit("b-page", "dev-b", || ());
        // dev-a is busy, so its tile cannot start; dev-b's page takes the other slot.
        assert_eq!(next(&g), "b-page");
        assert!(nothing_starts(&g));
        for _ in 0..4 {
            let _ = g.release.send(());
        }
        assert_eq!(next(&g), "a-tile");
        let _ = g.release.send(());
    }

    #[test]
    fn tiles_are_dropped_sooner_than_pages_when_nobody_asks_again() {
        let (q, g) = gated(PreviewQueueConfig {
            total: 1,
            stale_after: Duration::from_secs(30),
            ..PreviewQueueConfig::default()
        });
        q.submit("busy", "d", || ());
        assert_eq!(next(&g), "busy");
        q.submit_low("old-tile", "d", Duration::from_millis(60), || ());
        q.submit_low("kept-tile", "d", Duration::from_secs(30), || ());
        thread::sleep(Duration::from_millis(120));
        // Asking again keeps a tile alive.
        assert_eq!(
            q.submit_low("kept-tile", "d", Duration::from_secs(30), || ()),
            Enqueued::Queued { position: 1 }
        );
        let _ = g.release.send(());
        assert_eq!(next(&g), "kept-tile");
        assert_eq!(q.queued_len(), 0, "the stale tile was dropped, not run");
        let _ = g.release.send(());
    }

    #[test]
    fn a_full_queue_drops_tiles_before_pages() {
        let (q, g) = gated(PreviewQueueConfig {
            total: 1,
            max_queued: 3,
            ..PreviewQueueConfig::default()
        });
        let long = Duration::from_secs(30);
        q.submit("busy", "d", || ());
        assert_eq!(next(&g), "busy");
        q.submit("page1", "d", || ());
        q.submit_low("tile1", "d", long, || ());
        q.submit_low("tile2", "d", long, || ());
        // Full: a new page evicts the oldest tile, not page1.
        q.submit("page2", "d", || ());
        assert!(q.status("page1").is_some());
        assert_eq!(q.status("tile1"), None);
        assert!(q.status("tile2").is_some());
        assert!(q.status("page2").is_some());
        for _ in 0..8 {
            let _ = g.release.send(());
        }
    }

    #[test]
    fn paused_queue_holds_jobs() {
        let (q, g) = gated(PreviewQueueConfig::default());
        q.set_paused(true);
        q.submit("held", "d", || ());
        assert!(nothing_starts(&g));
        q.set_paused(false);
        assert_eq!(next(&g), "held");
        let _ = g.release.send(());
    }

    #[test]
    fn a_panicking_job_frees_its_slots() {
        let ran = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&ran);
        let q = PreviewQueue::new(PreviewQueueConfig::default(), move |key: &str, ()| {
            r.fetch_add(1, Ordering::SeqCst);
            assert!(key != "boom", "renderer blew up");
        });
        q.submit("boom", "d", || ());
        q.submit("fine", "d", || ());
        let deadline = Instant::now() + Duration::from_secs(5);
        while ran.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            ran.load(Ordering::SeqCst),
            2,
            "the device slot was released"
        );
    }
}
