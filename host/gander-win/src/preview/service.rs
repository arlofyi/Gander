// SPDX-License-Identifier: Apache-2.0
//! The transport-independent preview service: `/preview/info`, `/preview` and `/preview/text`.
//!
//! **WP-14 wires this into the router.** Each handler authenticates, checks the `preview`
//! permission, resolves `path` through the path resolver (WP-11) to an open read-only [`File`]
//! and calls one method with that file, the resolved name's extension and the device id:
//!
//! | endpoint | method | success | notes |
//! | --- | --- | --- | --- |
//! | `GET .../preview/info?path=` | [`PreviewService::info`] | `200` JSON [`PreviewInfo`] | may wait up to [`PreviewServiceConfig::info_wait`] |
//! | `GET .../preview?path=&page=&width=&format=` | [`PreviewService::page`] | `200` image, `ETag`, `Cache-Control` | may wait up to [`PreviewServiceConfig::page_wait`] |
//! | `GET .../preview/text?path=&offset=&limit=` | [`PreviewService::text`] | `200` `text/plain; charset=utf-8` | in-process, bounded |
//! | `GET .../preview/tile?path=&page=&level=&x=&y=` | [`PreviewService::tile`] | `200` 512 px WebP, `ETag`, `Cache-Control` | deep zoom (WP-67), low priority on the same queue |
//!
//! Failures are a [`PreviewError`]: `e.http_status()` and the JSON body `e.to_body()` (for `503
//! generating` also a `Retry-After` header rounded up to whole seconds). The service never sees
//! a phone path, never decodes an image or document in the host process (the renderers run in
//! the worker), and never treats a cache hit as proof the file exists: every key is derived from
//! the open handle the resolver produced for this request.
//!
//! The methods **block** the calling thread for up to the configured wait (polling a cache that
//! queue threads fill), so call them from `spawn_blocking`. A wait of zero gives the purely
//! non-blocking behaviour of the thumbnail service.
//!
//! How each kind is served (see [`super::plan`]):
//!
//! - images, RAW, PDF, video posters: one worker job per page;
//! - PowerPoint (COM): one worker job per slide (`Slides(n).Export`);
//! - Word, Excel (COM) and LibreOffice: the document is exported to PDF once, the PDF is kept in
//!   the preview cache, and pages are rendered from it (so `preview/info` reports the page count
//!   once the first page has been rendered);
//! - deep-zoom tiles (WP-67): one worker job per tile for images, RAW and PDF (a region decode
//!   for images, a source-rect render for PDF); Word, Excel and LibreOffice tiles come from the
//!   exported PDF; PowerPoint (COM) slides have none. Tiles are cached by file identity, page,
//!   level, column and row, and queued behind pages;
//! - when the Office render of page 1 fails (Office busy, timeout, a document it cannot open) and
//!   the document embeds a thumbnail, that thumbnail is served instead (`degraded`), and the
//!   render is retried after a minute.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gander_core::error::{ErrorBody, ErrorCode};
use gander_core::protocol::files::{ImageFormat, PreviewInfo, PreviewKind};
use gander_core::protocol::host::PreviewKinds;
use gander_core::tiles::{MAX_LEVELS, MAX_TILE_PAGES, Pyramid, TILE_SIZE, TilePage};

use super::office::{OfficeApp, OfficeBackend, OfficeEnv};
use super::queue::{PreviewQueue, PreviewQueueConfig};
use super::runner::{JobResult, JobRunner, WorkerRunner};
use super::text::{self, TextError};
use super::{
    DEFAULT_PREVIEW_WIDTH, PREVIEW_CACHE_CONTROL, PREVIEW_MAX_WIDTH, PREVIEW_MIN_WIDTH,
    PREVIEW_RENDERER_VERSION, Plan, plan,
};
use crate::render::job::{OutputFormat, PreviewMeta, RenderJob, RenderKind, RenderOp};
use crate::thumbs::cache::{FileIdentity, ThumbCache, fnv1a64};
use crate::thumbs::identity::file_identity;
use crate::thumbs::normalise_ext;
use crate::thumbs::queue::Enqueued;

/// How long "no preview for this content" is remembered (keyed by content identity, so an edited
/// file is retried at once).
const NEGATIVE_TTL: Duration = Duration::from_secs(60 * 60);
/// How long a host-side failure or a busy Office is remembered.
const TRANSIENT_TTL: Duration = Duration::from_secs(30);
/// How long a degraded (thumbnail) answer stands before the real render is tried again.
const DEGRADED_TTL: Duration = Duration::from_secs(60);
/// Bound on remembered failures and infos.
const MAX_ENTRIES: usize = 10_000;
/// Worker watchdog for an Office automation job.
const OFFICE_TIMEOUT_MS: u64 = 30_000;
/// WebP quality of a tile (pages use 85; tiles are 4 to 16 times as many bytes per page).
/// Part of the cache key through [`TILE_RENDERER_VERSION`].
const TILE_QUALITY: u8 = 80;
/// Version of the tile output; bump it when tiles cached so far would be wrong.
const TILE_RENDERER_VERSION: u32 = 1;
/// A queued tile not asked for again within this long is dropped (a client that still wants it
/// asks every few seconds at most, see `retry_after_ms`; one that scrolled away stops asking).
const TILE_STALE_AFTER: Duration = Duration::from_secs(15);
/// Wall-clock allowance of a tile worker job.
const TILE_TIMEOUT_MS: u64 = 20_000;
/// Message of the 400 for a tile that does not exist.
const TILE_RANGE_MSG: &str = "tile out of range";
/// Largest column or row the service passes on to a worker (a level has at most 64 tiles a side).
const MAX_TILE_INDEX: u32 = 1024;
/// Content type of the text endpoint.
pub const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// Tuning of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviewServiceConfig {
    /// The render queue (1 per device, 2 overall by default).
    pub queue: PreviewQueueConfig,
    /// How long [`PreviewService::page`] waits for a render before answering `503 generating`.
    pub page_wait: Duration,
    /// How long [`PreviewService::info`] waits for the metadata job.
    pub info_wait: Duration,
}

impl Default for PreviewServiceConfig {
    fn default() -> Self {
        Self {
            queue: PreviewQueueConfig::default(),
            page_wait: Duration::from_millis(1500),
            info_wait: Duration::from_millis(2000),
        }
    }
}

/// A `/preview/info` request after authentication and path resolution.
#[derive(Debug)]
pub struct InfoRequest {
    /// The resolved file, opened for reading by the resolver.
    pub file: File,
    /// Extension of the resolved file name (with or without the dot; case-insensitive).
    pub ext: String,
    /// The requesting device id (the per-device queue limit).
    pub device: String,
}

/// A `/preview` request after authentication and path resolution.
#[derive(Debug)]
pub struct PageRequest {
    /// The resolved file, opened for reading by the resolver.
    pub file: File,
    /// Extension of the resolved file name.
    pub ext: String,
    /// The requesting device id (the per-device queue limit).
    pub device: String,
    /// `page` query value; `None` means 1.
    pub page: Option<u32>,
    /// `width` query value; `None` means [`DEFAULT_PREVIEW_WIDTH`], larger than
    /// [`PREVIEW_MAX_WIDTH`] is clamped.
    pub width: Option<u32>,
    /// `format` query value.
    pub format: ImageFormat,
}

/// A `/preview/text` request after authentication and path resolution.
#[derive(Debug)]
pub struct TextRequest {
    /// The resolved file, opened for reading by the resolver.
    pub file: File,
    /// `offset` query value (bytes of decoded text).
    pub offset: u64,
    /// `limit` query value; `None` means 64 KiB; the maximum is 1 MiB.
    pub limit: Option<u64>,
}

/// A `/preview/tile` request after authentication and path resolution.
#[derive(Debug)]
pub struct TileRequest {
    /// The resolved file, opened for reading by the resolver.
    pub file: File,
    /// Extension of the resolved file name.
    pub ext: String,
    /// The requesting device id (the per-device queue limit).
    pub device: String,
    /// `page` query value; `None` means 1.
    pub page: Option<u32>,
    /// Pyramid level, 0 to levels - 1 (see [`gander_core::tiles`]).
    pub level: u32,
    /// Tile column.
    pub x: u32,
    /// Tile row.
    pub y: u32,
}

/// A deep-zoom tile ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileResponse {
    /// WebP bytes.
    pub bytes: Vec<u8>,
    /// `image/webp`.
    pub content_type: &'static str,
    /// Strong ETag (quoted), from the file version, page, level and position.
    pub etag: String,
    /// [`PREVIEW_CACHE_CONTROL`].
    pub cache_control: &'static str,
}

/// A preview page ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageResponse {
    /// Encoded image.
    pub bytes: Vec<u8>,
    /// `image/webp` or `image/jpeg`.
    pub content_type: &'static str,
    /// Strong ETag (quoted); differs between the real render and a degraded thumbnail.
    pub etag: String,
    /// [`PREVIEW_CACHE_CONTROL`].
    pub cache_control: &'static str,
    /// The page this is.
    pub page: u32,
    /// Page count, when known (always for PDF; for Office after the document has been rendered).
    pub pages: Option<u32>,
    /// The renderer that produced it, as in `preview/info`.
    pub renderer: &'static str,
    /// The embedded thumbnail was served because the Office render failed.
    pub degraded: bool,
}

/// A slice of decoded text ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextResponse {
    /// UTF-8 bytes of the slice.
    pub bytes: Vec<u8>,
    /// [`TEXT_CONTENT_TYPE`].
    pub content_type: &'static str,
    /// `X-Gander-Text-Encoding`: `utf-8`, `utf-16le`, `utf-16be` or `windows-1252`.
    pub encoding: &'static str,
    /// `X-Gander-Text-Total`: total size of the decoded text in bytes (for a UTF-8 file, the file
    /// size minus a BOM). See [`text`] for what it means for other encodings.
    pub total: u64,
    /// Where this slice really starts (the requested offset, moved to a character boundary).
    pub offset: u64,
    /// Where the next slice starts. Use this, not `offset + bytes.len()`, to continue.
    pub next_offset: u64,
    /// More text follows.
    pub has_more: bool,
    /// The file is longer than the decoded window (non-UTF-8 files over 8 MiB).
    pub window_truncated: bool,
}

/// Why no preview was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewError {
    /// A query value is out of range (page past the end, limit out of range, page 0) -> 400
    /// `bad_request`.
    BadRequest(&'static str),
    /// No preview for this type, or rendering produced nothing -> 415 `unsupported_type`.
    Unsupported,
    /// A binary file asked for as text -> 415 `unsupported_type`.
    Binary,
    /// Queued or running -> 503 `generating` with `retry_after_ms`.
    Generating {
        /// Client retry hint.
        retry_after_ms: u64,
    },
    /// Host-side failure -> 500 `internal` (never with details).
    Internal,
}

impl PreviewError {
    /// The protocol error code.
    pub fn code(self) -> ErrorCode {
        match self {
            PreviewError::BadRequest(_) => ErrorCode::BadRequest,
            PreviewError::Unsupported | PreviewError::Binary => ErrorCode::UnsupportedType,
            PreviewError::Generating { .. } => ErrorCode::Generating,
            PreviewError::Internal => ErrorCode::Internal,
        }
    }

