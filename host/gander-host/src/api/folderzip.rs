// SPDX-License-Identifier: Apache-2.0
//! `GET /v1/zip?share=&path=[&dry_run=1]`: a folder as one streamed ZIP (WP-68).
//!
//! The phone asks for this explicitly (the client confirms the size and count first, using
//! `dry_run=1`). The host:
//!
//! 1. checks the `fetch` permission and resolves the folder like every other request;
//! 2. walks the tree with the path resolver (every directory is resolved from the share root by
//!    protocol path, segment by segment; nothing is joined onto a root and opened). Listings'
//!    rules apply exactly: hidden and system files unless the share shows them, always-omitted
//!    names, `hidden_patterns`. Links and junctions the resolver refuses are NOT followed; they
//!    and unsendable names go to `_gander-skipped.txt` at the end of the archive;
//! 3. stops the walk as soon as a limit is crossed (`[zip]` in `config.toml`: 4 GiB and 20,000
//!    entries by default, plus a time budget) and answers `413 too_large` BEFORE streaming;
//! 4. streams the archive from a blocking task through a bounded channel: STORED entries, ZIP64
//!    when needed ([`gander_core::zipstream`]), the folder as the top-level directory, no
//!    `Content-Length` (chunked), no `Range` or resume. The task ends, and the open file is
//!    closed, when the client disconnects (the body is dropped).
//!
//! Each file is opened by the resolver when its turn comes and read through that handle, up to the
//! size measured by the walk (a file that grew is cut there; one that shrank while being read ends
//! the download with a broken connection, as `/file` does). One audit `fetch` event with
//! `kind: "zip"` is written when the download ends: the folder path, the entry count and the
//! bytes sent. File names are never logged.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{RawQuery as AxRawQuery, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gander_core::error::ErrorCode;
use gander_core::path::{RejectReason, ResolveError, ValidatedPath};
use gander_core::protocol::files::{EntryKind, Permission, ZipPlan};
use gander_core::protocol::{ShareId, Timestamp};
use gander_core::zipstream::ZipStream;
use gander_win::path::{Resolved, ShareRoot};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use super::browse::source;
use super::fetch::content_disposition;
use super::query::RawQuery;
use super::{ApiState, Requester, bad_request, blocking, internal};
use crate::audit::Outcome;
use crate::server::{ApiError, Caller, ConnInfo};

/// Bytes gathered before a chunk is handed to the response body.
const CHUNK: usize = 256 * 1024;

/// Skipped items named in `_gander-skipped.txt`; beyond this they are only counted.
const MAX_NOTED: usize = 1000;

/// The note listing what is not in the archive.
const NOTE_NAME: &str = "_gander-skipped.txt";

/// What the walk is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Limits {
    /// Largest total size of the files.
    pub max_bytes: u64,
    /// Most entries (files and folders below the requested one).
    pub max_entries: u64,
    /// Wall-clock time the walk may take.
    pub budget: Duration,
}

/// Which limit a walk crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Over {
    /// The files add up to more than `max_bytes`.
    Bytes,
    /// More than `max_entries` entries (or a directory too big to list whole).
    Entries,
    /// The walk used up its time budget.
    Time,
}

/// Why a walk did not produce a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WalkError {
    /// A limit was crossed; nothing is streamed.
    Over(Over),
    /// The client went away.
    Cancelled,
}

/// Why an item is not in the archive (the words of `_gander-skipped.txt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Skip {
    /// A link, junction or mount point (not followed).
    Link,
    /// The name cannot be sent (not representable, or refused by the path rules).
    Name,
    /// Access denied or in use.
    Unreadable,
    /// Gone by the time its turn came.
    Gone,
    /// Became a folder (or a file) while the zip was being made.
    Changed,
    /// Included only up to the size it had when measured.
    Grew,
}

