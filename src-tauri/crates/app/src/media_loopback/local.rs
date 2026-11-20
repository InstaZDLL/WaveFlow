//! A local file the asset scope allows, served on Linux, where WebKitGTK
//! cannot play the asset protocol (see [`super`]). A **fragmented** MP4
//! (Apple's motion covers) stalls over HTTP after a couple of seconds, so
//! the first request for one rewrites it as an ordinary MP4 before serving
//! it ([`playable`]).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tauri::{AppHandle, Manager};
use waveflow_core::artwork::{motion_cache, mp4_defrag};

use super::{range_response, video_mime};
use crate::dlna::http::{build_range_body, parse_range, range_not_satisfiable};
use crate::state::AppState;

/// Answer a request for the local file `path`.
pub(super) async fn serve(app: &AppHandle, path: Option<PathBuf>, headers: &HeaderMap) -> Response {
    let Some(path) = path.filter(|p| is_plain_absolute(p)) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(mime) = video_mime(&path) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    // The same check the asset protocol makes, so this serves exactly what
    // the webview could already read — no wider, and no narrower either.
    if !app.asset_protocol_scope().is_allowed(&path) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let mut served = playable(app, &path).await;
    let mut opened = tokio::fs::File::open(&served).await;
    if opened.is_err() && served != path {
        // A converted copy evicted between the lookup and the open: the
        // lookup now sees it gone and makes it again.
        served = playable(app, &path).await;
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
    let Some((status, start, length)) = parse_range(range, total).window(total) else {
        return range_not_satisfiable(total);
    };
    let body = match build_range_body(file, start, length).await {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "media loopback: read failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    range_response(status, mime, (start, length, total), body)
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
    // Only an ISO-BMFF file is ever fragmented this way; a WebM or Matroska
    // music video is served as it is.
    if !matches!(video_mime(path), Some("video/mp4" | "video/quicktime")) {
        return path.to_path_buf();
    }
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
pub(super) fn is_plain_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}
