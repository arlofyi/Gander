// SPDX-License-Identifier: Apache-2.0
//! Previews: `/preview/info`, `/preview` (one page as an image) and `/preview/text`.
//!
//! - [`plan`] decides, from the extension and what is installed, how a file is previewed
//!   ([`Plan`]) and so what `preview` kind a listing advertises.
//! - [`text`]: encoding detection, binary sniff and offset-addressed reads (in the host, safe
//!   Rust, bounded).
//! - [`office`]: which Office applications and LibreOffice exist (registry and known folders).
//! - [`queue::PreviewQueue`]: 1 render per device, 2 overall.
//! - [`service::PreviewService`]: what the three handlers call (WP-14 wires it into the router).
//! - [`runner`]: runs one worker job and cleans up after Office; [`procs`]: the process helpers
//!   that keep Office instances from outliving their job.

use gander_core::protocol::files::PreviewKind;

use crate::render::job::RenderKind;
use crate::thumbs::{AUDIO_EXTS, IMAGE_EXTS, OFFICE_EXTS, RAW_EXTS, VIDEO_EXTS};

pub mod office;
pub mod queue;
pub mod text;

#[cfg(windows)]
mod libreoffice;
#[cfg(windows)]
pub mod procs;
#[cfg(windows)]
pub mod runner;
#[cfg(windows)]
pub mod service;

use office::OfficeBackend;

/// Version of the preview renderers' output. Part of every preview cache key and ETag: bump it
/// when a change makes cached pages wrong.
pub const PREVIEW_RENDERER_VERSION: u32 = 1;

/// Widest preview the host renders (`capabilities.preview_max_width`). Larger requests are
/// clamped.
pub const PREVIEW_MAX_WIDTH: u32 = 2048;

/// Narrowest preview width; smaller requests are raised to it.
pub const PREVIEW_MIN_WIDTH: u32 = 16;

/// Width used when the request names none (the protocol spec's example).
pub const DEFAULT_PREVIEW_WIDTH: u32 = 1080;

/// `Cache-Control` for preview pages (immutable by ETag + page + width).
pub const PREVIEW_CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// Extensions previewed as text without sniffing first (a file with any other extension is
/// still previewed as text when it sniffs as text).
const TEXT_EXTS: &[&str] = &[
    "txt",
    "md",
    "markdown",
    "log",
    "json",
    "jsonl",
    "yaml",
    "yml",
    "toml",
    "csv",
    "tsv",
    "ini",
    "cfg",
    "conf",
    "xml",
    "html",
    "htm",
    "css",
    "js",
    "mjs",
    "ts",
    "tsx",
    "jsx",
    "py",
    "rs",
    "go",
    "java",
    "kt",
    "c",
    "cc",
    "cpp",
    "h",
    "hpp",
    "cs",
    "sql",
    "sh",
    "ps1",
    "bat",
    "cmd",
    "rb",
    "php",
    "lua",
    "swift",
    "gradle",
    "properties",
    "tex",
    "rst",
    "srt",
    "vtt",
];

/// How a file is previewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// A raster image, or a camera RAW's embedded JPEG: resized to the requested width.
    Image(RenderKind),
    /// A PDF, rendered page by page by `Windows.Data.Pdf`.
    Pdf,
    /// The poster frame of a video (shell thumbnail handler).
    Video,
    /// Decoded text.
    Text,
    /// An Office or ODF document rendered by this backend.
    Office(OfficeBackend),
    /// No preview.
    None,
}

impl Plan {
    /// The `preview` kind this plan advertises.
    pub fn kind(&self) -> PreviewKind {
        match self {
            Plan::Image(_) => PreviewKind::Image,
            Plan::Pdf => PreviewKind::Pdf,
            Plan::Video => PreviewKind::VideoPoster,
            Plan::Text => PreviewKind::Text,
            Plan::Office(_) => PreviewKind::Office,
            Plan::None => PreviewKind::None,
        }
    }

    /// The renderer name reported in `preview/info`.
    pub fn renderer(&self) -> &'static str {
        match self {
            Plan::Image(RenderKind::Raw) => "raw_embedded",
            Plan::Image(_) => "wic",
            Plan::Pdf => "windows_pdf",
            Plan::Video => "shell_thumbnail",
            Plan::Text => "text",
            Plan::Office(backend) => backend.renderer_name(),
            Plan::None => "none",
        }
    }

    /// Whether the document has pages (PDF and Office); images and posters are page 1 only.
    pub fn is_paged(&self) -> bool {
        matches!(self, Plan::Pdf | Plan::Office(_))
    }
}

