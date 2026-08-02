//! Per-track looping Canvas (issue #442) — a short muted mp4 the user sets
//! on a single track, played behind the now-playing view Spotify-Canvas
//! style. Mirrors the manual motion-cover path (issue #408,
//! [`super::motion_artwork`]) but keyed per TRACK instead of per album, and
//! stored in its own [`AppPaths::profile_canvas_dir`] namespace.
//!
//! Core-only in spirit: nothing here touches a plugin or the network. The
//! file is picked by the user, validated by magic bytes, hash-addressed into
//! a never-evicted directory, and surfaced back to the webview as a local
//! path the `<video>` overlay converts through the asset protocol.

use std::time::Duration;

use serde::Serialize;

use tauri::State;
use waveflow_core::artwork::motion_cache;

use crate::error::AppResult;
use crate::offline;
use crate::state::AppState;

/// Per-plugin call timeout for the Canvas fanout — a hung provider must not
/// stall the now-playing path. Matches the motion-artwork budget.
const CANVAS_PLUGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Hard cap on a user-supplied Canvas clip, mirroring
/// [`super::motion_artwork`]'s manual-cover cap: a deliberately-chosen file
/// deserves a generous ceiling, but still needs *a* limit since this
/// directory is never evicted.
const MAX_CANVAS_MP4_BYTES: u64 = 64 * 1024 * 1024;

/// A track's Canvas clip, resolved to a local absolute path the webview
/// renders through `convertFileSrc`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackCanvas {
    /// Absolute path to the on-disk mp4 in the profile's `canvas/` dir.
    pub local_path: String,
}

/// Look up `track_id`'s Canvas clip, if one was set via
/// [`set_track_canvas_from_file`]. Returns `None` when the track has none —
/// callers render the static cover in that case.
#[tauri::command]
pub async fn get_track_canvas(
    state: State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<TrackCanvas>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT hash, format FROM track_canvas WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
    let Some((hash, format)) = row else {
        return Ok(None);
    };
    let path = state
        .paths
        .profile_canvas_dir(profile_id)
        .join(format!("{hash}.{format}"));
    Ok(Some(TrackCanvas {
        local_path: path.to_string_lossy().into_owned(),
    }))
}

/// Set `track_id`'s Canvas from a local mp4 file, replacing any previous
/// one. Validated by magic bytes (rejecting at pick time beats rendering a
/// black rectangle) and size-capped, then hash-addressed into the
/// never-evicted [`AppPaths::profile_canvas_dir`].
#[tauri::command]
pub async fn set_track_canvas_from_file(
    state: State<'_, AppState>,
    track_id: i64,
    file_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let canvas_dir = state.paths.profile_canvas_dir(profile_id);

    let hash =
        super::media_file::store_hash_addressed_mp4(&canvas_dir, &file_path, MAX_CANVAS_MP4_BYTES)
            .await?;

    sqlx::query(
        "INSERT INTO track_canvas (track_id, hash, format, created_at)
         VALUES (?, ?, 'mp4', ?)
         ON CONFLICT(track_id) DO UPDATE SET
            hash = excluded.hash, format = excluded.format, created_at = excluded.created_at",
    )
    .bind(track_id)
    .bind(&hash)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&*pool)
    .await?;

    Ok(())
}

/// Clear `track_id`'s Canvas, if any — the now-playing view falls back to
/// the static cover. The old file is left on disk (same "future GC pass"
/// tradeoff as the manual motion cover); a no-op when there was nothing to
/// clear.
#[tauri::command]
pub async fn clear_track_canvas(state: State<'_, AppState>, track_id: i64) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query("DELETE FROM track_canvas WHERE track_id = ?")
        .bind(track_id)
        .execute(&*pool)
        .await?;
    Ok(())
}

// ----- plugin-sourced Canvas (issue #473) ---------------------------------

/// A `canvas`-world plugin's resolved Canvas for a track. The URL is
/// **remote** — loaded by the webview `<video>` directly, like the
/// non-cached motion cover — and sits ONE rung below the manual local
/// [`TrackCanvas`] in the backdrop precedence
/// (manual > plugin > motion > slideshow > static cover).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCanvas {
    /// Directly-playable remote mp4 URL.
    pub url: String,
    /// Which plugin produced it.
    pub plugin_id: String,
}

/// Ask enabled `canvas`-world plugins for a track's Canvas, returning the
/// first hit. Resolves `None` when offline, when no canvas plugin is
/// installed, or when none has a Canvas for this track.
///
/// **Fail-soft**: a plugin error, panic, or timeout is logged and skipped,
/// never surfaced as a command error — the caller falls back down the
/// backdrop precedence (motion artwork → slideshow → static cover), so a
/// misbehaving Canvas provider can never break the now-playing view. Mirrors
/// the [`super::motion_artwork`] fanout (per-plugin lock + blocking task +
/// timeout), minus the local cache: a Canvas URL is ephemeral and rendered
/// straight by the webview.
#[tauri::command]
pub async fn fetch_track_canvas(
    state: State<'_, AppState>,
    artist: String,
    title: String,
    album: Option<String>,
    duration_ms: Option<u32>,
) -> AppResult<Option<PluginCanvas>> {
    if offline::is_offline() {
        return Ok(None);
    }

    let plugin_ids =
        super::plugins::enabled_plugin_ids_for_world(&state, "waveflow:canvas").await?;
    if plugin_ids.is_empty() {
        return Ok(None);
    }

    let mut set = tokio::task::JoinSet::new();
    for plugin_id in plugin_ids {
        // Grab the per-plugin lock HANDLE only (fast map op); the guard is
        // acquired inside the blocking task so it spans the real call and an
        // enable/uninstall can't race an in-flight lookup.
        let lock_arc = super::plugins::plugin_lock_arc(&state, &plugin_id).await;
        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let artist_owned = artist.clone();
        let title_owned = title.clone();
        let album_owned = album.clone();

        set.spawn(async move {
            let outcome = tokio::time::timeout(
                CANVAS_PLUGIN_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    let _guard = lock_arc.blocking_lock_owned();
                    waveflow_core::plugin::runtime::canvas_track_canvas(
                        &runtime,
                        &paths,
                        &id_owned,
                        &artist_owned,
                        &title_owned,
                        album_owned.as_deref(),
                        duration_ms,
                    )
                }),
            )
            .await;
            (plugin_id, outcome)
        });
    }

    // First safe hit wins; remaining tasks abort when `set` drops.
    while let Some(joined) = set.join_next().await {
        let (plugin_id, outcome) = match joined {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%e, "canvas task join failed; skipping");
                continue;
            }
        };
        match outcome {
            Ok(Ok(Ok(Some(canvas)))) => {
                // SSRF guard: the URL is handed to the webview `<video>`, so
                // reject a non-https / internal / loopback target before it
                // leaves the backend. Reuses the shared media-URL safety
                // check the motion cover uses (one primitive, no drift).
                if !motion_cache::is_safe_motion_url(&canvas.url) {
                    tracing::warn!(plugin_id, "canvas plugin returned an unsafe url; skipping");
                    continue;
                }
                return Ok(Some(PluginCanvas {
                    url: canvas.url,
                    plugin_id,
                }));
            }
            Ok(Ok(Ok(None))) => { /* this plugin has no Canvas for the track */ }
            Ok(Ok(Err(e))) => tracing::warn!(plugin_id, %e, "canvas plugin failed; skipping"),
            Ok(Err(e)) => tracing::warn!(plugin_id, %e, "canvas task panicked; skipping"),
            Err(_) => tracing::warn!(plugin_id, "canvas plugin timed out; skipping"),
        }
    }

    Ok(None)
}
