//! Loopback HTTP for the webview's video, where the webview cannot read a
//! file itself: an ephemeral port on `127.0.0.1`, a token minted per launch,
//! and two kinds of file.
//!
//! - **A local video, on Linux** ([`local`]). WebKitGTK plays `<video>`
//!   through GStreamer, and GStreamer has no source for Tauri's asset
//!   protocol: every local Canvas clip and cached motion cover was refused
//!   ("no URI handler implemented for asset"). WebKit's HTTP source plays an
//!   ordinary MP4, so those are served from here instead, only video files
//!   the asset scope already lets the webview read. Windows and macOS play
//!   the asset URL directly.
//! - **A music video on a network share, on every platform** (#1133), which
//!   no webview can read: [`share_url`] names it by its locator, and the
//!   request is answered from the share through the source's own client,
//!   range by range ([`serve_share`]). Only a clip a lookup handed out is
//!   served, through the source it was found on while that one is still
//!   registered ([`crate::netlib::clip::allowed_source`]). A fragmented MP4
//!   there is served as it is, as a server's clip is.
//!
//! Started on first use and kept for the life of the process. A failure to
//! bind is logged and reported as `None`; the caller shows the static cover.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tauri::AppHandle;
use tokio::sync::OnceCell;
use waveflow_core::network_locator;

use crate::dlna::http::{parse_range, range_not_satisfiable};
use crate::netlib::fs::NetError;

#[cfg(target_os = "linux")]
mod local;

static BASE_URL: OnceCell<Option<String>> = OnceCell::const_new();

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    token: String,
}

/// `http://127.0.0.1:<port>/media?token=<token>`, to which the caller
/// appends `&path=<percent-encoded absolute path>`. Starts the server on
/// the first call.
pub async fn base_url(app: &AppHandle) -> Option<String> {
    BASE_URL.get_or_init(|| start(app.clone())).await.clone()
}

/// The address the webview plays the clip at `locator`, on a share, from.
/// `None` when the server could not start.
pub async fn share_url(app: &AppHandle, locator: &str) -> Option<String> {
    let base = base_url(app).await?;
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("locator", locator)
        .finish();
    Some(format!("{base}&{query}"))
}

async fn start(app: AppHandle) -> Option<String> {
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let listener =
        match tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).await {
            Ok(listener) => listener,
            Err(err) => {
                tracing::warn!(%err, "media loopback: bind failed, local clips will not play");
                return None;
            }
        };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            tracing::warn!(%err, "media loopback: no local address");
            return None;
        }
    };
    let router = Router::new().route("/media", get(serve)).with_state(Ctx {
        app,
        token: token.clone(),
    });
    tauri::async_runtime::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            tracing::warn!(%err, "media loopback stopped");
        }
    });
    tracing::info!(port, "media loopback listening");
    Some(format!("http://127.0.0.1:{port}/media?token={token}"))
}

async fn serve(State(ctx): State<Ctx>, RawQuery(query): RawQuery, headers: HeaderMap) -> Response {
    let (mut token, mut path, mut locator) = (None, None, None);
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "token" => token = Some(value.into_owned()),
            "path" => path = Some(PathBuf::from(value.into_owned())),
            "locator" => locator = Some(value.into_owned()),
            _ => {}
        }
    }
    if token.as_deref() != Some(ctx.token.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(locator) = locator {
        return serve_share(locator, &headers).await;
    }
    #[cfg(target_os = "linux")]
    {
        local::serve(&ctx.app, path, &headers).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Windows and macOS play a local file through the asset protocol.
        let _ = (&ctx.app, path);
        StatusCode::FORBIDDEN.into_response()
    }
}

/// Bytes asked of the share per read while a clip on it streams: the block
/// a share's background download asks for.
const SHARE_BLOCK: u64 = 1024 * 1024;

/// Answer a request for the clip at `locator`, on a share, from the share.
async fn serve_share(locator: String, headers: &HeaderMap) -> Response {
    // The source the lookup found the clip on, not whichever one carries its
    // uuid now: see `netlib::clip`.
    let Some(source) = crate::netlib::clip::allowed_source(&locator) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Some(mime) = video_mime(Path::new(&locator)) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Some(parsed) = network_locator::parse(&locator) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let path = parsed.path.to_string();
    let stat = {
        let (source, path) = (Arc::clone(&source), path.clone());
        crate::netlib::run_cancellable(async move { source.stat(&path).await }).await
    };
    let total = match stat {
        Ok(Ok(entry)) if !entry.is_dir => entry.size,
        Ok(Ok(_) | Err(NetError::NotFound)) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(err)) | Err(err) => {
            tracing::warn!(
                code = err.code(),
                "media loopback: a clip on a share did not answer"
            );
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let Some((status, start, length)) = parse_range(range, total).window(total) else {
        return range_not_satisfiable(total);
    };
    let body = Body::from_stream(share_stream(source, path, start, start + length));
    range_response(status, mime, (start, length, total), body)
}

/// Bytes `[from, to)` of `path` on `source`, read one [`SHARE_BLOCK`] at a
/// time as the webview takes them, so a request it drops (a seek) stops the
/// reads. A failed or short read ends the body with an error, which the
/// webview reports as a clip that cannot play rather than as its end, and so
/// do sources loaded again meanwhile (a profile switch): `source` is then no
/// longer theirs, and the response stops before its next block.
fn share_stream(
    source: Arc<crate::netlib::Source>,
    path: String,
    from: u64,
    to: u64,
) -> impl futures::Stream<Item = std::io::Result<Bytes>> {
    futures::stream::unfold(Some(from), move |at| {
        let (source, path) = (Arc::clone(&source), path.clone());
        async move {
            let at = at.filter(|at| *at < to)?;
            if !crate::netlib::clip::is_current(&source) {
                return Some((Err(std::io::Error::other("the sources changed")), None));
            }
            let len = SHARE_BLOCK.min(to - at);
            // Dropped with the body when the webview gives up on the
            // request (a seek): the read stops, rather than retrying on a
            // share that went quiet and marking it down for nobody.
            let read =
                crate::netlib::run_cancellable(async move { source.read_at(&path, at, len).await })
                    .await;
            match read {
                Ok(Ok(bytes)) if !bytes.is_empty() => {
                    let next = at + bytes.len() as u64;
                    Some((Ok(Bytes::from(bytes)), Some(next)))
                }
                Ok(Ok(_)) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the clip on the share is shorter than it was",
                    )),
                    None,
                )),
                Ok(Err(err)) | Err(err) => {
                    tracing::warn!(
                        code = err.code(),
                        "media loopback: a clip on a share stopped"
                    );
                    Some((Err(std::io::Error::other(err.code())), None))
                }
            }
        }
    })
}

/// The answer to a `Range` request for `[start, start + length)` of a
/// `total`-byte video: its headers and `body`.
fn range_response(
    status: StatusCode,
    mime: &'static str,
    (start, length, total): (u64, u64, u64),
    body: Body,
) -> Response {
    let mut out = HeaderMap::new();
    out.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    out.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if status == StatusCode::PARTIAL_CONTENT {
        let end = start + length - 1;
        if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
            out.insert(header::CONTENT_RANGE, value);
        }
    }
    (status, out, body).into_response()
}

/// Only the containers a Canvas, a music video or a motion cover can be; anything else in
/// the scope (covers, artist images) is not this server's to hand out.
fn video_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        // A track's music video (#1073) can be either of these.
        "webm" => Some("video/webm"),
        "mkv" => Some("video/x-matroska"),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
