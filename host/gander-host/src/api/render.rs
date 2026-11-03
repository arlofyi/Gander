// SPDX-License-Identifier: Apache-2.0
//! `GET /shares/{share}/thumb` (WP-15), `/preview/info`, `/preview`, `/preview/text` (WP-16) and
//! `/preview/tile` (WP-67).
//!
//! Each handler checks the `preview` permission, resolves the path to an open handle and hands
//! that handle to the service on the blocking pool (the preview service may wait up to ~2 s for
//! a render). Service errors carry their own protocol body; a `503 generating` also gets
//! `Retry-After`.

use std::sync::Arc;

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, RawQuery as AxRawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use gander_core::error::ErrorCode;
use gander_core::protocol::ShareId;
use gander_core::protocol::files::{ImageFormat, Permission, ThumbPriority};
use gander_win::preview::service::{InfoRequest, PageRequest, TextRequest, TileRequest};
use gander_win::thumbs::queue::Priority;
use gander_win::thumbs::service::ThumbRequest;

use super::browse::source;
use super::fetch::file_ext;
use super::query::RawQuery;
use super::{ApiState, Opened, Requester, bad_request, blocking, internal, none_match};
use crate::server::{ApiError, Caller, ConnInfo};

/// Header with the offset of the next text slice (protocol addition, WP-14/WP-16).
pub const TEXT_NEXT_HEADER: &str = "X-Gander-Text-Next";

fn unsupported(what: &str) -> ApiError {
    ApiError::new(
        ErrorCode::UnsupportedType,
        format!("{what} are not available on this computer."),
    )
}

fn not_a_file() -> ApiError {
    ApiError::new(ErrorCode::UnsupportedType, "Folders have no preview.")
}

fn hv(s: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(s).map_err(|_| internal())
}

/// The resolved file, its extension and the device id the services want.
fn file_parts(opened: Opened) -> Result<(std::fs::File, String), ApiError> {
    if opened.resolved.kind() != gander_core::protocol::files::EntryKind::File {
        return Err(not_a_file());
    }
    let ext = file_ext(opened.resolved.name()).unwrap_or_default();
    Ok((opened.resolved.into_file(), ext))
}

/// An immutable image answer, or 304 when the client already has it.
fn image_response(
    inm: Option<&str>,
    bytes: Vec<u8>,
    content_type: &str,
    etag: &str,
    cache_control: &str,
) -> Result<Response, ApiError> {
    let etag_v = hv(etag)?;
    if inm.is_some_and(|v| none_match(v, etag)) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag_v)]).into_response());
    }
    Response::builder()
        .header(header::CONTENT_TYPE, hv(content_type)?)
        .header(header::ETAG, etag_v)
        .header(header::CACHE_CONTROL, hv(cache_control)?)
        .header("X-Content-Type-Options", "nosniff")
        .body(Body::from(bytes))
        .map_err(|_| internal())
}

fn inm(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

pub(super) async fn thumb(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let size = q.number::<u32>("size")?;
    let priority = match q.non_empty("priority")? {
        None => ThumbPriority::default(),
        Some(p) => ThumbPriority::parse(p)
            .ok_or_else(|| bad_request("priority must be visible or prefetch."))?,
    };
    let inm = inm(&headers);
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Preview, q.path(), "thumb")?;
        let Some(thumbs) = api.thumbs.clone() else {
            return Err(unsupported("Thumbnails"));
        };
        let (file, ext) = file_parts(opened)?;
        let resp = thumbs
            .thumb(ThumbRequest {
                file,
                ext,
                size,
                priority: match priority {
                    ThumbPriority::Visible => Priority::Visible,
                    ThumbPriority::Prefetch => Priority::Prefetch,
                },
            })
            .map_err(|e| ApiError(e.to_body()))?;
        image_response(
            inm.as_deref(),
            resp.bytes,
            resp.content_type,
            &resp.etag,
            resp.cache_control,
        )
    })
    .await
}

