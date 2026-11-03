// SPDX-License-Identifier: Apache-2.0
//! Shares, browsing, thumbnails, previews and fetch: `/v1/shares/...` and `/v1/recent`.
//!
//! Query-string types (`*Query`) describe the accepted parameters; hosts ignore unknown
//! parameters. Comma-separated list parameters (`kinds`, `ext`) are kept as strings with split
//! helpers, so any URL-encoded form parser can fill them.

use serde::{Deserialize, Serialize};

use super::host::SearchCapability;
use super::ids::{ShareId, Timestamp};
pub use crate::tiles::TilePage;

/// A per-share, per-device permission. v1 is read-only: there is no write permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Browse directory listings, stat and search.
    List,
    /// Thumbnails and host-rendered previews.
    Preview,
    /// Download file bytes and hashes.
    Fetch,
    /// Open the file in a desktop app and stream its window (trusted devices only).
    Stream,
}

/// Outcome of the stream launch policy for one device and file, and a share's default for
/// unknown handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamDecision {
    /// Streaming would start without a prompt.
    Allowed,
    /// Streaming is refused; `POST /stream` returns the reason.
    Refused,
    /// The host owner will be asked.
    Ask,
}

/// `GET /v1/shares` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharesResponse {
    /// Shares visible to the calling device.
    pub shares: Vec<ShareInfo>,
}

/// One share as a device sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareInfo {
    /// Share id.
    pub id: ShareId,
    /// Display name.
    pub name: String,
    /// Host path of the share root; present only if the owner enabled "show folder path to
    /// devices" for this share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_display: Option<String>,
    /// What this device may do in the share.
    pub permissions: Vec<Permission>,
    /// Whether hidden and system entries are listed.
    pub show_hidden: bool,
    /// Stream decision for handlers the catalogue does not know yet.
    pub stream_default: StreamDecision,
    /// How `/search` works in this share: `true` (the bounded walk over names) or `"indexed"`
    /// (the Windows Search index, which also finds text inside files; WP-66). Absent from a host
    /// that predates it, which means the same as `capabilities.search`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchCapability>,
}

/// Entry kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
}

/// Attribute flags of an entry (path-resolution spec, "Listing semantics").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryFlag {
    /// Hidden attribute (listed only with `show_hidden`).
    Hidden,
    /// Read-only attribute.
    Readonly,
    /// System attribute (listed only with `show_hidden`).
    System,
    /// Cloud placeholder; opening it downloads the content on the host.
    CloudOnly,
    /// Reparse point of a denied class; shown greyed and not openable.
    Link,
    /// EFS-encrypted.
    Encrypted,
}

/// The preview kind the host would produce for a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    /// Resized image.
    Image,
    /// Rendered PDF pages.
    Pdf,
    /// Decoded text.
    Text,
    /// Rendered Office pages.
    Office,
    /// Poster frame of a video.
    VideoPoster,
    /// No preview; the client suggests Stream or Fetch.
    None,
}

/// A directory entry, as returned by list, stat, search and recent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Name exactly as stored (no Unicode normalisation).
    pub name: String,
    /// File or directory.
    pub kind: EntryKind,
    /// Size in bytes (0 for directories).
    #[serde(default)]
    pub size: u64,
    /// Last modification time.
    #[serde(with = "time::serde::rfc3339")]
    pub mtime: Timestamp,
    /// Creation time (Windows) or status change time (POSIX).
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub ctime: Option<Timestamp>,
    /// Lower-case extension without the dot, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
    /// MIME type by extension, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Attribute flags.
    #[serde(default)]
    pub flags: Vec<EntryFlag>,
    /// Prediction that a thumbnail can be generated (not a promise).
    #[serde(default)]
    pub thumb: bool,
    /// Preview kind the host would produce.
    pub preview: PreviewKind,
    /// Stream policy outcome for this device and file (steps 1-6, no prompt).
    pub stream: StreamDecision,
    /// Share id; present in `GET /recent` results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareId>,
    /// Share-relative path; present in search and recent results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Type-specific metadata; present in `GET /stat?meta=1` when the host can get it cheaply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<EntryMeta>,
}

/// Type-specific metadata, keyed by type: `{"image": {...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryMeta {
    /// Image metadata.
    Image(ImageMeta),
    /// Video metadata.
    Video(VideoMeta),
    /// PDF metadata.
    Pdf(PdfMeta),
    /// Audio metadata.
    Audio(AudioMeta),
    /// Office document metadata.
    Office(OfficeMeta),
}

/// `meta.image`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMeta {
    /// Pixel width before orientation is applied.
    pub width: u32,
    /// Pixel height before orientation is applied.
    pub height: u32,
    /// EXIF orientation 1-8, if present.
    pub orientation: Option<u8>,
    /// EXIF capture time as stored (EXIF has no time zone, so this is not RFC 3339).
    pub taken_at: Option<String>,
    /// Camera make and model.
    pub camera: Option<String>,
}

