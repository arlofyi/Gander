// SPDX-License-Identifier: Apache-2.0
//! The transport-independent `/thumb` service.
//!
//! **WP-14 wires this into the router.** The handler for
//! `GET /v1/shares/{share}/thumb?path=&size=` authenticates, checks the share permission, resolves
//! `path` through the path resolver (WP-11) to an open read-only [`File`], then calls
//! [`ThumbService::thumb`] with that file, the resolved name's extension, the `size` query value
//! and the request's priority. It maps the result:
//!
//! - `Ok(ThumbResponse)` -> `200`, `Content-Type`, `ETag`, `Cache-Control` from the response.
//! - `Err(e)` -> `e.http_status()` with the JSON body `e.to_body()` (for `503 generating` also a
//!   `Retry-After` header rounded up to whole seconds).
//!
//! The service never sees a phone path, never decodes bytes in-process (the renderer runs a
//! worker), and never treats a cache hit as proof the file exists: the key is derived from the
//! open handle the resolver produced for this request.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gander_core::error::{ErrorBody, ErrorCode};

use super::cache::{ContentKey, FileIdentity, ThumbCache};
use super::queue::{Priority, QueueConfig, RenderQueue};
use super::{DEFAULT_THUMB_SIZE, THUMB_SIZES, classify, normalise_ext};
use crate::render::job::{OutputFormat, RENDERER_VERSION, RenderJob, RenderKind};

/// `Cache-Control` for thumbnails (protocol spec).
pub const THUMB_CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// How long "no thumbnail for this content" is remembered (keyed by content identity, so an
/// edited file is retried at once).
const NEGATIVE_TTL: Duration = Duration::from_secs(60 * 60);
/// How long a host-side failure (worker could not be spawned, cache write failed) is remembered.
const FAILURE_TTL: Duration = Duration::from_secs(30);
/// Bound on remembered failures.
const MAX_FAILURES: usize = 10_000;

/// What a renderer produced for one job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderResult {
    /// Encoded image bytes.
    Image(Vec<u8>),
    /// No thumbnail for this file: unsupported, decoder error, timeout, crash, icon only.
    NoThumbnail,
    /// The host could not run the job (spawn failure); retried after a short delay.
    HostFailure,
}

/// Something that turns an open file into a thumbnail. Production: [`WorkerRenderer`].
pub trait ThumbRenderer: Send + Sync + 'static {
    /// Renders `file` (kind and extension already classified) at `size`.
    fn render(&self, file: &File, job: &RenderJob) -> RenderResult;
}

/// The production renderer: one `gander-render.exe` per job, under a job object.
#[derive(Debug, Clone)]
pub struct WorkerRenderer {
    /// Worker location and limits.
    pub config: crate::render::worker::WorkerConfig,
}

impl ThumbRenderer for WorkerRenderer {
    fn render(&self, file: &File, job: &RenderJob) -> RenderResult {
        use crate::render::worker::{WorkerOutcome, run_job};
        match run_job(&self.config, file, job) {
            Ok(report) => match report.outcome {
                WorkerOutcome::Ok(bytes) if !bytes.is_empty() => RenderResult::Image(bytes),
                _ => RenderResult::NoThumbnail,
            },
            Err(_) => RenderResult::HostFailure,
        }
    }
}

/// A `/thumb` request after authentication and path resolution.
#[derive(Debug)]
pub struct ThumbRequest {
    /// The resolved file, opened for reading by the resolver.
    pub file: File,
    /// Extension of the resolved file name (with or without the dot; case-insensitive).
    pub ext: String,
    /// `size` query value; `None` means [`DEFAULT_THUMB_SIZE`].
    pub size: Option<u32>,
    /// `Visible` for `priority=visible`, else `Prefetch`.
    pub priority: Priority,
}

/// A thumbnail ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbResponse {
    /// Encoded image.
    pub bytes: Vec<u8>,
    /// `image/webp` (or `image/jpeg`).
    pub content_type: &'static str,
    /// Strong ETag (quoted).
    pub etag: String,
    /// [`THUMB_CACHE_CONTROL`].
    pub cache_control: &'static str,
}