impl Skip {
    fn text(self) -> &'static str {
        match self {
            Skip::Link => "a link, junction or mount point (not followed)",
            Skip::Name => "its name cannot be sent",
            Skip::Unreadable => "could not be read (access denied or in use)",
            Skip::Gone => "was removed while the zip was being made",
            Skip::Changed => "changed kind while the zip was being made",
            Skip::Grew => {
                "grew while the zip was being made (included only up to its size when measured)"
            }
        }
    }
}

/// The items the note will name: the first [`MAX_NOTED`], and how many there were in all.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Notes {
    /// Path below the top-level folder and why.
    pub list: Vec<(String, Skip)>,
    /// Every item, named or not.
    pub total: u64,
}

impl Notes {
    fn add(&mut self, rel: &str, why: Skip) {
        self.total += 1;
        if self.list.len() < MAX_NOTED {
            self.list.push((rel.to_owned(), why));
        }
    }
}

/// One entry the archive will hold.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Planned {
    /// Share-relative protocol path (handed to the resolver, never joined onto a root).
    pub path: String,
    /// Path below the top-level folder, `/` separated.
    pub rel: String,
    /// A folder.
    pub dir: bool,
    /// Size when measured (0 for a folder).
    pub size: u64,
    /// Last write time.
    pub mtime: Timestamp,
}

/// The outcome of the bounded walk.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Plan {
    /// Files and folders, each folder before what is in it.
    pub entries: Vec<Planned>,
    /// Items left out.
    pub skipped: Notes,
    /// Total size of the files.
    pub bytes: u64,
    /// Cloud placeholders among the files.
    pub cloud_only: u64,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn skip_for(e: &ResolveError) -> Skip {
    match e.reason() {
        Some(RejectReason::NotFound) => Skip::Gone,
        Some(RejectReason::ReparsePoint) => Skip::Link,
        Some(RejectReason::Forbidden) | None => Skip::Unreadable,
        Some(_) => Skip::Name,
    }
}

struct Walker<'a> {
    root: &'a ShareRoot,
    limits: Limits,
    cancel: &'a AtomicBool,
    deadline: Instant,
    plan: Plan,
}

