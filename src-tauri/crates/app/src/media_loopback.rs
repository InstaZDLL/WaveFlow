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
//! ordinary MP4 before serving it ([`playable`]). Windows and macOS
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
use tauri::{AppHandle, Manager};
use tokio::sync::OnceCell;
use waveflow_core::artwork::{motion_cache, mp4_defrag};

use crate::dlna::http::{build_range_body, parse_range};
use crate::state::AppState;

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
    if !ctx.app.asset_protocol_scope().is_allowed(&path) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut served = playable(&ctx.app, &path).await;
    let mut opened = tokio::fs::File::open(&served).await;
    if opened.is_err() && served != path {
        // A converted copy evicted between the lookup and the open: the
        // lookup now sees it gone and makes it again.
        served = playable(&ctx.app, &path).await;
        opened = tokio::fs::File::open(&served).await;
    }
    let path = served;
    let file = match opened {
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

/// Where converted copies of clips WaveFlow does not own go, under the
/// cache root, and how large that folder may grow (oldest evicted first).
const COPIES_DIR: &str = "linux_video";
const COPIES_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// The file's length and mtime.
type Stamp = (u64, SystemTime);

/// What each file resolved to — itself, or its converted copy — with the
/// stamp it had then: a file that changes is looked at again, one that
/// does not is never re-read.
static RESOLVED: LazyLock<Mutex<HashMap<PathBuf, (Stamp, PathBuf)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One conversion at a time. The requests that arrive during one wait for
/// it, then find the file resolved.
static CONVERTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn stamp(path: &Path) -> Option<Stamp> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn resolved(path: &Path, stamp: Option<Stamp>) -> Option<PathBuf> {
    let stamp = stamp?;
    let seen = RESOLVED.lock().ok()?;
    seen.get(path)
        // A copy the eviction has since removed is made again.
        .filter(|(then, served)| *then == stamp && served.is_file())
        .map(|(_, served)| served.clone())
}

/// The file to serve for `path`: itself, or an ordinary MP4 made from it
/// when it is a fragmented one.
///
/// WebKitGTK stops a fragmented MP4 served over HTTP a couple of seconds
/// in, for good; the same frames indexed up front play
/// ([`waveflow_core::artwork::mp4_defrag`]). Who owns the file decides
/// where the conversion goes. One under WaveFlow's own data or cache
/// directories — a downloaded or chosen cover, a clip the app copied
/// there — is rewritten in place, through a temporary file renamed over
/// it. Anything else, a clip in the user's music folder, is never
/// modified: the converted copy goes to [`COPIES_DIR`], keyed by the
/// file's path, length and mtime, and is served instead. A file that
/// cannot be converted is served as it is and logged.
async fn playable(app: &AppHandle, path: &Path) -> PathBuf {
    if let Some(served) = resolved(path, stamp(path).await) {
        return served;
    }
    let _converting = CONVERTING.lock().await;
    let Some(before) = stamp(path).await else {
        return path.to_path_buf();
    };
    if let Some(served) = resolved(path, Some(before)) {
        return served;
    }

    let paths = &app.state::<AppState>().paths;
    let owned = path.starts_with(&paths.root) || path.starts_with(&paths.cache_root);
    let copy = (!owned).then(|| copy_path(&paths.cache_root, path, before));
    let source = path.to_path_buf();
    let served = match tokio::task::spawn_blocking(move || convert(&source, copy.as_deref())).await
    {
        Ok(Ok(Some(converted))) => {
            tracing::info!(
                path = %path.display(),
                served = %converted.display(),
                "media loopback: converted a fragmented mp4"
            );
            converted
        }
        Ok(Ok(None)) => path.to_path_buf(),
        Ok(Err(err)) => {
            tracing::warn!(%err, path = %path.display(), "media loopback: could not convert a fragmented mp4");
            path.to_path_buf()
        }
        Err(err) => {
            tracing::warn!(%err, "media loopback: conversion task failed");
            path.to_path_buf()
        }
    };
    // An in-place rewrite changed the file, so it is recorded under the
    // stamp it has now. A copy is recorded under the stamp of the content
    // it was made from: were the file edited meanwhile, the next request
    // sees the difference and converts it again. A file that failed is
    // recorded too, so it is not re-read on every range request.
    let key = if owned {
        stamp(path).await
    } else {
        Some(before)
    };
    if let (Some(key), Ok(mut seen)) = (key, RESOLVED.lock()) {
        seen.insert(path.to_path_buf(), (key, served.clone()));
    }
    served
}

/// Where the converted copy of a file WaveFlow does not own goes: named
/// after its path, length and mtime, so an edited file gets a new copy.
fn copy_path(cache_root: &Path, path: &Path, (len, mtime): Stamp) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.as_os_str().as_bytes());
    hasher.update(&len.to_le_bytes());
    let nanos = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.update(&nanos.to_le_bytes());
    cache_root
        .join(COPIES_DIR)
        .join(format!("{}.mp4", hasher.finalize().to_hex()))
}

/// `Ok(Some(path to serve))` when the file is fragmented and has been
/// converted — into `copy` when given, in place otherwise.
fn convert(path: &Path, copy: Option<&Path>) -> std::io::Result<Option<PathBuf>> {
    if let Some(copy) = copy.filter(|c| c.is_file()) {
        // Made on an earlier launch; the bump keeps it off the eviction end.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(copy)
            .and_then(|f| f.set_modified(SystemTime::now()));
        return Ok(Some(copy.to_path_buf()));
    }
    // Headers only: an ordinary clip, however large, is never read whole.
    if !mp4_defrag::is_fragmented(&mut std::io::BufReader::new(std::fs::File::open(path)?))? {
        return Ok(None);
    }
    let input = std::fs::read(path)?;
    let Some(output) = mp4_defrag::defragment(&input).map_err(std::io::Error::other)? else {
        return Ok(None);
    };
    let target = copy.unwrap_or(path);
    let dir = target.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Room is made before the copy lands, so the eviction can never take
    // the file about to be served. A copy larger than the whole folder is
    // not made at all (no clip or cover comes near it): the original is
    // served instead, and the failure logged.
    if copy.is_some() {
        if output.len() as u64 > COPIES_MAX_BYTES {
            return Err(std::io::Error::other(
                "converted copy larger than the folder it would go to",
            ));
        }
        let room = COPIES_MAX_BYTES.saturating_sub(output.len() as u64);
        motion_cache::evict_lru(dir, room);
    }
    // `.part`, so a crash mid-write leaves an orphan the eviction, like the
    // motion cache's, knows to prune.
    let staged = dir.join(format!(".{name}.{}.part", std::process::id()));
    let written = std::fs::write(&staged, &output).and_then(|()| std::fs::rename(&staged, target));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }
    Ok(Some(target.to_path_buf()))
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