/// Why no thumbnail was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbError {
    /// `size` is not one of [`THUMB_SIZES`] -> 400 `bad_request`.
    BadSize,
    /// Type not thumbnailable, or rendering produced nothing -> 415 `unsupported_type`.
    Unsupported,
    /// Queued or running -> 503 `generating` with `retry_after_ms`.
    Generating {
        /// Client retry hint.
        retry_after_ms: u64,
    },
    /// Host-side failure -> 500 `internal` (never with details).
    Internal,
}

impl ThumbError {
    /// The protocol error code.
    pub fn code(self) -> ErrorCode {
        match self {
            ThumbError::BadSize => ErrorCode::BadRequest,
            ThumbError::Unsupported => ErrorCode::UnsupportedType,
            ThumbError::Generating { .. } => ErrorCode::Generating,
            ThumbError::Internal => ErrorCode::Internal,
        }
    }

    /// The HTTP status.
    pub fn http_status(self) -> u16 {
        self.code().http_status()
    }

    /// The JSON error body (messages contain no paths).
    pub fn to_body(self) -> ErrorBody {
        match self {
            ThumbError::BadSize => ErrorBody::new(self.code(), "size must be 128, 256 or 512"),
            ThumbError::Unsupported => {
                ErrorBody::new(self.code(), "no thumbnail is available for this file")
            }
            ThumbError::Generating { retry_after_ms } => {
                ErrorBody::retry_after(self.code(), "thumbnail is being generated", retry_after_ms)
            }
            ThumbError::Internal => ErrorBody::new(self.code(), "internal error"),
        }
    }
}

struct Job {
    file: File,
    key: ContentKey,
    job: RenderJob,
}

#[derive(Debug, Clone, Copy)]
struct Failure {
    at: Instant,
    permanent: bool,
}

struct Shared<R> {
    cache: Arc<ThumbCache>,
    renderer: R,
    failures: Mutex<HashMap<String, Failure>>,
}

impl<R> Shared<R> {
    fn record_failure(&self, key: &str, permanent: bool) {
        let mut f = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        if f.len() >= MAX_FAILURES {
            f.clear();
        }
        f.insert(
            key.to_owned(),
            Failure {
                at: Instant::now(),
                permanent,
            },
        );
    }

    fn failure(&self, key: &str) -> Option<Failure> {
        let mut f = self.failures.lock().unwrap_or_else(|p| p.into_inner());
        let entry = *f.get(key)?;
        let ttl = if entry.permanent {
            NEGATIVE_TTL
        } else {
            FAILURE_TTL
        };
        if entry.at.elapsed() > ttl {
            f.remove(key);
            return None;
        }
        Some(entry)
    }
}

/// Shell-provider lookup, memoised per extension.
type ShellLookup = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Cache + queue + renderer behind the `/thumb` endpoint. Share it behind an `Arc`.
pub struct ThumbService<R: ThumbRenderer> {
    shared: Arc<Shared<R>>,
    queue: RenderQueue<Job>,
    shell_lookup: ShellLookup,
    shell_memo: Mutex<HashMap<String, bool>>,
}

impl<R: ThumbRenderer> std::fmt::Debug for ThumbService<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbService")
            .field("cache", &self.shared.cache)
            .field("queue", &self.queue)
            .finish_non_exhaustive()
    }
}

impl ThumbService<WorkerRenderer> {
    /// The production service: worker renderer, real shell-provider lookup, default queue
    /// (concurrency 2).
    pub fn with_worker(
        cache: Arc<ThumbCache>,
        worker: crate::render::worker::WorkerConfig,
    ) -> Self {
        Self::new(
            cache,
            WorkerRenderer { config: worker },
            QueueConfig::default(),
            Box::new(super::identity::has_shell_thumbnail_provider),
        )
    }
}

impl<R: ThumbRenderer> ThumbService<R> {
    /// A service over any renderer (tests use fakes).
    pub fn new(
        cache: Arc<ThumbCache>,
        renderer: R,
        queue: QueueConfig,
        shell_lookup: ShellLookup,
    ) -> Self {
        let shared = Arc::new(Shared {
            cache,
            renderer,
            failures: Mutex::new(HashMap::new()),
        });
        let handler_shared = Arc::clone(&shared);
        let queue = RenderQueue::new(queue, move |key: &str, job: Job| {
            match handler_shared.renderer.render(&job.file, &job.job) {
                RenderResult::Image(bytes) => {
                    if handler_shared.cache.put(&job.key, &bytes).is_err() {
                        handler_shared.record_failure(key, false);
                    }
                }
                RenderResult::NoThumbnail => handler_shared.record_failure(key, true),
                RenderResult::HostFailure => handler_shared.record_failure(key, false),
            }
        });
        Self {
            shared,
            queue,
            shell_lookup,
            shell_memo: Mutex::new(HashMap::new()),
        }
    }

