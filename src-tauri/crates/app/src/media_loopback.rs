//! Loopback HTTP for the webview's local video, on Linux.
//!
//! WebKitGTK plays `<video>` through GStreamer, and GStreamer has no
//! source for Tauri's asset protocol: every local Canvas clip and cached
//! motion cover was refused ("no URI handler implemented for asset").
//! WebKit's HTTP source plays an ordinary MP4, so those are served from
//! `127.0.0.1` instead: an ephemeral port, a token minted per launch, and
//! only video files the asset scope already lets the webview read. A
//! **fragmented** MP4 (Apple's motion covers) stalls over HTTP after a
//! couple of seconds, so the frontend hands those to MediaSource and never
//! asks this server for them (`usePlayableVideo.ts`). Windows and macOS
//! play the asset URL directly and never start this.
//!
//! Started on first use and kept for the life of the process. A failure to
//! bind is logged and reported as `None`; the caller shows the static cover.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tauri::AppHandle;
use tokio::sync::OnceCell;

use crate::dlna::http::{build_range_body, parse_range};

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
    let (mut token, mut path) = (None, None);
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "token" => token = Some(value.into_owned()),
            "path" => path = Some(PathBuf::from(value.into_owned())),
            _ => {}
        }
    }
    if token.as_deref() != Some(ctx.token.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(path) = path.filter(|p| is_plain_absolute(p)) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(mime) = video_mime(&path) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    // The same check the asset protocol makes, so this serves exactly what
    // the webview could already read — no wider, and no narrower either.
    if !tauri::Manager::asset_protocol_scope(&ctx.app).is_allowed(&path) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let file = match tokio::fs::File::open(&path).await {
        Ok(file) => file,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let total = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match range.and_then(|r| parse_range(r, total)) {
        Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end),
        None => (StatusCode::OK, 0, total.saturating_sub(1)),
    };
    let length = if total == 0 {
        0
    } else {
        end.saturating_sub(start) + 1
    };
    let body = match build_range_body(file, start, length).await {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "media loopback: read failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut out = HeaderMap::new();
    out.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    out.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(value) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
            out.insert(header::CONTENT_RANGE, value);
        }
    }
    (status, out, body).into_response()
}

/// Absolute, and no `..`: a scope pattern like `profiles/**` is matched as
/// text, so a parent component could otherwise climb out of it.
fn is_plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// Only the containers a clip or a motion cover can be; anything else in
/// the scope (covers, artist images) is not this server's to hand out.
fn video_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_plain_absolute, video_mime};
    use std::path::Path;

    #[test]
    fn only_plain_absolute_paths_are_accepted() {
        assert!(is_plain_absolute(Path::new("/home/a/canvas/x.mp4")));
        assert!(!is_plain_absolute(Path::new("/home/a/../../etc/x.mp4")));
        assert!(!is_plain_absolute(Path::new("relative/x.mp4")));
    }

    #[test]
    fn only_video_containers_are_served() {
        assert_eq!(video_mime(Path::new("/a/clip.MP4")), Some("video/mp4"));
        assert_eq!(
            video_mime(Path::new("/a/clip.mov")),
            Some("video/quicktime")
        );
        assert_eq!(video_mime(Path::new("/a/cover.jpg")), None);
        assert_eq!(video_mime(Path::new("/a/noext")), None);
    }
}
