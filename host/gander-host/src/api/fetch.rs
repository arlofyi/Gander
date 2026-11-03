// SPDX-License-Identifier: Apache-2.0
//! `GET/HEAD /shares/{share}/file` and `GET /shares/{share}/hash`.
//!
//! `/file` semantics:
//!
//! - strong `ETag` from the open handle: `"<volume>.<file id>-<size>-<last write>"` (hex), so a
//!   rename keeps it and any write changes it;
//! - one `Range` (`a-b`, `a-`, `-n`) -> 206 with `Content-Range`; a list of ranges (multipart)
//!   or an unsatisfiable / malformed byte range -> 416 `range_not_satisfiable` with
//!   `Content-Range: bytes */size`; a range in another unit is ignored (full 200, RFC 9110);
//! - `If-Range` (with `Range`) or `If-Match` that does not match the current strong ETag (or the
//!   `Last-Modified` date for a date-valued `If-Range`) -> 412 `file_changed`: the client
//!   restarts instead of splicing two versions;
//! - bytes are read from the resolved handle on the blocking pool and streamed; if the file
//!   shrinks while being sent the body ends with an error (the connection is cut, never padded).

use std::fs::File;
use std::io;
use std::os::windows::fs::FileExt as _;
use std::sync::Arc;

use axum::Extension;
use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery as AxRawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gander_core::error::ErrorCode;
use gander_core::protocol::ShareId;
use gander_core::protocol::files::{EntryKind, HashResponse, Permission};
use gander_core::protocol::host::HashAlgo;
use gander_win::thumbs::cache::FileIdentity;
use gander_win::thumbs::identity::file_identity;

use super::browse::source;
use super::hash::{HashOutcome, retry_estimate_ms};
use super::query::RawQuery;
use super::{ApiState, Requester, bad_request, blocking, internal};
use crate::audit::Outcome;
use crate::server::{ApiError, Caller, ConnInfo};

/// Read size per chunk of the streamed body.
const CHUNK: usize = 256 * 1024;

/// The strong ETag of one version of a file.
pub(super) fn strong_etag(id: &FileIdentity) -> String {
    format!(
        "\"{:x}.{:x}-{:x}-{:x}\"",
        id.volume, id.file_id, id.size, id.mtime
    )
}

/// `Last-Modified` (IMF-fixdate) from FILETIME ticks.
fn http_date(filetime: i64) -> String {
    let ts = gander_core::path::timestamp_from_filetime(filetime)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let t = ts.to_offset(time::UtcOffset::UTC);
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[usize::from(t.weekday().number_days_from_monday())],
        t.day(),
        MONTHS[usize::from(u8::from(t.month()) - 1)],
        t.year(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// `Content-Disposition: attachment` with an ASCII fallback and the RFC 5987 UTF-8 name.
pub(super) fn content_disposition(name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(name.len() * 3);
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// Lower-case extension of a file name, as listings report it.
pub(super) fn file_ext(name: &str) -> Option<String> {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => Some(name[i + 1..].to_lowercase()),
        _ => None,
    }
}

/// A parsed `Range` header against a file of `size` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RangeReq {
    /// No byte range: send everything.
    Full,
    /// Bytes `start..=end`.
    Part {
        /// First byte.
        start: u64,
        /// Last byte (inclusive).
        end: u64,
    },
    /// 416.
    Unsatisfiable,
}

pub(super) fn parse_range(value: &str, size: u64) -> RangeReq {
    let value = value.trim();
    let Some((unit, spec)) = value.split_once('=') else {
        return RangeReq::Unsatisfiable;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return RangeReq::Full;
    }
    if spec.contains(',') {
        // Multipart ranges are refused (protocol spec).
        return RangeReq::Unsatisfiable;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeReq::Unsatisfiable;
    };
    let (a, b) = (a.trim(), b.trim());
    let num = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    if size == 0 {
        return RangeReq::Unsatisfiable;
    }
    match (a.is_empty(), b.is_empty()) {
        (true, false) => match num(b) {
            Some(n) if n > 0 => RangeReq::Part {
                start: size - n.min(size),
                end: size - 1,
            },
            _ => RangeReq::Unsatisfiable,
        },
        (false, true) => match num(a) {
            Some(start) if start < size => RangeReq::Part {
                start,
                end: size - 1,
            },
            _ => RangeReq::Unsatisfiable,
        },
        (false, false) => match (num(a), num(b)) {
            (Some(start), Some(end)) if start <= end && start < size => RangeReq::Part {
                start,
                end: end.min(size - 1),
            },
            _ => RangeReq::Unsatisfiable,
        },
        (true, true) => RangeReq::Unsatisfiable,
    }
}

/// Streams `len` bytes from `start` of `file`, read on the blocking pool.
fn body_from_file(file: File, start: u64, len: u64) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<io::Result<Bytes>>(4);
    tokio::task::spawn_blocking(move || {
        let mut offset = start;
        let mut remaining = len;
        while remaining > 0 {
            let n = usize::try_from(remaining).unwrap_or(CHUNK).min(CHUNK);
            let mut buf = vec![0u8; n];
            let item = match file.seek_read(&mut buf, offset) {
                Ok(0) => Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the file shrank while being sent",
                )),
                Ok(k) => {
                    buf.truncate(k);
                    offset += k as u64;
                    remaining -= k as u64;
                    Ok(Bytes::from(buf))
                }
                Err(e) => Err(e),
            };
            let failed = item.is_err();
            if tx.blocking_send(item).is_err() || failed {
                break; // the client went away, or the read failed
            }
        }
    });
    Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
}

