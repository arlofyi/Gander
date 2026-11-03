// SPDX-License-Identifier: Apache-2.0
//! Which directories are watched, and for how long: the [`WatchRegistry`].
//!
//! `share.changed` is not free: every watched directory is an open handle and a pending
//! `ReadDirectoryChangesW`. So only directories somebody looked at recently are watched
//! (protocol spec: "shares that have been listed ... in the last 10 minutes"):
//!
//! - `GET /list` (WP-14) calls [`WatchRegistry::touch_armed`] with the share and the
//!   share-relative directory BEFORE it enumerates the directory ("arm, then list"): the first
//!   call waits (a few milliseconds, at most 250 ms) until the OS watch is open, so a change made
//!   while the listing is being read is reported instead of lost. Later calls return at once.
//!   [`WatchRegistry::touch`] is the non-blocking variant for callers that cannot await; there a
//!   change in the few milliseconds before the watch opens goes unnoticed.
//! - Each touch pushes the directory's expiry out to now + `ttl` (10 min). An untouched directory
//!   stops being watched at its expiry (checked at least every `sweep_interval`).
//! - At most `max_watches` directories are watched; a touch beyond that evicts the directory
//!   closest to expiry.
//! - Changes are COALESCED: the first change starts a `debounce` (500 ms) window, everything else
//!   in the window folds into it, and ONE `share.changed` goes out when it ends. The window is
//!   armed again only by the next change, so a directory under constant writes yields at most two
//!   events per second and a quiet one yields none.
//!
//! The OS side is a [`WatchBackend`] (production: `system::WindowsWatch`), so the policy here is
//! tested without a filesystem.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gander_core::protocol::ShareId;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// How long [`WatchRegistry::touch_armed`] waits for the OS watch to open.
const ARM_TIMEOUT: Duration = Duration::from_millis(250);

/// Longest share-relative directory a touch may name (the resolver's limit is shorter).
const MAX_DIR_LEN: usize = 4096;

/// What a watch reports to its [`ChangeNotifier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    /// Something in the directory changed (or the OS dropped changes: treat it all as changed).
    Changed,
    /// The watch is over (directory deleted, volume gone). The registry forgets it, emits a final
    /// change and watches again on the next touch.
    Ended,
}

/// Called by a backend, from any thread, when its watch sees a change. Cheap and non-blocking.
pub type ChangeNotifier = Arc<dyn Fn(WatchEvent) + Send + Sync>;

/// A running OS watch; dropping it stops the watch.
pub type WatchHandle = Box<dyn Send + 'static>;

/// Opens OS watches. May block (it resolves the directory and opens handles); the registry calls
/// it on the blocking pool.
pub trait WatchBackend: Send + Sync + fmt::Debug + 'static {
    /// Starts watching `dir` (share-relative, `/`-separated, empty for the share root) of `share`
    /// and calls `changed` whenever something in that directory (not below it) changes. Errors
    /// (unknown share, directory gone, refused by the path resolver) mean "do not watch".
    fn watch(&self, share: &ShareId, dir: &str, changed: ChangeNotifier)
    -> io::Result<WatchHandle>;
}

/// Where coalesced changes go: `(share, share-relative directory)`.
pub type ChangeSink = Arc<dyn Fn(&ShareId, &str) + Send + Sync>;

/// Timing and limits of the registry.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// How long a directory stays watched after its last touch (10 min).
    pub ttl: Duration,
    /// Coalescing window (500 ms).
    pub debounce: Duration,
    /// Directories watched at once (64).
    pub max_watches: usize,
    /// Upper bound between expiry checks (30 s).
    pub sweep_interval: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(10 * 60),
            debounce: Duration::from_millis(500),
            max_watches: 64,
            sweep_interval: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    share: ShareId,
    dir: String,
}

impl Key {
    /// `None` for a directory name no listing could have produced.
    fn new(share: &ShareId, dir: &str) -> Option<Self> {
        let dir = dir.trim_matches('/');
        (dir.len() <= MAX_DIR_LEN).then(|| Self {
            share: share.clone(),
            dir: dir.to_owned(),
        })
    }
}

