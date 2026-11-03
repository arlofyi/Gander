// SPDX-License-Identifier: Apache-2.0
//! `GET /shares`, `/list`, `/stat`, `/search` and `/recent`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use axum::Extension;
use axum::Json;
use axum::extract::{Path, RawQuery as AxRawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gander_core::error::ErrorCode;
use gander_core::path::ListedEntry;
use gander_core::protocol::files::{
    EntryKind, EntryMeta, ImageMeta, ListQuery, ListResponse, OfficeMeta, PdfMeta, Permission,
    PreviewKind, RecentResponse, SearchResponse, SearchVia, ShareInfo, SharesResponse, SortKey,
    SortOrder, StreamDecision,
};
use gander_core::protocol::{ShareId, Timestamp};
use gander_core::search::SearchScope;
use gander_win::path::{Enumerated, ListError, ShareRoot};
use gander_win::preview::service::InfoRequest;

use super::indexed;
use super::query::RawQuery;
use super::{ApiState, Deco, Requester, bad_request, blocking, internal, none_match};
use crate::server::{ApiError, Caller, ConnInfo};

pub(super) fn source(conn: &Option<Extension<ConnInfo>>) -> Option<std::net::IpAddr> {
    conn.as_ref().map(|Extension(c)| c.peer.ip())
}

fn list_error(e: ListError) -> ApiError {
    match e {
        ListError::NotADirectory => {
            ApiError::new(ErrorCode::BadPath, "This is a file, not a folder.")
        }
        ListError::Cursor(_) => bad_request("The cursor is not valid for this listing."),
        ListError::Io(e) => {
            tracing::warn!(error = %e, "directory enumeration failed");
            internal()
        }
    }
}

/// Joins a share-relative directory path and a child name for the `path` field (never used to
/// open anything: every open goes through the resolver).
fn child_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

// ---------------------------------------------------------------------------------------------
// /shares

pub(super) async fn shares(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
) -> Result<Json<SharesResponse>, ApiError> {
    let who = Requester::new(&caller, None)?;
    // Asking the indexer about each share root is COM work (cached for 30 s): blocking pool.
    blocking(move || {
        let config = api.config();
        let shares = config
            .shares_for(&who.device)
            .map(|s| ShareInfo {
                id: s.id.clone(),
                name: s.name.clone(),
                path_display: s.show_path.then(|| s.root.display().to_string()),
                permissions: s.permissions_for(&who.device).to_vec(),
                show_hidden: s.show_hidden,
                stream_default: s.stream_default,
                search: Some(api.share_search(s)),
            })
            .collect();
        Ok(Json(SharesResponse { shares }))
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// /list

fn parse_sort(v: Option<&str>) -> Result<SortKey, ApiError> {
    Ok(match v {
        None | Some("name") => SortKey::Name,
        Some("mtime") => SortKey::Mtime,
        Some("size") => SortKey::Size,
        Some("kind") => SortKey::Kind,
        Some(_) => return Err(bad_request("sort must be name, mtime, size or kind.")),
    })
}

fn parse_order(v: Option<&str>) -> Result<SortOrder, ApiError> {
    Ok(match v {
        None | Some("asc") => SortOrder::Asc,
        Some("desc") => SortOrder::Desc,
        Some(_) => return Err(bad_request("order must be asc or desc.")),
    })
}

fn list_query(q: &RawQuery) -> Result<ListQuery, ApiError> {
    let kinds = q.non_empty("kinds")?.map(str::to_owned);
    if let Some(k) = &kinds
        && k.split(',')
            .any(|v| !v.is_empty() && v != "file" && v != "dir")
    {
        return Err(bad_request("kinds must be a list of file and dir."));
    }
    Ok(ListQuery {
        path: String::new(),
        cursor: q.non_empty("cursor")?.map(str::to_owned),
        limit: q.number("limit")?,
        sort: parse_sort(q.non_empty("sort")?)?,
        order: parse_order(q.non_empty("order")?)?,
        kinds,
        ext: q.non_empty("ext")?.map(str::to_owned),
    })
}

fn preview_byte(p: PreviewKind) -> u8 {
    match p {
        PreviewKind::Image => 1,
        PreviewKind::Pdf => 2,
        PreviewKind::Text => 3,
        PreviewKind::Office => 4,
        PreviewKind::VideoPoster => 5,
        PreviewKind::None => 0,
    }
}

fn stream_byte(s: StreamDecision) -> u8 {
    match s {
        StreamDecision::Allowed => 1,
        StreamDecision::Refused => 2,
        StreamDecision::Ask => 3,
    }
}

fn sorted_list(v: Option<&str>) -> String {
    let mut parts: Vec<String> = v
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    parts.sort();
    parts.dedup();
    parts.join(",")
}

/// The list ETag: see the module docs of [`super`] ("List ETag and cursor contract"). The
/// per-entry hashes are summed, so the value does not depend on enumeration order.
pub(super) fn list_etag(
    path: &str,
    q: &ListQuery,
    all: &Enumerated,
    mut deco: impl FnMut(&ListedEntry) -> Deco,
) -> String {
    let mut acc: u128 = 0;
    for e in &all.entries {
        let d = deco(e);
        let a = e.attrs;
        let bits = u8::from(a.hidden)
            | u8::from(a.readonly) << 1
            | u8::from(a.system) << 2
            | u8::from(a.cloud_only) << 3
            | u8::from(a.link) << 4
            | u8::from(a.encrypted) << 5;
        let mut h = blake3::Hasher::new();
        h.update(&(e.name.len() as u64).to_le_bytes());
        h.update(e.name.as_bytes());
        h.update(&[
            u8::from(e.kind == EntryKind::Dir),
            bits,
            u8::from(d.thumb),
            preview_byte(d.preview),
            stream_byte(d.stream),
        ]);
        h.update(d.mime.unwrap_or("").as_bytes());
        h.update(&[0]);
        h.update(&e.size.to_le_bytes());
        h.update(&e.mtime.unix_timestamp_nanos().to_le_bytes());
        h.update(
            &e.ctime
                .map_or(i128::MIN, |t| t.unix_timestamp_nanos())
                .to_le_bytes(),
        );
        let digest = h.finalize();
        let mut first = [0u8; 16];
        first.copy_from_slice(&digest.as_bytes()[..16]);
        acc = acc.wrapping_add(u128::from_le_bytes(first));
    }
    let mut h = blake3::Hasher::new();
    h.update(b"gander-list-v1\0");
    h.update(path.as_bytes());
    h.update(&[0]);
    h.update(format!("{:?}|{:?}|", q.sort, q.order).as_bytes());
    h.update(sorted_list(q.kinds.as_deref()).as_bytes());
    h.update(b"|");
    h.update(sorted_list(q.ext.as_deref()).as_bytes());
    h.update(b"|");
    h.update(&(all.entries.len() as u64).to_le_bytes());
    h.update(&[u8::from(all.truncated)]);
    h.update(&all.unlistable.to_le_bytes());
    h.update(&acc.to_le_bytes());
    let hex = h.finalize().to_hex();
    format!("W/\"{}\"", &hex.as_str()[..32])
}

pub(super) async fn list(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let query = list_query(&q)?;
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let share = ShareId::from(share.as_str());
    let opened = blocking({
        let api = Arc::clone(&api);
        let who = who.clone();
        move || api.open(&who, &share, Permission::List, q.path(), "list")
    })
    .await?;
    // Arm, then list: the directory watch is open before the enumeration starts, so a change
    // made while the listing is read is still reported as `share.changed`.
    if let Some(events) = api.events()
        && opened.resolved.kind() == EntryKind::Dir
    {
        events
            .touch_armed(
                &opened.share.id,
                &opened.resolved.path().to_protocol_string(),
            )
            .await;
    }
    blocking(move || {
        let all = opened
            .root
            .enumerate(&opened.resolved)
            .map_err(list_error)?;
        let path = opened.resolved.path().to_protocol_string();
        api.with_decorator(&who, &opened.share, |d| {
            let etag = list_etag(&path, &query, &all, |e| d.deco(e));
            let etag_value = HeaderValue::from_str(&etag).map_err(|_| internal())?;
            if inm.as_deref().is_some_and(|v| none_match(v, &etag)) {
                return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag_value)]).into_response());
            }
            let truncated = all.truncated;
            let unlistable = all.unlistable;
            let page = gander_core::path::page(all.entries, &query, api.options.list_max)
                .map_err(|_| bad_request("The cursor is not valid for this listing."))?;
            let entries = page.entries.iter().map(|e| d.entry(e)).collect();
            let body = ListResponse {
                path,
                entries,
                next_cursor: page.next_cursor,
                truncated,
                unlistable,
                etag,
            };
            Ok((
                [
                    (header::ETAG, etag_value),
                    (
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("private, no-cache"),
                    ),
                ],
                Json(body),
            )
                .into_response())
        })
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// /stat

fn worker_meta(info: &gander_core::protocol::files::PreviewInfo) -> Option<EntryMeta> {
    match info.kind {
        PreviewKind::Image => match (info.width, info.height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => {
                Some(EntryMeta::Image(ImageMeta {
                    width,
                    height,
                    orientation: None,
                    taken_at: None,
                    camera: None,
                }))
            }
            _ => None,
        },
        PreviewKind::Pdf => info.pages.map(|pages| EntryMeta::Pdf(PdfMeta { pages })),
        PreviewKind::Office => Some(EntryMeta::Office(OfficeMeta { pages: info.pages })),
        _ => None,
    }
}

pub(super) async fn stat(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Json<gander_core::protocol::files::StatResponse>, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let want_meta = match q.non_empty("meta")? {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => return Err(bad_request("meta must be 0 or 1.")),
    };
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let opened = api.open(&who, &share, Permission::List, q.path(), "stat")?;
        let listed = opened.resolved.stat().map_err(|e| {
            tracing::warn!(error = %e, "stat failed");
            internal()
        })?;
        let mut entry = api.with_decorator(&who, &opened.share, |d| d.entry(&listed));
        if want_meta && listed.kind == EntryKind::File && !listed.attrs.link {
            let ext = listed.ext();
            if let Ok(file) = opened.resolved.try_clone_file() {
                entry.meta = super::meta::sniff(&file, ext.as_deref());
            }
            // Formats the header sniff does not cover: the worker's `Info` op (WP-16), which
            // spends preview resources, so it needs the `preview` permission.
            if entry.meta.is_none()
                && let Some(previews) = &api.previews
                && opened.share.allows(&who.device, Permission::Preview)
                && let Some(ext) = ext
                && let Ok(file) = opened.resolved.try_clone_file()
                && let Ok(info) = previews.info(InfoRequest {
                    file,
                    ext,
                    device: who.device.to_string(),
                })
            {
                entry.meta = worker_meta(&info);
            }
        }
        Ok(Json(entry))
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// /search

/// Directories a suspended search keeps queued at most; more are dropped (`partial`).
const MAX_SEARCH_QUEUE: usize = 100_000;
/// Suspended searches kept at once.
const MAX_SEARCH_SESSIONS: usize = 64;

/// A breadth-first search walk that can be continued with a cursor.
#[derive(Debug)]
pub(super) struct SearchSession {
    device: gander_core::protocol::DeviceId,
    share: ShareId,
    needle: String,
    path: String,
    /// Directories still to enumerate (share-relative, in BFS order).
    queue: VecDeque<String>,
    /// Matches found but not yet returned.
    pending: VecDeque<(String, ListedEntry)>,
    /// Matches returned so far (the cursor's fallback position).
    emitted: u64,
    /// Matches still to drop when re-walking for an expired cursor.
    skip: u64,
    /// Directories were dropped because the queue was full.
    dropped: bool,
    at: Instant,
}

struct Cursor {
    id: String,
    skip: u64,
}

fn parse_search_cursor(s: &str) -> Result<Cursor, ApiError> {
    let bad = || bad_request("The cursor is not valid for this search.");
    let rest = s.strip_prefix("s1.").ok_or_else(bad)?;
    let (id, skip) = rest.split_once('.').ok_or_else(bad)?;
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(bad());
    }
    Ok(Cursor {
        id: id.to_owned(),
        skip: skip.parse().map_err(|_| bad())?,
    })
}

/// Continues `s` until `limit` matches, the end of the walk, or the deadline. Returns the
/// matches and whether the time bound stopped it. At least one directory is processed per call,
/// so a cursor always makes progress.
fn search_walk(
    root: &ShareRoot,
    s: &mut SearchSession,
    limit: usize,
    deadline: Instant,
) -> (Vec<(String, ListedEntry)>, bool) {
    let mut out = Vec::new();
    let mut first = true;
    loop {
        while out.len() < limit {
            match s.pending.pop_front() {
                Some(m) => out.push(m),
                None => break,
            }
        }
        if out.len() >= limit {
            return (out, false);
        }
        let Some(dir) = s.queue.pop_front() else {
            return (out, false);
        };
        if !first && Instant::now() >= deadline {
            s.queue.push_front(dir);
            return (out, true);
        }
        first = false;
        // A directory that vanished or is refused is skipped; the root was checked up front.
        let Ok(resolved) = root.resolve(dir.as_bytes()) else {
            continue;
        };
        let Ok(mut all) = root.enumerate(&resolved) else {
            continue;
        };
        all.entries.sort_by(|a, b| a.name.cmp(&b.name));
        for e in all.entries {
            let p = child_path(&dir, &e.name);
            if e.kind == EntryKind::Dir && !e.attrs.link {
                if s.queue.len() < MAX_SEARCH_QUEUE {
                    s.queue.push_back(p.clone());
                } else {
                    s.dropped = true;
                }
            }
            if e.name.to_lowercase().contains(&s.needle) {
                if s.skip > 0 {
                    s.skip -= 1;
                } else {
                    s.pending.push_back((p, e));
                }
            }
        }
    }
}

pub(super) async fn search(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Json<SearchResponse>, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let needle = q
        .str("q")?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad_request("q must not be empty."))?
        .to_lowercase();
    let limit = q
        .number::<u32>("limit")?
        .unwrap_or(100)
        .clamp(1, api.options.search_max.max(1)) as usize;
    // `scope` (WP-66): what `q` is matched against. Only the index can read file text; the walk
    // always matches names, and `via` in the answer says which one ran.
    let scope = match q.non_empty("scope")? {
        None => SearchScope::Name,
        Some(v) => SearchScope::parse(v).ok_or_else(indexed::bad_scope)?,
    };
    // An `i1.` cursor continues an index search, an `s1.` cursor a walk.
    let raw_cursor = q.non_empty("cursor")?;
    let index_cursor = raw_cursor
        .filter(|c| indexed::is_index_cursor(c))
        .map(indexed::parse_cursor)
        .transpose()?;
    let cursor = raw_cursor
        .filter(|c| !indexed::is_index_cursor(c))
        .map(parse_search_cursor)
        .transpose()?;
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let started = Instant::now();
        let opened = api.open(&who, &share, Permission::List, q.path(), "search")?;
        if opened.resolved.kind() != EntryKind::Dir {
            return Err(ApiError::new(
                ErrorCode::BadPath,
                "This is a file, not a folder.",
            ));
        }
        let deadline = started + api.options.search_budget;
        // A walk in progress (`s1.` cursor) stays a walk. Otherwise the index answers when the
        // folder is crawled and everything works; if not, the walk starts (over, for an index
        // cursor whose index is gone: clients merge results by path).
        if cursor.is_none()
            && let Some(answer) = indexed::search(&indexed::Query {
                api: &api,
                who: &who,
                opened: &opened,
                needle: &needle,
                scope,
                limit,
                cursor: index_cursor.as_ref(),
                deadline,
            })?
        {
            return Ok(Json(answer));
        }
        let path = opened.resolved.path().to_protocol_string();
        let fresh = |skip: u64| SearchSession {
            device: who.device.clone(),
            share: share.clone(),
            needle: needle.clone(),
            path: path.clone(),
            queue: VecDeque::from([path.clone()]),
            pending: VecDeque::new(),
            emitted: skip,
            skip,
            dropped: false,
            at: Instant::now(),
        };
        let (id, mut session) = match &cursor {
            None => (ulid::Ulid::new().to_string(), fresh(0)),
            Some(c) => {
                let found = api
                    .searches
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&c.id);
                match found {
                    Some(s)
                        if s.device == who.device
                            && s.share == share
                            && s.needle == needle
                            && s.path == path
                            && s.emitted == c.skip =>
                    {
                        (c.id.clone(), s)
                    }
                    Some(s) if s.device != who.device => {
                        // Someone else's cursor: put it back and start over.
                        api.searches
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(c.id.clone(), s);
                        (ulid::Ulid::new().to_string(), fresh(c.skip))
                    }
                    // Expired, replayed or for another query: re-walk and skip what was sent.
                    _ => (c.id.clone(), fresh(c.skip)),
                }
            }
        };
        let (found, timed_out) = search_walk(&opened.root, &mut session, limit, deadline);
        session.emitted += found.len() as u64;
        let more = !session.pending.is_empty() || !session.queue.is_empty();
        let partial = timed_out || session.dropped;
        let next_cursor = more.then(|| format!("s1.{id}.{}", session.emitted));
        let entries = api.with_decorator(&who, &opened.share, |d| {
            found
                .iter()
                .map(|(p, e)| {
                    let mut entry = d.entry(e);
                    entry.path = Some(p.clone());
                    entry
                })
                .collect()
        });
        if more {
            session.at = Instant::now();
            let ttl = api.options.search_session_ttl;
            let mut sessions = api.searches.lock().unwrap_or_else(|e| e.into_inner());
            sessions.retain(|_, s| s.at.elapsed() < ttl);
            if sessions.len() >= MAX_SEARCH_SESSIONS
                && let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, s)| s.at)
                    .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest);
            }
            sessions.insert(id, session);
        }
        Ok(Json(SearchResponse {
            entries,
            partial,
            next_cursor,
            via: Some(SearchVia::Walk),
        }))
    })
    .await
}

