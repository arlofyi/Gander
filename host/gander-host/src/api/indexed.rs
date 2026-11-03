// SPDX-License-Identifier: Apache-2.0
//! `GET /v1/shares/{share}/search` answered from the Windows Search index (WP-66), and the
//! per-share `search: "indexed"` advertisement.
//!
//! # When the index is used
//!
//! A search is answered from the index when the folder being searched is inside the indexer's
//! crawl scope and nothing goes wrong on the way. Otherwise (folder not crawled, the Windows
//! Search service stopped, any COM or provider error, search text the SQL builder refuses) the
//! caller falls back to the bounded walk, so the answer is the same kind of answer, only slower and
//! name-only. `via` in the response says which one produced it. The index is never asked to do
//! anything that changes it.
//!
//! # Every path the index returns is untrusted
//!
//! The index is a database somebody else keeps: rows can be stale, can name files that were
//! replaced by a junction since, or can sit in a folder the share hides. A row is turned into a
//! result like this and only like this:
//!
//! 1. [`gander_core::search::relative_to`] strips the searched folder's own path (the one the
//!    path resolver reported for the OPEN handle, never a string joined from the phone's input)
//!    and returns the rest; a row not below the folder is dropped.
//! 2. The rest is split into segments and appended to the searched folder's share-relative path,
//!    which gives a share-relative protocol path. The path is then resolved by
//!    [`ShareRoot::resolve`]: every segment from the share root, `ValidatedPath` rules (no `..`,
//!    no `:` so no alternate data streams, no reserved names, no trailing dots), `hidden_patterns`,
//!    reparse points of a denied class refused (junctions and symlinks, even ones that point
//!    inside the share), 8.3 aliases refused, and the final handle checked to be inside the root.
//!    Whatever does not resolve is dropped, not reported.
//! 3. Each ancestor below the searched folder and the item itself must pass the listing filter
//!    (`ListFilter::keeps`): hidden and system entries unless the share shows them, and the
//!    always-omitted names. So the index finds exactly what a listing would have shown.
//! 4. The entry (size, times, flags, name) comes from the handle, not from the index, so a stale
//!    row cannot lie about a file; for a name search the stored name must contain the text, so a
//!    renamed file does not match its old name.
//!
//! Permissions are the same as the walk's: `list` on the share, checked before this module runs.
//!
//! # Paging
//!
//! The cursor is `i1.<tag>.<offset>`: `offset` is how many index rows were consumed, and `tag`
//! binds it to the share, folder, text and scope of the query that made it (a cursor of another
//! query is `bad_request`). A page reads at most [`MAX_SCAN_ROWS`] rows and stops early at 2 s;
//! then the answer is `partial` and carries a cursor. `partial` is also set while the indexer still
//! has items to crawl, and when the result list reaches [`MAX_OFFSET`] rows. A walk cursor (`s1.`)
//! always continues the walk. An index cursor whose index is no longer usable starts the walk over;
//! clients that merge by path (the Android client does) lose nothing.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use gander_core::error::ErrorCode;
use gander_core::path::{ListFilter, ListedEntry};
use gander_core::protocol::ShareId;
use gander_core::protocol::files::{EntryKind, SearchResponse, SearchVia};
use gander_core::protocol::host::SearchCapability;
use gander_core::search::{
    IndexScope, MAX_OFFSET, MAX_SCAN_ROWS, SearchScope, build_sql, check_needle,
    contains_expression, relative_to,
};
use gander_win::path::ShareRoot;

use super::{ApiState, Opened, Requester, RootKey, bad_request};
use crate::server::ApiError;

/// How long a share's "is it indexed" answer is reused for `/shares` and `/me`.
const STATUS_TTL: Duration = Duration::from_secs(30);

/// Why the index could not answer. Never shown to the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexFailure(pub String);

impl std::fmt::Display for IndexFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the indexer says about a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexStatus {
    /// The folder is in the crawl scope.
    pub crawled: bool,
    /// The indexer still has items to crawl.
    pub catching_up: bool,
}

/// How a query on the index ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexRun {
    /// The indexer still has items to crawl.
    pub catching_up: bool,
    /// Rows consumed after `skip` (those the visitor took and those without a usable value).
    pub consumed: u64,
    /// The visitor refused a row, so there are more.
    pub stopped: bool,
}

/// The search index as the API uses it: [`WindowsSearch`] in production, a fake in tests.
pub trait IndexBackend: Send + Sync + std::fmt::Debug {
    /// Whether `dir` is crawled (read-only).
    fn status(&self, dir: &IndexScope) -> Result<IndexStatus, IndexFailure>;