fn file_changed() -> ApiError {
    ApiError::new(
        ErrorCode::FileChanged,
        "The file changed on the computer; start the download again.",
    )
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn hv(s: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(s).map_err(|_| internal())
}

pub(super) async fn file(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    method: Method,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let share = ShareId::from(share.as_str());
    let range = header_str(&headers, header::RANGE);
    let if_range = header_str(&headers, header::IF_RANGE);
    let if_match = header_str(&headers, header::IF_MATCH);
    let head = method == Method::HEAD;
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Fetch, q.path(), "fetch")?;
        if opened.resolved.kind() != EntryKind::File {
            return Err(ApiError::new(
                ErrorCode::BadPath,
                "This is a folder, not a file.",
            ));
        }
        let name = opened.resolved.name().to_owned();
        let path = opened.resolved.path().to_protocol_string();
        let file = opened.resolved.into_file();
        let id = file_identity(&file).map_err(|e| {
            tracing::warn!(error = %e, "reading file identity failed");
            internal()
        })?;
        let size = id.size;
        let etag = strong_etag(&id);
        let last_modified = http_date(id.mtime);

        let refuse_changed = |reason: &str| {
            api.refused(&who, &share, Some(&path), "fetch", reason);
            file_changed()
        };
        if let Some(m) = &if_match
            && m.trim() != "*"
            && !m.split(',').any(|t| t.trim() == etag)
        {
            return Err(refuse_changed("if_match"));
        }
        let part = match range.as_deref() {
            None => RangeReq::Full,
            Some(r) => parse_range(r, size),
        };
        if range.is_some()
            && let Some(ir) = &if_range
        {
            let ir = ir.trim();
            let matches = if ir.starts_with('"') || ir.starts_with("W/") {
                ir == etag
            } else {
                ir == last_modified
            };
            if !matches {
                return Err(refuse_changed("if_range"));
            }
        }
        if part == RangeReq::Unsatisfiable {
            let mut resp = ApiError::new(
                ErrorCode::RangeNotSatisfiable,
                "Only one satisfiable byte range is supported.",
            )
            .into_response();
            resp.headers_mut()
                .insert(header::CONTENT_RANGE, hv(&format!("bytes */{size}"))?);
            return Ok(resp);
        }
        let (status, start, len) = match part {
            RangeReq::Part { start, end } => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
            _ => (StatusCode::OK, 0, size),
        };
        let ext = file_ext(&name);
        let mut builder = Response::builder()
            .status(status)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, len)
            .header(header::ETAG, hv(&etag)?)
            .header(header::LAST_MODIFIED, hv(&last_modified)?)
            .header(
                header::CONTENT_TYPE,
                super::mime::content_type(ext.as_deref()),
            )
            .header(
                header::CONTENT_DISPOSITION,
                hv(&content_disposition(&name))?,
            )
            .header(header::CACHE_CONTROL, "private, no-cache")
            .header("X-Content-Type-Options", "nosniff");
        if status == StatusCode::PARTIAL_CONTENT {
            builder = builder.header(
                header::CONTENT_RANGE,
                hv(&format!("bytes {start}-{}/{size}", start + len - 1))?,
            );
        }
        if head {
            return builder.body(Body::empty()).map_err(|_| internal());
        }
        let mut ev = api.accessed(&who, &share, &path, "fetch");
        if status == StatusCode::PARTIAL_CONTENT {
            ev = ev.detail("range", format!("{start}-{}", start + len - 1));
        }
        api.audit.record(ev);
        builder
            .body(body_from_file(file, start, len))
            .map_err(|_| internal())
    })
    .await
}