    /// The HTTP status.
    pub fn http_status(self) -> u16 {
        self.code().http_status()
    }

    /// The JSON error body (messages contain no paths).
    pub fn to_body(self) -> ErrorBody {
        match self {
            PreviewError::BadRequest(msg) => ErrorBody::new(self.code(), msg),
            PreviewError::Unsupported => {
                ErrorBody::new(self.code(), "no preview is available for this file")
            }
            PreviewError::Binary => ErrorBody::new(self.code(), "this file is not text"),
            PreviewError::Generating { retry_after_ms } => ErrorBody::retry_after(
                self.code(),
                "the preview is being generated",
                retry_after_ms,
            ),
            PreviewError::Internal => ErrorBody::new(self.code(), "internal error"),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Keys

fn id_string(id: &FileIdentity) -> String {
    format!(
        "{:016x}-{:032x}-{:x}-{:x}",
        id.volume, id.file_id, id.size, id.mtime
    )
}

/// One rendered page of one version of one file.
#[derive(Debug, Clone, Copy)]
struct PageKey {
    identity: FileIdentity,
    page: u32,
    width: u32,
    format: OutputFormat,
    /// The low-fidelity embedded thumbnail served when the real render failed.
    degraded: bool,
}

impl PageKey {
    /// The cache key; always ends with the format extension (the cache finds files by it).
    fn name(&self) -> String {
        format!(
            "p-{}-v{}-n{}-w{}-{}{}",
            id_string(&self.identity),
            PREVIEW_RENDERER_VERSION,
            self.page,
            self.width,
            if self.degraded { "lo-" } else { "" },
            self.format.extension()
        )
    }

    fn etag(&self) -> String {
        format!("\"p-{:016x}\"", fnv1a64(self.name().as_bytes()))
    }

    fn primary(self) -> Self {
        Self {
            degraded: false,
            ..self
        }
    }

    fn fallback(self) -> Self {
        Self {
            degraded: true,
            ..self
        }
    }
}

/// One rendered tile of one version of one file.
#[derive(Debug, Clone, Copy)]
struct TileKey {
    identity: FileIdentity,
    page: u32,
    level: u32,
    x: u32,
    y: u32,
}

impl TileKey {
    /// The cache key; ends with the extension the cache finds files by.
    fn name(&self) -> String {
        format!(
            "t-{}-v{}-n{}-l{}-x{}-y{}-webp",
            id_string(&self.identity),
            TILE_RENDERER_VERSION,
            self.page,
            self.level,
            self.x,
            self.y
        )
    }

    fn etag(&self) -> String {
        format!("\"t-{:016x}\"", fnv1a64(self.name().as_bytes()))
    }
}

/// The renderer name tile timings are kept under (apart from the page timings of the same
/// renderer).
fn tile_renderer(plan: &Plan) -> &'static str {
    match plan {
        Plan::Image(RenderKind::Raw) => "raw_tile",
        Plan::Image(_) => "wic_tile",
        _ => "windows_pdf_tile",
    }
}

fn pdf_name(id: &FileIdentity) -> String {
    format!("d-{}-v{}-pdf", id_string(id), PREVIEW_RENDERER_VERSION)
}

fn info_name(id: &FileIdentity) -> String {
    format!("i-{}-v{}", id_string(id), PREVIEW_RENDERER_VERSION)
}

// ---------------------------------------------------------------------------------------------
// Shared state

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// The content cannot be previewed (decoder error, timeout, encrypted).
    NoPreview,
    /// The page does not exist.
    PageOutOfRange,
    /// Something temporary (spawn failure, cache write, the user's Office is open).
    Transient,
    /// The real render failed and a thumbnail was served instead.
    Degraded,
}

impl FailureKind {
    fn ttl(self) -> Duration {
        match self {
            FailureKind::NoPreview | FailureKind::PageOutOfRange => NEGATIVE_TTL,
            FailureKind::Transient => TRANSIENT_TTL,
            FailureKind::Degraded => DEGRADED_TTL,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FailureRecord {
    at: Instant,
    kind: FailureKind,
}

struct InfoWork {
    file: File,
    name: String,
    kind: RenderKind,
    ext: String,
}

struct PageWork {
    file: File,
    key: PageKey,
    plan: Plan,
    ext: String,
    quality: u8,
}

struct TileWork {
    file: File,
    key: TileKey,
    plan: Plan,
    ext: String,
}

enum Work {
    Info(InfoWork),
    Page(PageWork),
    Tile(TileWork),
}

struct Shared<R> {
    cache: Arc<ThumbCache>,
    runner: R,
    office: OfficeEnv,
    infos: Mutex<HashMap<String, PreviewMeta>>,
    failures: Mutex<HashMap<String, FailureRecord>>,
    /// Exponentially weighted job durations per renderer name, in milliseconds.
    timings: Mutex<HashMap<&'static str, f64>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// First-render estimates (ms) before anything has been measured; PowerPoint, Word and Excel
/// figures are cold-start measurements on the development machine (Windows 11, Office 16).
fn default_estimate(renderer: &str) -> f64 {
    match renderer {
        "text" => 20.0,
        "wic" => 120.0,
        "raw_embedded" => 200.0,
        "windows_pdf" => 350.0,
        "wic_tile" => 150.0,
        "raw_tile" => 300.0,
        "windows_pdf_tile" => 450.0,
        "shell_thumbnail" => 900.0,
        "office_com_powerpoint" => 4_000.0,
        "office_com_word" | "office_com_excel" => 5_000.0,
        "libreoffice" => 8_000.0,
        _ => 500.0,
    }
}

impl<R: JobRunner> Shared<R> {
    fn estimate(&self, renderer: &str) -> u32 {
        let ms = lock(&self.timings)
            .get(renderer)
            .copied()
            .unwrap_or_else(|| default_estimate(renderer));
        ms.round().clamp(0.0, f64::from(u32::MAX)) as u32
    }

    fn record_timing(&self, renderer: &'static str, took: Duration) {
        let ms = took.as_secs_f64() * 1000.0;
        let mut t = lock(&self.timings);
        let e = t.entry(renderer).or_insert(ms);
        *e = *e * 0.7 + ms * 0.3;
    }

    fn record_failure(&self, name: &str, kind: FailureKind) {
        let mut f = lock(&self.failures);
        if f.len() >= MAX_ENTRIES {
            f.clear();
        }
        f.insert(
            name.to_owned(),
            FailureRecord {
                at: Instant::now(),
                kind,
            },
        );
    }

    fn failure(&self, name: &str) -> Option<FailureKind> {
        let mut f = lock(&self.failures);
        let rec = *f.get(name)?;
        if rec.at.elapsed() > rec.kind.ttl() {
            f.remove(name);
            return None;
        }
        Some(rec.kind)
    }

    fn store_info(&self, name: &str, meta: PreviewMeta) {
        let mut i = lock(&self.infos);
        if i.len() >= MAX_ENTRIES {
            i.clear();
        }
        i.insert(name.to_owned(), meta);
    }

    fn process(&self, work: Work) {
        match work {
            Work::Info(w) => self.process_info(w),
            Work::Page(w) => self.process_page(w),
            Work::Tile(w) => self.process_tile(w),
        }
    }

    fn process_tile(&self, w: TileWork) {
        let started = Instant::now();
        let name = w.key.name();
        match self.render_tile(&w) {
            Ok(bytes) => {
                if self.cache.put_named(&name, "webp", &bytes).is_err() {
                    self.record_failure(&name, FailureKind::Transient);
                } else if !matches!(w.plan, Plan::Office(_)) {
                    // An Office tile may include the one-off PDF export: not a tile's cost.
                    self.record_timing(tile_renderer(&w.plan), started.elapsed());
                }
            }
            Err(kind) => {
                let kind = if kind == FailureKind::NoPreview
                    && matches!(w.plan, Plan::Office(_))
                    && self.user_office_running(&w.plan)
                {
                    FailureKind::Transient
                } else {
                    kind
                };
                self.record_failure(&name, kind);
            }
        }
    }

    fn tile_job(&self, kind: RenderKind, ext: &str, key: &TileKey) -> RenderJob {
        let mut job = RenderJob::tile(kind, ext, key.page, key.level, key.x, key.y);
        job.quality = TILE_QUALITY;
        job.timeout_ms = TILE_TIMEOUT_MS;
        job
    }

    fn render_tile(&self, w: &TileWork) -> Result<Vec<u8>, FailureKind> {
        let key = &w.key;
        match &w.plan {
            Plan::Image(kind) => self.direct(&w.file, self.tile_job(*kind, &w.ext, key)),
            Plan::Pdf => self.direct(&w.file, self.tile_job(RenderKind::Pdf, &w.ext, key)),
            Plan::Office(OfficeBackend::Com(OfficeApp::PowerPoint)) => Err(FailureKind::NoPreview),
            Plan::Office(backend) => {
                let pdf = self.office_pdf(&w.file, &w.ext, backend, &key.identity)?;
                self.direct(&pdf, self.tile_job(RenderKind::Pdf, "pdf", key))
            }
            Plan::Video | Plan::Text | Plan::None => Err(FailureKind::NoPreview),
        }
    }

    fn process_info(&self, w: InfoWork) {
        let job = RenderJob::info(w.kind, &w.ext);
        match self.runner.run(&w.file, &job) {
            JobResult::Output(bytes) => match serde_json::from_slice::<PreviewMeta>(&bytes) {
                Ok(meta) => self.store_info(&w.name, meta),
                Err(_) => self.record_failure(&w.name, FailureKind::NoPreview),
            },
            JobResult::HostFailure => self.record_failure(&w.name, FailureKind::Transient),
            _ => self.record_failure(&w.name, FailureKind::NoPreview),
        }
    }

    fn process_page(&self, w: PageWork) {
        let started = Instant::now();
        let name = w.key.name();
        let ext = w.key.format.extension();
        match self.render(&w) {
            Ok(bytes) => {
                if self.cache.put_named(&name, ext, &bytes).is_err() {
                    self.record_failure(&name, FailureKind::Transient);
                } else {
                    self.record_timing(w.plan.renderer(), started.elapsed());
                }
            }
            Err(kind) => {
                let office = matches!(w.plan, Plan::Office(_));
                if office
                    && w.key.page == 1
                    && kind != FailureKind::PageOutOfRange
                    && let Some(bytes) = self.embedded_thumbnail(&w)
                {
                    let fallback = w.key.fallback().name();
                    if self.cache.put_named(&fallback, ext, &bytes).is_ok() {
                        self.record_failure(&name, FailureKind::Degraded);
                        return;
                    }
                }
                let kind = if office
                    && kind == FailureKind::NoPreview
                    && self.user_office_running(&w.plan)
                {
                    // The render was refused because the owner has Office open: not a verdict on
                    // the document.
                    FailureKind::Transient
                } else {
                    kind
                };
                self.record_failure(&name, kind);
            }
        }
    }

    fn user_office_running(&self, plan: &Plan) -> bool {
        match plan {
            Plan::Office(OfficeBackend::Com(app)) => self.runner.office_busy(*app),
            _ => false,
        }
    }

    fn job_for(
        &self,
        kind: RenderKind,
        ext: &str,
        page: u32,
        key: &PageKey,
        quality: u8,
    ) -> RenderJob {
        let mut job = RenderJob::page(kind, ext, page, key.width);
        job.format = key.format;
        job.quality = quality;
        if kind == RenderKind::Office {
            job.timeout_ms = OFFICE_TIMEOUT_MS;
        }
        job
    }

    fn render(&self, w: &PageWork) -> Result<Vec<u8>, FailureKind> {
        let key = &w.key;
        match &w.plan {
            Plan::Image(kind) => self.direct(
                &w.file,
                self.job_for(*kind, &w.ext, key.page, key, w.quality),
            ),
            Plan::Pdf => self.direct(
                &w.file,
                self.job_for(RenderKind::Pdf, &w.ext, key.page, key, w.quality),
            ),
            Plan::Video => self.direct(
                &w.file,
                self.job_for(RenderKind::Video, &w.ext, key.page, key, w.quality),
            ),
            Plan::Office(OfficeBackend::Com(OfficeApp::PowerPoint)) => self.direct(
                &w.file,
                self.job_for(RenderKind::Office, &w.ext, key.page, key, w.quality),
            ),
            Plan::Office(backend) => self.office_via_pdf(w, backend),
            Plan::Text | Plan::None => Err(FailureKind::NoPreview),
        }
    }

    fn direct(&self, file: &File, job: RenderJob) -> Result<Vec<u8>, FailureKind> {
        map_result(self.runner.run(file, &job))
    }

    /// Word, Excel and LibreOffice: export once, keep the PDF, render its pages.
    fn office_via_pdf(
        &self,
        w: &PageWork,
        backend: &OfficeBackend,
    ) -> Result<Vec<u8>, FailureKind> {
        let key = &w.key;
        let pdf_file = self.office_pdf(&w.file, &w.ext, backend, &key.identity)?;
        self.direct(
            &pdf_file,
            self.job_for(RenderKind::Pdf, "pdf", key.page, key, w.quality),
        )
    }

    /// The PDF export of an Office document: exported once (Office automation or LibreOffice),
    /// kept in the cache and opened from there afterwards. The first export also records the
    /// page count and tile sizes for `preview/info`.
    fn office_pdf(
        &self,
        file: &File,
        ext: &str,
        backend: &OfficeBackend,
        identity: &FileIdentity,
    ) -> Result<File, FailureKind> {
        let pdf = pdf_name(identity);
        let transient = |_: io::Error| FailureKind::Transient;
        let pdf_file = match self.cache.open_named_file(&pdf, "pdf").map_err(transient)? {
            Some(f) => f,
            None => {
                let result = match backend {
                    OfficeBackend::Com(_) => {
                        let mut export =
                            RenderJob::new(RenderKind::Office, ext, RenderOp::ExportPdf);
                        export.timeout_ms = OFFICE_TIMEOUT_MS;
                        self.runner.run(file, &export)
                    }
                    OfficeBackend::LibreOffice(soffice) => {
                        self.runner.convert_with_libreoffice(file, ext, soffice)
                    }
                };
                let bytes = map_result(result)?;
                self.cache
                    .put_named(&pdf, "pdf", &bytes)
                    .map_err(transient)?;
                self.cache
                    .open_named_file(&pdf, "pdf")
                    .map_err(transient)?
                    .ok_or(FailureKind::Transient)?
            }
        };
        // The page count is known now: remember it for `preview/info`.
        let iname = info_name(identity);
        if !lock(&self.infos).contains_key(&iname)
            && let JobResult::Output(bytes) = self
                .runner
                .run(&pdf_file, &RenderJob::info(RenderKind::Pdf, "pdf"))
            && let Ok(meta) = serde_json::from_slice::<PreviewMeta>(&bytes)
        {
            self.store_info(&iname, meta);
        }
        Ok(pdf_file)
    }

    /// The thumbnail the author's application embedded in the document, at the requested width.
    fn embedded_thumbnail(&self, w: &PageWork) -> Option<Vec<u8>> {
        let size = w.key.width.clamp(16, 4096);
        let mut job = RenderJob::thumb(RenderKind::Office, &w.ext, size);
        job.format = w.key.format;
        job.quality = w.quality;
        match self.runner.run(&w.file, &job) {
            JobResult::Output(bytes) => Some(bytes),
            _ => None,
        }
    }
}

fn map_result(result: JobResult) -> Result<Vec<u8>, FailureKind> {
    match result {
        JobResult::Output(bytes) => Ok(bytes),
        JobResult::PageOutOfRange | JobResult::TileOutOfRange => Err(FailureKind::PageOutOfRange),
        JobResult::HostFailure => Err(FailureKind::Transient),
        JobResult::Unsupported | JobResult::TimedOut | JobResult::DecoderError => {
            Err(FailureKind::NoPreview)
        }
    }
}

/// Whether a file previewed with `plan` has deep-zoom tiles: images, RAW, PDF and the Office
/// documents served through a PDF export. A PowerPoint slide is exported as a picture, so it has
/// none.
fn has_tiles(plan: &Plan) -> bool {
    match plan {
        Plan::Image(_) | Plan::Pdf => true,
        Plan::Office(OfficeBackend::Com(OfficeApp::PowerPoint)) => false,
        Plan::Office(_) => true,
        Plan::Video | Plan::Text | Plan::None => false,
    }
}

/// The `tile_size`, `tile_levels` and `tile_pages` of `preview/info`: absent unless some page is
/// large enough for a second level (a page that fits in one tile is served whole by `preview`).
fn tile_fields(
    plan: &Plan,
    meta: &PreviewMeta,
) -> (Option<u32>, Option<u32>, Option<Vec<TilePage>>) {
    if !has_tiles(plan) {
        return (None, None, None);
    }
    let pages: Vec<TilePage> = meta
        .tile_pages
        .iter()
        .take(MAX_TILE_PAGES)
        .map_while(|&(w, h)| Pyramid::new(w, h).map(|p| p.page()))
        .collect();
    let Some(levels) = pages.iter().map(|p| p.levels).max().filter(|&l| l >= 2) else {
        return (None, None, None);
    };
    (Some(TILE_SIZE), Some(levels), Some(pages))
}

// ---------------------------------------------------------------------------------------------
// The service

/// Cache + queue + runner behind the three preview endpoints. Share it behind an `Arc`.
pub struct PreviewService<R: JobRunner> {
    shared: Arc<Shared<R>>,
    queue: PreviewQueue<Work>,
    config: PreviewServiceConfig,
}

impl<R: JobRunner> std::fmt::Debug for PreviewService<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewService")
            .field("office", &self.shared.office)
            .field("queue", &self.queue)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl PreviewService<WorkerRunner> {
    /// The production service: one `gander-render.exe` per job, default queue (1 per device, 2
    /// overall). `cache` should be `ThumbCache::open_in(root, "previews", cap)`.
    pub fn with_worker(
        cache: Arc<ThumbCache>,
        worker: crate::render::worker::WorkerConfig,
        office: OfficeEnv,
    ) -> Self {
        Self::new(
            cache,
            WorkerRunner { config: worker },
            office,
            PreviewServiceConfig::default(),
        )
    }
}

/// What a cache lookup for a page found.
enum Lookup {
    Hit(PageResponse),
    Failed(PreviewError),
    Miss,
}

impl<R: JobRunner> PreviewService<R> {
    /// A service over any runner (tests use fakes).
    pub fn new(
        cache: Arc<ThumbCache>,
        runner: R,
        office: OfficeEnv,
        config: PreviewServiceConfig,
    ) -> Self {
        let shared = Arc::new(Shared {
            cache,
            runner,
            office,
            infos: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            timings: Mutex::new(HashMap::new()),
        });
        let handler_shared = Arc::clone(&shared);
        let queue = PreviewQueue::new(config.queue, move |_key: &str, work: Work| {
            handler_shared.process(work);
        });
        Self {
            shared,
            queue,
            config,
        }
    }

    /// Pauses or resumes rendering (battery below 15 %).
    pub fn set_paused(&self, paused: bool) {
        self.queue.set_paused(paused);
    }

    /// The cache behind the service.
    pub fn cache(&self) -> &Arc<ThumbCache> {
        &self.shared.cache
    }

    /// The Office setup this service detected (or was given).
    pub fn office(&self) -> &OfficeEnv {
        &self.shared.office
    }

    /// The `preview` kind a listing advertises for a file with this extension.
    pub fn predict(&self, ext: &str) -> PreviewKind {
        plan(ext, &self.shared.office).kind()
    }

    /// `capabilities.preview_kinds`.
    pub fn preview_kinds(&self) -> PreviewKinds {
        PreviewKinds {
            image: true,
            pdf: true,
            text: true,
            office: self.shared.office.capability(),
            video_poster: true,
            raw: true,
        }
    }

    fn identity(file: &File) -> Result<FileIdentity, PreviewError> {
        match file_identity(file) {
            Ok(id) => Ok(id),
            Err(e) if e.kind() == io::ErrorKind::IsADirectory => Err(PreviewError::Unsupported),
            Err(_) => Err(PreviewError::Internal),
        }
    }

    fn retry_after(&self, status: Enqueued, renderer: &str) -> u64 {
        let est = u64::from(self.shared.estimate(renderer));
        match status {
            Enqueued::Queued { position } => (est * position.max(1) as u64 / 2).clamp(250, 5_000),
            Enqueued::Running => (est / 2).clamp(250, 3_000),
        }
    }

    /// Polls `check` every 10 ms for up to `wait`.
    fn wait_for<T>(wait: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(v) = check() {
                return Some(v);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    // -----------------------------------------------------------------------------------------
    // /preview/info

    /// Answers `GET /preview/info`: what a preview of this file would be.
    ///
    /// Kinds that need the file parsed (images, PDF, presentations) run a metadata job on the
    /// preview queue and wait up to [`PreviewServiceConfig::info_wait`]; the result is kept in
    /// memory per file version. Everything else is answered at once. A file with no preview
    /// answers `kind: none` (the client suggests Stream or Fetch), not an error.
    pub fn info(&self, req: InfoRequest) -> Result<PreviewInfo, PreviewError> {
        let ext = normalise_ext(&req.ext);
        let identity = Self::identity(&req.file)?;
        let mut plan = plan(&ext, &self.shared.office);
        if plan == Plan::None && super::may_be_text(&ext) && text::probe(&req.file).is_ok() {
            plan = Plan::Text; // an unknown extension that sniffs as text
        }
        match &plan {
            Plan::None => Ok(self.info_of(&plan, PreviewMeta::default(), None, plan.renderer())),
            Plan::Text => match text::decoded_total(&req.file) {
                Ok((_, total)) => {
                    Ok(self.info_of(&plan, PreviewMeta::default(), Some(total), plan.renderer()))
                }
                Err(TextError::Binary) => {
                    Ok(self.info_of(&Plan::None, PreviewMeta::default(), None, "none"))
                }
                Err(TextError::IsDirectory) => Err(PreviewError::Unsupported),
                Err(TextError::Io(_)) => Err(PreviewError::Internal),
            },
            Plan::Video => Ok(self.info_of(&plan, PreviewMeta::default(), None, plan.renderer())),
            Plan::Image(kind) => {
                let meta = self.info_by_job(req, &identity, *kind, &ext, &plan)?;
                Ok(self.info_of(&plan, meta, None, plan.renderer()))
            }
            Plan::Pdf => {
                let meta = self.info_by_job(req, &identity, RenderKind::Pdf, &ext, &plan)?;
                Ok(self.info_of(&plan, meta, None, plan.renderer()))
            }
            Plan::Office(OfficeBackend::Com(OfficeApp::PowerPoint)) => {
                let meta = self.info_by_job(req, &identity, RenderKind::Office, &ext, &plan)?;
                Ok(self.info_of(&plan, meta, None, plan.renderer()))
            }
            Plan::Office(_) => self.info_office_via_pdf(req, &identity, &plan),
        }
    }

    fn info_of(
        &self,
        plan: &Plan,
        meta: PreviewMeta,
        text_bytes: Option<u64>,
        estimate_for: &str,
    ) -> PreviewInfo {
        let (tile_size, tile_levels, tile_pages) = tile_fields(plan, &meta);
        PreviewInfo {
            kind: plan.kind(),
            pages: meta.pages,
            width: meta.width,
            height: meta.height,
            text_bytes,
            renderer: plan.renderer().to_owned(),
            estimated_ms: self.shared.estimate(estimate_for),
            tile_size,
            tile_levels,
            tile_pages,
        }
    }

    /// Looks for the metadata of `name`, as a finished result or a remembered failure.
    fn info_state(&self, name: &str) -> Option<Result<PreviewMeta, PreviewError>> {
        if let Some(meta) = lock(&self.shared.infos).get(name) {
            return Some(Ok(meta.clone()));
        }
        self.shared.failure(name).map(|kind| {
            Err(match kind {
                FailureKind::Transient => PreviewError::Internal,
                _ => PreviewError::Unsupported,
            })
        })
    }

    fn info_by_job(
        &self,
        req: InfoRequest,
        identity: &FileIdentity,
        kind: RenderKind,
        ext: &str,
        plan: &Plan,
    ) -> Result<PreviewMeta, PreviewError> {
        let name = info_name(identity);
        if let Some(done) = self.info_state(&name) {
            return done;
        }
        let device = req.device;
        let ext = ext.to_owned();
        let work = InfoWork {
            file: req.file,
            name: name.clone(),
            kind,
            ext,
        };
        let status = self.queue.submit(&name, &device, || Work::Info(work));
        match Self::wait_for(self.config.info_wait, || self.info_state(&name)) {
            Some(done) => done,
            None => Err(PreviewError::Generating {
                retry_after_ms: self.retry_after(status, plan.renderer()),
            }),
        }
    }

    /// Word, Excel and LibreOffice documents: the page count exists once the PDF does.
    fn info_office_via_pdf(
        &self,
        req: InfoRequest,
        identity: &FileIdentity,
        plan: &Plan,
    ) -> Result<PreviewInfo, PreviewError> {
        let name = info_name(identity);
        let pdf = pdf_name(identity);
        let have_pdf = self.shared.cache.contains_named(&pdf).unwrap_or(false);
        // Rendered pages cost a PDF render each; the first one also pays for the export.
        let estimate_for = if have_pdf {
            "windows_pdf"
        } else {
            plan.renderer()
        };
        if let Some(Ok(meta)) = self.info_state(&name) {
            return Ok(self.info_of(plan, meta, None, estimate_for));
        }
        if have_pdf && let Ok(Some(file)) = self.shared.cache.open_named_file(&pdf, "pdf") {
            let work = InfoWork {
                file,
                name: name.clone(),
                kind: RenderKind::Pdf,
                ext: "pdf".to_owned(),
            };
            let status = self.queue.submit(&name, &req.device, || Work::Info(work));
            return match Self::wait_for(self.config.info_wait, || self.info_state(&name)) {
                Some(Ok(meta)) => Ok(self.info_of(plan, meta, None, "windows_pdf")),
                Some(Err(e)) => Err(e),
                None => Err(PreviewError::Generating {
                    retry_after_ms: self.retry_after(status, "windows_pdf"),
                }),
            };
        }
        // Not rendered yet: the page count is unknown (null), the estimate says what the first
        // page will cost.
        Ok(self.info_of(plan, PreviewMeta::default(), None, estimate_for))
    }

    // -----------------------------------------------------------------------------------------
    // /preview

    fn lookup(&self, key: &PageKey, plan: &Plan) -> Result<Lookup, PreviewError> {
        let ext = key.format.extension();
        let pages = lock(&self.shared.infos)
            .get(&info_name(&key.identity))
            .and_then(|m| m.pages);
        let response =
            |bytes: Vec<u8>, key: &PageKey, renderer: &'static str, degraded: bool| PageResponse {
                bytes,
                content_type: key.format.content_type(),
                etag: key.etag(),
                cache_control: PREVIEW_CACHE_CONTROL,
                page: key.page,
                pages,
                renderer,
                degraded,
            };
        let primary = key.primary();
        match self.shared.cache.get_named(&primary.name(), ext) {
            Ok(Some(bytes)) => {
                return Ok(Lookup::Hit(response(
                    bytes,
                    &primary,
                    plan.renderer(),
                    false,
                )));
            }
            Ok(None) => {}
            Err(_) => return Err(PreviewError::Internal),
        }
        let Some(kind) = self.shared.failure(&primary.name()) else {
            return Ok(Lookup::Miss);
        };
        Ok(match kind {
            FailureKind::Degraded => {
                let fallback = primary.fallback();
                match self.shared.cache.get_named(&fallback.name(), ext) {
                    Ok(Some(bytes)) => {
                        Lookup::Hit(response(bytes, &fallback, "office_thumbnail", true))
                    }
                    _ => Lookup::Failed(PreviewError::Internal),
                }
            }
            FailureKind::NoPreview => Lookup::Failed(PreviewError::Unsupported),
            FailureKind::PageOutOfRange => {
                Lookup::Failed(PreviewError::BadRequest("page out of range"))
            }
            FailureKind::Transient => Lookup::Failed(PreviewError::Internal),
        })
    }

    /// Answers `GET /preview`: one page of the document, a resized image or a poster frame. A
    /// miss queues the render and waits up to [`PreviewServiceConfig::page_wait`], then answers
    /// `503 generating` (the client polls with the `retry_after_ms` it is given).
    pub fn page(&self, req: PageRequest) -> Result<PageResponse, PreviewError> {
        let page = req.page.unwrap_or(1);
        if page == 0 {
            return Err(PreviewError::BadRequest("page must be 1 or more"));
        }
        let width = match req.width {
            None => DEFAULT_PREVIEW_WIDTH,
            Some(0) => return Err(PreviewError::BadRequest("width must be 1 or more")),
            Some(w) => w.clamp(PREVIEW_MIN_WIDTH, PREVIEW_MAX_WIDTH),
        };
        let ext = normalise_ext(&req.ext);
        let identity = Self::identity(&req.file)?;
        let plan = plan(&ext, &self.shared.office);
        if matches!(plan, Plan::None | Plan::Text) {
            return Err(PreviewError::Unsupported);
        }
        if !plan.is_paged() && page != 1 {
            return Err(PreviewError::BadRequest("page out of range"));
        }
        let format = match req.format {
            ImageFormat::Webp => OutputFormat::Webp,
            ImageFormat::Jpeg => OutputFormat::Jpeg,
        };
        let key = PageKey {
            identity,
            page,
            width,
            format,
            degraded: false,
        };
        match self.lookup(&key, &plan)? {
            Lookup::Hit(r) => return Ok(r),
            Lookup::Failed(e) => return Err(e),
            Lookup::Miss => {}
        }
        // A page past the end of a document whose length is already known never reaches a worker.
        let pages = lock(&self.shared.infos)
            .get(&info_name(&identity))
            .and_then(|m| m.pages);
        if pages.is_some_and(|n| page > n) {
            return Err(PreviewError::BadRequest("page out of range"));
        }
        let quality = match format {
            OutputFormat::Webp => 85,
            OutputFormat::Jpeg => 88,
        };
        let name = key.name();
        let device = req.device;
        let work = PageWork {
            file: req.file,
            key,
            plan: plan.clone(),
            ext,
            quality,
        };
        let status = self.queue.submit(&name, &device, || Work::Page(work));
        let outcome = Self::wait_for(self.config.page_wait, || match self.lookup(&key, &plan) {
            Ok(Lookup::Miss) => None,
            other => Some(other),
        });
        match outcome {
            Some(Ok(Lookup::Hit(r))) => Ok(r),
            Some(Ok(Lookup::Failed(e))) | Some(Err(e)) => Err(e),
            Some(Ok(Lookup::Miss)) | None => Err(PreviewError::Generating {
                retry_after_ms: self.retry_after(status, plan.renderer()),
            }),
        }
    }

    // -----------------------------------------------------------------------------------------
    // /preview/tile

    /// What the cache or the failure memory says about a tile: a hit, a verdict, or nothing yet.
    fn tile_lookup(&self, key: &TileKey) -> Result<Option<TileResponse>, PreviewError> {
        let name = key.name();
        match self.shared.cache.get_named(&name, "webp") {
            Ok(Some(bytes)) => {
                return Ok(Some(TileResponse {
                    bytes,
                    content_type: OutputFormat::Webp.content_type(),
                    etag: key.etag(),
                    cache_control: PREVIEW_CACHE_CONTROL,
                }));
            }
            Ok(None) => {}
            Err(_) => return Err(PreviewError::Internal),
        }
        match self.shared.failure(&name) {
            None => Ok(None),
            Some(FailureKind::NoPreview) => Err(PreviewError::Unsupported),
            Some(FailureKind::PageOutOfRange) => Err(PreviewError::BadRequest(TILE_RANGE_MSG)),
            Some(FailureKind::Transient | FailureKind::Degraded) => Err(PreviewError::Internal),
        }
    }

    /// Answers `GET /preview/tile`: one 512 px tile of a page, for deep zoom. Tiles exist for
    /// images, RAW previews, PDF and Office documents rendered through PDF (everything else:
    /// `Unsupported`); a level, column, row or page the page does not have is a
    /// `BadRequest`. A miss is queued behind pages (see [`PreviewQueue::submit_low`]) and waited
    /// for up to [`PreviewServiceConfig::page_wait`], then answered `503 generating`; a client
    /// that no longer needs the tile stops asking and the queue drops it.
    pub fn tile(&self, req: TileRequest) -> Result<TileResponse, PreviewError> {
        let page = req.page.unwrap_or(1);
        if page == 0 {
            return Err(PreviewError::BadRequest("page must be 1 or more"));
        }
        if req.level >= MAX_LEVELS || req.x > MAX_TILE_INDEX || req.y > MAX_TILE_INDEX {
            return Err(PreviewError::BadRequest(TILE_RANGE_MSG));
        }
        let ext = normalise_ext(&req.ext);
        let identity = Self::identity(&req.file)?;
        let plan = plan(&ext, &self.shared.office);
        if !has_tiles(&plan) {
            return Err(PreviewError::Unsupported);
        }
        if !plan.is_paged() && page != 1 {
            return Err(PreviewError::BadRequest(TILE_RANGE_MSG));
        }
        let key = TileKey {
            identity,
            page,
            level: req.level,
            x: req.x,
            y: req.y,
        };
        if let Some(hit) = self.tile_lookup(&key)? {
            return Ok(hit);
        }
        // What `preview/info` already measured rules out tiles that do not exist without a
        // worker (and a page past the end of a known document).
        let known = lock(&self.shared.infos).get(&info_name(&identity)).cloned();
        if let Some(meta) = &known {
            if meta.pages.is_some_and(|n| page > n) {
                return Err(PreviewError::BadRequest(TILE_RANGE_MSG));
            }
            let size = meta.tile_pages.get(page as usize - 1);
            if let Some(&(w, h)) = size
                && Pyramid::new(w, h)
                    .and_then(|p| p.tile(req.level, req.x, req.y))
                    .is_none()
            {
                return Err(PreviewError::BadRequest(TILE_RANGE_MSG));
            }
        }
        let office_without_pdf = matches!(plan, Plan::Office(_))
            && !self
                .shared
                .cache
                .contains_named(&pdf_name(&identity))
                .unwrap_or(false);
        let estimate_for = if office_without_pdf {
            plan.renderer()
        } else {
            tile_renderer(&plan)
        };
        let name = key.name();
        let device = req.device;
        let work = TileWork {
            file: req.file,
            key,
            plan,
            ext,
        };
        let status = self
            .queue
            .submit_low(&name, &device, TILE_STALE_AFTER, || Work::Tile(work));
        let outcome = Self::wait_for(self.config.page_wait, || match self.tile_lookup(&key) {
            Ok(None) => None,
            other => Some(other),
        });
        match outcome {
            Some(Ok(Some(r))) => Ok(r),
            Some(Err(e)) => Err(e),
            Some(Ok(None)) | None => Err(PreviewError::Generating {
                retry_after_ms: self.retry_after(status, estimate_for),
            }),
        }
    }

    // -----------------------------------------------------------------------------------------
    // /preview/text

    /// Answers `GET /preview/text`: decoded text from `offset`, at most `limit` bytes (default
    /// 64 KiB, hard cap 1 MiB). Runs on the calling thread: a bounded read, no worker.
    pub fn text(&self, req: TextRequest) -> Result<TextResponse, PreviewError> {
        let limit = req.limit.unwrap_or(text::DEFAULT_LIMIT);
        if limit == 0 || limit > text::MAX_LIMIT {
            return Err(PreviewError::BadRequest(
                "limit must be between 1 and 1048576",
            ));
        }
        match text::read_chunk(&req.file, req.offset, limit) {
            Ok(chunk) => Ok(TextResponse {
                has_more: chunk.has_more(),
                bytes: chunk.bytes,
                content_type: TEXT_CONTENT_TYPE,
                encoding: chunk.encoding.label(),
                total: chunk.total,
                offset: chunk.offset,
                next_offset: chunk.next_offset,
                window_truncated: chunk.window_truncated,
            }),
            Err(TextError::Binary) => Err(PreviewError::Binary),
            Err(TextError::IsDirectory) => Err(PreviewError::Unsupported),
            Err(TextError::Io(_)) => Err(PreviewError::Internal),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Handler = Box<dyn Fn(&RenderJob, &File) -> JobResult + Send + Sync>;

    /// A runner that records every job and answers from a closure.
    struct Fake {
        calls: Mutex<Vec<RenderJob>>,
        libreoffice: AtomicUsize,
        handler: Handler,
        busy: bool,
    }

    #[derive(Clone)]
    struct FakeRunner(Arc<Fake>);

    impl FakeRunner {
        fn new(handler: impl Fn(&RenderJob, &File) -> JobResult + Send + Sync + 'static) -> Self {
            Self(Arc::new(Fake {
                calls: Mutex::new(Vec::new()),
                libreoffice: AtomicUsize::new(0),
                handler: Box::new(handler),
                busy: false,
            }))
        }

        fn calls(&self) -> Vec<RenderJob> {
            self.0.calls.lock().unwrap().clone()
        }

        fn count(&self, pred: impl Fn(&RenderJob) -> bool) -> usize {
            self.calls().iter().filter(|j| pred(j)).count()
        }
    }

    impl JobRunner for FakeRunner {
        fn run(&self, file: &File, job: &RenderJob) -> JobResult {
            self.0.calls.lock().unwrap().push(job.clone());
            (self.0.handler)(job, file)
        }

        fn convert_with_libreoffice(&self, _file: &File, _ext: &str, _soffice: &Path) -> JobResult {
            self.0.libreoffice.fetch_add(1, Ordering::SeqCst);
            JobResult::Output(b"%PDF fake from libreoffice".to_vec())
        }

        fn office_busy(&self, _app: OfficeApp) -> bool {
            self.0.busy
        }
    }

    fn meta_json(pages: u32) -> Vec<u8> {
        serde_json::to_vec(&PreviewMeta {
            pages: Some(pages),
            width: Some(794),
            height: Some(1123),
            tile_pages: Vec::new(),
        })
        .unwrap()
    }

    /// Metadata that also lists the top-level tile size of each page.
    fn meta_json_tiles(sizes: &[(u32, u32)]) -> Vec<u8> {
        serde_json::to_vec(&PreviewMeta {
            pages: Some(sizes.len() as u32),
            width: Some(794),
            height: Some(1123),
            tile_pages: sizes.to_vec(),
        })
        .unwrap()
    }

    /// Answers like a healthy worker: `page:<n>:<width>` for pages, JSON for info, a PDF for
    /// the Office export, `thumb` for thumbnails.
    fn healthy(pages: u32) -> impl Fn(&RenderJob, &File) -> JobResult + Send + Sync + 'static {
        move |job, _| match job.op {
            RenderOp::Page { page, width } => {
                JobResult::Output(format!("page:{page}:{width}:{}", job.quality).into_bytes())
            }
            RenderOp::Info => JobResult::Output(meta_json(pages)),
            RenderOp::ExportPdf => JobResult::Output(b"%PDF fake".to_vec()),
            RenderOp::Thumb { .. } => JobResult::Output(b"thumb".to_vec()),
            RenderOp::Tile { page, level, x, y } => JobResult::Output(
                format!("tile:{page}:{level}:{x}:{y}:{}", job.quality).into_bytes(),
            ),
        }
    }

    struct Fixture {
        dir: PathBuf,
        svc: PreviewService<FakeRunner>,
        runner: FakeRunner,
    }

    impl Fixture {
        fn new(name: &str, runner: FakeRunner, office: OfficeEnv) -> Self {
            Self::with_config(name, runner, office, PreviewServiceConfig::default())
        }

        fn with_config(
            name: &str,
            runner: FakeRunner,
            office: OfficeEnv,
            config: PreviewServiceConfig,
        ) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("gander-previewsvc-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let cache =
                Arc::new(ThumbCache::open_in(&dir.join("cache"), "previews", 1 << 30).unwrap());
            let svc = PreviewService::new(cache, runner.clone(), office, config);
            Self { dir, svc, runner }
        }

        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }

        fn page_req(&self, path: &Path, device: &str) -> PageRequest {
            PageRequest {
                file: File::open(path).unwrap(),
                ext: path.extension().unwrap().to_string_lossy().into_owned(),
                device: device.to_owned(),
                page: None,
                width: None,
                format: ImageFormat::Webp,
            }
        }

        fn page(
            &self,
            path: &Path,
            page: Option<u32>,
            width: Option<u32>,
        ) -> Result<PageResponse, PreviewError> {
            let mut req = self.page_req(path, "dev-a");
            req.page = page;
            req.width = width;
            self.svc.page(req)
        }

        fn info(&self, path: &Path) -> Result<PreviewInfo, PreviewError> {
            self.svc.info(InfoRequest {
                file: File::open(path).unwrap(),
                ext: path
                    .extension()
                    .map_or(String::new(), |e| e.to_string_lossy().into_owned()),
                device: "dev-a".into(),
            })
        }

        fn text(
            &self,
            path: &Path,
            offset: u64,
            limit: Option<u64>,
        ) -> Result<TextResponse, PreviewError> {
            self.svc.text(TextRequest {
                file: File::open(path).unwrap(),
                offset,
                limit,
            })
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn async_config() -> PreviewServiceConfig {
        PreviewServiceConfig {
            page_wait: Duration::ZERO,
            info_wait: Duration::ZERO,
            ..PreviewServiceConfig::default()
        }
    }

    /// Polls like a client honouring `retry_after_ms`.
    fn poll<T>(mut ask: impl FnMut() -> Result<T, PreviewError>) -> Result<T, PreviewError> {
        for _ in 0..400 {
            match ask() {
                Err(PreviewError::Generating { retry_after_ms }) => {
                    assert!((250..=5000).contains(&retry_after_ms), "{retry_after_ms}");
                    std::thread::sleep(Duration::from_millis(15));
                }
                other => return other,
            }
        }
        panic!("never finished");
    }

    #[test]
    fn a_page_is_503_then_200_then_served_from_cache() {
        let fx = Fixture::with_config(
            "flow",
            FakeRunner::new(healthy(1)),
            OfficeEnv::none(),
            async_config(),
        );
        let png = fx.file("a.png", b"png bytes");
        let first = fx.page(&png, None, Some(640));
        assert!(matches!(first, Err(PreviewError::Generating { .. })));
        assert_eq!(first.unwrap_err().http_status(), 503);
        let ok = poll(|| fx.page(&png, None, Some(640))).unwrap();
        assert_eq!(ok.bytes, b"page:1:640:85");
        assert_eq!(ok.content_type, "image/webp");
        assert_eq!(ok.cache_control, PREVIEW_CACHE_CONTROL);
        assert!(ok.etag.starts_with("\"p-"));
        assert_eq!((ok.page, ok.renderer, ok.degraded), (1, "wic", false));
        // Cached: same bytes and ETag, no second render.
        let again = fx.page(&png, Some(1), Some(640)).unwrap();
        assert_eq!(again, ok);
        assert_eq!(
            fx.runner.count(|j| matches!(j.op, RenderOp::Page { .. })),
            1
        );
    }

    #[test]
    fn width_page_and_format_are_validated_clamped_and_keyed() {
        let fx = Fixture::new("params", FakeRunner::new(healthy(1)), OfficeEnv::none());
        let png = fx.file("a.png", b"x");
        assert_eq!(
            fx.page(&png, Some(0), None).unwrap_err(),
            PreviewError::BadRequest("page must be 1 or more")
        );
        assert_eq!(fx.page(&png, None, Some(0)).unwrap_err().http_status(), 400);
        // An image has one page.
        assert_eq!(
            fx.page(&png, Some(2), None).unwrap_err(),
            PreviewError::BadRequest("page out of range")
        );
        // Default width, clamped maximum and minimum.
        assert_eq!(fx.page(&png, None, None).unwrap().bytes, b"page:1:1080:85");
        assert_eq!(
            fx.page(&png, None, Some(9999)).unwrap().bytes,
            b"page:1:2048:85"
        );
        assert_eq!(fx.page(&png, None, Some(3)).unwrap().bytes, b"page:1:16:85");
        // JPEG is its own entry, at quality 88, with its own content type and ETag.
        let mut req = fx.page_req(&png, "dev-a");
        req.format = ImageFormat::Jpeg;
        let jpeg = fx.svc.page(req).unwrap();
        assert_eq!(jpeg.content_type, "image/jpeg");
        assert_eq!(jpeg.bytes, b"page:1:1080:88");
        assert_ne!(jpeg.etag, fx.page(&png, None, None).unwrap().etag);
    }

    #[test]
    fn rename_keeps_the_entry_and_touch_invalidates_it() {
        let fx = Fixture::new("rename", FakeRunner::new(healthy(1)), OfficeEnv::none());
        let a = fx.file("photo.jpg", b"abc");
        let first = fx.page(&a, None, Some(800)).unwrap();
        let b = fx.dir.join("renamed.jpg");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(
            fx.page(&b, None, Some(800)).unwrap(),
            first,
            "cache hit after rename"
        );
        assert_eq!(
            fx.runner.count(|j| matches!(j.op, RenderOp::Page { .. })),
            1
        );

        let f = std::fs::OpenOptions::new().write(true).open(&b).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        let touched = fx.page(&b, None, Some(800)).unwrap();
        assert_ne!(touched.etag, first.etag, "a new version has a new ETag");
        assert_eq!(
            fx.runner.count(|j| matches!(j.op, RenderOp::Page { .. })),
            2
        );
    }

    #[test]
    fn types_without_a_preview_are_415_and_text_has_its_own_endpoint() {
        let fx = Fixture::new("refuse", FakeRunner::new(healthy(1)), OfficeEnv::none());
        for name in ["notes.txt", "song.flac", "thing.bin", "deck.pptx"] {
            let p = fx.file(name, b"x");
            let e = fx.page(&p, None, None).unwrap_err();
            assert_eq!(e, PreviewError::Unsupported, "{name}");
            assert_eq!(e.http_status(), 415);
            assert!(
                !e.to_body().error.message.contains(name),
                "no paths in messages"
            );
        }
        assert_eq!(fx.runner.calls().len(), 0, "no worker is started for them");
    }

    #[test]
    fn a_failed_render_is_415_not_an_endless_503() {
        let runner = FakeRunner::new(|_, _| JobResult::DecoderError);
        let fx = Fixture::with_config("fail", runner, OfficeEnv::none(), async_config());
        let bad = fx.file("broken.pdf", b"%PDF-");
        let end = poll(|| fx.page(&bad, None, None)).unwrap_err();
        assert_eq!(end, PreviewError::Unsupported);
        assert_eq!(
            fx.page(&bad, None, None).unwrap_err(),
            PreviewError::Unsupported
        );
        assert_eq!(
            fx.runner.count(|j| matches!(j.op, RenderOp::Page { .. })),
            1,
            "remembered"
        );
        // A host-side failure is a 500 and is retried after its short memory expires.
        let runner = FakeRunner::new(|_, _| JobResult::HostFailure);
        let fx = Fixture::with_config("hostfail", runner, OfficeEnv::none(), async_config());
        let png = fx.file("a.png", b"x");
        assert_eq!(
            poll(|| fx.page(&png, None, None)).unwrap_err(),
            PreviewError::Internal
        );
    }

    #[test]
    fn pdf_info_comes_from_a_job_and_bounds_the_pages() {
        let fx = Fixture::new("pdfinfo", FakeRunner::new(healthy(3)), OfficeEnv::none());
        let pdf = fx.file("doc.pdf", b"%PDF-1.4");
        let info = fx.info(&pdf).unwrap();
        assert_eq!(info.kind, PreviewKind::Pdf);
        assert_eq!(
            (info.pages, info.width, info.height),
            (Some(3), Some(794), Some(1123))
        );
        assert_eq!(info.renderer, "windows_pdf");
        assert_eq!(info.text_bytes, None);
        assert!(info.estimated_ms > 0);
        // Remembered: a second info does not run another job.
        fx.info(&pdf).unwrap();
        assert_eq!(fx.runner.count(|j| j.op == RenderOp::Info), 1);
        // Pages 1..=3 render, page 4 is rejected without a worker.
        assert_eq!(fx.page(&pdf, Some(3), Some(500)).unwrap().page, 3);
        let before = fx.runner.calls().len();
        assert_eq!(
            fx.page(&pdf, Some(4), Some(500)).unwrap_err(),
            PreviewError::BadRequest("page out of range")
        );
        assert_eq!(fx.runner.calls().len(), before);
        let body = PreviewError::BadRequest("page out of range").to_body();
        assert_eq!(body.error.code, ErrorCode::BadRequest);
        // The page response reports the count too.
        assert_eq!(fx.page(&pdf, Some(1), Some(500)).unwrap().pages, Some(3));
    }

    #[test]
    fn a_page_past_the_end_reported_by_the_worker_is_a_client_error() {
        let runner = FakeRunner::new(|_, _| JobResult::PageOutOfRange);
        let fx = Fixture::with_config("oor", runner, OfficeEnv::none(), async_config());
        let pdf = fx.file("doc.pdf", b"%PDF-1.4");
        let end = poll(|| fx.page(&pdf, Some(9), None)).unwrap_err();
        assert_eq!(end, PreviewError::BadRequest("page out of range"));
        assert_eq!(end.http_status(), 400);
    }

    #[test]
    fn info_without_a_job_for_text_video_and_unpreviewable_files() {
        let fx = Fixture::new("infokinds", FakeRunner::new(healthy(1)), OfficeEnv::none());
        let txt = fx.file("notes.txt", "héllo wörld".as_bytes());
        let info = fx.info(&txt).unwrap();
        assert_eq!(info.kind, PreviewKind::Text);
        assert_eq!(info.text_bytes, Some("héllo wörld".len() as u64));
        assert_eq!(info.renderer, "text");
        assert_eq!(info.pages, None);

        // An unknown extension that sniffs as text is text; one that does not is `none`.
        let log = fx.file("run.weirdext", b"line one\nline two\n");
        assert_eq!(fx.info(&log).unwrap().kind, PreviewKind::Text);
        let bin = fx.file("blob.weirdext", &[0u8, 1, 2, 3, 0, 0, 9]);
        let none = fx.info(&bin).unwrap();
        assert_eq!(
            (none.kind, none.renderer.as_str(), none.estimated_ms > 0),
            (PreviewKind::None, "none", true)
        );
        // A .txt that is really binary is `none` too.
        let fake_txt = fx.file("fake.txt", &[0u8, 1, 2, 3, 0, 0, 9]);
        assert_eq!(fx.info(&fake_txt).unwrap().kind, PreviewKind::None);

        let mp4 = fx.file("clip.mp4", b"not a video");
        let v = fx.info(&mp4).unwrap();
        assert_eq!(
            (v.kind, v.renderer.as_str()),
            (PreviewKind::VideoPoster, "shell_thumbnail")
        );
        // Without Office the deck has no renderer: `none`.
        let pptx = fx.file("deck.pptx", b"zip");
        assert_eq!(fx.info(&pptx).unwrap().kind, PreviewKind::None);
        assert_eq!(fx.runner.calls().len(), 0, "none of these needed a worker");
    }

    #[test]
    fn a_corrupt_file_has_no_info() {
        let runner = FakeRunner::new(|_, _| JobResult::DecoderError);
        let fx = Fixture::new("badinfo", runner, OfficeEnv::none());
        let pdf = fx.file("broken.pdf", b"%PDF-");
        assert_eq!(fx.info(&pdf).unwrap_err(), PreviewError::Unsupported);
    }

    fn powerpoint_env() -> OfficeEnv {
        OfficeEnv {
            powerpoint: true,
            ..OfficeEnv::none()
        }
    }

    #[test]
    fn powerpoint_renders_slides_and_degrades_to_the_embedded_thumbnail_for_page_one() {
        // The slide export fails (a document PowerPoint cannot open); the thumbnail works.
        let runner = FakeRunner::new(|job, _| match job.op {
            RenderOp::Page { .. } => JobResult::DecoderError,
            RenderOp::Thumb { size } => JobResult::Output(format!("embedded:{size}").into_bytes()),
            RenderOp::Info => JobResult::Output(meta_json(5)),
            RenderOp::ExportPdf => JobResult::Unsupported,
            RenderOp::Tile { .. } => JobResult::Unsupported,
        });
        let fx = Fixture::with_config("ppt-degrade", runner, powerpoint_env(), async_config());
        let deck = fx.file("deck.pptx", b"zip");
        let page1 = poll(|| fx.page(&deck, Some(1), Some(900))).unwrap();
        assert!(page1.degraded);
        assert_eq!(page1.renderer, "office_thumbnail");
        assert_eq!(page1.bytes, b"embedded:900");
        // Served again from the cache without another render attempt.
        let calls = fx.runner.calls().len();
        assert_eq!(fx.page(&deck, Some(1), Some(900)).unwrap(), page1);
        assert_eq!(fx.runner.calls().len(), calls);
        // Page 2 has no thumbnail to fall back to.
        let end = poll(|| fx.page(&deck, Some(2), Some(900))).unwrap_err();
        assert_eq!(end, PreviewError::Unsupported);
    }

    #[test]
    fn no_thumbnail_and_a_failed_render_is_415() {
        let runner = FakeRunner::new(|_, _| JobResult::Unsupported);
        let fx = Fixture::with_config("ppt-none", runner, powerpoint_env(), async_config());
        let deck = fx.file("plain.pptx", b"zip");
        assert_eq!(
            poll(|| fx.page(&deck, None, None)).unwrap_err(),
            PreviewError::Unsupported
        );
    }

    #[test]
    fn the_owners_open_powerpoint_is_a_temporary_condition_not_a_verdict() {
        let runner = FakeRunner(Arc::new(Fake {
            calls: Mutex::new(Vec::new()),
            libreoffice: AtomicUsize::new(0),
            handler: Box::new(|_, _| JobResult::Unsupported),
            busy: true,
        }));
        let fx = Fixture::with_config("ppt-busy", runner, powerpoint_env(), async_config());
        let deck = fx.file("plain.pptx", b"zip");
        // 500 (retry later) rather than a remembered 415.
        assert_eq!(
            poll(|| fx.page(&deck, None, None)).unwrap_err(),
            PreviewError::Internal
        );
    }

    #[test]
    fn word_documents_export_to_pdf_once_and_pages_come_from_the_pdf() {
        let word = OfficeEnv {
            word: true,
            ..OfficeEnv::none()
        };
        let fx = Fixture::new("word", FakeRunner::new(healthy(2)), word);
        let doc = fx.file("report.docx", b"zip");
        // Before any render the page count is unknown.
        let before = fx.info(&doc).unwrap();
        assert_eq!((before.kind, before.pages), (PreviewKind::Office, None));
        assert_eq!(before.renderer, "office_com_word");

        let p1 = fx.page(&doc, Some(1), Some(700)).unwrap();
        let p2 = fx.page(&doc, Some(2), Some(700)).unwrap();
        assert_eq!(
            (p1.bytes.as_slice(), p2.bytes.as_slice()),
            (&b"page:1:700:85"[..], &b"page:2:700:85"[..])
        );
        assert_eq!(
            fx.runner.count(|j| j.op == RenderOp::ExportPdf),
            1,
            "one Office export"
        );
        assert_eq!(
            fx.runner
                .count(|j| matches!(j.op, RenderOp::Page { .. }) && j.kind == RenderKind::Pdf),
            2
        );
        assert_eq!(
            fx.runner
                .count(|j| matches!(j.op, RenderOp::Page { .. }) && j.kind == RenderKind::Office),
            0
        );
        // Now the page count is known and bounds the requests.
        let after = fx.info(&doc).unwrap();
        assert_eq!(after.pages, Some(2));
        assert_eq!(
            fx.page(&doc, Some(3), Some(700)).unwrap_err(),
            PreviewError::BadRequest("page out of range")
        );
    }

    #[test]
    fn libreoffice_converts_when_office_is_absent() {
        let lo = OfficeEnv {
            libreoffice: Some(PathBuf::from(r"C:\LO\program\soffice.exe")),
            ..OfficeEnv::none()
        };
        let fx = Fixture::new("lo", FakeRunner::new(healthy(2)), lo);
        assert_eq!(
            fx.svc.preview_kinds().office,
            gander_core::protocol::host::OfficePreview::Render
        );
        let doc = fx.file("slides.odp", b"zip");
        let info = fx.info(&doc).unwrap();
        assert_eq!(
            (info.kind, info.renderer.as_str()),
            (PreviewKind::Office, "libreoffice")
        );
        let p = fx.page(&doc, Some(2), Some(640)).unwrap();
        assert_eq!(p.bytes, b"page:2:640:85");
        fx.page(&doc, Some(1), Some(640)).unwrap();
        assert_eq!(
            fx.runner.0.libreoffice.load(Ordering::SeqCst),
            1,
            "converted once"
        );
        assert_eq!(
            fx.runner.count(|j| j.op == RenderOp::ExportPdf),
            0,
            "COM was not involved"
        );
    }

    #[test]
    fn capabilities_and_listing_predictions_follow_the_office_setup() {
        let none = Fixture::new("cap-none", FakeRunner::new(healthy(1)), OfficeEnv::none());
        use gander_core::protocol::host::OfficePreview;
        assert_eq!(none.svc.preview_kinds().office, OfficePreview::Thumbnail);
        assert_eq!(none.svc.predict("pptx"), PreviewKind::None);
        assert_eq!(none.svc.predict("PDF"), PreviewKind::Pdf);
        assert_eq!(none.svc.predict(".png"), PreviewKind::Image);
        assert_eq!(none.svc.predict("md"), PreviewKind::Text);
        let ppt = Fixture::new("cap-ppt", FakeRunner::new(healthy(1)), powerpoint_env());
        assert_eq!(ppt.svc.preview_kinds().office, OfficePreview::Render);
        assert_eq!(ppt.svc.predict("pptx"), PreviewKind::Office);
        assert_eq!(ppt.svc.predict("docx"), PreviewKind::None);
        let kinds = ppt.svc.preview_kinds();
        assert!(kinds.image && kinds.pdf && kinds.text && kinds.video_poster && kinds.raw);
    }

    #[test]
    fn text_endpoint_decodes_pages_and_refuses_binary() {
        let fx = Fixture::new("text", FakeRunner::new(healthy(1)), OfficeEnv::none());
        let body = "Grüße 世界\r\nzweite Zeile\r\n";
        let mut utf16 = vec![0xFF, 0xFE];
        for u in body.encode_utf16() {
            utf16.extend(u.to_le_bytes());
        }
        let p = fx.file("note.txt", &utf16);
        let r = fx.text(&p, 0, None).unwrap();
        assert_eq!(r.content_type, "text/plain; charset=utf-8");
        assert_eq!(r.encoding, "utf-16le");
        assert_eq!(r.bytes, body.as_bytes());
        assert_eq!(r.total, body.len() as u64);
        assert!(!r.has_more);

        // Paging by next_offset reassembles the text.
        let mut got: Vec<u8> = Vec::new();
        let mut offset = 0;
        loop {
            let r = fx.text(&p, offset, Some(9)).unwrap();
            got.extend(&r.bytes);
            offset = r.next_offset;
            if !r.has_more {
                break;
            }
        }
        assert_eq!(got, body.as_bytes());

        assert_eq!(fx.text(&p, 0, Some(0)).unwrap_err().http_status(), 400);
        assert_eq!(
            fx.text(&p, 0, Some(1 << 20)).unwrap().bytes.len(),
            body.len()
        );
        assert_eq!(
            fx.text(&p, 0, Some((1 << 20) + 1))
                .unwrap_err()
                .http_status(),
            400
        );

        let bin = fx.file("blob.dat", &[0x50, 0x4B, 3, 4, 0, 0, 0, 1]);
        let e = fx.text(&bin, 0, None).unwrap_err();
        assert_eq!((e, e.http_status()), (PreviewError::Binary, 415));
        assert_eq!(e.to_body().error.code, ErrorCode::UnsupportedType);
    }

    #[test]
    fn error_bodies_carry_codes_and_retry_hints() {
        let g = PreviewError::Generating {
            retry_after_ms: 1500,
        };
        assert_eq!(g.http_status(), 503);
        assert_eq!(g.to_body().error.retry_after_ms, 1500);
        assert_eq!(g.to_body().error.code, ErrorCode::Generating);
        assert_eq!(PreviewError::Internal.http_status(), 500);
        assert_eq!(
            PreviewError::Internal.to_body().error.message,
            "internal error"
        );
        assert_eq!(PreviewError::Unsupported.code(), ErrorCode::UnsupportedType);
    }

    fn tile_req(
        _fx: &Fixture,
        path: &Path,
        page: Option<u32>,
        level: u32,
        x: u32,
        y: u32,
    ) -> TileRequest {
        TileRequest {
            file: File::open(path).unwrap(),
            ext: path.extension().unwrap().to_string_lossy().into_owned(),
            device: "dev-a".into(),
            page,
            level,
            x,
            y,
        }
    }

    fn tile_ok(
        fx: &Fixture,
        path: &Path,
        page: Option<u32>,
        level: u32,
        x: u32,
        y: u32,
    ) -> Result<TileResponse, PreviewError> {
        poll(|| fx.svc.tile(tile_req(fx, path, page, level, x, y)))
    }

    fn tile_jobs(fx: &Fixture) -> Vec<RenderJob> {
        fx.runner
            .calls()
            .into_iter()
            .filter(|j| matches!(j.op, RenderOp::Tile { .. }))
            .collect()
    }

    #[test]
    fn a_tile_is_503_then_200_then_served_from_the_cache() {
        let fx = Fixture::with_config(
            "tile-flow",
            FakeRunner::new(healthy(1)),
            OfficeEnv::none(),
            async_config(),
        );
        let big = fx.file("big.png", b"png bytes");
        let first = fx.svc.tile(tile_req(&fx, &big, None, 3, 2, 1));
        assert!(matches!(first, Err(PreviewError::Generating { .. })));
        assert_eq!(first.unwrap_err().http_status(), 503);
        let ok = tile_ok(&fx, &big, None, 3, 2, 1).unwrap();
        assert_eq!(ok.bytes, b"tile:1:3:2:1:80");
        assert_eq!(ok.content_type, "image/webp");
        assert_eq!(ok.cache_control, PREVIEW_CACHE_CONTROL);
        assert!(ok.etag.starts_with("\"t-"));
        // Cached: the same answer without another job, whatever the explicit page spelling.
        let again = fx.svc.tile(tile_req(&fx, &big, Some(1), 3, 2, 1)).unwrap();
        assert_eq!(again, ok);
        assert_eq!(tile_jobs(&fx).len(), 1);
        let job = &tile_jobs(&fx)[0];
        assert_eq!(
            (job.kind, job.format, job.quality),
            (RenderKind::Image, OutputFormat::Webp, 80)
        );
        // Another level, column or row is another tile with its own ETag.
        let other = tile_ok(&fx, &big, None, 3, 2, 2).unwrap();
        let other_level = tile_ok(&fx, &big, None, 4, 2, 1).unwrap();
        assert_ne!(other.etag, ok.etag);
        assert_ne!(other_level.etag, ok.etag);
        assert_eq!(tile_jobs(&fx).len(), 3);
    }

    #[test]
    fn tiles_are_keyed_by_file_identity_so_rename_keeps_and_touch_invalidates() {
        let fx = Fixture::new("tile-id", FakeRunner::new(healthy(1)), OfficeEnv::none());
        let a = fx.file("photo.jpg", b"abc");
        let first = fx.svc.tile(tile_req(&fx, &a, None, 1, 0, 0)).unwrap();
        let b = fx.dir.join("renamed.jpg");
        std::fs::rename(&a, &b).unwrap();
        assert_eq!(
            fx.svc.tile(tile_req(&fx, &b, None, 1, 0, 0)).unwrap(),
            first,
            "cache hit after rename"
        );
        assert_eq!(tile_jobs(&fx).len(), 1);
        let f = std::fs::OpenOptions::new().write(true).open(&b).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
            .unwrap();
        drop(f);
        let touched = fx.svc.tile(tile_req(&fx, &b, None, 1, 0, 0)).unwrap();
        assert_ne!(touched.etag, first.etag, "an edited file has new tiles");
        assert_eq!(tile_jobs(&fx).len(), 2);
    }

    #[test]
    fn only_images_pdf_and_office_through_pdf_have_tiles() {
        let fx = Fixture::new("tile-kinds", FakeRunner::new(healthy(1)), powerpoint_env());
        for name in [
            "notes.txt",
            "clip.mp4",
            "song.flac",
            "blob.bin",
            "deck.pptx",
        ] {
            let p = fx.file(name, b"x");
            let e = fx.svc.tile(tile_req(&fx, &p, None, 0, 0, 0)).unwrap_err();
            assert_eq!(e, PreviewError::Unsupported, "{name}");
            assert_eq!(e.http_status(), 415);
        }
        assert_eq!(fx.runner.calls().len(), 0, "no worker for them");
        for name in ["a.png", "b.jpg", "c.pdf", "d.dng"] {
            let p = fx.file(name, b"x");
            assert!(
                fx.svc.tile(tile_req(&fx, &p, None, 0, 0, 0)).is_ok(),
                "{name}"
            );
        }
        let kinds: Vec<RenderKind> = tile_jobs(&fx).iter().map(|j| j.kind).collect();
        assert_eq!(
            kinds,
            [
                RenderKind::Image,
                RenderKind::Image,
                RenderKind::Pdf,
                RenderKind::Raw
            ]
        );
    }

    #[test]
    fn bad_tile_requests_are_400_without_a_worker() {
        let fx = Fixture::new("tile-bad", FakeRunner::new(healthy(1)), OfficeEnv::none());
        let png = fx.file("a.png", b"x");
        let range = PreviewError::BadRequest("tile out of range");
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &png, Some(0), 0, 0, 0))
                .unwrap_err(),
            PreviewError::BadRequest("page must be 1 or more")
        );
        // An image has one page; level 7 and beyond cannot exist; absurd columns are refused.
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &png, Some(2), 0, 0, 0))
                .unwrap_err(),
            range
        );
        assert_eq!(
            fx.svc.tile(tile_req(&fx, &png, None, 7, 0, 0)).unwrap_err(),
            range
        );
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &png, None, 0, 5000, 0))
                .unwrap_err(),
            range
        );
        assert_eq!(range.http_status(), 400);
        assert_eq!(fx.runner.calls().len(), 0);
    }

    #[test]
    fn tiles_the_known_pyramid_lacks_are_400_without_a_worker() {
        // 3 pages: two A1 sheets (6 levels, 14 x 20 tiles at the top) and a small page.
        let sizes = [(7016, 9933), (7016, 9933), (600, 400)];
        let runner = FakeRunner::new(move |job, f| match job.op {
            RenderOp::Info => JobResult::Output(meta_json_tiles(&sizes)),
            _ => healthy(3)(job, f),
        });
        let fx = Fixture::new("tile-known", runner, OfficeEnv::none());
        let pdf = fx.file("sheets.pdf", b"%PDF-1.4");
        let info = fx.info(&pdf).unwrap();
        assert_eq!(info.tile_size, Some(512));
        assert_eq!(info.tile_levels, Some(6));
        let pages = info.tile_pages.unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(
            (pages[0].width, pages[0].height, pages[0].levels),
            (7016, 9933, 6)
        );
        assert_eq!(
            (pages[2].width, pages[2].height, pages[2].levels),
            (600, 400, 2)
        );
        let before = fx.runner.calls().len();
        let range = PreviewError::BadRequest("tile out of range");
        // Top level is 14 x 20; (14, 0) and (0, 20) are outside, so is level 6 and page 4.
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &pdf, Some(1), 5, 14, 0))
                .unwrap_err(),
            range
        );
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &pdf, Some(1), 5, 0, 20))
                .unwrap_err(),
            range
        );
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &pdf, Some(1), 6, 0, 0))
                .unwrap_err(),
            range
        );
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &pdf, Some(4), 0, 0, 0))
                .unwrap_err(),
            range
        );
        // The small page has two levels only.
        assert_eq!(
            fx.svc
                .tile(tile_req(&fx, &pdf, Some(3), 2, 0, 0))
                .unwrap_err(),
            range
        );
        assert_eq!(fx.runner.calls().len(), before, "no tile job was started");
        // Real tiles still render, including the last of the top level.
        let last = fx
            .svc
            .tile(tile_req(&fx, &pdf, Some(2), 5, 13, 19))
            .unwrap();
        assert_eq!(last.bytes, b"tile:2:5:13:19:80");
        assert_eq!(tile_jobs(&fx)[0].kind, RenderKind::Pdf);
    }

    #[test]
    fn a_tile_the_worker_says_does_not_exist_is_a_remembered_400() {
        let runner = FakeRunner::new(|_, _| JobResult::TileOutOfRange);
        let fx = Fixture::with_config("tile-oor", runner, OfficeEnv::none(), async_config());
        let png = fx.file("a.png", b"x");
        let end = tile_ok(&fx, &png, None, 2, 9, 9).unwrap_err();
        assert_eq!(end, PreviewError::BadRequest("tile out of range"));
        assert_eq!(end.http_status(), 400);
        assert_eq!(
            fx.svc.tile(tile_req(&fx, &png, None, 2, 9, 9)).unwrap_err(),
            end
        );
        assert_eq!(tile_jobs(&fx).len(), 1, "remembered");
    }

    #[test]
    fn a_failed_tile_render_is_415_and_a_host_failure_is_500() {
        let runner = FakeRunner::new(|_, _| JobResult::DecoderError);
        let fx = Fixture::with_config("tile-fail", runner, OfficeEnv::none(), async_config());
        let bad = fx.file("broken.pdf", b"%PDF-");
        assert_eq!(
            tile_ok(&fx, &bad, None, 0, 0, 0).unwrap_err(),
            PreviewError::Unsupported
        );
        let runner = FakeRunner::new(|_, _| JobResult::HostFailure);
        let fx = Fixture::with_config("tile-host", runner, OfficeEnv::none(), async_config());
        let png = fx.file("a.png", b"x");
        assert_eq!(
            tile_ok(&fx, &png, None, 0, 0, 0).unwrap_err(),
            PreviewError::Internal
        );
    }

    #[test]
    fn word_tiles_come_from_the_pdf_export_and_the_info_then_lists_them() {
        let word = OfficeEnv {
            word: true,
            ..OfficeEnv::none()
        };
        let runner = FakeRunner::new(|job, f| match job.op {
            RenderOp::Info => JobResult::Output(meta_json_tiles(&[(2480, 3508), (2480, 3508)])),
            _ => healthy(2)(job, f),
        });
        let fx = Fixture::new("tile-word", runner, word);
        let doc = fx.file("report.docx", b"zip");
        // Before the PDF exists the info has no tile sizes.
        assert_eq!(fx.info(&doc).unwrap().tile_pages, None);
        let t = fx.svc.tile(tile_req(&fx, &doc, Some(2), 3, 1, 1)).unwrap();
        assert_eq!(t.bytes, b"tile:2:3:1:1:80");
        assert_eq!(fx.runner.count(|j| j.op == RenderOp::ExportPdf), 1);
        let jobs = tile_jobs(&fx);
        assert_eq!(
            (jobs.len(), jobs[0].kind, jobs[0].ext.as_str()),
            (1, RenderKind::Pdf, "pdf")
        );
        // The page route reuses the export; the info now carries the tile pages.
        fx.page(&doc, Some(1), Some(700)).unwrap();
        assert_eq!(fx.runner.count(|j| j.op == RenderOp::ExportPdf), 1);
        let info = fx.info(&doc).unwrap();
        assert_eq!(info.tile_levels, Some(4));
        assert_eq!(info.tile_pages.unwrap().len(), 2);
    }

    #[test]
    fn info_lists_tiles_only_for_pages_that_need_a_second_level() {
        let sizes: Vec<(u32, u32)> = vec![(6000, 4500)];
        let runner = FakeRunner::new(move |job, f| match job.op {
            RenderOp::Info => JobResult::Output(meta_json_tiles(&sizes)),
            _ => healthy(1)(job, f),
        });
        let fx = Fixture::new("tile-info-img", runner, OfficeEnv::none());
        let big = fx.file("photo.jpg", b"x");
        let info = fx.info(&big).unwrap();
        assert_eq!(
            (info.tile_size, info.tile_levels),
            (Some(512), Some(5)),
            "{info:?}"
        );
        assert_eq!(info.tile_pages.unwrap()[0].width, 6000);

        // A page that fits in one tile has none, and neither has text, video or a slide deck.
        let runner = FakeRunner::new(|job, f| match job.op {
            RenderOp::Info => JobResult::Output(meta_json_tiles(&[(400, 300)])),
            _ => healthy(1)(job, f),
        });
        let fx = Fixture::new("tile-info-small", runner, powerpoint_env());
        let small = fx.file("icon.png", b"x");
        let info = fx.info(&small).unwrap();
        assert_eq!(
            (info.tile_size, info.tile_levels, info.tile_pages),
            (None, None, None)
        );
        let deck = fx.file("deck.pptx", b"zip");
        let info = fx.info(&deck).unwrap();
        assert_eq!(info.kind, PreviewKind::Office);
        assert_eq!(info.tile_pages, None, "a PowerPoint slide is a picture");
        let txt = fx.file("notes.txt", b"hello");
        assert_eq!(fx.info(&txt).unwrap().tile_pages, None);
    }

    #[test]
    fn a_page_goes_before_the_tiles_queued_ahead_of_it() {
        // One slot: hold it with a first tile, queue tiles, then ask for a page.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (o, g) = (Arc::clone(&order), Arc::clone(&gate));
        let runner = FakeRunner::new(move |job, _| {
            let label = match job.op {
                RenderOp::Tile { x, .. } => format!("tile{x}"),
                RenderOp::Page { page, .. } => format!("page{page}"),
                _ => return JobResult::Output(meta_json(1)),
            };
            if label == "tile0" {
                let (m, cv) = &*g;
                let mut open = m.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
            }
            o.lock().unwrap().push(label);
            JobResult::Output(b"x".to_vec())
        });
        let config = PreviewServiceConfig {
            queue: PreviewQueueConfig {
                total: 1,
                per_device: 1,
                ..PreviewQueueConfig::default()
            },
            ..async_config()
        };
        let fx = Fixture::with_config("tile-prio", runner, OfficeEnv::none(), config);
        let pdf = fx.file("doc.pdf", b"%PDF-1.4");
        let _ = fx.svc.tile(tile_req(&fx, &pdf, Some(1), 3, 0, 0)); // takes the slot and blocks
        std::thread::sleep(Duration::from_millis(100));
        let _ = fx.svc.tile(tile_req(&fx, &pdf, Some(1), 3, 1, 0));
        let _ = fx.svc.tile(tile_req(&fx, &pdf, Some(1), 3, 2, 0));
        let _ = fx.page(&pdf, Some(1), Some(500));
        {
            let (m, cv) = &*gate;
            *m.lock().unwrap() = true;
            cv.notify_all();
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while order.lock().unwrap().len() < 4 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            *order.lock().unwrap(),
            ["tile0", "page1", "tile1", "tile2"],
            "the page was asked for last and ran first after the running tile"
        );
    }

    #[test]
    fn at_most_one_render_per_device_and_two_overall() {
        let active = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let peak_total = Arc::new(AtomicUsize::new(0));
        let peak_device = Arc::new(AtomicUsize::new(0));
        let (a, pt, pd) = (
            Arc::clone(&active),
            Arc::clone(&peak_total),
            Arc::clone(&peak_device),
        );
        let runner = FakeRunner::new(move |job, _| {
            if !matches!(job.op, RenderOp::Page { .. }) {
                return JobResult::Output(meta_json(1));
            }
            let device = job.ext.clone(); // the test encodes the device in the extension
            {
                let mut m = a.lock().unwrap();
                *m.entry(device.clone()).or_default() += 1;
                pt.fetch_max(m.values().sum(), Ordering::SeqCst);
                pd.fetch_max(m[&device], Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(60));
            *a.lock().unwrap().get_mut(&device).unwrap() -= 1;
            JobResult::Output(b"x".to_vec())
        });
        let fx = Fixture::with_config("limits", runner, OfficeEnv::none(), async_config());
        // Distinct files so nothing is de-duplicated; the extension doubles as the device tag.
        let mut files = Vec::new();
        for (device, ext) in [("dev-a", "png"), ("dev-b", "jpg"), ("dev-c", "gif")] {
            for i in 0..3 {
                files.push((
                    device,
                    fx.file(&format!("{device}-{i}.{ext}"), format!("{i}").as_bytes()),
                ));
            }
        }
        for (device, path) in &files {
            let mut req = fx.page_req(path, device);
            req.width = Some(300);
            let _ = fx.svc.page(req);
        }
        for (device, path) in &files {
            poll(|| {
                let mut req = fx.page_req(path, device);
                req.width = Some(300);
                fx.svc.page(req)
            })
            .unwrap();
        }
        assert_eq!(
            peak_device.load(Ordering::SeqCst),
            1,
            "one render per device"
        );
        assert!(
            peak_total.load(Ordering::SeqCst) <= 2,
            "two renders overall"
        );
        assert_eq!(
            peak_total.load(Ordering::SeqCst),
            2,
            "and the second slot is used"
        );
    }
}