    /// Pauses or resumes generation (battery below 15 %).
    pub fn set_paused(&self, paused: bool) {
        self.queue.set_paused(paused);
    }

    /// The cache behind the service.
    pub fn cache(&self) -> &Arc<ThumbCache> {
        &self.shared.cache
    }

    fn has_shell_provider(&self, ext: &str) -> bool {
        let mut memo = self.shell_memo.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(&v) = memo.get(ext) {
            return v;
        }
        let v = (self.shell_lookup)(ext);
        memo.insert(ext.to_owned(), v);
        v
    }

    /// Whether the extension is thumbnailable at all (the listing's `thumb` prediction).
    pub fn predicts_thumb(&self, ext: &str) -> bool {
        classify(ext, |e| self.has_shell_provider(e)).is_some()
    }

    /// Answers one `/thumb` request: cached bytes, or `503 generating` after queueing the job.
    pub fn thumb(&self, req: ThumbRequest) -> Result<ThumbResponse, ThumbError> {
        let size = req.size.unwrap_or(DEFAULT_THUMB_SIZE);
        if !THUMB_SIZES.contains(&size) {
            return Err(ThumbError::BadSize);
        }
        let ext = normalise_ext(&req.ext);
        let kind = classify(&ext, |e| self.has_shell_provider(e)).ok_or(ThumbError::Unsupported)?;
        let identity = match super::identity::file_identity(&req.file) {
            Ok(id) => id,
            Err(e) if e.kind() == io::ErrorKind::IsADirectory => {
                return Err(ThumbError::Unsupported);
            }
            Err(_) => return Err(ThumbError::Internal),
        };
        self.thumb_for(req.file, identity, kind, &ext, size, req.priority)
    }