#[derive(Debug)]
struct Shared {
    /// Directories somebody wants watched, with their expiry. The source of truth: the task makes
    /// the running watches match it.
    interest: Mutex<HashMap<Key, Instant>>,
    /// Directories whose OS watch is open (maintained by the task).
    armed: Mutex<HashSet<Key>>,
    /// Signalled whenever `armed` or `interest` may have changed.
    armed_changed: Notify,
    wake: Notify,
    watching: AtomicUsize,
    config: WatchConfig,
}

/// One running watch owned by the task.
struct Watching {
    _handle: WatchHandle,
    /// Set by the backend's notifier, cleared by the task right BEFORE it emits, so a change that
    /// lands during the emission starts the next window instead of being lost.
    dirty: Arc<AtomicBool>,
}

/// The recently-listed-directories registry. Create it with [`WatchRegistry::spawn`]; share it as
/// the `Arc` it returns.
#[derive(Debug)]
pub struct WatchRegistry {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl WatchRegistry {
    /// Starts the task. Must be called inside a tokio runtime.
    pub fn spawn(
        backend: Arc<dyn WatchBackend>,
        sink: ChangeSink,
        config: WatchConfig,
    ) -> Arc<Self> {
        let shared = Arc::new(Shared {
            interest: Mutex::new(HashMap::new()),
            armed: Mutex::new(HashSet::new()),
            armed_changed: Notify::new(),
            wake: Notify::new(),
            watching: AtomicUsize::new(0),
            config,
        });
        let task = tokio::spawn(run(Arc::clone(&shared), backend, sink));
        Arc::new(Self { shared, task })
    }

    /// Records that `dir` of `share` was just listed: watch it (if not already) and keep watching
    /// it for another `ttl`. `dir` is share-relative, `/`-separated, empty for the share root;
    /// leading and trailing slashes are ignored. Cheap and non-blocking, but the watch opens
    /// shortly AFTER this returns; see [`WatchRegistry::touch_armed`].
    pub fn touch(&self, share: &ShareId, dir: &str) {
        let Some(key) = Key::new(share, dir) else {
            return;
        };
        let expires = Instant::now() + self.shared.config.ttl;
        let mut interest = self
            .shared
            .interest
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(e) = interest.get_mut(&key) {
            *e = expires;
            return;
        }
        if interest.len() >= self.shared.config.max_watches.max(1)
            && let Some(oldest) = interest
                .iter()
                .min_by_key(|(_, e)| **e)
                .map(|(k, _)| k.clone())
        {
            interest.remove(&oldest);
        }
        interest.insert(key, expires);
        drop(interest);
        self.shared.wake.notify_one();
    }

    /// [`WatchRegistry::touch`], then waits until the OS watch for the directory is open (at most
    /// 250 ms; returns early when the directory cannot be watched). Call it BEFORE enumerating
    /// the directory: anything that changes after this returns produces `share.changed`, so the
    /// listing that follows cannot miss a change. Returns at once when the directory was already
    /// being watched.
    pub async fn touch_armed(&self, share: &ShareId, dir: &str) {
        self.touch(share, dir);
        let Some(key) = Key::new(share, dir) else {
            return;
        };
        let deadline = Instant::now() + ARM_TIMEOUT;
        loop {
            // Register for the wake-up BEFORE checking, so a change in between is not lost.
            let notified = self.shared.armed_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let armed = self
                .shared
                .armed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&key);
            let wanted = self
                .shared
                .interest
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&key);
            if armed || !wanted {
                return;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return;
            }
        }
    }

    /// Directories currently wanted (touched within `ttl`).
    pub fn interested(&self) -> usize {
        self.shared
            .interest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// OS watches currently running.
    pub fn watching(&self) -> usize {
        self.shared.watching.load(Ordering::Relaxed)
    }
}

impl Drop for WatchRegistry {
    fn drop(&mut self) {
        // Dropping the task drops every OS watch with it.
        self.task.abort();
    }
}

/// Resolves when `at` is reached; never when there is nothing scheduled.
async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending::<()>().await,
    }
}