pub(super) async fn preview_info(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Preview, q.path(), "preview")?;
        let Some(previews) = api.previews.clone() else {
            return Err(unsupported("Previews"));
        };
        let (file, ext) = file_parts(opened)?;
        let info = previews
            .info(InfoRequest {
                file,
                ext,
                device: who.device.to_string(),
            })
            .map_err(|e| ApiError(e.to_body()))?;
        Ok(axum::Json(info).into_response())
    })
    .await
}

pub(super) async fn preview_page(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let page = q.number::<u32>("page")?;
    let width = q.number::<u32>("width")?;
    let format = match q.non_empty("format")? {
        None | Some("webp") => ImageFormat::Webp,
        Some("jpeg") => ImageFormat::Jpeg,
        Some(_) => return Err(bad_request("format must be webp or jpeg.")),
    };
    let inm = inm(&headers);
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Preview, q.path(), "preview")?;
        let Some(previews) = api.previews.clone() else {
            return Err(unsupported("Previews"));
        };
        let (file, ext) = file_parts(opened)?;
        let resp = previews
            .page(PageRequest {
                file,
                ext,
                device: who.device.to_string(),
                page,
                width,
                format,
            })
            .map_err(|e| ApiError(e.to_body()))?;
        image_response(
            inm.as_deref(),
            resp.bytes,
            resp.content_type,
            &resp.etag,
            resp.cache_control,
        )
    })
    .await
}

/// `GET /shares/{share}/preview/tile?path=&page=&level=&x=&y=`: one 512 px WebP tile of a page
/// (deep zoom, WP-67). `level`, `x` and `y` are required; the answer is immutable by ETag.
pub(super) async fn preview_tile(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let page = q.number::<u32>("page")?;
    let required = |name: &str| {
        q.number::<u32>(name)?
            .ok_or_else(|| bad_request(&format!("{name} is required.")))
    };
    let (level, x, y) = (required("level")?, required("x")?, required("y")?);
    let inm = inm(&headers);
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Preview, q.path(), "preview")?;
        let Some(previews) = api.previews.clone() else {
            return Err(unsupported("Previews"));
        };
        let (file, ext) = file_parts(opened)?;
        let resp = previews
            .tile(TileRequest {
                file,
                ext,
                device: who.device.to_string(),
                page,
                level,
                x,
                y,
            })
            .map_err(|e| ApiError(e.to_body()))?;
        image_response(
            inm.as_deref(),
            resp.bytes,
            resp.content_type,
            &resp.etag,
            resp.cache_control,
        )
    })
    .await
}

pub(super) async fn preview_text(
    State(api): State<Arc<ApiState>>,
    Extension(caller): Extension<Caller>,
    conn: Option<Extension<ConnInfo>>,
    Path(share): Path<String>,
    AxRawQuery(raw): AxRawQuery,
) -> Result<Response, ApiError> {
    let who = Requester::new(&caller, source(&conn))?;
    let q = RawQuery::parse(raw.as_deref())?;
    let offset = q.number::<u64>("offset")?.unwrap_or(0);
    let limit = q.number::<u64>("limit")?;
    let share = ShareId::from(share.as_str());
    blocking(move || {
        let opened = api.open(&who, &share, Permission::Preview, q.path(), "preview")?;
        let Some(previews) = api.previews.clone() else {
            return Err(unsupported("Previews"));
        };
        let (file, _) = file_parts(opened)?;
        let resp = previews
            .text(TextRequest {
                file,
                offset,
                limit,
            })
            .map_err(|e| ApiError(e.to_body()))?;
        Response::builder()
            .header(header::CONTENT_TYPE, hv(resp.content_type)?)
            .header("X-Gander-Text-Encoding", hv(resp.encoding)?)
            .header("X-Gander-Text-Total", resp.total)
            .header(TEXT_NEXT_HEADER, resp.next_offset)
            .header(header::CACHE_CONTROL, "private, no-cache")
            .header("X-Content-Type-Options", "nosniff")
            .body(Body::from(resp.bytes))
            .map_err(|_| internal())
    })
    .await
}
