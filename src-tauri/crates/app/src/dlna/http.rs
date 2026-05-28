//! axum router for the DLNA MediaServer.
//!
//! Routes:
//!   GET  /description.xml             — root device descriptor
//!   GET  /service/ContentDirectory.xml — CDS SCPD
//!   GET  /service/ConnectionManager.xml — CM SCPD
//!   POST /control/ContentDirectory    — SOAP control endpoint (étape 4)
//!   GET  /stream/<track_id>           — audio bytes with Range support
//!   GET  /art/<hash>                  — artwork shim (delegates to the
//!                                        per-profile artwork dir or the
//!                                        shared metadata_artwork dir)
//!   GET  /healthz                     — diag (always 200 "ok")

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use sqlx::SqlitePool;
use tokio::io::{AsyncSeekExt, SeekFrom};
use tokio_util::io::ReaderStream;

use crate::dlna::{cds, description};

/// Shared state handed to every axum handler. `pool_factory` returns
/// the active profile's pool on demand so a profile switch flips the
/// data source without restarting the server.
#[derive(Clone)]
pub struct ServerCtx {
    pub server_name: String,
    pub base_url: String,
    pub pool: SqlitePool,
    pub profile_artwork_dir: PathBuf,
    pub metadata_artwork_dir: PathBuf,
}

pub fn router(ctx: ServerCtx) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/description.xml", get(serve_description))
        .route(
            "/service/ContentDirectory.xml",
            get(|| async {
                xml_response(description::CONTENT_DIRECTORY_SCPD.to_string())
            }),
        )
        .route(
            "/service/ConnectionManager.xml",
            get(|| async {
                xml_response(description::CONNECTION_MANAGER_SCPD.to_string())
            }),
        )
        .route("/stream/{track_id}", get(serve_stream))
        .route("/art/{hash}", get(serve_art))
        .route("/control/ContentDirectory", post(cds::handle_control))
        // Stub the ConnectionManager control endpoint with a 200 OK
        // empty SOAP body — controllers query GetProtocolInfo right
        // after discovery; failing it makes them deprioritise us.
        .route(
            "/control/ConnectionManager",
            post(|| async {
                let body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:GetProtocolInfoResponse xmlns:u="urn:schemas-upnp-org:service:ConnectionManager:1">
      <Source>http-get:*:audio/mpeg:*,http-get:*:audio/flac:*,http-get:*:audio/wav:*,http-get:*:audio/ogg:*,http-get:*:audio/mp4:*</Source>
      <Sink></Sink>
    </u:GetProtocolInfoResponse>
  </s:Body>
</s:Envelope>"#;
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
                    body,
                )
            }),
        )
        .with_state(Arc::new(ctx))
}

async fn serve_description(State(ctx): State<Arc<ServerCtx>>) -> Response {
    let body = description::device_descriptor(&ctx.server_name, &ctx.base_url);
    xml_response(body)
}

fn xml_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
        body,
    )
        .into_response()
}