/// `meta.video`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMeta {
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Codec name, for example `h264`.
    pub codec: Option<String>,
}

/// `meta.pdf`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfMeta {
    /// Page count.
    pub pages: u32,
}

/// `meta.audio`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioMeta {
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Track title.
    pub title: Option<String>,
    /// Artist.
    pub artist: Option<String>,
}

/// `meta.office`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficeMeta {
    /// Page or slide count, when cheaply known.
    pub pages: Option<u32>,
}

/// `GET /v1/shares/{share}/list` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListResponse {
    /// The listed share-relative path.
    pub path: String,
    /// Entries of this page.
    pub entries: Vec<Entry>,
    /// Opaque cursor for the next page; `null` on the last page.
    pub next_cursor: Option<String>,
    /// The directory had more than 50,000 entries and was cut.
    pub truncated: bool,
    /// Entries omitted because their names are not valid UTF-8.
    pub unlistable: u64,
    /// Weak ETag over the entry list (also sent as the `ETag` header).
    pub etag: String,
}

/// `GET /v1/shares/{share}/stat` response: one entry, with `meta` when `meta=1`.
pub type StatResponse = Entry;

/// `GET /v1/shares/{share}/search` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Matches, each with `path`.
    pub entries: Vec<Entry>,
    /// The walk stopped at the time or result bound, or the index was still crawling or the
    /// result list was capped.
    pub partial: bool,
    /// Cursor to continue; `null` when done.
    pub next_cursor: Option<String>,
    /// What produced the answer. Absent from a host that predates it (the walk).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<SearchVia>,
}

/// What answered a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchVia {
    /// The Windows Search index (names, and file text for `scope=content`).
    Index,
    /// The bounded breadth-first walk (names only).
    Walk,
}

/// `GET /v1/recent` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentResponse {
    /// Recently modified files, newest first, each with `share` and `path`.
    pub entries: Vec<Entry>,
    /// The bounded walk did not cover everything.
    pub partial: bool,
}

/// `GET /v1/shares/{share}/preview/info` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewInfo {
    /// Preview kind.
    pub kind: PreviewKind,
    /// Page count for paged kinds.
    pub pages: Option<u32>,
    /// Native width of page 1 or the image.
    pub width: Option<u32>,
    /// Native height of page 1 or the image.
    pub height: Option<u32>,
    /// Text size in bytes for `text`.
    pub text_bytes: Option<u64>,
    /// Renderer that will be used, for example `windows_pdf`.
    pub renderer: String,
    /// Host's estimate of the first render time.
    pub estimated_ms: u32,
    /// Edge length of a deep-zoom tile in pixels (WP-67); present only when `tile_pages` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_size: Option<u32>,
    /// Most levels any page has (`level` runs from 0 to `tile_levels - 1`); absent when the file
    /// has no deep zoom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_levels: Option<u32>,
    /// Top-level pixel size and level count of each page, index 0 being page 1 (at most
    /// [`crate::tiles::MAX_TILE_PAGES`] pages; later pages have no deep zoom).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tile_pages: Option<Vec<TilePage>>,
}

/// `GET /v1/shares/{share}/hash` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashResponse {
    /// Algorithm used.
    pub algo: super::host::HashAlgo,
    /// Lower-case hex digest.
    pub hex: String,
    /// File size the hash covers.
    pub size: u64,
    /// Strong ETag of the file version hashed.
    pub etag: String,
}

/// `GET /v1/zip?dry_run=1` response: what a folder download would contain (WP-68).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipPlan {
    /// Files and folders that would be in the zip (not counting the folder itself or the note
    /// listing skipped items).
    pub entries: u64,
    /// Total size of the files, in bytes. The zip is slightly larger (headers) and stores the
    /// files uncompressed.
    pub bytes: u64,
    /// Items left out: links and junctions the host does not follow, and names it cannot send.
    /// They are listed in `_gander-skipped.txt` inside the zip.
    pub skipped: u64,
    /// Files whose data is not on the computer (cloud placeholders): zipping them makes the
    /// computer download them first.
    pub cloud_only: u64,
}

/// Sort key for listings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// By name (default).
    #[default]
    Name,
    /// By modification time.
    Mtime,
    /// By size.
    Size,
    /// By kind, then extension.
    Kind,
}

/// Sort order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    /// Ascending (default).
    #[default]
    Asc,
    /// Descending.
    Desc,
}

/// Image encoding for previews.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    /// WebP (default).
    #[default]
    Webp,
    /// JPEG.
    Jpeg,
}