/// How a file with this extension (any case, optional dot) is previewed on a machine with this
/// Office setup. Preview kinds follow the spec's order: `pdf` for PDFs, `image` for images and
/// RAW, `office` when a render path exists (else `none`), `text`, `video_poster`, else `none`.
pub fn plan(ext: &str, office: &office::OfficeEnv) -> Plan {
    let ext = crate::thumbs::normalise_ext(ext);
    let e = ext.as_str();
    if IMAGE_EXTS.contains(&e) {
        Plan::Image(RenderKind::Image)
    } else if RAW_EXTS.contains(&e) {
        Plan::Image(RenderKind::Raw)
    } else if e == "pdf" {
        Plan::Pdf
    } else if OFFICE_EXTS.contains(&e) {
        // Documents with an Office renderer get page previews; the rest keep their embedded
        // thumbnail for lists only.
        office.backend_for(e).map_or(Plan::None, Plan::Office)
    } else if VIDEO_EXTS.contains(&e) {
        Plan::Video
    } else if TEXT_EXTS.contains(&e) {
        Plan::Text
    } else {
        // Audio has cover art for thumbnails only; everything else is unknown until sniffed.
        Plan::None
    }
}

/// Whether a file whose extension has no [`Plan`] may still turn out to be text. Documents and
/// audio are known binary formats: without a renderer they have no preview, whatever their first
/// bytes look like.
pub fn may_be_text(ext: &str) -> bool {
    let ext = crate::thumbs::normalise_ext(ext);
    let e = ext.as_str();
    !(OFFICE_EXTS.contains(&e) || AUDIO_EXTS.contains(&e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use office::OfficeApp;

    #[test]
    fn kinds_follow_the_spec_order() {
        let none = office::OfficeEnv::none();
        assert_eq!(plan("PDF", &none).kind(), PreviewKind::Pdf);
        assert_eq!(plan(".jpg", &none).kind(), PreviewKind::Image);
        assert_eq!(plan("dng", &none), Plan::Image(RenderKind::Raw));
        assert_eq!(plan("mp4", &none).kind(), PreviewKind::VideoPoster);
        assert_eq!(plan("rs", &none).kind(), PreviewKind::Text);
        assert_eq!(plan("flac", &none).kind(), PreviewKind::None);
        assert_eq!(plan("", &none).kind(), PreviewKind::None);
        assert_eq!(plan("exe", &none).kind(), PreviewKind::None);
    }

    #[test]
    fn office_documents_are_previewed_only_with_a_renderer() {
        let mut env = office::OfficeEnv::none();
        // No renderer: `none` (the list thumbnail still comes from the embedded thumbnail).
        assert_eq!(plan("pptx", &env), Plan::None);
        env.powerpoint = true;
        assert_eq!(
            plan("pptx", &env),
            Plan::Office(OfficeBackend::Com(OfficeApp::PowerPoint))
        );
        assert_eq!(plan("pptx", &env).kind(), PreviewKind::Office);
        assert_eq!(plan("pptx", &env).renderer(), "office_com_powerpoint");
        // Word is not installed: docx has no renderer, and macro-enabled files never do.
        assert_eq!(plan("docx", &env), Plan::None);
        assert_eq!(plan("pptm", &env), Plan::None);
        assert!(plan("pptx", &env).is_paged());
        assert!(!plan("png", &env).is_paged());
    }

    #[test]
    fn only_unknown_types_are_sniffed_for_text() {
        assert!(may_be_text("weird"));
        assert!(may_be_text(""));
        assert!(!may_be_text("PPTX"));
        assert!(!may_be_text(".flac"));
    }

    #[test]
    fn renderer_names_match_the_protocol_examples() {
        let none = office::OfficeEnv::none();
        assert_eq!(plan("pdf", &none).renderer(), "windows_pdf");
        assert_eq!(plan("png", &none).renderer(), "wic");
        assert_eq!(plan("txt", &none).renderer(), "text");
    }
}
