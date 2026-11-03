// SPDX-License-Identifier: Apache-2.0
//! Listing semantics (path-resolution spec, "Listing semantics"), the OS-independent half.
//!
//! The platform crate enumerates a directory handle into [`ListedEntry`] values, using
//! [`ListFilter`] to drop always-omitted, hidden/system (unless `show_hidden`) and
//! `hidden_patterns` entries while it enumerates, and stops at [`MAX_LIST_ENTRIES`]. [`page`]
//! then filters by kind/extension, sorts and cuts one page with an opaque cursor.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use super::hidden::{HiddenPatterns, is_always_omitted};
use crate::protocol::files::{
    Entry, EntryFlag, EntryKind, ListQuery, PreviewKind, SortKey, SortOrder, StreamDecision,
};
use crate::protocol::ids::Timestamp;

/// Directories with more entries than this return the first `MAX_LIST_ENTRIES` and
/// `truncated: true`.
pub const MAX_LIST_ENTRIES: usize = 50_000;

/// Attribute flags of an entry, decoded from the OS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct EntryAttrs {
    /// Hidden attribute.
    pub hidden: bool,
    /// Read-only attribute.
    pub readonly: bool,
    /// System attribute.
    pub system: bool,
    /// Cloud placeholder whose data is not local (`RECALL_ON_DATA_ACCESS` or `RECALL_ON_OPEN`).
    pub cloud_only: bool,
    /// A reparse point of a denied class: shown greyed, never openable.
    pub link: bool,
    /// EFS-encrypted.
    pub encrypted: bool,
}

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_ATTRIBUTE_ENCRYPTED: u32 = 0x4000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

impl EntryAttrs {
    /// Decodes Win32 `FILE_ATTRIBUTE_*` bits plus the reparse tag (0 when not a reparse point).
    pub const fn from_windows(attributes: u32, reparse_tag: u32) -> Self {
        let is_reparse = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
        Self {
            hidden: attributes & FILE_ATTRIBUTE_HIDDEN != 0,
            readonly: attributes & FILE_ATTRIBUTE_READONLY != 0,
            system: attributes & FILE_ATTRIBUTE_SYSTEM != 0,
            cloud_only: attributes
                & (FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                != 0,
            link: is_reparse && !super::reparse_tag_allowed(reparse_tag),
            encrypted: attributes & FILE_ATTRIBUTE_ENCRYPTED != 0,
        }
    }

    /// The protocol flags, in a stable order.
    pub fn flags(&self) -> Vec<EntryFlag> {
        [
            (self.hidden, EntryFlag::Hidden),
            (self.readonly, EntryFlag::Readonly),
            (self.system, EntryFlag::System),
            (self.cloud_only, EntryFlag::CloudOnly),
            (self.link, EntryFlag::Link),
            (self.encrypted, EntryFlag::Encrypted),
        ]
        .into_iter()
        .filter_map(|(on, f)| on.then_some(f))
        .collect()
    }
}

/// One enumerated directory entry (or a stat result), before protocol decoration.
#[derive(Debug, Clone, PartialEq)]
pub struct ListedEntry {
    /// Name exactly as stored.
    pub name: String,
    /// File or directory.
    pub kind: EntryKind,
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Last modification time.
    pub mtime: Timestamp,
    /// Creation time (Windows).
    pub ctime: Option<Timestamp>,
    /// Attribute flags.
    pub attrs: EntryAttrs,
}

impl ListedEntry {
    /// Lower-case extension without the dot (`None` for directories and dot-files like
    /// `.gitignore`).
    pub fn ext(&self) -> Option<String> {
        if self.kind == EntryKind::Dir {
            return None;
        }
        match self.name.rfind('.') {
            Some(i) if i > 0 && i + 1 < self.name.len() => Some(self.name[i + 1..].to_lowercase()),
            _ => None,
        }
    }