    /// Runs `sql` against the index when `dir` is crawled (`Ok(None)` when it is not) and feeds
    /// the `System.ItemUrl` of each row after the first `skip` to `visit`: `Continue` takes the
    /// row, `Break` leaves it unconsumed and stops.
    fn run(
        &self,
        dir: &IndexScope,
        sql: &str,
        skip: u64,
        visit: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Option<IndexRun>, IndexFailure>;
}

/// The Windows Search service through `gander_win::search`. Each call opens its own session on
/// the calling thread (inside a `ComScope`), so it is called from the blocking pool.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsSearch;

impl IndexBackend for WindowsSearch {
    fn status(&self, dir: &IndexScope) -> Result<IndexStatus, IndexFailure> {
        let session =
            gander_win::search::IndexSession::open().map_err(|e| IndexFailure(e.to_string()))?;
        let crawled = session
            .is_in_scope(dir)
            .map_err(|e| IndexFailure(e.to_string()))?;
        Ok(IndexStatus {
            crawled,
            catching_up: session.backlog().is_some_and(|b| b.is_catching_up()),
        })
    }

    fn run(
        &self,
        dir: &IndexScope,
        sql: &str,
        skip: u64,
        visit: &mut dyn FnMut(&str) -> ControlFlow<()>,
    ) -> Result<Option<IndexRun>, IndexFailure> {
        let session =
            gander_win::search::IndexSession::open().map_err(|e| IndexFailure(e.to_string()))?;
        if !session
            .is_in_scope(dir)
            .map_err(|e| IndexFailure(e.to_string()))?
        {
            return Ok(None);
        }
        let catching_up = session.backlog().is_some_and(|b| b.is_catching_up());
        let end = session
            .query(sql, skip, visit)
            .map_err(|e| IndexFailure(e.to_string()))?;
        Ok(Some(IndexRun {
            catching_up,
            consumed: end.consumed,
            stopped: end.stopped,
        }))
    }
}

// ---------------------------------------------------------------------------------------------
// Cursor

const CURSOR_PREFIX: &str = "i1.";

/// A parsed `i1.<tag>.<offset>` cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IndexCursor {
    tag: String,
    offset: u64,
}

fn bad_cursor() -> ApiError {
    bad_request("The cursor is not valid for this search.")
}

/// Whether `cursor` is an index cursor (the walk's start with `s1.`).
pub(super) fn is_index_cursor(cursor: &str) -> bool {
    cursor.starts_with(CURSOR_PREFIX)
}

/// Parses an index cursor.
pub(super) fn parse_cursor(cursor: &str) -> Result<IndexCursor, ApiError> {
    let rest = cursor.strip_prefix(CURSOR_PREFIX).ok_or_else(bad_cursor)?;
    let (tag, offset) = rest.split_once('.').ok_or_else(bad_cursor)?;
    if tag.len() != 16 || !tag.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad_cursor());
    }
    if offset.is_empty() || offset.len() > 12 || !offset.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad_cursor());
    }
    Ok(IndexCursor {
        tag: tag.to_owned(),
        offset: offset.parse().map_err(|_| bad_cursor())?,
    })
}

