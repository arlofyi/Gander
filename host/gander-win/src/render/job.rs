// SPDX-License-Identifier: Apache-2.0
//! The job description the host writes to a render worker's stdin, and the worker's exit codes.
//!
//! Wire format: one JSON object (UTF-8, at most [`MAX_JOB_BYTES`]) on stdin, then EOF. The worker
//! never receives a path: `handle` is the value of a read-only file handle the host duplicated
//! into the worker process before resuming it. Output image bytes go to stdout, diagnostics to
//! stderr (never file contents, paths or secrets), and the outcome is the exit code.

use serde::{Deserialize, Serialize};

/// Protocol version of [`RenderJob`]; the worker refuses any other value with [`EXIT_BAD_JOB`].
pub const JOB_VERSION: u32 = 1;

/// Version of the renderers' output. Part of every cache key and thumbnail ETag: bump it whenever
/// a change makes previously cached output wrong (new orientation handling, new encoder settings).
pub const RENDERER_VERSION: u32 = 1;

/// Largest job description the worker reads from stdin.
pub const MAX_JOB_BYTES: usize = 64 * 1024;

/// Diagnostic the worker prints (stderr) when a `Page` job names a page past the end of the
/// document. The exit code is still [`EXIT_DECODER_ERROR`]; the host looks for this text to turn
/// it into a client error instead of "no preview".
pub const PAGE_OUT_OF_RANGE_MSG: &str = "page out of range";

/// Diagnostic the worker prints (stderr) when a `Tile` job names a level, column or row that the
/// page does not have (or a page past the end). The exit code is [`EXIT_DECODER_ERROR`]; the host
/// looks for this text to answer a client error instead of "no preview".
pub const TILE_OUT_OF_RANGE_MSG: &str = "tile out of range";

/// Prefix of the stderr line an Office automation worker prints once it owns an Office process:
/// `gander-render: office-pid=<pid>`. The host kills that process if the worker was killed before
/// it could clean up.
pub const OFFICE_PID_PREFIX: &str = "gander-render: office-pid=";

/// Exit code: success, the image is on stdout.
pub const EXIT_OK: u32 = 0;
/// Exit code: the job description was malformed or the handle unusable (a host bug, not a file
/// problem). Not part of the spec's table; the spec lists 0, 2, 3 and 4 only.
pub const EXIT_BAD_JOB: u32 = 1;
/// Exit code: no renderer for this file (no decoder, no embedded thumbnail, icon only, encrypted).
pub const EXIT_UNSUPPORTED: u32 = 2;
/// Exit code: the worker's own watchdog fired, or the host killed it on timeout.
pub const EXIT_TIMEOUT: u32 = 3;
/// Exit code: the decoder rejected the bytes (malformed, truncated, over the pixel limit).
pub const EXIT_DECODER_ERROR: u32 = 4;

/// What kind of renderer the worker should use. The host picks it from the extension
/// ([`crate::thumbs::classify`]); the worker still validates the bytes itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderKind {
    /// Raster image: WIC first, the `image` crate for formats WIC lacks.
    Image,
    /// Camera RAW: embedded JPEG preview, else WIC (Raw Image Extension).
    Raw,
    /// PDF: page 1 through `Windows.Data.Pdf`.
    Pdf,
    /// OOXML / ODF container: embedded thumbnail from the zip.
    Office,
    /// Video poster frame through the shell thumbnail handler.
    Video,
    /// Audio cover art through the shell thumbnail handler.
    Audio,
    /// Any other type with a registered shell thumbnail provider.
    Shell,
}

impl RenderKind {
    /// Whether the job goes through `IShellItemImageFactory` (needs an STA thread and a shell
    /// item, which the worker derives from the handle).
    pub const fn uses_shell(self) -> bool {
        matches!(
            self,
            RenderKind::Video | RenderKind::Audio | RenderKind::Shell
        )
    }
}

/// Output encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Lossy WebP; alpha preserved.
    #[default]
    Webp,
    /// Baseline JPEG; alpha composited over white.
    Jpeg,
}

impl OutputFormat {
    /// The HTTP `Content-Type` of this encoding.
    pub const fn content_type(self) -> &'static str {
        match self {
            OutputFormat::Webp => "image/webp",
            OutputFormat::Jpeg => "image/jpeg",
        }
    }

    /// File extension used by the cache.
    pub const fn extension(self) -> &'static str {
        match self {
            OutputFormat::Webp => "webp",
            OutputFormat::Jpeg => "jpg",
        }
    }
}

/// The operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RenderOp {
    /// A thumbnail whose longest side is at most `size` (never upscaled).
    Thumb {
        /// Longest side in pixels.
        size: u32,
    },
    /// One preview page, `width` pixels wide (images and posters are never upscaled; PDF and
    /// Office pages are rendered at `width`). Output: the encoded image.
    Page {
        /// 1-based page number (always 1 for images and posters).
        page: u32,
        /// Target width in pixels.
        width: u32,
    },
    /// One 512 px deep-zoom tile (WP-67), geometry per [`gander_core::tiles`]. Images and RAW
    /// previews are cut out of the decoded frame; PDF pages are rendered for the tile's area only.
    /// Output: the encoded tile image.
    Tile {
        /// 1-based page number (always 1 for images).
        page: u32,
        /// Pyramid level, 0 to levels - 1.
        level: u32,
        /// Tile column.
        x: u32,
        /// Tile row.
        y: u32,
    },
    /// Cheap metadata about the file. Output: a JSON [`PreviewMeta`] instead of an image.
    Info,
    /// Office automation only: export the whole document to PDF. Output: the PDF bytes.
    ExportPdf,
}

