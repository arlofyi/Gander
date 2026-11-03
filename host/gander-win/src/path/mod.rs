// SPDX-License-Identifier: Apache-2.0
//! Windows path resolver and directory listing (`docs/specs/2026-11-01-path-resolution.md`).
//!
//! The single place where a protocol path becomes an open handle. Steps 1-4 (validation) come
//! from [`gander_core::path::ValidatedPath`]; this module performs steps 5-8:
//!
//! - every segment is opened with `NtCreateFile` RELATIVE TO THE PARENT HANDLE and
//!   `FILE_OPEN_REPARSE_POINT`, never by a joined path string;
//! - reparse points are checked against the tag policy
//!   ([`gander_core::path::reparse_tag_allowed`]); junctions and symlinks are refused even when
//!   they point inside the share;
//! - the stored name is compared with the requested one, refusing 8.3 aliases;
//! - the final handle's normalised path must be the canonical root or below it (step 7).
//!
//! Callers (Browse, Fetch, thumbnails, previews, stream launch) take the returned [`Resolved`]
//! handle and never reopen by path.

mod nt;
#[cfg(test)]
mod tests;

use std::fs::File;
use std::io;
use std::ops::ControlFlow;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::path::{Path, PathBuf};

use gander_core::config::ShareConfig;
use gander_core::path::{
    CursorError, EntryAttrs, HiddenPatterns, ListFilter, ListPage, ListedEntry, MAX_LIST_ENTRIES,
    RejectReason, ResolveError, ValidatedPath, reparse_tag_allowed, timestamp_from_filetime,
};
use gander_core::protocol::Timestamp;
use gander_core::protocol::files::{EntryKind, ListQuery};

/// An open share root: the root directory handle, its canonical path and the share's listing
/// options. Cheap to share between threads (`&ShareRoot` is enough to resolve).
#[derive(Debug)]
pub struct ShareRoot {
    handle: OwnedHandle,
    /// `\\?\`-prefixed, normalised final path of the root, as UTF-16 without terminator.
    canonical: Vec<u16>,
    show_hidden: bool,
    hidden: HiddenPatterns,
}