    /// The protocol [`Entry`] with neutral decoration: no mime, no thumb prediction,
    /// `preview: none`, `stream: refused`. WP-14/WP-15 overwrite those fields.
    pub fn to_entry(&self) -> Entry {
        Entry {
            name: self.name.clone(),
            kind: self.kind,
            size: if self.kind == EntryKind::Dir {
                0
            } else {
                self.size
            },
            mtime: self.mtime,
            ctime: self.ctime,
            ext: self.ext(),
            mime: None,
            flags: self.attrs.flags(),
            thumb: false,
            preview: PreviewKind::None,
            stream: StreamDecision::Refused,
            share: None,
            path: None,
            meta: None,
        }
    }
}

/// Converts a Windows `FILETIME` (100 ns ticks since 1601-01-01 UTC) to a timestamp. Returns
/// `None` for 0 (unknown) and for values outside the representable range.
pub fn timestamp_from_filetime(ticks: i64) -> Option<Timestamp> {
    const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    if ticks <= 0 {
        return None;
    }
    let nanos = i128::from(ticks - UNIX_EPOCH_TICKS) * 100;
    Timestamp::from_unix_timestamp_nanos(nanos).ok()
}

/// The per-share filter applied while enumerating one directory.
#[derive(Debug, Clone, Copy)]
pub struct ListFilter<'a> {
    /// The share's `show_hidden`.
    pub show_hidden: bool,
    /// The share's `hidden_patterns`.
    pub hidden: &'a HiddenPatterns,
    /// Segments of the directory being listed (share-relative).
    pub dir: &'a [&'a str],
}

impl ListFilter<'_> {
    /// Whether an entry with this name and these attributes is listed.
    pub fn keeps(&self, name: &str, attrs: &EntryAttrs) -> bool {
        if name == "." || name == ".." || is_always_omitted(name) {
            return false;
        }
        if !self.show_hidden && (attrs.hidden || attrs.system) {
            return false;
        }
        !self.hidden.is_child_hidden(self.dir, name)
    }
}

/// One page of a listing.
#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    /// Entries of this page, in order.
    pub entries: Vec<ListedEntry>,
    /// Cursor for the next page, `None` on the last page.
    pub next_cursor: Option<String>,
}

/// The cursor does not parse or belongs to a different sort (sent as `bad_request`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid list cursor")]
pub struct CursorError;

/// The sort position of one entry. Total order: dirs first, then the primary key, then the
/// case-folded name, then the exact name (names are unique within a directory).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SortPos {
    /// Cursor format version.
    v: u8,
    /// `name|mtime|size|kind`.
    sort: SortKey,
    /// `asc|desc`.
    order: SortOrder,
    /// 0 for directories, 1 for files.
    group: u8,
    /// mtime in ns, size, or 0.
    num: i128,
    /// Lower-case extension for `kind`, else empty.
    ext: String,
    /// Case-folded name.
    fold: String,
    /// Exact name.
    name: String,
}

impl SortPos {
    fn of(e: &ListedEntry, sort: SortKey, order: SortOrder) -> Self {
        let num = match sort {
            SortKey::Mtime => e.mtime.unix_timestamp_nanos(),
            SortKey::Size => i128::from(e.size),
            SortKey::Name | SortKey::Kind => 0,
        };
        Self {
            v: 1,
            sort,
            order,
            group: u8::from(e.kind != EntryKind::Dir),
            num,
            ext: if sort == SortKey::Kind {
                e.ext().unwrap_or_default()
            } else {
                String::new()
            },
            fold: e.name.to_lowercase(),
            name: e.name.clone(),
        }
    }

    fn cmp_with(&self, other: &Self) -> Ordering {
        let within = self
            .num
            .cmp(&other.num)
            .then_with(|| self.ext.cmp(&other.ext))
            .then_with(|| self.fold.cmp(&other.fold))
            .then_with(|| self.name.cmp(&other.name));
        let within = match self.order {
            SortOrder::Asc => within,
            SortOrder::Desc => within.reverse(),
        };
        // Directories sort first regardless of the order.
        self.group.cmp(&other.group).then(within)
    }

    fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        b64::encode(&json)
    }

    fn decode(s: &str, sort: SortKey, order: SortOrder) -> Result<Self, CursorError> {
        let bytes = b64::decode(s).ok_or(CursorError)?;
        let pos: SortPos = serde_json::from_slice(&bytes).map_err(|_| CursorError)?;
        if pos.v != 1 || pos.sort != sort || pos.order != order {
            return Err(CursorError);
        }
        Ok(pos)
    }
}

