// SPDX-License-Identifier: Apache-2.0
//! Render queue: bounded concurrency, visible-first priority, de-duplication by key.
//!
//! - `concurrency` jobs run at once (thumbnails: 2), each on its own queue thread. A job is one
//!   call of the handler, which for thumbnails spawns one worker process.
//! - Two lanes: [`Priority::Visible`] is always drained before [`Priority::Prefetch`]. A visible
//!   request for a job already waiting in the prefetch lane promotes it.
//! - A request for a key that is already queued returns its 1-based position, and the caller
//!   answers `503 generating` with `retry_after_ms = 200 * position` (renderer spec).
//! - Clients cancel by no longer asking: the queue records when each job was last requested, and
//!   a job not requested again within `stale_after` is dropped when it reaches the front (the
//!   row scrolled out of view). [`RenderQueue::cancel`] drops one explicitly.
//! - [`RenderQueue::set_paused`] holds every queued job (on battery below 15 %, WP-17).

use std::collections::{HashSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Retry hint per queue position (renderer spec: 200 ms x position).
pub const RETRY_MS_PER_POSITION: u64 = 200;

/// Lane of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Priority {
    /// The row is on screen (`priority=visible`).
    Visible,
    /// Anything else (prefetch, background).
    #[default]
    Prefetch,
}

/// Queue tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueConfig {
    /// Jobs running at once.
    pub concurrency: usize,
    /// Most jobs waiting; beyond it the oldest prefetch (else oldest visible) job is dropped.
    pub max_queued: usize,
    /// A waiting job not re-requested for this long is dropped instead of run.
    pub stale_after: Duration,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            concurrency: 2,
            max_queued: 1024,
            stale_after: Duration::from_secs(10),
        }
    }
}

/// Where a submitted key stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enqueued {
    /// Waiting at this 1-based position (visible lane first).
    Queued {
        /// 1-based position across both lanes.
        position: usize,
    },
    /// Being rendered right now.
    Running,
}

impl Enqueued {
    /// `retry_after_ms` for the `503 generating` answer.
    pub fn retry_after_ms(self) -> u64 {
        match self {
            Enqueued::Queued { position } => RETRY_MS_PER_POSITION * position.max(1) as u64,
            Enqueued::Running => RETRY_MS_PER_POSITION,
        }
    }
}

struct Entry<J> {
    key: String,
    payload: J,
    last_requested: Instant,
}

struct State<J> {
    visible: VecDeque<Entry<J>>,
    prefetch: VecDeque<Entry<J>>,
    running: HashSet<String>,
    paused: bool,
    shutdown: bool,
}

type Handler<J> = dyn Fn(&str, J) + Send + Sync;

struct Inner<J> {
    state: Mutex<State<J>>,
    wake: Condvar,
    handler: Box<Handler<J>>,
    config: QueueConfig,
}

impl<J> Inner<J> {
    fn lock(&self) -> MutexGuard<'_, State<J>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A fixed pool of queue threads running `handler(key, payload)` for submitted jobs.
pub struct RenderQueue<J: Send + 'static> {
    inner: Arc<Inner<J>>,
    threads: Vec<JoinHandle<()>>,
}