impl ShareRoot {
    /// Opens `root` (which may itself be a junction or symlink; it is resolved once, here) and
    /// records its canonical form. Fails if the root is not a directory.
    pub fn open(root: &Path, show_hidden: bool, hidden: HiddenPatterns) -> io::Result<Self> {
        let handle = nt::open_root(root)?;
        let (attrs, _) = nt::attribute_tag(handle.as_handle())?;
        if attrs & nt::FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "share root is not a directory",
            ));
        }
        let mut canonical = nt::final_dos_path(handle.as_handle())?;
        // A volume root comes back as `\\?\C:\`; keep the form without the trailing separator
        // so the step-7 prefix test is uniform.
        if canonical.last() == Some(&BACKSLASH) {
            canonical.pop();
        }
        Ok(Self {
            handle,
            canonical,
            show_hidden,
            hidden,
        })
    }

    /// Opens the root of a configured share with its `show_hidden` and `hidden_patterns`.
    pub fn from_config(share: &ShareConfig) -> io::Result<Self> {
        let hidden = HiddenPatterns::new(&share.hidden_patterns)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Self::open(&share.root, share.show_hidden, hidden)
    }

    /// The canonical (`\\?\`-prefixed, normalised) root path recorded at open. Compare it with
    /// the value recorded at share creation to detect a root that moved.
    pub fn canonical_root(&self) -> PathBuf {
        PathBuf::from(std::ffi::OsString::from_wide(&self.canonical))
    }

    /// The share's `show_hidden`.
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// The share's compiled `hidden_patterns`.
    pub fn hidden_patterns(&self) -> &HiddenPatterns {
        &self.hidden
    }

    /// Resolves a protocol path (percent-decoded once by the HTTP layer, raw bytes) to a handle.
    pub fn resolve(&self, protocol_path: &[u8]) -> Result<Resolved, ResolveError> {
        let path = ValidatedPath::parse(protocol_path)?;
        self.resolve_validated(path)
    }

    /// Resolves an already validated path (steps 5-8).
    pub fn resolve_validated(&self, path: ValidatedPath) -> Result<Resolved, ResolveError> {
        // hidden_patterns make entries unresolvable. Matching is case-insensitive and the
        // short-name check below guarantees the requested names are the stored names up to case.
        if self.hidden.is_hidden(&path.segment_strs()) {
            return Err(RejectReason::NotFound.into());
        }
        let segments = path.segments();
        let mut current: Option<OwnedHandle> = None;
        let mut is_dir = true;
        let mut name = String::new();
        for (i, seg) in segments.iter().enumerate() {
            let last = i + 1 == segments.len();
            let parent = current
                .as_ref()
                .map_or_else(|| self.handle.as_handle(), AsHandle::as_handle);
            let wide: Vec<u16> = seg.encode_utf16().collect();
            // 6a/6b
            let mut child = nt::open_relative(parent, &wide).map_err(status_to_error)?;
            // 6c
            let (attrs, tag) = nt::attribute_tag(child.as_handle())?;
            let allowed_reparse = if attrs & nt::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                if !reparse_tag_allowed(tag) {
                    return Err(RejectReason::ReparsePoint.into());
                }
                true
            } else {
                false
            };
            // 6d
            let stored = nt::stored_name(child.as_handle())?;
            if !nt::eq_ignore_case(&stored, &wide) {
                return Err(RejectReason::ShortName.into());
            }
            // 6e
            is_dir = attrs & nt::FILE_ATTRIBUTE_DIRECTORY != 0;
            if !last && !is_dir {
                return Err(RejectReason::NotADirectory.into());
            }
            // An allowed reparse point (cloud placeholder, WOF, dedup) is re-opened relative to
            // its own handle without FILE_OPEN_REPARSE_POINT so its filter serves the real data.
            // If the filter is not running the original handle is kept (still safe, step 7
            // re-checks the final handle).
            if allowed_reparse && let Ok(h) = nt::reopen(child.as_handle()) {
                child = h;
            }
            if last {
                name = String::from_utf16_lossy(&stored);
            }
            // 6f: the previous parent is dropped (closed) here.
            current = Some(child);
        }
        let handle = match current {
            Some(h) => h,
            // The root: a fresh handle to the same directory, so the caller owns its
            // enumeration state.
            None => nt::open_relative(self.handle.as_handle(), &[]).map_err(status_to_error)?,
        };
        // Step 7.
        self.verify_within_root(handle.as_handle())?;
        Ok(Resolved {
            handle,
            path,
            name,
            kind: if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
        })
    }

    /// Step 7: the final handle's normalised DOS path must be the canonical root or start with
    /// `canonical_root + '\'`. A mismatch is [`RejectReason::Escaped`]: a check above has a bug
    /// and the caller must log it at error level.
    pub(crate) fn verify_within_root(
        &self,
        handle: BorrowedHandle<'_>,
    ) -> Result<(), ResolveError> {
        let final_path = nt::final_dos_path(handle)?;
        let root = self.canonical.as_slice();
        let inside = final_path == root
            || (final_path.len() > root.len()
                && final_path.starts_with(root)
                && final_path[root.len()] == BACKSLASH);
        if inside {
            Ok(())
        } else {
            Err(RejectReason::Escaped.into())
        }
    }

    /// Enumerates the directory behind `dir` through its handle, applying the share's listing
    /// filter (always-omitted names, hidden/system unless `show_hidden`, `hidden_patterns`).
    /// Stops after [`MAX_LIST_ENTRIES`] kept entries and reports `truncated`.
    pub fn enumerate(&self, dir: &Resolved) -> Result<Enumerated, ListError> {
        if dir.kind != EntryKind::Dir {
            return Err(ListError::NotADirectory);
        }
        let dir_segments = dir.path.segment_strs();
        let filter = ListFilter {
            show_hidden: self.show_hidden,
            hidden: &self.hidden,
            dir: &dir_segments,
        };
        let mut out = Enumerated::default();
        nt::enumerate(dir.handle.as_handle(), |raw| {
            if raw.name == [DOT] || raw.name == [DOT, DOT] {
                return ControlFlow::Continue(());
            }
            let Ok(name) = String::from_utf16(raw.name) else {
                out.unlistable += 1;
                return ControlFlow::Continue(());
            };
            let attrs = EntryAttrs::from_windows(raw.attributes, raw.reparse_tag);
            if !filter.keeps(&name, &attrs) {
                return ControlFlow::Continue(());
            }
            if out.entries.len() == MAX_LIST_ENTRIES {
                out.truncated = true;
                return ControlFlow::Break(());
            }
            out.entries.push(ListedEntry {
                name,
                kind: kind_of(raw.attributes),
                size: if raw.attributes & nt::FILE_ATTRIBUTE_DIRECTORY != 0 {
                    0
                } else {
                    raw.size
                },
                mtime: timestamp_or_epoch(raw.last_write),
                ctime: timestamp_from_filetime(raw.creation),
                attrs,
            });
            ControlFlow::Continue(())
        })?;
        Ok(out)
    }

    /// One page of the listing of `dir`: [`Self::enumerate`] then
    /// [`gander_core::path::page`] (kinds/ext filters, sort, cursor, limit clamped to
    /// `list_max`).
    pub fn list(
        &self,
        dir: &Resolved,
        query: &ListQuery,
        list_max: u32,
    ) -> Result<Listing, ListError> {
        let all = self.enumerate(dir)?;
        let page = gander_core::path::page(all.entries, query, list_max)?;
        Ok(Listing {
            page,
            truncated: all.truncated,
            unlistable: all.unlistable,
        })
    }
}

const BACKSLASH: u16 = b'\\' as u16;
const DOT: u16 = b'.' as u16;