/// Metadata the worker reports for [`RenderOp::Info`] (JSON on stdout).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewMeta {
    /// Page or slide count, when the format knows it without rendering.
    pub pages: Option<u32>,
    /// Native width of the image or of page 1.
    pub width: Option<u32>,
    /// Native height of the image or of page 1.
    pub height: Option<u32>,
    /// Size in pixels of each page at the top tile level (an image's own pixels, a PDF page at
    /// 300 dpi; at most [`gander_core::tiles::MAX_TILE_PAGES`] pages), empty when the file has no
    /// deep zoom.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tile_pages: Vec<(u32, u32)>,
}

/// One render job, as written to the worker's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderJob {
    /// Always [`JOB_VERSION`].
    pub v: u32,
    /// Handle value valid inside the worker process (read-only access).
    pub handle: u64,
    /// Renderer selection.
    pub kind: RenderKind,
    /// Lower-case extension without the dot, a decoder hint only (never a path).
    pub ext: String,
    /// What to produce, e.g. `{"type":"thumb","size":256}`.
    pub op: RenderOp,
    /// Output encoding.
    pub format: OutputFormat,
    /// Encoder quality, 1..=100.
    pub quality: u8,
    /// Worker-side watchdog in milliseconds; the worker exits with [`EXIT_TIMEOUT`] when it fires.
    pub timeout_ms: u64,
}

impl RenderJob {
    /// A job for `op` with the spec's output defaults (WebP quality 85 for pages).
    pub fn new(kind: RenderKind, ext: &str, op: RenderOp) -> Self {
        let quality = if matches!(op, RenderOp::Page { .. }) {
            85
        } else {
            80
        };
        Self {
            v: JOB_VERSION,
            handle: 0,
            kind,
            ext: ext.to_ascii_lowercase(),
            op,
            format: OutputFormat::Webp,
            quality,
            timeout_ms: 15_000,
        }
    }

    /// A preview page job.
    pub fn page(kind: RenderKind, ext: &str, page: u32, width: u32) -> Self {
        Self::new(kind, ext, RenderOp::Page { page, width })
    }

    /// A deep-zoom tile job (WebP quality 80).
    pub fn tile(kind: RenderKind, ext: &str, page: u32, level: u32, x: u32, y: u32) -> Self {
        Self::new(kind, ext, RenderOp::Tile { page, level, x, y })
    }

    /// A metadata job.
    pub fn info(kind: RenderKind, ext: &str) -> Self {
        Self::new(kind, ext, RenderOp::Info)
    }

    /// A thumbnail job with the spec's defaults (WebP quality 80). `handle` is filled in by the
    /// spawner once the handle exists in the worker.
    pub fn thumb(kind: RenderKind, ext: &str, size: u32) -> Self {
        Self {
            v: JOB_VERSION,
            handle: 0,
            kind,
            ext: ext.to_ascii_lowercase(),
            op: RenderOp::Thumb { size },
            format: OutputFormat::Webp,
            quality: 80,
            timeout_ms: 15_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_round_trips() {
        let mut job = RenderJob::thumb(RenderKind::Office, "PPTX", 256);
        job.handle = 0x1a4;
        let json = serde_json::to_string(&job).unwrap();
        assert!(
            json.contains("\"op\":{\"type\":\"thumb\",\"size\":256}"),
            "{json}"
        );
        assert!(json.contains("\"ext\":\"pptx\""), "{json}");
        let back: RenderJob = serde_json::from_str(&json).unwrap();
        assert_eq!(back, job);
    }

    #[test]
    fn preview_ops_round_trip() {
        let page = RenderJob::page(RenderKind::Pdf, "PDF", 3, 1080);
        let json = serde_json::to_string(&page).unwrap();
        assert!(
            json.contains("\"op\":{\"type\":\"page\",\"page\":3,\"width\":1080}"),
            "{json}"
        );
        assert_eq!(page.quality, 85);
        assert_eq!(serde_json::from_str::<RenderJob>(&json).unwrap(), page);

        let tile = RenderJob::tile(RenderKind::Pdf, "PDF", 2, 3, 4, 5);
        let json = serde_json::to_string(&tile).unwrap();
        assert!(
            json.contains("\"op\":{\"type\":\"tile\",\"page\":2,\"level\":3,\"x\":4,\"y\":5}"),
            "{json}"
        );
        assert_eq!(tile.quality, 80);
        assert_eq!(serde_json::from_str::<RenderJob>(&json).unwrap(), tile);

        let info = RenderJob::info(RenderKind::Image, "jpg");
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"op\":{\"type\":\"info\"}"), "{json}");
        assert_eq!(serde_json::from_str::<RenderJob>(&json).unwrap(), info);

        let export = RenderJob::new(RenderKind::Office, "docx", RenderOp::ExportPdf);
        let json = serde_json::to_string(&export).unwrap();
        assert!(json.contains("\"type\":\"export_pdf\""), "{json}");
        assert_eq!(serde_json::from_str::<RenderJob>(&json).unwrap(), export);

        let meta = PreviewMeta {
            pages: Some(3),
            width: Some(794),
            height: None,
            tile_pages: vec![(2480, 3508), (7016, 9933)],
        };
        let back: PreviewMeta =
            serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn unknown_fields_are_refused() {
        let json = r#"{"v":1,"handle":1,"kind":"image","ext":"png","op":{"type":"thumb","size":128},
            "format":"webp","quality":80,"timeout_ms":1000,"path":"C:/x"}"#;
        assert!(serde_json::from_str::<RenderJob>(json).is_err());
    }
}