/// Filters by the query's `kinds` and `ext`, sorts and cuts one page.
///
/// - `kinds`: `file`, `dir`; unknown values are ignored; empty means all.
/// - `ext`: case-insensitive, applies to files only (directories stay navigable).
/// - `limit`: default 200, clamped to `1..=list_max`.
/// - `cursor`: from a previous page with the same `sort` and `order`, else [`CursorError`].
pub fn page(
    mut entries: Vec<ListedEntry>,
    query: &ListQuery,
    list_max: u32,
) -> Result<ListPage, CursorError> {
    let kinds = query.kinds();
    let want_files = kinds.is_empty() || kinds.contains(&"file");
    let want_dirs = kinds.is_empty() || kinds.contains(&"dir");
    let exts: Vec<String> = query.exts().iter().map(|e| e.to_lowercase()).collect();
    entries.retain(|e| match e.kind {
        EntryKind::Dir => want_dirs,
        EntryKind::File => {
            want_files && (exts.is_empty() || e.ext().is_some_and(|x| exts.contains(&x)))
        }
    });

    let (sort, order) = (query.sort, query.order);
    let mut keyed: Vec<(SortPos, ListedEntry)> = entries
        .into_iter()
        .map(|e| (SortPos::of(&e, sort, order), e))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp_with(&b.0));

    let start = match query.cursor.as_deref() {
        None | Some("") => 0,
        Some(c) => {
            let after = SortPos::decode(c, sort, order)?;
            keyed.partition_point(|(k, _)| k.cmp_with(&after) != Ordering::Greater)
        }
    };
    let limit = query
        .limit
        .unwrap_or(ListQuery::DEFAULT_LIMIT)
        .clamp(1, list_max.max(1)) as usize;
    let end = start.saturating_add(limit).min(keyed.len());
    let next_cursor = (end < keyed.len() && end > start).then(|| keyed[end - 1].0.encode());
    let entries = keyed
        .into_iter()
        .skip(start)
        .take(end - start)
        .map(|(_, e)| e)
        .collect();
    Ok(ListPage {
        entries,
        next_cursor,
    })
}