fn kind_of(attributes: u32) -> EntryKind {
    if attributes & nt::FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

fn timestamp_or_epoch(ticks: i64) -> Timestamp {
    timestamp_from_filetime(ticks).unwrap_or(Timestamp::UNIX_EPOCH)
}

// NTSTATUS values mapped to reject reasons (ntstatus.h).
const STATUS_NO_SUCH_FILE: i32 = 0xC000_000F_u32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034_u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003A_u32 as i32;
const STATUS_SHARING_VIOLATION: i32 = 0xC000_0043_u32 as i32;
const STATUS_DELETE_PENDING: i32 = 0xC000_0056_u32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103_u32 as i32;
const STATUS_IO_REPARSE_TAG_NOT_HANDLED: i32 = 0xC000_0279_u32 as i32;

/// Step 6b: map an `NtCreateFile` failure to a reject reason, or an internal I/O error.
fn status_to_error(status: i32) -> ResolveError {
    let reason = match status {
        STATUS_OBJECT_NAME_NOT_FOUND
        | STATUS_OBJECT_PATH_NOT_FOUND
        | STATUS_NO_SUCH_FILE
        | STATUS_DELETE_PENDING => RejectReason::NotFound,
        STATUS_ACCESS_DENIED | STATUS_SHARING_VIOLATION => RejectReason::Forbidden,
        STATUS_NOT_A_DIRECTORY => RejectReason::NotADirectory,
        nt::STATUS_OBJECT_NAME_INVALID => RejectReason::InvalidChar,
        STATUS_IO_REPARSE_TAG_NOT_HANDLED => RejectReason::ReparsePoint,
        other => return ResolveError::Io(nt::status_to_io(other)),
    };
    ResolveError::Rejected(reason)
}

/// A resolved share item: an open handle plus what the walk learned about it.
///
/// The handle was opened with read-data/list-directory, read-attributes and synchronous I/O,
/// shared read/write/delete. All further I/O must go through it (or a duplicate of it).
#[derive(Debug)]
pub struct Resolved {
    handle: OwnedHandle,
    path: ValidatedPath,
    name: String,
    kind: EntryKind,
}

impl Resolved {
    /// The validated share-relative path that was requested.
    pub fn path(&self) -> &ValidatedPath {
        &self.path
    }

    /// File or directory.
    pub fn kind(&self) -> EntryKind {
        self.kind
    }

    /// The stored name of the item (exact case as on disk); empty for the share root.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The open handle.
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }

    /// The item's absolute path as the OS reports it for the OPEN handle
    /// (`GetFinalPathNameByHandleW`, normalised), without the `\\?\` prefix (`\\?\UNC\` becomes
    /// `\\`). For handing a file to an app (Stream launch, WP-53): the path comes from the handle
    /// the resolver opened and checked, never from joining the phone's path onto the root.
    pub fn final_path(&self) -> std::io::Result<std::path::PathBuf> {
        let wide = nt::final_dos_path(self.handle.as_handle())?;
        let s = String::from_utf16(&wide)
            .map_err(|_| std::io::Error::other("path is not valid UTF-16"))?;
        let plain = if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned()
        };
        Ok(std::path::PathBuf::from(plain))
    }

    /// Gives up the handle.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }

    /// The handle as a `File` for reading (files only; reading a directory handle fails).
    pub fn into_file(self) -> File {
        File::from(self.handle)
    }

    /// A duplicate of the handle as a `File` (same file object, shared file position).
    pub fn try_clone_file(&self) -> io::Result<File> {
        Ok(File::from(self.handle.try_clone()?))
    }

    /// Stat through the handle (for `GET /stat`). The name is the stored name.
    pub fn stat(&self) -> io::Result<ListedEntry> {
        let (attrs, creation, last_write, size, _) = nt::stat(self.handle.as_handle())?;
        let (_, tag) = nt::attribute_tag(self.handle.as_handle())?;
        Ok(ListedEntry {
            name: self.name.clone(),
            kind: self.kind,
            size: if self.kind == EntryKind::Dir { 0 } else { size },
            mtime: timestamp_or_epoch(last_write),
            ctime: timestamp_from_filetime(creation),
            attrs: EntryAttrs::from_windows(attrs, tag),
        })
    }
}

/// All listable entries of one directory, before sorting and paging.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Enumerated {
    /// Kept entries, in enumeration order (at most [`MAX_LIST_ENTRIES`]).
    pub entries: Vec<ListedEntry>,
    /// More than [`MAX_LIST_ENTRIES`] entries were listable.
    pub truncated: bool,
    /// Entries omitted because their names are not valid UTF-16 (unpaired surrogates).
    pub unlistable: u64,
}

/// One page of a listing plus the directory-level flags of `ListResponse`.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    /// Entries of this page and the next cursor.
    pub page: ListPage,
    /// See [`Enumerated::truncated`].
    pub truncated: bool,
    /// See [`Enumerated::unlistable`].
    pub unlistable: u64,
}

/// Listing failure.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    /// The resolved item is a file (sent as `bad_path`).
    #[error("not a directory")]
    NotADirectory,
    /// The cursor is invalid (sent as `bad_request`).
    #[error(transparent)]
    Cursor(#[from] CursorError),
    /// Enumeration failed (sent as `internal`).
    #[error("directory enumeration failed: {0}")]
    Io(#[from] io::Error),
}
