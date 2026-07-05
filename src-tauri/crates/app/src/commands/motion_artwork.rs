//! Animated album artwork (Phase 3) — motion covers from metadata plugins.
//!
//! The `waveflow:metadata/v1` world's `album-info` gained optional
//! `motion-cover-url` / `motion-cover-tall-url` fields at WIT 1.1.0 (Apple
//! Music-style looping video covers). This command fans a lookup out to
//! every enabled metadata plugin and returns the first motion URL found,
//! for the now-playing / immersive views to render behind the cover.
//!
//! Honours [`offline::is_offline`] — a plugin's own HTTP is already gated
//! by the host, but short-circuiting here avoids instantiating the wasm at
//! all when the network is off.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::offline;
use crate::state::AppState;

/// `app_setting` key for the opt-in local motion-artwork cache (default off).
const CACHE_ENABLED_KEY: &str = "motion_artwork.cache_enabled";

/// Hard ceiling on the on-disk motion cache (LRU-evicted down to this). 1 GiB
/// ≈ 350-500 animated covers at the ~2-3 MB H.264 1080 renditions we resolve.
const MOTION_CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Per-file cap on a downloaded motion mp4 — refuses a hostile/oversized body
/// before it reaches disk. Apple's 1080 H.264 renditions are a few MB; 64 MB
/// is generous headroom that still rejects anything absurd.
const MAX_MP4_BYTES: u64 = 64 * 1024 * 1024;

/// Per-plugin wall-clock bound on one `album-info` lookup. A cold Apple
/// resolve is a handful of sequential host HTTP GETs (each already capped
/// at 15 s by the host client); this caps the whole chain so one slow or
/// hung plugin can't stall the now-playing path. Generous because the
/// overlay is non-blocking UI (the static cover shows meanwhile) and a
/// resolved result is cached plugin-side after the first hit.
const PLUGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Resolved motion artwork for an album — the looping video URL(s) plus
/// which plugin produced them (attribution / diagnostics).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionArtwork {
    /// Square animated cover — a directly-playable progressive `.mp4`.
    /// The host renders it in a native `<video>` with no HLS.js, so a
    /// plugin with an HLS source resolves it to an mp4 rendition before
    /// returning (a bare `.m3u8` won't play on WebView2).
    pub square_url: String,
    /// Taller lock-screen variant, when the plugin offers one.
    pub tall_url: Option<String>,
    pub plugin_id: String,
}