impl<J: Send + 'static> std::fmt::Debug for RenderQueue<J> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderQueue")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl<J: Send + 'static> RenderQueue<J> {
    /// Starts `config.concurrency` queue threads.
    pub fn new(config: QueueConfig, handler: impl Fn(&str, J) + Send + Sync + 'static) -> Self {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                visible: VecDeque::new(),
                prefetch: VecDeque::new(),
                running: HashSet::new(),
                paused: false,
                shutdown: false,
            }),
            wake: Condvar::new(),
            handler: Box::new(handler),
            config,
        });
        let threads = (0..config.concurrency.max(1))
            .map(|i| {
                let inner = Arc::clone(&inner);
                thread::Builder::new()
                    .name(format!("gander-render-queue-{i}"))
                    .spawn(move || run(&inner))
                    .expect("spawn render queue thread")
            })
            .collect();
        Self { inner, threads }
    }

    /// Submits `key`, creating the payload only if the key is not already queued or running.
    pub fn submit(&self, key: &str, priority: Priority, payload: impl FnOnce() -> J) -> Enqueued {
        let mut st = self.inner.lock();
        if st.running.contains(key) {
            return Enqueued::Running;
        }
        let now = Instant::now();
        if let Some(i) = st.visible.iter().position(|e| e.key == key) {
            st.visible[i].last_requested = now;
            return Enqueued::Queued { position: i + 1 };
        }
        if let Some(i) = st.prefetch.iter().position(|e| e.key == key) {
            if priority == Priority::Visible {
                let mut e = st.prefetch.remove(i).expect("index from position");
                e.last_requested = now;
                st.visible.push_back(e);
                let position = st.visible.len();
                return Enqueued::Queued { position };
            }
            st.prefetch[i].last_requested = now;
            return Enqueued::Queued {
                position: st.visible.len() + i + 1,
            };
        }
        if st.visible.len() + st.prefetch.len() >= self.inner.config.max_queued
            && st.prefetch.pop_front().is_none()
        {
            st.visible.pop_front();
        }
        let entry = Entry {
            key: key.to_owned(),
            payload: payload(),
            last_requested: now,
        };
        let position = match priority {
            Priority::Visible => {
                st.visible.push_back(entry);
                st.visible.len()
            }
            Priority::Prefetch => {
                st.prefetch.push_back(entry);
                st.visible.len() + st.prefetch.len()
            }
        };
        drop(st);
        self.inner.wake.notify_one();
        Enqueued::Queued { position }
    }

    /// Where `key` stands, if it is queued or running.
    pub fn status(&self, key: &str) -> Option<Enqueued> {
        let st = self.inner.lock();
        if st.running.contains(key) {
            return Some(Enqueued::Running);
        }
        st.visible
            .iter()
            .chain(st.prefetch.iter())
            .position(|e| e.key == key)
            .map(|i| Enqueued::Queued { position: i + 1 })
    }

    /// Drops a waiting job (a running one finishes). Returns whether one was dropped.
    pub fn cancel(&self, key: &str) -> bool {
        let mut st = self.inner.lock();
        let before = st.visible.len() + st.prefetch.len();
        st.visible.retain(|e| e.key != key);
        st.prefetch.retain(|e| e.key != key);
        before != st.visible.len() + st.prefetch.len()
    }

    /// Pauses or resumes starting new jobs.
    pub fn set_paused(&self, paused: bool) {
        self.inner.lock().paused = paused;
        self.inner.wake.notify_all();
    }

    /// Jobs waiting (both lanes).
    pub fn queued_len(&self) -> usize {
        let st = self.inner.lock();
        st.visible.len() + st.prefetch.len()
    }
}