impl Walker<'_> {
    fn push(&mut self, entry: Planned) -> Result<(), WalkError> {
        if self.plan.entries.len() as u64 >= self.limits.max_entries {
            return Err(WalkError::Over(Over::Entries));
        }
        self.plan.entries.push(entry);
        Ok(())
    }

    fn dir(&mut self, dir: &Resolved, dir_path: &str, rel: &str) -> Result<(), WalkError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(WalkError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(WalkError::Over(Over::Time));
        }
        let listing = match self.root.enumerate(dir) {
            Ok(l) => l,
            Err(_) => {
                self.plan.skipped.add(rel, Skip::Unreadable);
                return Ok(());
            }
        };
        if listing.truncated {
            // More than the listing's own cap: it cannot be sent whole.
            return Err(WalkError::Over(Over::Entries));
        }
        for _ in 0..listing.unlistable {
            self.plan
                .skipped
                .add(&join(rel, "(unreadable name)"), Skip::Name);
        }
        let mut entries = listing.entries;
        entries.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        for e in entries {
            let child_rel = join(rel, &e.name);
            let child_path = join(dir_path, &e.name);
            if e.attrs.link {
                self.plan.skipped.add(&child_rel, Skip::Link);
                continue;
            }
            match e.kind {
                EntryKind::Dir => match self.root.resolve(child_path.as_bytes()) {
                    Ok(child) if child.kind() == EntryKind::Dir => {
                        self.push(Planned {
                            path: child_path.clone(),
                            rel: child_rel.clone(),
                            dir: true,
                            size: 0,
                            mtime: e.mtime,
                        })?;
                        self.dir(&child, &child_path, &child_rel)?;
                    }
                    Ok(_) => self.plan.skipped.add(&child_rel, Skip::Changed),
                    Err(err) => self.plan.skipped.add(&child_rel, skip_for(&err)),
                },
                EntryKind::File => {
                    // The name rules are checked now so the totals are what will be sent; the
                    // resolver opens the file when its turn comes.
                    if ValidatedPath::parse_str(&child_path).is_err() {
                        self.plan.skipped.add(&child_rel, Skip::Name);
                        continue;
                    }
                    self.plan.bytes = self.plan.bytes.saturating_add(e.size);
                    if self.plan.bytes > self.limits.max_bytes {
                        return Err(WalkError::Over(Over::Bytes));
                    }
                    self.plan.cloud_only += u64::from(e.attrs.cloud_only);
                    self.push(Planned {
                        path: child_path,
                        rel: child_rel,
                        dir: false,
                        size: e.size,
                        mtime: e.mtime,
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// The bounded walk of `start` (the requested folder, at `start_path`).
pub(super) fn walk(
    root: &ShareRoot,
    start: &Resolved,
    start_path: &str,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Plan, WalkError> {
    let mut w = Walker {
        root,
        limits,
        cancel,
        deadline: Instant::now() + limits.budget,
        plan: Plan::default(),
    };
    w.dir(start, start_path, "")?;
    Ok(w.plan)
}

/// The top-level folder name of the archive: the folder's own name (the share's name for the
/// share root) with anything an extractor could read as a path separator or a reserved character
/// replaced.
pub(super) fn archive_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', ' ']);
    if trimmed.is_empty() {
        "folder".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The note's file name: `_gander-skipped.txt`, or a numbered variant when the folder already
/// holds a top-level item of that name.
fn note_name(plan: &Plan) -> String {
    let taken = |n: &str| {
        plan.entries
            .iter()
            .any(|e| !e.rel.contains('/') && e.rel.eq_ignore_ascii_case(n))
    };
    if !taken(NOTE_NAME) {
        return NOTE_NAME.to_owned();
    }
    (2..)
        .map(|i| format!("_gander-skipped ({i}).txt"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| NOTE_NAME.to_owned())
}

fn note_text(folder: &str, notes: &Notes) -> String {
    let mut s = format!(
        "These items of \"{folder}\" are not in this zip, or not all of them is.\r\n\
         Gander does not follow links or junctions and leaves out names it cannot send.\r\n\r\n"
    );
    for (path, why) in &notes.list {
        s.push_str(path);
        s.push('\t');
        s.push_str(why.text());
        s.push_str("\r\n");
    }
    let listed = notes.list.len() as u64;
    if notes.total > listed {
        s.push_str(&format!("... and {} more\r\n", notes.total - listed));
    }
    s
}

/// A [`Write`] that hands the archive to the response body in chunks of about [`CHUNK`] bytes.
/// A closed body (the client went away) or the cancel flag ends the writing with `BrokenPipe`.
struct ChannelWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
    cancel: Arc<AtomicBool>,
}

impl ChannelWriter {
    fn new(tx: mpsc::Sender<io::Result<Bytes>>, cancel: Arc<AtomicBool>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(CHUNK + 4096),
            cancel,
        }
    }

    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(
            &mut self.buf,
            Vec::with_capacity(CHUNK + 4096),
        ));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

/// What a finished (or interrupted) write did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Written {
    /// Files and folders written (not the top-level folder or the note).
    pub entries: u64,
    /// File bytes written.
    pub bytes: u64,
}

/// Opens the planned file through the resolver. `Err` carries why it is not available and, for a
/// resolver rejection, the reason.
fn open_planned(
    root: &ShareRoot,
    path: &str,
) -> Result<(std::fs::File, u64), (Skip, Option<RejectReason>)> {
    match root.resolve(path.as_bytes()) {
        Ok(r) if r.kind() == EntryKind::File => {
            let file = r.into_file();
            match file.metadata() {
                Ok(m) => Ok((file, m.len())),
                Err(_) => Err((Skip::Unreadable, None)),
            }
        }
        Ok(_) => Err((Skip::Changed, None)),
        Err(e) => Err((skip_for(&e), e.reason())),
    }
}

/// Everything [`ZipJob::write`] needs besides the output.
pub(super) struct ZipJob<'a> {
    pub root: &'a ShareRoot,
    pub plan: &'a Plan,
    /// The top-level folder name inside the archive.
    pub folder: &'a str,
    /// The folder's own modification time.
    pub folder_mtime: Timestamp,
    /// The offset DOS timestamps are written in.
    pub local: time::UtcOffset,
    pub cancel: &'a AtomicBool,
    /// Hears a resolver step-7 escape (a bug) with the item's path.
    pub on_bug: &'a dyn Fn(&str, RejectReason),
}

impl ZipJob<'_> {
    /// Writes the archive into `out`; `written` is up to date even when this fails.
    pub(super) fn write<W: Write>(&self, out: W, written: &mut Written) -> io::Result<W> {
        let at = |t: Timestamp| t.to_offset(self.local);
        let folder = self.folder;
        let mut z = ZipStream::new(out);
        z.add_dir(folder, at(self.folder_mtime))?;
        let mut notes = self.plan.skipped.clone();
        for p in &self.plan.entries {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            let name = format!("{folder}/{}", p.rel);
            if p.dir {
                z.add_dir(&name, at(p.mtime))?;
                written.entries += 1;
                continue;
            }
            match open_planned(self.root, &p.path) {
                Ok((mut file, now)) => {
                    let len = now.min(p.size);
                    if now > p.size {
                        notes.add(&p.rel, Skip::Grew);
                    }
                    z.add_file(&name, at(p.mtime), len, &mut file)?;
                    written.entries += 1;
                    written.bytes += len;
                }
                Err((why, reason)) => {
                    if let Some(r) = reason
                        && r.is_bug()
                    {
                        (self.on_bug)(&p.path, r);
                    }
                    notes.add(&p.rel, why);
                }
            }
        }
        if notes.total > 0 {
            let text = note_text(folder, &notes);
            z.add_bytes(
                &format!("{folder}/{}", note_name(self.plan)),
                at(Timestamp::now_utc()),
                text.as_bytes(),
            )?;
        }
        z.finish()
    }
}

fn human_bytes(n: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if n >= GIB && n.is_multiple_of(GIB) {
        format!("{} GiB", n / GIB)
    } else if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else {
        format!("{} MiB", n.div_ceil(MIB))
    }
}

fn too_large(over: Over, limits: &Limits) -> ApiError {
    let message = match over {
        Over::Time => "This folder took too long to measure for a zip download.".to_owned(),
        Over::Bytes | Over::Entries => format!(
            "This folder is over the limit for a zip download ({} and {} items at most).",
            human_bytes(limits.max_bytes),
            limits.max_entries
        ),
    };
    ApiError::new(ErrorCode::TooLarge, message)
}

/// Sets the flag when dropped: the walk or the writer stops when the request is gone.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// What the blocking part of a request found.
struct Prepared {
    share: ShareId,
    root: Arc<ShareRoot>,
    /// The folder's share-relative path.
    path: String,
    /// The top-level folder name (before cleaning).
    name: String,
    mtime: Timestamp,
    plan: Plan,
}

fn hv(s: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(s).map_err(|_| internal())
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v, Some("1" | "true"))
}

pub(super) async fn zip(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    method: Method,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let Some(share) = q.non_empty("share")?.map(ShareId::from) else {
        return Err(bad_request("The share parameter is required."));
    };
    let dry_run = truthy(q.non_empty("dry_run")?);
    let head = method == Method::HEAD;

    // A real download takes one of the host's zip slots; measuring does not.
    let permit: Option<OwnedSemaphorePermit> = if dry_run || head {
        None
    } else {
        match Arc::clone(&api.zip_slots).try_acquire_owned() {
            Ok(p) => Some(p),
            Err(_) => {
                return Err(ApiError::retry_after(
                    ErrorCode::TooManyRequests,
                    "The computer is already making zips; try again in a moment.",
                    5_000,
                ));
            }
        }
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(Arc::clone(&cancel));
    let prepared = {
        let api = Arc::clone(&api);
        let who = who.clone();
        let cancel = Arc::clone(&cancel);
        let path = q.path().to_vec();
        blocking(move || {
            let opened = api.open(&who, &share, Permission::Fetch, &path, "fetch")?;
            if opened.resolved.kind() != EntryKind::Dir {
                return Err(ApiError::new(
                    ErrorCode::BadPath,
                    "This is a file, not a folder.",
                ));
            }
            let zip = api.config().zip;
            let limits = Limits {
                max_bytes: zip.max_bytes(),
                max_entries: u64::from(zip.max_entries),
                budget: api.options.zip_walk_budget,
            };
            let folder_path = opened.resolved.path().to_protocol_string();
            let name = if opened.resolved.name().is_empty() {
                opened.share.name.clone()
            } else {
                opened.resolved.name().to_owned()
            };
            let mtime = opened
                .resolved
                .stat()
                .map_or(Timestamp::UNIX_EPOCH, |e| e.mtime);
            let plan = match walk(
                &opened.root,
                &opened.resolved,
                &folder_path,
                limits,
                &cancel,
            ) {
                Ok(plan) => plan,
                Err(WalkError::Over(over)) => {
                    api.refused(&who, &share, Some(&folder_path), "fetch", "zip_too_large");
                    return Err(too_large(over, &limits));
                }
                // The request is gone; nobody reads this answer.
                Err(WalkError::Cancelled) => return Err(internal()),
            };
            Ok(Prepared {
                share,
                root: opened.root,
                path: folder_path,
                name,
                mtime,
                plan,
            })
        })
        .await?
    };

    let entries = prepared.plan.entries.len() as u64;
    let bytes = prepared.plan.bytes;
    if dry_run {
        return Ok(axum::Json(ZipPlan {
            entries,
            bytes,
            skipped: prepared.plan.skipped.total,
            cloud_only: prepared.plan.cloud_only,
        })
        .into_response());
    }

    let folder = archive_folder_name(&prepared.name);
    let builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            hv(&content_disposition(&format!("{folder}.zip")))?,
        )
        .header(header::ACCEPT_RANGES, "none")
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("X-Gander-Zip-Entries", entries)
        .header("X-Gander-Zip-Bytes", bytes);
    if head {
        return builder.body(Body::empty()).map_err(|_| internal());
    }

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(4);
    let task_cancel = Arc::clone(&cancel);
    let api2 = Arc::clone(&api);
    tokio::task::spawn_blocking(move || {
        let _slot = permit;
        let Prepared {
            share,
            root,
            path,
            mtime,
            plan,
            ..
        } = prepared;
        let local = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let out = ChannelWriter::new(tx.clone(), Arc::clone(&task_cancel));
        let mut written = Written::default();
        let bug = |p: &str, r: RejectReason| {
            api2.audit
                .resolver_bug(&who.device, who.source, &share, p, r.as_str());
        };
        let job = ZipJob {
            root: &root,
            plan: &plan,
            folder: &folder,
            folder_mtime: mtime,
            local,
            cancel: &task_cancel,
            on_bug: &bug,
        };
        let result = job.write(out, &mut written);
        let (outcome, how) = match &result {
            Ok(_) => (Outcome::Ok, "complete"),
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => (Outcome::Warning, "cancelled"),
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "folder zip failed while streaming");
                (Outcome::Error, "failed")
            }
        };
        if let Err(e) = result
            && e.kind() != io::ErrorKind::BrokenPipe
        {
            // Cut the response: a body that ends with an error never looks like a whole zip.
            let _ = tx.blocking_send(Err(e));
        }
        api2.audit.record(
            api2.accessed(&who, &share, &path, "fetch")
                .outcome(outcome)
                .detail("kind", "zip")
                .detail("entries", written.entries)
                .detail("bytes", written.bytes)
                .detail("skipped", plan.skipped.total)
                .detail("result", how),
        );
    });

    let body = Body::from_stream(futures_util::stream::unfold(
        (rx, guard),
        |(mut rx, guard)| async move { rx.recv().await.map(|item| (item, (rx, guard))) },
    ));
    builder.body(body).map_err(|_| internal())
}

#[cfg(test)]
mod tests;