/// Ask every enabled `waveflow:metadata` plugin for `album-info(artist,
/// album)` **concurrently** and return the first that carries a
/// `motion-cover-url`. Returns `None` when offline, when no metadata plugin
/// is installed, or when none has motion artwork for the album.
///
/// The lookups fan out (each on its own blocking task) and the first hit
/// wins; the rest are dropped. Every lookup is bounded by [`PLUGIN_TIMEOUT`],
/// so one slow or hung plugin can't stall the others or the now-playing
/// path. The pre-spawn loop only grabs each plugin's lock HANDLE (a fast
/// map op, never blocking on a contended plugin); the guard itself is taken
/// INSIDE the blocking task (`blocking_lock_owned`) so it spans the actual,
/// uncancellable work — the timeout unblocks the caller, but the guard is
/// only released when the work truly finishes, so an enable/uninstall can't
/// race an in-flight lookup. A plugin that errors, panics, or times out is
/// logged + skipped.
#[tauri::command]
pub async fn fetch_album_motion_artwork(
    state: State<'_, AppState>,
    artist: String,
    album: String,
) -> AppResult<Option<MotionArtwork>> {
    if offline::is_offline() {
        return Ok(None);
    }

    // Read the opt-in local-cache flag once up front. When on, the resolved
    // remote mp4 is downloaded into the shared LRU cache and the overlay is
    // pointed at the local file (offline + no re-download on the next play).
    let cache_locally = motion_cache_enabled(&state).await;
    let cache_dir = state.paths.motion_cache_dir.clone();

    let plugin_ids =
        super::plugins::enabled_plugin_ids_for_world(&state, "waveflow:metadata").await?;

    tracing::debug!(
        %artist,
        %album,
        plugins = plugin_ids.len(),
        cache = cache_locally,
        "resolving motion artwork"
    );

    let mut set = tokio::task::JoinSet::new();
    for plugin_id in plugin_ids {
        // Grab the per-plugin lock HANDLE only (fast map op) — the loop must
        // not block on a contended plugin. The guard is acquired inside the
        // blocking task below so it spans the real work.
        let lock_arc = super::plugins::plugin_lock_arc(&state, &plugin_id).await;
        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let artist_owned = artist.clone();
        let album_owned = album.clone();

        set.spawn(async move {
            let outcome = tokio::time::timeout(
                PLUGIN_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    // Acquire + hold the per-plugin lock for the ACTUAL
                    // duration of the (uncancellable) call. Because it lives
                    // inside the blocking closure, the guard is released only
                    // when `metadata_album_info` returns — even if the outer
                    // async timeout already fired — so an enable/uninstall
                    // can't race an in-flight lookup after an early drop.
                    let _guard = lock_arc.blocking_lock_owned();
                    waveflow_core::plugin::runtime::metadata_album_info(
                        &runtime,
                        &paths,
                        &id_owned,
                        &artist_owned,
                        &album_owned,
                    )
                }),
            )
            .await;
            (plugin_id, outcome)
        });
    }

    // Consume results as they complete; first motion cover wins (remaining
    // tasks are aborted when `set` drops on the early return).
    while let Some(joined) = set.join_next().await {
        let (plugin_id, outcome) = match joined {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%e, "motion artwork task join failed; skipping");
                continue;
            }
        };
        match outcome {
            Ok(Ok(Ok(info))) => {
                if let Some(remote_square) = info.motion_cover_url {
                    // When the local cache is on, download the mp4 and point the
                    // overlay at the on-disk copy; fall back to the remote URL if
                    // the download fails so the feature degrades gracefully.
                    let square_url = if cache_locally {
                        match cache_motion_mp4(&cache_dir, &remote_square).await {
                            Ok(path) => path.to_string_lossy().into_owned(),
                            Err(e) => {
                                tracing::warn!(%e, "motion cache download failed; serving remote url");
                                remote_square
                            }
                        }
                    } else {
                        remote_square
                    };
                    tracing::info!(
                        plugin_id,
                        %artist,
                        %album,
                        cached = cache_locally,
                        "motion artwork resolved"
                    );
                    return Ok(Some(MotionArtwork {
                        square_url,
                        tall_url: info.motion_cover_tall_url,
                        plugin_id,
                    }));
                }
                // Reached the plugin, no motion for this album — keep going.
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!(plugin_id, %e, "metadata album-info failed; skipping");
            }
            Ok(Err(e)) => {
                tracing::warn!(plugin_id, %e, "metadata lookup task panicked; skipping");
            }
            Err(_elapsed) => {
                tracing::warn!(plugin_id, "metadata lookup timed out; skipping");
            }
        }
    }

    tracing::debug!(%artist, %album, "no motion artwork from any metadata plugin");
    Ok(None)
}

// ----- local motion cache --------------------------------------------------

/// Read the opt-in local-cache flag from the shared `app.db`. Defaults to
/// `false` (off) — same bool parse convention as every other `app_setting`.
async fn motion_cache_enabled(state: &AppState) -> bool {
    sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_setting WHERE key = ?",
    )
    .bind(CACHE_ENABLED_KEY)
    .fetch_optional(&state.app_db)
    .await
    .ok()
    .flatten()
    .map(|v| v == "true" || v == "1")
    .unwrap_or(false)
}