/// Binds a cursor to the query it continues.
fn cursor_tag(share: &ShareId, path: &str, needle: &str, scope: SearchScope) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"gander-search-index-v1\0");
    for part in [share.as_str(), path, needle, scope.as_str()] {
        h.update(&(part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().to_hex().as_str()[..16].to_owned()
}

// ---------------------------------------------------------------------------------------------
// Mapping a row to a result

/// What one share-relative path turned out to be: the stored name and the entry from its handle,
/// or `None` when the resolver or the listing filter refuses it.
type Checked = Option<(String, ListedEntry)>;

/// Turns the untrusted tail of an index URL into a visible, resolved entry, remembering the
/// verdict for every path prefix so a folder with many hits is checked once.
struct Mapper<'a> {
    root: &'a ShareRoot,
    /// The searched folder's share-relative segments.
    base: Vec<String>,
    /// Verdict per share-relative path, as built (index spelling).
    seen: HashMap<String, Checked>,
}

impl<'a> Mapper<'a> {
    fn new(root: &'a ShareRoot, base: Vec<String>) -> Self {
        Self {
            root,
            base,
            seen: HashMap::new(),
        }
    }

    /// The share-relative path (in the stored spelling) and the entry for `rest`, the part of an
    /// index URL below the searched folder; `None` when any step is refused.
    fn map(&mut self, rest: &str) -> Option<(String, ListedEntry)> {
        let segments: Vec<&str> = rest.split('/').collect();
        let mut path = self.base.join("/");
        let mut shown = path.clone();
        let mut dir: Vec<&str> = self.base.iter().map(String::as_str).collect();
        let last = segments.len() - 1;
        for (i, seg) in segments.iter().enumerate() {
            // An empty segment (`a//b`, a trailing slash) is never a real path; the resolver
            // would refuse it too, this just avoids the work.
            if seg.is_empty() {
                return None;
            }
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(seg);
            let checked = match self.seen.get(&path) {
                Some(c) => c.clone(),
                None => {
                    let c = self.check(&path, &dir);
                    self.seen.insert(path.clone(), c.clone());
                    c
                }
            };
            let (stored, entry) = checked?;
            if !shown.is_empty() {
                shown.push('/');
            }
            shown.push_str(&stored);
            if i == last {
                return Some((shown, entry));
            }
            if entry.kind != EntryKind::Dir {
                return None;
            }
            dir.push(seg);
        }
        None
    }

    /// Resolve `path` (steps 1 to 8 of the path resolver), stat it through its handle and apply
    /// the listing filter for its parent directory `dir`.
    fn check(&self, path: &str, dir: &[&str]) -> Checked {
        let resolved = self.root.resolve(path.as_bytes()).ok()?;
        let entry = resolved.stat().ok()?;
        let filter = ListFilter {
            show_hidden: self.root.show_hidden(),
            hidden: self.root.hidden_patterns(),
            dir,
        };
        filter
            .keeps(&entry.name, &entry.attrs)
            .then(|| (entry.name.clone(), entry))
    }
}

// ---------------------------------------------------------------------------------------------
// The search

/// One search request, after the handler parsed and authorised it.
pub(super) struct Query<'a> {
    pub api: &'a ApiState,
    pub who: &'a Requester,
    pub opened: &'a Opened,
    /// The text, trimmed and lower-cased.
    pub needle: &'a str,
    pub scope: SearchScope,
    pub limit: usize,
    pub cursor: Option<&'a IndexCursor>,
    pub deadline: Instant,
}

/// The index's answer, or `Ok(None)` when the walk has to answer instead.
pub(super) fn search(q: &Query<'_>) -> Result<Option<SearchResponse>, ApiError> {
    let Some(backend) = q.api.index.as_deref() else {
        return Ok(None);
    };
    if check_needle(q.needle).is_err() {
        return Ok(None);
    }
    // The folder's real path, from the handle the resolver opened.
    let Ok(dir_path) = q.opened.resolved.final_path() else {
        return Ok(None);
    };
    let Some(dir_text) = dir_path.to_str() else {
        return Ok(None);
    };
    let Ok(dir) = IndexScope::for_dir(dir_text) else {
        return Ok(None);
    };

    let path = q.opened.resolved.path().to_protocol_string();
    let tag = cursor_tag(&q.opened.share.id, &path, q.needle, q.scope);
    let skip = match q.cursor {
        Some(c) if c.tag != tag => return Err(bad_cursor()),
        Some(c) => c.offset,
        None => 0,
    };
    if skip >= MAX_OFFSET {
        return Ok(Some(SearchResponse {
            entries: Vec::new(),
            partial: true,
            next_cursor: None,
            via: Some(SearchVia::Index),
        }));
    }
    let Ok(sql) = build_sql(&dir, q.needle, q.scope, skip + MAX_SCAN_ROWS + 1) else {
        return Ok(None);
    };

    // A name search checks the stored name itself: `LIKE` is only the prefilter (it over-matches
    // `_` in older dialects and a renamed file may still be in the index under its old name). A
    // content search trusts the index for the text, and still filters by name when there was no
    // searchable word and the statement fell back to names only.
    let check_name = match q.scope {
        SearchScope::Name => true,
        SearchScope::Content => contains_expression(q.needle).is_none(),
    };
    let base: Vec<String> = q.opened.resolved.path().segments().to_vec();
    let mut mapper = Mapper::new(&q.opened.root, base);
    let mut hits: Vec<(String, ListedEntry)> = Vec::new();
    let mut scanned = 0u64;
    let mut out_of_budget = false;
    let mut visit = |url: &str| -> ControlFlow<()> {
        if hits.len() >= q.limit {
            // One row past the page: there is more.
            return ControlFlow::Break(());
        }
        if scanned >= MAX_SCAN_ROWS || (scanned > 0 && Instant::now() >= q.deadline) {
            out_of_budget = true;
            return ControlFlow::Break(());
        }
        scanned += 1;
        let Some(rest) = relative_to(url, &dir) else {
            return ControlFlow::Continue(());
        };
        let Some((shown, entry)) = mapper.map(rest) else {
            return ControlFlow::Continue(());
        };
        if check_name && !entry.name.to_lowercase().contains(q.needle) {
            return ControlFlow::Continue(());
        }
        hits.push((shown, entry));
        ControlFlow::Continue(())
    };
    let run = match backend.run(&dir, &sql, skip, &mut visit) {
        Ok(Some(run)) => run,
        Ok(None) => return Ok(None),
        Err(e) => {
            tracing::debug!(error = %e, "Windows Search is not usable, walking instead");
            return Ok(None);
        }
    };

    let next_offset = skip + run.consumed;
    let more = run.stopped;
    let too_deep = more && next_offset >= MAX_OFFSET;
    let partial = out_of_budget || too_deep || run.catching_up;
    let next_cursor = (more && !too_deep).then(|| format!("{CURSOR_PREFIX}{tag}.{next_offset}"));
    let entries = q.api.with_decorator(q.who, &q.opened.share, |d| {
        hits.iter()
            .map(|(p, e)| {
                let mut entry = d.entry(e);
                entry.path = Some(p.clone());
                entry
            })
            .collect()
    });
    Ok(Some(SearchResponse {
        entries,
        partial,
        next_cursor,
        via: Some(SearchVia::Index),
    }))
}

