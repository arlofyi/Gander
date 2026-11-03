// SPDX-License-Identifier: Apache-2.0
//! Thumbnails: type classification, content-identity cache, render queue and the
//! transport-independent `/thumb` service.
//!
//! - [`classify`] maps an extension to a [`RenderKind`] (or refuses it).
//! - [`cache::ThumbCache`]: files under `<root>/thumbs/<2 hex>/<key>.webp`, index and LRU in redb,
//!   2 GiB cap, evict at 95 % down to 80 %.
//! - [`queue::RenderQueue`]: concurrency 2, visible-first priority, de-duplication, position-based
//!   `retry_after_ms`.
//! - [`service::ThumbService`]: what the `/thumb` handler calls (WP-14 wires it into the router).

use crate::render::job::RenderKind;

pub mod cache;
#[cfg(windows)]
pub mod identity;
pub mod queue;
#[cfg(windows)]
pub mod service;

/// Thumbnail sizes the host renders (`capabilities.thumb_sizes`).
pub const THUMB_SIZES: [u32; 3] = [128, 256, 512];

/// Default size when the request names none.
pub const DEFAULT_THUMB_SIZE: u32 = 256;

pub(crate) const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "webp", "tif", "tiff", "heic",
    "heif", "avif", "jxl", "ico", "jxr", "wdp",
];
pub(crate) const RAW_EXTS: &[&str] = &["cr2", "cr3", "nef", "arw", "dng", "raf", "orf", "rw2"];
pub(crate) const OFFICE_EXTS: &[&str] = &[
    "docx", "xlsx", "pptx", "docm", "xlsm", "pptm", "odt", "ods", "odp", "vsdx",
];
pub(crate) const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "mov", "avi", "webm", "m4v", "wmv"];
pub(crate) const AUDIO_EXTS: &[&str] = &["mp3", "flac", "m4a", "ogg", "wav", "wma"];

/// Types the shell fallback must never be asked about: a shortcut's thumbnail is its *target's*
/// thumbnail (which may live outside the share), and executables only ever yield icons.
const SHELL_DENY_EXTS: &[&str] = &[
    "lnk",
    "url",
    "website",
    "pif",
    "scf",
    "appref-ms",
    "library-ms",
    "search-ms",
    "searchconnector-ms",
    "exe",
    "dll",
    "com",
    "scr",
    "cpl",
    "msi",
    "msp",
    "bat",
    "cmd",
    "ps1",
    "vbs",
    "js",
    "jse",
    "wsf",
    "hta",
    "sys",
    "drv",
    "ocx",
];

/// Normalises an extension: strips one leading dot, lower-cases ASCII.
pub fn normalise_ext(ext: &str) -> String {
    ext.strip_prefix('.').unwrap_or(ext).to_ascii_lowercase()
}

/// The renderer for a file with this extension, or `None` when no thumbnail can be produced.
///
/// `has_shell_provider` answers whether the extension has a registered shell thumbnail provider
/// (see [`identity::has_shell_thumbnail_provider`] on Windows); it is consulted only for
/// extensions that no built-in renderer handles.
pub fn classify(ext: &str, has_shell_provider: impl FnOnce(&str) -> bool) -> Option<RenderKind> {
    let ext = normalise_ext(ext);
    let e = ext.as_str();
    if e.is_empty() || SHELL_DENY_EXTS.contains(&e) {
        return None;
    }
    if IMAGE_EXTS.contains(&e) {
        Some(RenderKind::Image)
    } else if RAW_EXTS.contains(&e) {
        Some(RenderKind::Raw)
    } else if e == "pdf" {
        Some(RenderKind::Pdf)
    } else if OFFICE_EXTS.contains(&e) {
        Some(RenderKind::Office)
    } else if VIDEO_EXTS.contains(&e) {
        Some(RenderKind::Video)
    } else if AUDIO_EXTS.contains(&e) {
        Some(RenderKind::Audio)
    } else if has_shell_provider(e) {
        Some(RenderKind::Shell)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_kinds() {
        let no = |_: &str| false;
        assert_eq!(classify("JPG", no), Some(RenderKind::Image));
        assert_eq!(classify(".dng", no), Some(RenderKind::Raw));
        assert_eq!(classify("pdf", no), Some(RenderKind::Pdf));
        assert_eq!(classify("pptx", no), Some(RenderKind::Office));
        assert_eq!(classify("mkv", no), Some(RenderKind::Video));
        assert_eq!(classify("flac", no), Some(RenderKind::Audio));
        assert_eq!(classify("txt", no), None);
        assert_eq!(classify("", no), None);
    }

    #[test]
    fn shell_fallback_only_for_unknown_and_never_for_shortcuts() {
        let yes = |_: &str| true;
        assert_eq!(classify("psd", yes), Some(RenderKind::Shell));
        assert_eq!(classify("lnk", yes), None);
        assert_eq!(classify("URL", yes), None);
        assert_eq!(classify("exe", yes), None);
        assert_eq!(classify("png", yes), Some(RenderKind::Image));
    }
}