// ---------------------------------------------------------------------------------------------
// /recent

/// One share's bounded walk for `/recent`, reused for `recent_ttl`.
#[derive(Debug)]
pub(super) struct RecentWalk {
    at: Instant,
    /// Files found, with their share-relative paths.
    files: Vec<(String, ListedEntry)>,
    /// The walk did not cover the whole share.
    partial: bool,
}

/// Upper bound on one share's walk, whatever the entry count.
const RECENT_WALK_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

fn recent_walk(root: &ShareRoot, levels: usize, max_entries: usize) -> RecentWalk {
    let started = Instant::now();
    let mut files = Vec::new();
    let mut partial = false;
    let mut seen = 0usize;
    let mut queue = VecDeque::from([(String::new(), 1usize)]);
    'walk: while let Some((dir, level)) = queue.pop_front() {
        if started.elapsed() > RECENT_WALK_BUDGET {
            partial = true;
            break;
        }
        let Ok(resolved) = root.resolve(dir.as_bytes()) else {
            continue;
        };
        let Ok(all) = root.enumerate(&resolved) else {
            continue;
        };
        if all.truncated {
            partial = true;
        }
        for e in all.entries {
            seen += 1;
            if seen > max_entries {
                partial = true;
                break 'walk;
            }
            let p = child_path(&dir, &e.name);
            match e.kind {
                EntryKind::File => files.push((p, e)),
                EntryKind::Dir if e.attrs.link => {}
                EntryKind::Dir if level < levels => queue.push_back((p, level + 1)),
                EntryKind::Dir => partial = true,
            }
        }
    }
    RecentWalk {
        at: Instant::now(),
        files,
        partial,
    }
}

