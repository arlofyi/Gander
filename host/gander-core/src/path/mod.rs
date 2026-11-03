// SPDX-License-Identifier: Apache-2.0
//! Path resolution policy (`docs/specs/2026-11-01-path-resolution.md`), the OS-independent half.
//!
//! The platform crate (`gander-win`) performs the handle-relative walk; everything that can be
//! decided without touching the filesystem lives here so it is testable on any OS:
//!
//! - [`ValidatedPath`]: algorithm steps 1-4 (split, depth and length limits, per-segment checks).
//! - [`reparse_tag_allowed`]: the step 6c reparse-tag policy.
//! - [`HiddenPatterns`]: share-level globs that hide entries and make them unresolvable.
//! - [`listing`]: entry attributes, the listing filter, sorting, paging and the cursor.
//! - [`RejectReason`] / [`ResolveError`]: the rejection codes and their protocol error codes.
//!
//! Rejections are never detailed to the phone beyond [`RejectReason::error_code`]; the full
//! reason belongs in the local audit log.

mod hidden;
pub mod listing;
mod segments;

use std::fmt;

pub use hidden::{HiddenPatterns, PatternError, is_always_omitted};
pub use listing::{
    CursorError, EntryAttrs, ListFilter, ListPage, ListedEntry, MAX_LIST_ENTRIES, page,
    timestamp_from_filetime,
};
pub use segments::{MAX_PATH_BYTES, MAX_SEGMENTS, ValidatedPath};

use crate::error::ErrorCode;

/// Why the resolver refused a path. The wire names (`as_str`) are the `reason` values of
/// `protocol/test-vectors/paths.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// Step 2: an empty segment other than one trailing slash (`a//b`, `/a`).
    EmptySegment,
    /// Step 3: more than [`MAX_SEGMENTS`] segments.
    TooDeep,
    /// Step 3: more than [`MAX_PATH_BYTES`] bytes.
    TooLong,
    /// Step 4a: `.` or `..`.
    DotSegment,
    /// Step 4b: NUL, a control character, `\`, `:`, `<`, `>`, `"`, `|`, `?` or `*`.
    InvalidChar,
    /// Step 4c: the segment ends with `.` or a space.
    TrailingDotOrSpace,
    /// Step 4d: a reserved DOS device name (`CON`, `com1.txt`, ...).
    ReservedName,
    /// Step 4e: the segment is not valid UTF-8.
    InvalidUtf8,
    /// Step 6b: the item does not exist (also used for hidden patterns).
    NotFound,
    /// Step 6b: the OS refused access.
    Forbidden,
    /// Step 6c: a reparse point (or symlink) of a denied class.
    ReparsePoint,
    /// Step 6d: the requested name is an 8.3 alias or otherwise differs from the stored name.
    ShortName,
    /// Step 6e: an intermediate segment is a file.
    NotADirectory,
    /// Step 7: the final handle is outside the share root. A check above has a bug; the caller
    /// MUST log this at error level.
    Escaped,
}

impl RejectReason {
    /// The reason code, for example `"dot_segment"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            RejectReason::EmptySegment => "empty_segment",
            RejectReason::TooDeep => "too_deep",
            RejectReason::TooLong => "too_long",
            RejectReason::DotSegment => "dot_segment",
            RejectReason::InvalidChar => "invalid_char",
            RejectReason::TrailingDotOrSpace => "trailing_dot_or_space",
            RejectReason::ReservedName => "reserved_name",
            RejectReason::InvalidUtf8 => "invalid_utf8",
            RejectReason::NotFound => "not_found",
            RejectReason::Forbidden => "forbidden",
            RejectReason::ReparsePoint => "reparse_point",
            RejectReason::ShortName => "short_name",
            RejectReason::NotADirectory => "not_a_directory",
            RejectReason::Escaped => "escaped",
        }
    }

    /// The protocol error code sent to the phone.
    ///
    /// Protocol v1, "Error format": `not_found` is also used for hidden patterns and denied
    /// reparse points; `bad_path` covers every other resolver rejection. Access denied by the OS
    /// is `forbidden`.
    pub const fn error_code(self) -> ErrorCode {
        match self {
            RejectReason::NotFound | RejectReason::ReparsePoint => ErrorCode::NotFound,
            RejectReason::Forbidden => ErrorCode::Forbidden,
            _ => ErrorCode::BadPath,
        }
    }

    /// Whether this rejection means a resolver check has a bug (step 7 fired).
    pub const fn is_bug(self) -> bool {
        matches!(self, RejectReason::Escaped)
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result of a failed resolve: a policy rejection, or an unexpected OS error.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The path was refused by policy.
    #[error("path rejected: {0}")]
    Rejected(RejectReason),
    /// The OS failed in a way the policy does not cover (sent as `internal`).
    #[error("i/o error during path resolution: {0}")]
    Io(#[from] std::io::Error),
}

impl ResolveError {
    /// The protocol error code sent to the phone.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            ResolveError::Rejected(reason) => reason.error_code(),
            ResolveError::Io(_) => ErrorCode::Internal,
        }
    }

    /// The rejection reason, if this is a policy rejection.
    pub fn reason(&self) -> Option<RejectReason> {
        match self {
            ResolveError::Rejected(reason) => Some(*reason),
            ResolveError::Io(_) => None,
        }
    }
}