fn split_list(value: Option<&str>) -> Vec<&str> {
    value
        .map(|v| v.split(',').filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// Query of `GET /v1/shares/{share}/list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListQuery {
    /// Share-relative path; empty for the root.
    #[serde(default)]
    pub path: String,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size (default 200, capped at `limits.list_max`).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Sort key.
    #[serde(default)]
    pub sort: SortKey,
    /// Sort order.
    #[serde(default)]
    pub order: SortOrder,
    /// Comma-separated kinds filter (`file,dir`).
    #[serde(default)]
    pub kinds: Option<String>,
    /// Comma-separated extension filter (`pdf,pptx`).
    #[serde(default)]
    pub ext: Option<String>,
}

impl ListQuery {
    /// Default page size.
    pub const DEFAULT_LIMIT: u32 = 200;

    /// The kinds filter, split. Empty means "all kinds".
    pub fn kinds(&self) -> Vec<&str> {
        split_list(self.kinds.as_deref())
    }

    /// The extension filter, split. Empty means "all extensions".
    pub fn exts(&self) -> Vec<&str> {
        split_list(self.ext.as_deref())
    }
}

/// Query of `GET /v1/shares/{share}/stat`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatQuery {
    /// Share-relative path.
    #[serde(default)]
    pub path: String,
    /// `1` to include type-specific metadata.
    #[serde(default)]
    pub meta: u8,
}

/// Query of `GET /v1/shares/{share}/search`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Case-insensitive substring to match against names.
    pub q: String,
    /// Directory to search under; empty for the root.
    #[serde(default)]
    pub path: String,
    /// Result limit (default 100, capped at `limits.search_max`).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Query of `GET /v1/recent`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentQuery {
    /// Result limit (default 50).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Only files modified after this time.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub since: Option<Timestamp>,
}

/// Query of `GET /v1/shares/{share}/thumb`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbQuery {
    /// Share-relative path.
    #[serde(default)]
    pub path: String,
    /// Longest side in pixels; one of `capabilities.thumb_sizes` (default 256).
    #[serde(default)]
    pub size: Option<u32>,
    /// Queue priority (additive, WP-14): `visible` for rows on screen, `prefetch` (the default)
    /// for rows the client expects to show soon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<ThumbPriority>,
}

/// `priority` of `GET /v1/shares/{share}/thumb`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThumbPriority {
    /// The row is visible: queued ahead of prefetches.
    Visible,
    /// Prefetch (default when absent).
    #[default]
    Prefetch,
}

impl ThumbPriority {
    /// Parses the query value (`visible` | `prefetch`).
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "visible" => Some(Self::Visible),
            "prefetch" => Some(Self::Prefetch),
            _ => None,
        }
    }
}

/// Query of `GET /v1/shares/{share}/preview/info`, `/file` and `/hash` share this shape plus
/// their own extras; this is the preview page query.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewQuery {
    /// Share-relative path.
    #[serde(default)]
    pub path: String,
    /// 1-based page (default 1).
    #[serde(default)]
    pub page: Option<u32>,
    /// Target width in pixels (capped at `capabilities.preview_max_width`).
    #[serde(default)]
    pub width: Option<u32>,
    /// Output encoding.
    #[serde(default)]
    pub format: ImageFormat,
}

/// Query of `GET /v1/shares/{share}/preview/text`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPreviewQuery {
    /// Share-relative path.
    #[serde(default)]
    pub path: String,
    /// Byte offset into the decoded text.
    #[serde(default)]
    pub offset: u64,
    /// Bytes to return (default 65536, hard cap 1 MiB).
    #[serde(default)]
    pub limit: Option<u64>,
}

/// Query of `GET /v1/shares/{share}/hash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashQuery {
    /// Share-relative path.
    #[serde(default)]
    pub path: String,
    /// Algorithm.
    pub algo: super::host::HashAlgo,
}

/// Query of `GET /v1/shares/{share}/file` and `GET /v1/shares/{share}/preview/info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathQuery {
    /// Share-relative path.
    #[serde(default)]
    pub path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_query_splits_filters() {
        let q: ListQuery =
            serde_json::from_str(r#"{"path":"a","kinds":"file,dir","ext":"pdf,,pptx"}"#).unwrap();
        assert_eq!(q.kinds(), ["file", "dir"]);
        assert_eq!(q.exts(), ["pdf", "pptx"]);
        assert_eq!(q.sort, SortKey::Name);
        assert_eq!(ListQuery::default().kinds(), Vec::<&str>::new());
    }

    #[test]
    fn path_display_is_omitted_when_absent() {
        let share = ShareInfo {
            id: "shr_1".into(),
            name: "P".into(),
            path_display: None,
            permissions: vec![Permission::List],
            show_hidden: false,
            stream_default: StreamDecision::Ask,
            search: None,
        };
        let json = serde_json::to_value(&share).unwrap();
        assert!(json.get("path_display").is_none());
        assert!(json.get("search").is_none());
    }
}