async fn run(shared: Arc<Shared>, backend: Arc<dyn WatchBackend>, sink: ChangeSink) {
    let (tx, mut rx) = mpsc::unbounded_channel::<(Key, WatchEvent)>();
    let mut watching: HashMap<Key, Watching> = HashMap::new();
    let mut due: HashMap<Key, Instant> = HashMap::new();
    let mut sweep = tokio::time::interval(shared.config.sweep_interval);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        reconcile(&shared, &backend, &tx, &mut watching, &mut due).await;
        shared.watching.store(watching.len(), Ordering::Relaxed);
        *shared.armed.lock().unwrap_or_else(|e| e.into_inner()) =
            watching.keys().cloned().collect();
        shared.armed_changed.notify_waiters();

        let next_emit = due.values().min().copied();
        tokio::select! {
            () = shared.wake.notified() => {}
            _ = sweep.tick() => {}
            Some((key, event)) = rx.recv() => {
                match event {
                    WatchEvent::Changed => {
                        if watching.contains_key(&key) {
                            due.entry(key).or_insert_with(|| Instant::now() + shared.config.debounce);
                        }
                    }
                    WatchEvent::Ended => {
                        // Forget the dead watch (and the interest, so the next touch re-opens
                        // it) and tell the clients once: the directory they look at is gone.
                        watching.remove(&key);
                        due.remove(&key);
                        shared.interest.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
                        sink(&key.share, &key.dir);
                    }
                }
            }
            () = sleep_until_opt(next_emit) => {
                let now = Instant::now();
                let ready: Vec<Key> = due.iter().filter(|(_, at)| **at <= now).map(|(k, _)| k.clone()).collect();
                for key in ready {
                    due.remove(&key);
                    if let Some(w) = watching.get(&key) {
                        // Clear BEFORE emitting: a change after this point opens a new window.
                        w.dirty.store(false, Ordering::Release);
                        sink(&key.share, &key.dir);
                    }
                }
            }
        }
    }
}