impl From<RejectReason> for ResolveError {
    fn from(reason: RejectReason) -> Self {
        ResolveError::Rejected(reason)
    }
}

/// `IO_REPARSE_TAG_CLOUD` (cloud-files placeholder, base of the `CLOUD_1..F` family).
pub const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
/// `IO_REPARSE_TAG_CLOUD_MASK`: the bits that distinguish `CLOUD_1..CLOUD_F`.
pub const IO_REPARSE_TAG_CLOUD_MASK: u32 = 0x0000_F000;
/// `IO_REPARSE_TAG_WOF` (compact.exe / Windows Overlay Filter).
pub const IO_REPARSE_TAG_WOF: u32 = 0x8000_0017;
/// `IO_REPARSE_TAG_DEDUP` (Data Deduplication).
pub const IO_REPARSE_TAG_DEDUP: u32 = 0x8000_0013;
/// `IO_REPARSE_TAG_SYMLINK`.
pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
/// `IO_REPARSE_TAG_MOUNT_POINT` (junctions and volume mount points).
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `IO_REPARSE_TAG_APPEXECLINK`.
pub const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;
/// `IO_REPARSE_TAG_PROJFS`.
pub const IO_REPARSE_TAG_PROJFS: u32 = 0x9000_001C;

/// Step 6c: whether a reparse point carrying `tag` may be traversed or opened.
///
/// Allowed: the cloud-files family (OneDrive, Dropbox, Google Drive placeholders), WOF and dedup.
/// Everything else, including symlinks, junctions, mount points, app-exec links, ProjFS and
/// unknown tags, is denied.
pub const fn reparse_tag_allowed(tag: u32) -> bool {
    tag & !IO_REPARSE_TAG_CLOUD_MASK == IO_REPARSE_TAG_CLOUD
        || tag == IO_REPARSE_TAG_WOF
        || tag == IO_REPARSE_TAG_DEDUP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reparse_policy() {
        assert!(reparse_tag_allowed(IO_REPARSE_TAG_CLOUD));
        for n in 1..=0xF_u32 {
            assert!(
                reparse_tag_allowed(IO_REPARSE_TAG_CLOUD | (n << 12)),
                "cloud_{n}"
            );
        }
        assert!(reparse_tag_allowed(IO_REPARSE_TAG_WOF));
        assert!(reparse_tag_allowed(IO_REPARSE_TAG_DEDUP));
        for denied in [
            IO_REPARSE_TAG_SYMLINK,
            IO_REPARSE_TAG_MOUNT_POINT,
            IO_REPARSE_TAG_APPEXECLINK,
            IO_REPARSE_TAG_PROJFS,
            0,
            0xFFFF_FFFF,
            IO_REPARSE_TAG_CLOUD | 0x1,
        ] {
            assert!(!reparse_tag_allowed(denied), "{denied:#x}");
        }
    }

    #[test]
    fn error_codes_follow_protocol() {
        assert_eq!(RejectReason::NotFound.error_code(), ErrorCode::NotFound);
        assert_eq!(RejectReason::ReparsePoint.error_code(), ErrorCode::NotFound);
        assert_eq!(RejectReason::Forbidden.error_code(), ErrorCode::Forbidden);
        assert_eq!(RejectReason::DotSegment.error_code(), ErrorCode::BadPath);
        assert_eq!(RejectReason::Escaped.error_code(), ErrorCode::BadPath);
        assert!(RejectReason::Escaped.is_bug());
        let io = ResolveError::Io(std::io::Error::other("x"));
        assert_eq!(io.error_code(), ErrorCode::Internal);
    }
}
