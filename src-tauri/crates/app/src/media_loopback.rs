//! Loopback HTTP for the webview's local video, on Linux.
//!
//! WebKitGTK plays `<video>` through GStreamer, and GStreamer has no
//! source for Tauri's asset protocol: every local Canvas clip and cached
//! motion cover was refused ("no URI handler implemented for asset").
//! WebKit's HTTP source plays an ordinary MP4, so those are served from
//! `127.0.0.1` instead: an ephemeral port, a token minted per launch, and
//! only video files the asset scope already lets the webview read. A
//! **fragmented** MP4 (Apple's motion covers) stalls over HTTP after a
//! couple of seconds, so the first request for one rewrites it as an
//! ordinary MP4 before serving it ([`make_playable`]). Windows and macOS
//! play the asset URL directly and never start this.
//!
//! Started on first use and kept for the life of the process. A failure to
//! bind is logged and reported as `None`; the caller shows the static cover.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use axum::extract::{RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tauri::AppHandle;
use tokio::sync::OnceCell;
use waveflow_core::artwork::mp4_defrag;

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

    make_playable(&path).await;

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

/// How much of a file is read to tell whether it is fragmented.
const HEAD_BYTES: u64 = 1024 * 1024;

/// Files already checked, with the length and mtime they had then: a file
/// that changes is checked again, one that does not is never re-read.
static CHECKED: LazyLock<Mutex<HashMap<PathBuf, (u64, SystemTime)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One conversion at a time. The requests that arrive during one wait for
/// it, then find the file checked.
static CONVERTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn is_checked(path: &Path, stamp: Option<(u64, SystemTime)>) -> bool {
    stamp.is_some()
        && CHECKED
            .lock()
            .is_ok_and(|seen| seen.get(path) == stamp.as_ref())
}

/// Rewrite `path` in place as an ordinary MP4 if it is a fragmented one.
///
/// WebKitGTK stops a fragmented MP4 served over HTTP a couple of seconds
/// in, for good; the same frames indexed up front play
/// ([`waveflow_core::artwork::mp4_defrag`]). The rewrite goes through a
/// temporary file renamed over the original, so a reader never sees half
/// a file. Every file this server hands out is one WaveFlow wrote (a
/// downloaded or chosen cover, a clip), so rewriting it is the app
/// reorganising its own copy. A file that cannot be rewritten — a
/// read-only folder — is served as it is and logged.
async fn make_playable(path: &Path) {
    if is_checked(path, stamp(path).await) {
        return;
    }
    let _converting = CONVERTING.lock().await;
    let before = stamp(path).await;
    if before.is_none() || is_checked(path, before) {
        return;
    }
    let owned = path.to_path_buf();
    match tokio::task::spawn_blocking(move || defragment_in_place(&owned)).await {
        Ok(Ok(true)) => {
            tracing::info!(path = %path.display(), "media loopback: rewrote a fragmented mp4")
        }
        Ok(Ok(false)) => {}
        Ok(Err(err)) => {
            tracing::warn!(%err, path = %path.display(), "media loopback: could not rewrite a fragmented mp4")
        }
        Err(err) => tracing::warn!(%err, "media loopback: rewrite task failed"),
    }
    // Recorded even after a failure, so a file that cannot be rewritten is
    // not re-read on every range request; a change to it is seen again.
    if let (Some(now), Ok(mut seen)) = (stamp(path).await, CHECKED.lock()) {
        seen.insert(path.to_path_buf(), now);
    }
}

/// `Ok(true)` when the file was fragmented and has been rewritten.
fn defragment_in_place(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let mut head = Vec::new();
    std::fs::File::open(path)?
        .take(HEAD_BYTES)
        .read_to_end(&mut head)?;
    if !mp4_defrag::looks_fragmented(&head) {
        return Ok(false);
    }
    let input = std::fs::read(path)?;
    let Some(output) = mp4_defrag::defragment(&input).map_err(std::io::Error::other)? else {
        return Ok(false);
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `.part`, so a crash mid-write leaves an orphan the motion cache's
    // eviction already knows to prune.
    let staged = path.with_file_name(format!(".{name}.{}.part", std::process::id()));
    let written = std::fs::write(&staged, &output).and_then(|()| std::fs::rename(&staged, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    written.map(|()| true)
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
