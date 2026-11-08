//! `GET`/`HEAD /media/{blobId}` and `/media/{blobId}/preview`.
//! Unauthenticated: a blob id is a 96-bit random capability. `ServeFile` does Range and `HEAD`;
//! the validators are ours, because it would otherwise answer with an ETag of its own and could
//! 412 a capability URL.

use std::fmt::Write as _;

use axum::{
    body::Body,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use lobby_protocol::BlobId;
use serde::Deserialize;
use tower_http::services::ServeFile;
use tower_service::Service as _;

use crate::{
    db::blobs,
    error::{ApiError, AppPath},
    server::App,
};

pub const MEDIA_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
pub const MEDIA_CSP: &str = "default-src 'none'; sandbox";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Full,
    Preview,
}

#[derive(Deserialize)]
pub struct MediaQuery {
    download: Option<String>,
}

pub async fn full(
    State(app): State<App>,
    AppPath(id): AppPath<BlobId>,
    Query(q): Query<MediaQuery>,
    req: Request,
) -> Result<Response, ApiError> {
    serve(
        &app,
        &id,
        Variant::Full,
        q.download.as_deref() == Some("1"),
        req,
    )
    .await
}

pub async fn preview(
    State(app): State<App>,
    AppPath(id): AppPath<BlobId>,
    req: Request,
) -> Result<Response, ApiError> {
    serve(&app, &id, Variant::Preview, false, req).await
}

const CONDITIONALS: [HeaderName; 4] = [
    header::IF_MATCH,
    header::IF_UNMODIFIED_SINCE,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
];

async fn serve(
    app: &App,
    id: &BlobId,
    variant: Variant,
    download: bool,
    mut req: Request,
) -> Result<Response, ApiError> {
    let row = blobs::media(&app.db, id).await?;
    let not_found = || {
        ApiError::not_found(match variant {
            Variant::Full => "Media not found",
            Variant::Preview => "Media preview not found",
        })
    };
    let row = row.ok_or_else(not_found)?;
    let (path, mime, name) = match variant {
        Variant::Full => (row.path, row.mime, row.name),
        Variant::Preview => (
            row.preview.ok_or_else(not_found)?,
            "image/jpeg".to_owned(),
            preview_name(&row.name),
        ),
    };
    let tag = etag(id, variant);
    if not_modified(req.headers(), &tag) {
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        media_headers(res.headers_mut(), id, variant, &mime, &name, download);
        return Ok(res);
    }
    settle_range(req.headers_mut(), &tag);
    for h in CONDITIONALS {
        req.headers_mut().remove(h);
    }
    let mut res = ServeFile::new(app.blobs.resolve(&path))
        .call(req)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, blob = %id, "serving a blob failed");
            ApiError::internal()
        })?
        .map(Body::new);
    if res.status() == StatusCode::NOT_FOUND {
        return Err(not_found());
    }
    if res.status().is_success() {
        media_headers(res.headers_mut(), id, variant, &mime, &name, download);
    }
    Ok(res)
}

/// `If-Range` is ours to decide too: our tag keeps the Range, anything else means the client's
/// copy is not this blob, so it gets the whole file.
fn settle_range(headers: &mut HeaderMap, tag: &HeaderValue) {
    if let Some(if_range) = headers.remove(header::IF_RANGE)
        && if_range != *tag
    {
        headers.remove(header::RANGE);
    }
}

pub fn etag(id: &BlobId, variant: Variant) -> HeaderValue {
    let suffix = match variant {
        Variant::Full => "",
        Variant::Preview => "-preview",
    };
    HeaderValue::from_str(&format!("\"{id}{suffix}\"")).expect("an id is ASCII")
}

pub fn not_modified(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|candidate| {
            candidate == "*"
                || candidate.strip_prefix("W/").unwrap_or(candidate).as_bytes() == etag.as_bytes()
        })
}

pub fn media_headers(
    h: &mut HeaderMap,
    id: &BlobId,
    variant: Variant,
    mime: &str,
    name: &str,
    download: bool,
) {
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(MEDIA_CACHE_CONTROL),
    );
    h.insert(header::ETAG, etag(id, variant));
    h.remove(header::LAST_MODIFIED);
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(MEDIA_CSP),
    );
    if let Ok(mime) = HeaderValue::from_str(mime) {
        h.insert(header::CONTENT_TYPE, mime);
    }
    let download = download && variant == Variant::Full;
    h.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(mime, name, download),
    );
}

fn preview_name(name: &str) -> String {
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    format!("{stem}.jpg")
}

fn is_inline(mime: &str) -> bool {
    mime.starts_with("image/")
        || mime.starts_with("video/")
        || mime.starts_with("audio/")
        || mime == "application/pdf"
}

/// `inline` for image, video, audio and PDF unless `download`, else `attachment`; always with the
/// RFC 5987 `filename*` and an ASCII `filename` for old readers.
pub fn content_disposition(mime: &str, name: &str, download: bool) -> HeaderValue {
    let kind = if is_inline(mime) && !download {
        "inline"
    } else {
        "attachment"
    };
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | '%' | ';') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            )
        {
            encoded.push(char::from(b));
        } else {
            write!(encoded, "%{b:02X}").expect("writing to String cannot fail");
        }
    }
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("the value is built from ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> BlobId {
        "blb_2582c529769d2a353bd49743".parse().unwrap()
    }

    fn header_map(pairs: &[(HeaderName, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(k.clone(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn if_none_match_table() {
        let tag = etag(&id(), Variant::Full);
        let full = "\"blb_2582c529769d2a353bd49743\"";
        for (value, expected) in [
            (full.to_owned(), true),
            (format!("W/{full}"), true),
            ("*".to_owned(), true),
            (format!("\"other\", {full}"), true),
            (format!("\"other\" ,W/{full} "), true),
            ("\"other\"".to_owned(), false),
            ("\"blb_2582c529769d2a353bd49743-preview\"".to_owned(), false),
            (String::new(), false),
        ] {
            let h = header_map(&[(header::IF_NONE_MATCH, &value)]);
            assert_eq!(not_modified(&h, &tag), expected, "{value}");
        }
        assert!(!not_modified(&HeaderMap::new(), &tag));
    }

    #[test]
    fn if_range_with_a_foreign_validator_means_the_whole_file() {
        let tag = etag(&id(), Variant::Full);
        let mut ours = header_map(&[
            (header::RANGE, "bytes=0-3"),
            (header::IF_RANGE, "\"blb_2582c529769d2a353bd49743\""),
        ]);
        settle_range(&mut ours, &tag);
        assert!(ours.contains_key(header::RANGE) && !ours.contains_key(header::IF_RANGE));
        let mut theirs = header_map(&[
            (header::RANGE, "bytes=0-3"),
            (header::IF_RANGE, "\"stale\""),
        ]);
        settle_range(&mut theirs, &tag);
        assert!(!theirs.contains_key(header::RANGE));
    }
}