// ---------------------------------------------------------------------------------------------
// The capability

/// The folder a share's root is, in the plain `C:\...` form the index is asked about.
fn root_scope(root: &ShareRoot) -> Option<IndexScope> {
    let canonical = root.canonical_root();
    let text = canonical.to_str()?;
    IndexScope::for_dir(text.strip_prefix(r"\\?\").unwrap_or(text)).ok()
}

impl ApiState {
    /// What `search` this share offers: [`SearchCapability::Indexed`] when its root is in the
    /// index's crawl scope, else the bounded walk. Cached for [`STATUS_TTL`]. Blocking (COM):
    /// call from the blocking pool.
    pub(super) fn share_search(
        &self,
        share: &gander_core::config::ShareConfig,
    ) -> SearchCapability {
        let Some(backend) = self.index.as_deref() else {
            return SearchCapability::Substring;
        };
        let key = RootKey::of(share);
        if let Some((k, at, indexed)) = self
            .index_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&share.id)
            && *k == key
            && at.elapsed() < STATUS_TTL
        {
            return capability(*indexed);
        }
        let indexed = match self.root_of(share).ok().as_deref().and_then(root_scope) {
            Some(scope) => match backend.status(&scope) {
                Ok(s) => s.crawled,
                Err(e) => {
                    tracing::debug!(error = %e, share = %share.id, "Windows Search status unavailable");
                    false
                }
            },
            None => false,
        };
        self.index_status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(share.id.clone(), (key, Instant::now(), indexed));
        capability(indexed)
    }
}

fn capability(indexed: bool) -> SearchCapability {
    if indexed {
        SearchCapability::Indexed
    } else {
        SearchCapability::Substring
    }
}

/// The error for a `scope` value that is neither `name` nor `content`.
pub(super) fn bad_scope() -> ApiError {
    ApiError::new(ErrorCode::BadRequest, "scope must be name or content.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trip() {
        let tag = cursor_tag(&ShareId::from("shr_1"), "a/b", "needle", SearchScope::Name);
        assert_eq!(tag.len(), 16);
        let c = parse_cursor(&format!("i1.{tag}.1234")).unwrap();
        assert_eq!((c.tag.as_str(), c.offset), (tag.as_str(), 1234));
        assert!(is_index_cursor("i1.x"));
        assert!(!is_index_cursor("s1.a.1"));
    }

    #[test]
    fn cursor_tag_binds_every_part() {
        let s = ShareId::from("shr_1");
        let base = cursor_tag(&s, "a", "n", SearchScope::Name);
        assert_ne!(
            base,
            cursor_tag(&ShareId::from("shr_2"), "a", "n", SearchScope::Name)
        );
        assert_ne!(base, cursor_tag(&s, "b", "n", SearchScope::Name));
        assert_ne!(base, cursor_tag(&s, "a", "m", SearchScope::Name));
        assert_ne!(base, cursor_tag(&s, "a", "n", SearchScope::Content));
        // Parts cannot be shifted into each other.
        assert_ne!(
            cursor_tag(&s, "ab", "c", SearchScope::Name),
            cursor_tag(&s, "a", "bc", SearchScope::Name)
        );
    }

    #[test]
    fn bad_cursors() {
        for bad in [
            "",
            "i1.",
            "i1.0123456789abcdef",
            "i1.0123456789abcdef.",
            "i1.0123456789abcdef.x",
            "i1.0123456789abcdef.-1",
            "i1.0123456789abcdef.1.2",
            "i1.0123456789abcde.1",
            "i1.0123456789abcdeg.1",
            "i1.0123456789abcdef.1234567890123",
            "s1.0123456789abcdef.1",
        ] {
            assert!(parse_cursor(bad).is_err(), "{bad:?}");
        }
    }
}