/// Makes the running watches match the registry's interest: drops expired and evicted ones,
/// opens new ones.
async fn reconcile(
    shared: &Arc<Shared>,
    backend: &Arc<dyn WatchBackend>,
    tx: &mpsc::UnboundedSender<(Key, WatchEvent)>,
    watching: &mut HashMap<Key, Watching>,
    due: &mut HashMap<Key, Instant>,
) {
    let wanted: HashSet<Key> = {
        let now = Instant::now();
        let mut interest = shared.interest.lock().unwrap_or_else(|e| e.into_inner());
        interest.retain(|_, expires| *expires > now);
        interest.keys().cloned().collect()
    };
    watching.retain(|k, _| wanted.contains(k));
    due.retain(|k, _| wanted.contains(k));

    for key in wanted {
        if watching.contains_key(&key) {
            continue;
        }
        let dirty = Arc::new(AtomicBool::new(false));
        let notifier: ChangeNotifier = {
            let dirty = Arc::clone(&dirty);
            let tx = tx.clone();
            let key = key.clone();
            Arc::new(move |event| match event {
                WatchEvent::Changed => {
                    // One message per window: the flag stays set until the task emits.
                    if !dirty.swap(true, Ordering::AcqRel) {
                        let _ = tx.send((key.clone(), event));
                    }
                }
                WatchEvent::Ended => {
                    let _ = tx.send((key.clone(), event));
                }
            })
        };
        let opened = {
            let backend = Arc::clone(backend);
            let (share, dir) = (key.share.clone(), key.dir.clone());
            tokio::task::spawn_blocking(move || backend.watch(&share, &dir, notifier)).await
        };
        match opened {
            Ok(Ok(handle)) => {
                watching.insert(
                    key,
                    Watching {
                        _handle: handle,
                        dirty,
                    },
                );
            }
            Ok(Err(e)) => {
                // The directory cannot be watched (gone, not allowed, share removed). Forget the
                // interest so a later touch tries again. Paths stay out of the log.
                tracing::debug!(error = %e, "directory watch refused");
                shared
                    .interest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&key);
            }
            Err(e) => {
                tracing::warn!(error = %e, "directory watch task failed");
                shared
                    .interest
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&key);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! A backend with no filesystem: tests fire changes by hand.

    use super::*;

    #[derive(Default)]
    pub(crate) struct FakeBackend {
        notifiers: Mutex<HashMap<(String, String), ChangeNotifier>>,
        opened: AtomicUsize,
        closed: Arc<AtomicUsize>,
        refuse: Mutex<HashSet<(String, String)>>,
    }

    impl fmt::Debug for FakeBackend {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("FakeBackend")
        }
    }

    struct Guard(Arc<AtomicUsize>);

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl FakeBackend {
        pub(crate) fn fire(&self, share: &str, dir: &str, event: WatchEvent) -> bool {
            let n = self
                .notifiers
                .lock()
                .unwrap()
                .get(&(share.to_owned(), dir.to_owned()))
                .cloned();
            match n {
                Some(n) => {
                    n(event);
                    true
                }
                None => false,
            }
        }

        pub(crate) fn opened(&self) -> usize {
            self.opened.load(Ordering::SeqCst)
        }

        pub(crate) fn closed(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
        }

        pub(crate) fn refuse(&self, share: &str, dir: &str) {
            self.refuse
                .lock()
                .unwrap()
                .insert((share.to_owned(), dir.to_owned()));
        }
    }

    impl WatchBackend for FakeBackend {
        fn watch(
            &self,
            share: &ShareId,
            dir: &str,
            changed: ChangeNotifier,
        ) -> io::Result<WatchHandle> {
            let key = (share.to_string(), dir.to_owned());
            if self.refuse.lock().unwrap().contains(&key) {
                return Err(io::Error::new(io::ErrorKind::NotFound, "refused"));
            }
            self.notifiers.lock().unwrap().insert(key, changed);
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(Guard(Arc::clone(&self.closed))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBackend;
    use super::*;
    use std::sync::Mutex as StdMutex;

    type Seen = Arc<StdMutex<Vec<(String, String)>>>;

    fn setup(config: WatchConfig) -> (Arc<WatchRegistry>, Arc<FakeBackend>, Seen) {
        let backend = Arc::new(FakeBackend::default());
        let seen: Seen = Arc::default();
        let sink: ChangeSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |share, dir| {
                seen.lock()
                    .unwrap()
                    .push((share.to_string(), dir.to_owned()));
            })
        };
        let reg = WatchRegistry::spawn(backend.clone(), sink, config);
        (reg, backend, seen)
    }

    fn fast() -> WatchConfig {
        WatchConfig {
            ttl: Duration::from_secs(60),
            debounce: Duration::from_millis(60),
            max_watches: 64,
            sweep_interval: Duration::from_millis(30),
        }
    }

    async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        let t0 = std::time::Instant::now();
        while !ok() {
            assert!(t0.elapsed() < Duration::from_secs(3), "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn only_touched_directories_are_watched_and_a_burst_is_one_event() {
        let (reg, backend, seen) = setup(fast());
        let share = ShareId::from("shr_1");
        assert_eq!(backend.opened(), 0, "nothing is watched before a touch");
        reg.touch(&share, "Clients/Acme");
        reg.touch(&share, "/Clients/Acme/"); // same directory, slashes ignored
        until("watch opened", || {
            backend.fire("shr_1", "Clients/Acme", WatchEvent::Changed)
        })
        .await;
        assert_eq!(
            backend.opened(),
            1,
            "a repeated touch must not open a second watch"
        );

        // 50 changes inside one window coalesce into a single event.
        for _ in 0..49 {
            backend.fire("shr_1", "Clients/Acme", WatchEvent::Changed);
        }
        until("event emitted", || !seen.lock().unwrap().is_empty()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[("shr_1".to_owned(), "Clients/Acme".to_owned())]
        );

        // The next change after the window arms a new one.
        backend.fire("shr_1", "Clients/Acme", WatchEvent::Changed);
        until("second event", || seen.lock().unwrap().len() == 2).await;
    }

    #[tokio::test]
    async fn the_event_comes_after_the_window_not_before() {
        let (reg, backend, seen) = setup(WatchConfig {
            debounce: Duration::from_millis(300),
            ..fast()
        });
        reg.touch(&ShareId::from("shr_1"), "");
        until("watch opened", || {
            backend.fire("shr_1", "", WatchEvent::Changed)
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            seen.lock().unwrap().is_empty(),
            "emitted before the window ended"
        );
        until("event emitted", || !seen.lock().unwrap().is_empty()).await;
        assert_eq!(
            seen.lock().unwrap()[0].1,
            "",
            "the share root is the empty path"
        );
    }

    #[tokio::test]
    async fn untouched_directories_expire_and_touching_extends() {
        let (reg, backend, _seen) = setup(WatchConfig {
            ttl: Duration::from_millis(250),
            ..fast()
        });
        let share = ShareId::from("shr_1");
        reg.touch(&share, "a");
        reg.touch(&share, "b");
        until("two watches", || reg.watching() == 2).await;
        // Keep `a` alive past its original expiry; let `b` lapse.
        for _ in 0..6 {
            tokio::time::sleep(Duration::from_millis(80)).await;
            reg.touch(&share, "a");
        }
        until("b dropped", || reg.watching() == 1).await;
        assert_eq!(backend.closed(), 1, "the lapsed watch must be released");
        assert_eq!(reg.interested(), 1);
        assert!(backend.fire("shr_1", "a", WatchEvent::Changed));
        // Once everything lapses, nothing is watched.
        until("all dropped", || reg.watching() == 0).await;
        assert_eq!(backend.closed(), 2);
    }

    #[tokio::test]
    async fn the_cap_evicts_the_directory_closest_to_expiry() {
        let (reg, backend, _seen) = setup(WatchConfig {
            max_watches: 3,
            ..fast()
        });
        let share = ShareId::from("shr_1");
        for d in ["d1", "d2", "d3"] {
            reg.touch(&share, d);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        until("three watches", || reg.watching() == 3).await;
        reg.touch(&share, "d1"); // d2 is now the stalest
        tokio::time::sleep(Duration::from_millis(5)).await;
        reg.touch(&share, "d4");
        until("d2 evicted, d4 open", || {
            reg.watching() == 3 && backend.opened() == 4 && backend.closed() == 1
        })
        .await;
        assert_eq!(reg.interested(), 3);
    }

    #[tokio::test]
    async fn refused_directories_are_forgotten_and_retried_on_the_next_touch() {
        let (reg, backend, _seen) = setup(fast());
        let share = ShareId::from("shr_1");
        backend.refuse("shr_1", "gone");
        reg.touch(&share, "gone");
        until("interest forgotten", || reg.interested() == 0).await;
        assert_eq!(reg.watching(), 0);
        assert_eq!(backend.opened(), 0);
    }

    #[tokio::test]
    async fn an_ended_watch_notifies_once_and_reopens_on_the_next_touch() {
        let (reg, backend, seen) = setup(fast());
        let share = ShareId::from("shr_1");
        reg.touch(&share, "dir");
        until("opened", || backend.fire("shr_1", "dir", WatchEvent::Ended)).await;
        until("final event", || seen.lock().unwrap().len() == 1).await;
        until("watch released", || {
            reg.watching() == 0 && backend.closed() == 1
        })
        .await;
        assert_eq!(reg.interested(), 0);
        reg.touch(&share, "dir");
        until("reopened", || backend.opened() == 2 && reg.watching() == 1).await;
    }

    #[tokio::test]
    async fn touch_armed_returns_only_once_the_watch_is_open() {
        let (reg, backend, _seen) = setup(fast());
        let share = ShareId::from("shr_1");
        reg.touch_armed(&share, "docs").await;
        assert_eq!(
            backend.opened(),
            1,
            "armed means the backend already opened it"
        );
        assert!(backend.fire("shr_1", "docs", WatchEvent::Changed));
        // Already watched: returns at once without opening anything again.
        reg.touch_armed(&share, "docs").await;
        assert_eq!(backend.opened(), 1);
        // A refused directory returns promptly instead of waiting for the timeout.
        backend.refuse("shr_1", "gone");
        let t0 = std::time::Instant::now();
        reg.touch_armed(&share, "gone").await;
        assert!(
            t0.elapsed() < Duration::from_millis(200),
            "{:?}",
            t0.elapsed()
        );
        assert_eq!(reg.interested(), 1);
    }

    #[tokio::test]
    async fn oversized_directory_names_are_ignored() {
        let (reg, _backend, _seen) = setup(fast());
        reg.touch(&ShareId::from("shr_1"), &"a/".repeat(5000));
        assert_eq!(reg.interested(), 0);
    }
}