    fn thumb_for(
        &self,
        file: File,
        identity: FileIdentity,
        kind: RenderKind,
        ext: &str,
        size: u32,
        priority: Priority,
    ) -> Result<ThumbResponse, ThumbError> {
        let job = RenderJob::thumb(kind, ext, size);
        let key = ContentKey {
            identity,
            renderer_version: RENDERER_VERSION,
            size_px: size,
            format: job.format,
        };
        let format: OutputFormat = job.format;
        match self.shared.cache.get(&key) {
            Ok(Some(bytes)) => {
                return Ok(ThumbResponse {
                    bytes,
                    content_type: format.content_type(),
                    etag: key.etag(),
                    cache_control: THUMB_CACHE_CONTROL,
                });
            }
            Ok(None) => {}
            Err(_) => return Err(ThumbError::Internal),
        }
        let k = key.as_string();
        if let Some(f) = self.shared.failure(&k) {
            return Err(if f.permanent {
                ThumbError::Unsupported
            } else {
                ThumbError::Internal
            });
        }
        let status = self.queue.submit(&k, priority, || Job { file, key, job });
        Err(ThumbError::Generating {
            retry_after_ms: status.retry_after_ms(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Fake {
        calls: AtomicUsize,
    }

    impl ThumbRenderer for Arc<Fake> {
        fn render(&self, file: &File, job: &RenderJob) -> RenderResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            if job.ext == "bad" || len == 0 {
                return RenderResult::NoThumbnail;
            }
            RenderResult::Image(format!("thumb:{len}").into_bytes())
        }
    }

    struct Fixture {
        dir: PathBuf,
        fake: Arc<Fake>,
        svc: ThumbService<Arc<Fake>>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("gander-thumbsvc-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let cache = Arc::new(ThumbCache::open(&dir.join("cache"), 1 << 30).unwrap());
            let fake = Arc::new(Fake::default());
            let svc = ThumbService::new(
                cache,
                Arc::clone(&fake),
                QueueConfig::default(),
                Box::new(|e: &str| e == "psd"),
            );
            Self { dir, fake, svc }
        }

        fn ask(&self, path: &Path, size: Option<u32>) -> Result<ThumbResponse, ThumbError> {
            let ext = path.extension().unwrap().to_string_lossy().into_owned();
            self.svc.thumb(ThumbRequest {
                file: File::open(path).unwrap(),
                ext,
                size,
                priority: Priority::Visible,
            })
        }

        /// Polls like a client honouring `retry_after_ms`.
        fn ask_until_done(&self, path: &Path) -> Result<ThumbResponse, ThumbError> {
            for _ in 0..100 {
                match self.ask(path, Some(256)) {
                    Err(ThumbError::Generating { retry_after_ms }) => {
                        assert!(retry_after_ms >= 200);
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    other => return other,
                }
            }
            panic!("never finished");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn first_request_is_503_then_cached_200() {
        let fx = Fixture::new("flow");
        let p = fx.dir.join("a.png");
        std::fs::write(&p, b"12345").unwrap();
        let first = fx.ask(&p, None);
        assert!(matches!(first, Err(ThumbError::Generating { .. })));
        assert_eq!(first.unwrap_err().http_status(), 503);
        let ok = fx.ask_until_done(&p).unwrap();
        assert_eq!(ok.bytes, b"thumb:5");
        assert_eq!(ok.content_type, "image/webp");
        assert_eq!(ok.cache_control, THUMB_CACHE_CONTROL);
        assert!(ok.etag.starts_with('"'));
        assert_eq!(fx.fake.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn rename_keeps_the_entry_and_touch_invalidates_it() {
        let fx = Fixture::new("rename");
        let a = fx.dir.join("photo.jpg");
        std::fs::write(&a, b"abc").unwrap();
        let first = fx.ask_until_done(&a).unwrap();
        let b = fx.dir.join("renamed.jpg");
        std::fs::rename(&a, &b).unwrap();
        let after_rename = fx.ask(&b, Some(256)).expect("cache hit after rename");
        assert_eq!(after_rename, first);
        assert_eq!(fx.fake.calls.load(Ordering::SeqCst), 1);

        // Touch: same size, new last-write time.
        let f = std::fs::OpenOptions::new().write(true).open(&b).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        assert!(matches!(
            fx.ask(&b, Some(256)),
            Err(ThumbError::Generating { .. })
        ));
        let touched = fx.ask_until_done(&b).unwrap();
        assert_ne!(touched.etag, first.etag);
        assert_eq!(fx.fake.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn refusals_map_to_protocol_errors() {
        let fx = Fixture::new("refuse");
        let txt = fx.dir.join("notes.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert_eq!(fx.ask(&txt, None).unwrap_err(), ThumbError::Unsupported);
        assert_eq!(fx.ask(&txt, None).unwrap_err().http_status(), 415);

        let png = fx.dir.join("a.png");
        std::fs::write(&png, b"x").unwrap();
        let bad = fx.ask(&png, Some(300)).unwrap_err();
        assert_eq!(bad, ThumbError::BadSize);
        assert_eq!(bad.to_body().error.code, ErrorCode::BadRequest);

        let lnk = fx.dir.join("x.lnk");
        std::fs::write(&lnk, b"x").unwrap();
        assert_eq!(fx.ask(&lnk, None).unwrap_err(), ThumbError::Unsupported);

        // Shell fallback is consulted for unknown types.
        let psd = fx.dir.join("art.psd");
        std::fs::write(&psd, b"x").unwrap();
        assert!(matches!(
            fx.ask(&psd, None),
            Err(ThumbError::Generating { .. })
        ));
        assert!(fx.svc.predicts_thumb("PSD"));
        assert!(!fx.svc.predicts_thumb("txt"));
    }

    #[test]
    fn render_failure_becomes_415_not_an_endless_503() {
        let fx = Fixture::new("fail");
        let empty = fx.dir.join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        let end = fx.ask_until_done(&empty).unwrap_err();
        assert_eq!(end, ThumbError::Unsupported);
        let body = end.to_body();
        assert_eq!(body.error.code, ErrorCode::UnsupportedType);
        assert!(
            !body.error.message.contains("empty"),
            "no paths in messages"
        );
        assert_eq!(fx.fake.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn generating_body_carries_retry_after() {
        let body = ThumbError::Generating {
            retry_after_ms: 400,
        }
        .to_body();
        assert_eq!(body.error.code, ErrorCode::Generating);
        assert_eq!(body.error.retry_after_ms, 400);
    }
}