impl<J: Send + 'static> Drop for RenderQueue<J> {
    fn drop(&mut self) {
        {
            let mut st = self.inner.lock();
            st.shutdown = true;
            st.visible.clear();
            st.prefetch.clear();
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
                    let next = match st.visible.pop_front() {
                        Some(e) => Some(e),
                        None => st.prefetch.pop_front(),
                    };
                    if let Some(e) = next {
                        if e.last_requested.elapsed() > inner.config.stale_after {
                            continue;
                        }
                        st.running.insert(e.key.clone());
                        break e;
                    }
                }
                st = inner.wake.wait(st).unwrap_or_else(|p| p.into_inner());
            }
        };
        let key = entry.key;
        let _ = catch_unwind(AssertUnwindSafe(|| (inner.handler)(&key, entry.payload)));
        inner.lock().running.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A handler that blocks until released, recording the order of keys it ran.
    fn gated() -> (
        impl Fn(&str, ()) + Send + Sync + 'static,
        mpsc::Receiver<String>,
        mpsc::Sender<()>,
    ) {
        let (ran_tx, ran_rx) = mpsc::channel::<String>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let ran_tx = Mutex::new(ran_tx);
        let handler = move |key: &str, ()| {
            ran_tx.lock().unwrap().send(key.to_owned()).unwrap();
            let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(5));
        };
        (handler, ran_rx, go_tx)
    }

    fn recv(rx: &mpsc::Receiver<String>) -> String {
        rx.recv_timeout(Duration::from_secs(5)).expect("job ran")
    }

    #[test]
    fn concurrency_two_then_visible_first() {
        let (handler, ran, go) = gated();
        let q = RenderQueue::new(QueueConfig::default(), handler);
        q.submit("a", Priority::Prefetch, || ());
        q.submit("b", Priority::Prefetch, || ());
        let mut first = [recv(&ran), recv(&ran)];
        first.sort();
        assert_eq!(first, ["a".to_owned(), "b".to_owned()]);
        // Both slots busy: these wait.
        assert_eq!(
            q.submit("p1", Priority::Prefetch, || ()),
            Enqueued::Queued { position: 1 }
        );
        assert_eq!(
            q.submit("v1", Priority::Visible, || ()),
            Enqueued::Queued { position: 1 }
        );
        assert_eq!(q.status("p1"), Some(Enqueued::Queued { position: 2 }));
        assert_eq!(q.submit("a", Priority::Visible, || ()), Enqueued::Running);
        // Release one slot: the visible job runs before the earlier prefetch job.
        go.send(()).unwrap();
        assert_eq!(recv(&ran), "v1");
        go.send(()).unwrap();
        assert_eq!(recv(&ran), "p1");
        go.send(()).unwrap();
        go.send(()).unwrap();
    }

    #[test]
    fn duplicate_submit_keeps_one_job_and_visible_promotes() {
        let (handler, ran, go) = gated();
        let q = RenderQueue::new(
            QueueConfig {
                concurrency: 1,
                ..QueueConfig::default()
            },
            handler,
        );
        q.submit("busy", Priority::Visible, || ());
        assert_eq!(recv(&ran), "busy");
        q.submit("x", Priority::Prefetch, || ());
        q.submit("y", Priority::Prefetch, || ());
        let mut made = false;
        let again = q.submit("x", Priority::Prefetch, || made = true);
        assert!(!made, "payload is not rebuilt for a queued key");
        assert_eq!(again, Enqueued::Queued { position: 1 });
        assert_eq!(again.retry_after_ms(), 200);
        // Promote y.
        assert_eq!(
            q.submit("y", Priority::Visible, || ()),
            Enqueued::Queued { position: 1 }
        );
        assert_eq!(q.status("x"), Some(Enqueued::Queued { position: 2 }));
        assert_eq!(
            Enqueued::Queued { position: 2 }.retry_after_ms(),
            400,
            "200 ms x position"
        );
        go.send(()).unwrap();
        assert_eq!(recv(&ran), "y");
        go.send(()).unwrap();
        assert_eq!(recv(&ran), "x");
        go.send(()).unwrap();
    }

    #[test]
    fn stale_and_cancelled_jobs_are_dropped() {
        let (handler, ran, go) = gated();
        let q = RenderQueue::new(
            QueueConfig {
                concurrency: 1,
                // Wide enough that "busy" and "fresh" are never stale by the time a worker picks
                // them up, even on a loaded machine (50 ms was not: the whole-workspace run timed
                // out waiting for a job the queue had dropped as stale).
                stale_after: Duration::from_millis(750),
                ..QueueConfig::default()
            },
            handler,
        );
        q.submit("busy", Priority::Visible, || ());
        assert_eq!(recv(&ran), "busy");
        q.submit("stale", Priority::Visible, || ());
        q.submit("gone", Priority::Visible, || ());
        assert!(q.cancel("gone"));
        thread::sleep(Duration::from_millis(1_000));
        q.submit("fresh", Priority::Visible, || ());
        go.send(()).unwrap();
        assert_eq!(recv(&ran), "fresh");
        go.send(()).unwrap();
        assert_eq!(q.queued_len(), 0);
    }

    #[test]
    fn paused_queue_holds_jobs() {
        let (handler, ran, go) = gated();
        let q = RenderQueue::new(QueueConfig::default(), handler);
        q.set_paused(true);
        q.submit("held", Priority::Visible, || ());
        assert!(ran.recv_timeout(Duration::from_millis(150)).is_err());
        q.set_paused(false);
        assert_eq!(recv(&ran), "held");
        go.send(()).unwrap();
    }
}