pub(super) async fn hash(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let algo = match q.non_empty("algo")? {
        Some("blake3") => HashAlgo::Blake3,
        Some("sha256") => HashAlgo::Sha256,
        _ => return Err(bad_request("algo must be blake3 or sha256.")),
    };
    let share = ShareId::from(share.as_str());
    let runtime = tokio::runtime::Handle::current();
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Fetch, q.path(), "hash")?;
        if opened.resolved.kind() != EntryKind::File {
            return Err(ApiError::new(
                ErrorCode::BadPath,
                "This is a folder, not a file.",
            ));
        }
        let path = opened.resolved.path().to_protocol_string();
        let file = opened.resolved.into_file();
        let id = file_identity(&file).map_err(|_| internal())?;
        let etag = strong_etag(&id);
        let outcome = api.hashes.get_or_start(
            &runtime,
            file,
            id,
            etag.clone(),
            algo,
            api.options.hash_inline_max,
        );
        match outcome {
            HashOutcome::Done(hex) => {
                api.audit.record(
                    api.accessed(&who, &share, &path, "hash")
                        .detail("algo", format!("{algo:?}").to_lowercase()),
                );
                Ok(axum::Json(HashResponse {
                    algo,
                    hex,
                    size: id.size,
                    etag,
                })
                .into_response())
            }
            HashOutcome::Computing => Err(ApiError::retry_after(
                ErrorCode::Generating,
                "The hash is being computed.",
                retry_estimate_ms(id.size),
            )),
            HashOutcome::Changed => {
                api.audit.record(
                    api.accessed(&who, &share, &path, "hash")
                        .outcome(Outcome::Refused)
                        .detail("reason", "file_changed"),
                );
                Err(file_changed())
            }
            HashOutcome::Failed => Err(internal()),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        use RangeReq::*;
        assert_eq!(parse_range("bytes=0-9", 100), Part { start: 0, end: 9 });
        assert_eq!(parse_range("bytes=90-", 100), Part { start: 90, end: 99 });
        assert_eq!(parse_range("bytes=-10", 100), Part { start: 90, end: 99 });
        assert_eq!(parse_range("bytes=-1000", 100), Part { start: 0, end: 99 });
        assert_eq!(
            parse_range("bytes=50-1000", 100),
            Part { start: 50, end: 99 }
        );
        assert_eq!(parse_range("bytes=0-1,5-6", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=9-1", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=x-1", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=0-0", 0), Unsatisfiable);
        assert_eq!(parse_range("items=0-1", 100), Full);
        assert_eq!(parse_range("garbage", 100), Unsatisfiable);
    }

    #[test]
    fn disposition_and_date() {
        assert_eq!(
            content_disposition("Résumé \"v2\".pdf"),
            "attachment; filename=\"R_sum_ _v2_.pdf\"; filename*=UTF-8''R%C3%A9sum%C3%A9%20%22v2%22.pdf"
        );
        // 2001-09-09T01:46:40Z
        let ticks = 116_444_736_000_000_000 + 1_000_000_000 * 10_000_000;
        assert_eq!(http_date(ticks), "Sun, 09 Sep 2001 01:46:40 GMT");
    }
}