/// Stream audio with HTTP Range support. Yamaha / Sonos send a
/// `Range: bytes=START-END` header to seek; without `206 Partial
/// Content` they fall back to "play from zero" or refuse to play
/// long files at all.
async fn serve_stream(
    State(ctx): State<Arc<ServerCtx>>,
    Path(track_id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let row: Result<Option<(String,)>, _> =
        sqlx::query_as("SELECT file_path FROM track WHERE id = ? AND is_available = 1")
            .bind(track_id)
            .fetch_optional(&ctx.pool)
            .await;
    let path = match row {
        Ok(Some((p,))) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, "track not found").into_response(),
        Err(err) => {
            tracing::warn!(?err, track_id, "stream lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    };

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!(?err, %path, "stream open failed");
            return (StatusCode::NOT_FOUND, "file missing").into_response();
        }
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(?err, %path, "stream metadata failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "stat error").into_response();
        }
    };
    let total = metadata.len();
    let mime = mime_for_path(&path);

    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match range.and_then(|r| parse_range(r, total)) {
        Some((s, e)) => (StatusCode::PARTIAL_CONTENT, s, e),
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
    };

    let length = end.saturating_sub(start) + 1;
    let body = match build_range_body(file, start, length).await {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(?err, %path, start, length, "range read failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "read error").into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap());
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        let cr = format!("bytes {start}-{end}/{total}");
        headers.insert(header::CONTENT_RANGE, HeaderValue::from_str(&cr).unwrap());
    }
    // DLNA seek hint — controllers won't expose a scrubber without it.
    headers.insert(
        "transferMode.dlna.org",
        HeaderValue::from_static("Streaming"),
    );
    headers.insert(
        "contentFeatures.dlna.org",
        HeaderValue::from_static("DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"),
    );

    (status, headers, body).into_response()
}

/// Build a streaming body that reads the requested byte range in 64
/// KiB chunks. `take(length)` caps the reader so we never overshoot
/// the Range window even if the controller closes early.
async fn build_range_body(
    mut file: tokio::fs::File,
    start: u64,
    length: u64,
) -> std::io::Result<Body> {
    file.seek(SeekFrom::Start(start)).await?;
    let limited = tokio::io::AsyncReadExt::take(file, length);
    let stream = ReaderStream::with_capacity(limited, 64 * 1024);
    Ok(Body::from_stream(stream))
}

fn parse_range(raw: &str, total: u64) -> Option<(u64, u64)> {
    let raw = raw.strip_prefix("bytes=")?.trim();
    let (start_s, end_s) = raw.split_once('-')?;
    let start: u64 = start_s.trim().parse().ok()?;
    let end: u64 = if end_s.trim().is_empty() {
        total.saturating_sub(1)
    } else {
        end_s.trim().parse().ok()?
    };
    if start > end || end >= total {
        // `end >= total` includes the open-ended `bytes=0-` form
        // when the file is empty (unusual but possible) — fold to a
        // simple 200 response by returning None.
        return None;
    }
    Some((start, end))
}

fn mime_for_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("ogg" | "oga") => "audio/ogg",
        Some("m4a" | "mp4" | "aac") => "audio/mp4",
        _ => "application/octet-stream",
    }
}

/// Serve cached artwork by blake3 hash. Probes both the per-profile
/// `artwork/` dir (embedded + folder-cover bytes from the scanner)
/// and the shared `metadata_artwork/` dir (Deezer downloads). Returns
/// 404 when neither match — the controller will fall back to its
/// generic music-note placeholder.
async fn serve_art(
    State(ctx): State<Arc<ServerCtx>>,
    Path(hash_with_ext): Path<String>,
) -> Response {
    // Reject path traversal up-front. Both `/` and `\\` are checked so a
    // Windows-flavoured payload like `..\\..\\secret.jpg` can't escape the
    // artwork dir on a host whose Path parser accepts backslashes.
    if hash_with_ext.contains('/') || hash_with_ext.contains('\\') || hash_with_ext.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad hash").into_response();
    }
    for dir in [&ctx.profile_artwork_dir, &ctx.metadata_artwork_dir] {
        let candidate = dir.join(&hash_with_ext);
        if candidate.is_file() {
            return match tokio::fs::read(&candidate).await {
                Ok(bytes) => (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime_for_art(&hash_with_ext))],
                    bytes,
                )
                    .into_response(),
                Err(err) => {
                    tracing::warn!(?err, ?candidate, "art read failed");
                    (StatusCode::INTERNAL_SERVER_ERROR, "read error").into_response()
                }
            };
        }
    }
    (StatusCode::NOT_FOUND, "art missing").into_response()
}

fn mime_for_art(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("tiff") => "image/tiff",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_full_form() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-999", 1000), Some((500, 999)));
    }

    #[test]
    fn parse_range_open_ended() {
        // `bytes=START-` means "from START to EOF". Common from VLC.
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
    }

    #[test]
    fn parse_range_rejects_inverted_or_oob() {
        assert!(parse_range("bytes=900-100", 1000).is_none());
        assert!(parse_range("bytes=0-9999", 1000).is_none());
        assert!(parse_range("xyz=0-10", 1000).is_none());
    }

    #[test]
    fn mime_for_path_recognises_lossless_and_lossy() {
        assert_eq!(mime_for_path("a.flac"), "audio/flac");
        assert_eq!(mime_for_path("a.MP3"), "audio/mpeg");
        assert_eq!(mime_for_path("a.unknown"), "application/octet-stream");
    }
}