/// Minimal unpadded base64url (RFC 4648 section 5), for the opaque cursor.
mod b64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    pub(super) fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..=chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            }
        }
        out
    }

    pub(super) fn decode(s: &str) -> Option<Vec<u8>> {
        if s.len() % 4 == 1 {
            return None;
        }
        let mut out = Vec::with_capacity(s.len() * 3 / 4);
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in s.bytes() {
            let v = ALPHABET.iter().position(|&a| a == c)? as u32;
            acc = (acc << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64, mtime_s: i64) -> ListedEntry {
        ListedEntry {
            name: name.to_owned(),
            kind,
            size,
            mtime: Timestamp::from_unix_timestamp(mtime_s).unwrap(),
            ctime: None,
            attrs: EntryAttrs::default(),
        }
    }

    fn sample() -> Vec<ListedEntry> {
        vec![
            entry("b.txt", EntryKind::File, 30, 3),
            entry("A.pdf", EntryKind::File, 10, 1),
            entry("zdir", EntryKind::Dir, 0, 5),
            entry("c.PDF", EntryKind::File, 20, 2),
            entry("adir", EntryKind::Dir, 0, 4),
        ]
    }

    fn names(p: &ListPage) -> Vec<&str> {
        p.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn sorts_dirs_first_by_name() {
        let p = page(sample(), &ListQuery::default(), 1000).unwrap();
        assert_eq!(names(&p), ["adir", "zdir", "A.pdf", "b.txt", "c.PDF"]);
        assert!(p.next_cursor.is_none());
    }

    #[test]
    fn sort_desc_keeps_dirs_first() {
        let q = ListQuery {
            sort: SortKey::Size,
            order: SortOrder::Desc,
            ..Default::default()
        };
        let p = page(sample(), &q, 1000).unwrap();
        assert_eq!(names(&p), ["zdir", "adir", "b.txt", "c.PDF", "A.pdf"]);
    }

    #[test]
    fn kind_and_ext_filters() {
        let q = ListQuery {
            kinds: Some("file".into()),
            ext: Some("pdf".into()),
            sort: SortKey::Mtime,
            ..Default::default()
        };
        let p = page(sample(), &q, 1000).unwrap();
        assert_eq!(names(&p), ["A.pdf", "c.PDF"]);
    }

    #[test]
    fn paging_with_cursor_covers_everything_once() {
        for sort in [SortKey::Name, SortKey::Mtime, SortKey::Size, SortKey::Kind] {
            for order in [SortOrder::Asc, SortOrder::Desc] {
                let all = page(
                    sample(),
                    &ListQuery {
                        sort,
                        order,
                        ..Default::default()
                    },
                    1000,
                )
                .unwrap();
                let mut seen = Vec::new();
                let mut cursor = None;
                loop {
                    let q = ListQuery {
                        sort,
                        order,
                        limit: Some(2),
                        cursor: cursor.clone(),
                        ..Default::default()
                    };
                    let p = page(sample(), &q, 1000).unwrap();
                    seen.extend(p.entries.iter().map(|e| e.name.clone()));
                    match p.next_cursor {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                let expected: Vec<String> = all.entries.iter().map(|e| e.name.clone()).collect();
                assert_eq!(seen, expected, "{sort:?} {order:?}");
            }
        }
    }

    #[test]
    fn cursor_from_other_sort_is_rejected() {
        let q = ListQuery {
            limit: Some(1),
            ..Default::default()
        };
        let c = page(sample(), &q, 1000).unwrap().next_cursor.unwrap();
        let other = ListQuery {
            sort: SortKey::Size,
            cursor: Some(c),
            ..Default::default()
        };
        assert_eq!(page(sample(), &other, 1000), Err(CursorError));
        let garbage = ListQuery {
            cursor: Some("!!".into()),
            ..Default::default()
        };
        assert_eq!(page(sample(), &garbage, 1000), Err(CursorError));
    }

    #[test]
    fn limit_is_clamped() {
        let q = ListQuery {
            limit: Some(0),
            ..Default::default()
        };
        assert_eq!(page(sample(), &q, 1000).unwrap().entries.len(), 1);
        let q = ListQuery {
            limit: Some(10_000),
            ..Default::default()
        };
        assert_eq!(page(sample(), &q, 3).unwrap().entries.len(), 3);
    }

    #[test]
    fn filter_rules() {
        let hidden = HiddenPatterns::new(&["**/.git"]).unwrap();
        let f = ListFilter {
            show_hidden: false,
            hidden: &hidden,
            dir: &["src"],
        };
        let plain = EntryAttrs::default();
        let hid = EntryAttrs {
            hidden: true,
            ..Default::default()
        };
        assert!(f.keeps("main.rs", &plain));
        assert!(!f.keeps(".git", &plain));
        assert!(!f.keeps("x", &hid));
        assert!(!f.keeps("Thumbs.db", &plain));
        let shown = ListFilter {
            show_hidden: true,
            ..f
        };
        assert!(shown.keeps("x", &hid));
        assert!(!shown.keeps("desktop.ini", &hid));
    }

    #[test]
    fn windows_attributes_decode() {
        let a =
            EntryAttrs::from_windows(0x2 | 0x400 | 0x40_0000, super::super::IO_REPARSE_TAG_CLOUD);
        assert!(a.hidden && a.cloud_only && !a.link);
        let j = EntryAttrs::from_windows(0x10 | 0x400, super::super::IO_REPARSE_TAG_MOUNT_POINT);
        assert!(j.link);
        assert_eq!(j.flags(), [EntryFlag::Link]);
    }

    #[test]
    fn filetime_conversion() {
        assert_eq!(
            timestamp_from_filetime(116_444_736_000_000_000 + 10_000_000),
            Some(Timestamp::from_unix_timestamp(1).unwrap())
        );
        assert_eq!(timestamp_from_filetime(0), None);
    }

    #[test]
    fn ext_rules() {
        assert_eq!(
            entry("a.TAR.GZ", EntryKind::File, 0, 0).ext().as_deref(),
            Some("gz")
        );
        assert_eq!(entry(".gitignore", EntryKind::File, 0, 0).ext(), None);
        assert_eq!(entry("noext", EntryKind::File, 0, 0).ext(), None);
        assert_eq!(entry("d.x", EntryKind::Dir, 0, 0).ext(), None);
    }

    #[test]
    fn b64_round_trip() {
        for n in 0..20u8 {
            let data: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64::decode(&b64::encode(&data)).unwrap(), data);
        }
    }
}