/// Return the local path of `url`'s cached mp4, downloading it first if absent.
/// The file is hash-addressed by the (stable, per-album) source URL so the same
/// album always maps to the same file. On a hit the mtime is bumped so the LRU
/// eviction treats it as recently used; on a miss the body is streamed under
/// [`MAX_MP4_BYTES`], written atomically (`.part` → rename), and the cache is
/// evicted back under [`MOTION_CACHE_MAX_BYTES`].
async fn cache_motion_mp4(dir: &Path, url: &str) -> AppResult<PathBuf> {
    let hash = blake3::hash(url.as_bytes()).to_hex().to_string();
    let path = dir.join(format!("{hash}.mp4"));

    if path.exists() {
        // Best-effort access-LRU bump; a Windows share lock (webview reading
        // the file) just leaves the mtime at download time, which is fine.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        return Ok(path);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::Other(format!("http client: {e}")))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Other(format!("download {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(format!(
            "download {url}: HTTP {}",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_MP4_BYTES {
            return Err(AppError::Other(format!(
                "motion mp4 too large: {len} bytes (max {MAX_MP4_BYTES})"
            )));
        }
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Other(format!("read motion mp4 body: {e}")))?
    {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_MP4_BYTES {
            return Err(AppError::Other(format!(
                "motion mp4 exceeds {MAX_MP4_BYTES} bytes — refusing"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> AppResult<PathBuf> {
        std::fs::create_dir_all(&dir)?;
        // Atomic publish: write to a temp sibling then rename over the final
        // name, so a crash mid-write never leaves a truncated `.mp4` that a
        // later hit would treat as complete.
        let tmp = dir.join(format!(".{hash}.mp4.part"));
        std::fs::write(&tmp, &bytes)?;
        match std::fs::rename(&tmp, &path) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(AppError::Io(e));
            }
        }
        evict_motion_cache_lru(&dir, MOTION_CACHE_MAX_BYTES);
        Ok(path)
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?
}

/// Collect `(path, size, mtime)` for every `.mp4` in `dir`. Ignores the
/// `.part` temporaries and any non-mp4 stragglers.
fn motion_cache_entries(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((path, meta.len(), mtime));
        }
    }
    out
}

/// Delete the oldest cached mp4s (by mtime) until the total is at or under
/// `cap`. Best-effort — a file that fails to delete is left counted so a
/// locked entry can't spin the loop.
fn evict_motion_cache_lru(dir: &Path, cap: u64) {
    let mut entries = motion_cache_entries(dir);
    let mut total: u64 = entries.iter().map(|(_, size, _)| *size).sum();
    if total <= cap {
        return;
    }
    entries.sort_by_key(|(_, _, mtime)| *mtime); // oldest first
    for (path, size, _) in entries {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Total on-disk size + file count of the motion cache.
fn motion_cache_stats(dir: &Path) -> (u64, u64) {
    let entries = motion_cache_entries(dir);
    let size = entries.iter().map(|(_, s, _)| *s).sum();
    (size, entries.len() as u64)
}

/// The motion-cache toggle state + current on-disk footprint, for the
/// Settings → Plugins card.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MotionCacheInfo {
    pub enabled: bool,
    pub size_bytes: u64,
    pub file_count: u64,
}

/// Read the toggle + cache footprint for the settings UI.
#[tauri::command]
pub async fn get_motion_cache_info(state: State<'_, AppState>) -> AppResult<MotionCacheInfo> {
    let enabled = motion_cache_enabled(&state).await;
    let dir = state.paths.motion_cache_dir.clone();
    let (size_bytes, file_count) = tokio::task::spawn_blocking(move || motion_cache_stats(&dir))
        .await
        .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    Ok(MotionCacheInfo {
        enabled,
        size_bytes,
        file_count,
    })
}

/// Toggle the opt-in local motion cache. Turning it OFF does not purge the
/// existing files — that's the explicit "Clear cache" action below.
#[tauri::command]
pub async fn set_motion_cache_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(CACHE_ENABLED_KEY)
    .bind(if enabled { "true" } else { "false" })
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Delete every cached motion mp4 (and any leftover `.part` temporaries).
#[tauri::command]
pub async fn clear_motion_cache(state: State<'_, AppState>) -> AppResult<()> {
    let dir = state.paths.motion_cache_dir.clone();
    tokio::task::spawn_blocking(move || {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                let is_cache_file = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".mp4") || n.ends_with(".mp4.part"));
                if is_cache_file {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    })
    .await
    .map_err(|e| AppError::Other(format!("spawn_blocking: {e}")))?;
    Ok(())
}