pub(super) async fn recent(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Json<RecentResponse>, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let limit = q
        .number::<u32>("limit")?
        .unwrap_or(50)
        .clamp(1, api.options.search_max.max(1)) as usize;
    let since = q
        .non_empty("since")?
        .map(|s| {
            Timestamp::parse(s, &time::format_description::well_known::Rfc3339)
                .map_err(|_| bad_request("since must be an RFC 3339 date-time."))
        })
        .transpose()?;
    blocking(move || {
        let config = api.config();
        let mut partial = false;
        let mut candidates = Vec::new();
        for share in config
            .shares_for(&who.device)
            .filter(|s| s.allows(&who.device, Permission::List))
        {
            let cached = api
                .recent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&share.id)
                .filter(|w| w.at.elapsed() < api.options.recent_ttl)
                .cloned();
            let walk = match cached {
                Some(w) => w,
                None => {
                    let Ok(root) = api.root_of(share) else {
                        partial = true;
                        continue;
                    };
                    let w = Arc::new(recent_walk(
                        &root,
                        api.options.recent_levels,
                        api.options.recent_max_entries,
                    ));
                    api.recent
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(share.id.clone(), Arc::clone(&w));
                    w
                }
            };
            partial |= walk.partial;
            for (i, (_, e)) in walk.files.iter().enumerate() {
                if since.is_none_or(|t| e.mtime > t) {
                    candidates.push((e.mtime, share, Arc::clone(&walk), i));
                }
            }
        }
        candidates.sort_by_key(|c| std::cmp::Reverse(c.0));
        candidates.truncate(limit);
        let mut entries = Vec::with_capacity(candidates.len());
        for (_, share, walk, i) in candidates {
            let (path, e) = &walk.files[i];
            let mut entry = api.with_decorator(&who, share, |d| d.entry(e));
            entry.share = Some(share.id.clone());
            entry.path = Some(path.clone());
            entries.push(entry);
        }
        Ok(Json(RecentResponse { entries, partial }))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_cursor_format() {
        let c = parse_search_cursor("s1.01ABC.42").unwrap();
        assert_eq!((c.id.as_str(), c.skip), ("01ABC", 42));
        for bad in ["", "s1.", "s1.x", "s2.a.1", "s1.a.b", "s1.a/b.1"] {
            assert!(parse_search_cursor(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sorted_filter_lists() {
        assert_eq!(sorted_list(Some("PDF,,pptx,pdf")), "pdf,pptx");
        assert_eq!(sorted_list(None), "");
    }
}
