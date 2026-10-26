//! Playlist CRUD commands.
//!
//! Mirrors [`super::library`] but targets the `playlist` / `playlist_track`
//! tables. A playlist is an ordered, user-curated collection of tracks that
//! can cross library boundaries — the track rows themselves still live under
//! a `library_id`, the playlist just points at them through `playlist_track`.
//!
//! All mutations bump `playlist.updated_at` so the sidebar (which orders
//! playlists by `updated_at DESC` as a tie-break) reflects recent edits.

use chrono::Utc;
use sqlx::FromRow;

use waveflow_core::repository::{
    playlist::{PlaylistDraft, PlaylistRepository, PlaylistUpdate},
    sqlite::{
        playlist::{
            append_track_conn, append_tracks_conn, delete_conn, insert_custom_conn,
            remove_track_conn, reorder_track_conn, update_conn,
        },
        SqlitePlaylistRepository, SqliteTrackRepository,
    },
    track::{TrackRepository, TrackSource},
};

use crate::{
    error::{AppError, AppResult},
    state::{AppState, Leased},
};
// `Playlist` + input DTOs moved to `waveflow_core::domain::playlist` in
// the Phase 1.a refactor. Re-exported so existing call sites
// (`crate::commands::playlist::Playlist`) keep resolving.
pub use waveflow_core::domain::playlist::{CreatePlaylistInput, Playlist, UpdatePlaylistInput};

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// Build a playlist repository over the active profile's pool. Wrapped
/// in [`Leased`] for the same reason as `library::library_repo` — the
/// lease must outlive the repository (issue #332).
async fn playlist_repo(state: &AppState) -> AppResult<Leased<SqlitePlaylistRepository>> {
    let (pool, lease) = state.require_profile_pool().await?.into_parts();
    Ok(Leased::new(SqlitePlaylistRepository::new(pool), lease))
}

/// Resolve `cover_hash` to an absolute on-disk path if (and only if) the
/// file is present in the shared metadata cache. Mutates the playlist in
/// place — kept as a free function so both list and detail queries share
/// the resolver without duplicating the path glue.
fn resolve_cover_path(p: &mut Playlist, paths: &crate::paths::AppPaths) {
    if let Some(hash) = p.cover_hash.as_deref() {
        p.cover_path = crate::metadata_artwork::existing_path(&paths.metadata_artwork_dir, hash);
    }
}

/// List every playlist in the active profile, ordered by `position` first
/// (for future manual reordering) then `updated_at DESC` as a tie-break so
/// recently-edited playlists float to the top by default.
#[tauri::command]
pub async fn list_playlists(state: tauri::State<'_, AppState>) -> AppResult<Vec<Playlist>> {
    let mut playlists = playlist_repo(&state).await?.list_all_with_counts().await?;
    for p in &mut playlists {
        resolve_cover_path(p, &state.paths);
    }
    Ok(playlists)
}

/// Fetch a single playlist by id. Used by the PlaylistView header.
#[tauri::command]
pub async fn get_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<Playlist> {
    let mut playlist = playlist_repo(&state)
        .await?
        .get_with_counts(playlist_id)
        .await?
        .ok_or_else(|| {
            AppError::Other(format!(
                "playlist {playlist_id} not found in active profile"
            ))
        })?;
    resolve_cover_path(&mut playlist, &state.paths);
    Ok(playlist)
}

/// Create a new playlist. Follows the same defaults as
/// [`CreatePlaylistModal`](../../../../src/components/common/CreatePlaylistModal.tsx):
/// violet color, music icon.
#[tauri::command]
pub async fn create_playlist(
    state: tauri::State<'_, AppState>,
    input: CreatePlaylistInput,
) -> AppResult<Playlist> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Other("playlist name cannot be empty".into()));
    }
    let color_id = input.color_id.unwrap_or_else(|| "violet".to_string());
    let color_mode = input.color_mode.unwrap_or_else(|| "auto".to_string());
    if color_mode != "auto" && color_mode != "manual" {
        return Err(AppError::Other("invalid playlist color mode".into()));
    }
    let icon_id = input.icon_id.unwrap_or_else(|| "music".to_string());
    let now = now_millis();

    let draft = PlaylistDraft {
        name: name.clone(),
        description: input.description.clone(),
        color_id: color_id.clone(),
        color_mode: color_mode.clone(),
        icon_id: icon_id.clone(),
        now_ms: now,
    };

    // Atomic write + outbox enqueue (issue #193). The playlist
    // INSERT and the matching `sync_pending_op` row land in the
    // same SQLite transaction; either both commit or both roll
    // back. Closes the drift window the fire-and-forget
    // `enqueue_op` had between two consecutive commits.
    let pool = state.require_profile_pool().await?;
    let mut tx = pool.begin().await?;
    let id = insert_custom_conn(&mut tx, &draft).await?;
    // Mint the canonical id INSIDE the same tx so the playlist row +
    // sync_id_map row + outbox op all reference the same UUID. Other
    // devices reading the broadcast will route entity_id through
    // their own sync_id_map to find the local rowid.
    let canonical = crate::sync::canonical::ensure_local_playlist(&mut tx, id).await?;
    let stamp = crate::sync::hooks::enqueue_op_in_tx(
        &mut tx,
        &crate::sync::hooks::PendingOpDraft {
            entity: "playlist".into(),
            entity_id: canonical,
            field: None,
            op: "insert".into(),
            payload: Some(serde_json::json!({
                "name": name,
                "description": input.description,
                "color_id": color_id,
                "color_mode": color_mode,
                "icon_id": icon_id,
            })),
        },
    )
    .await?;
    // Phase B.0 — stamp the playlist row with the queued op's HLC +
    // payload_hash so the desktop's metadata_digest reflects the
    // INSERT. Only fires when `enqueue_op_in_tx` actually wrote (sync
    // gate / SyncMode::Local short-circuits return `None`); the row
    // INSERT itself proceeds either way.
    if let Some(stamp) = stamp {
        if let Some(fields) = crate::sync::payload::playlist::fields_from_row(&mut tx, id).await? {
            crate::sync::payload::playlist::stamp_in_tx(&mut tx, id, fields, stamp).await?;
        }
    }
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    Ok(Playlist {
        id,
        // Desktop single-tenant — profile boundary is the database
        // file itself. `0` is the sentinel waveflow-core's
        // `Playlist.profile_id` doc-comment defines for this side.
        profile_id: 0,
        name,
        description: input.description,
        color_id,
        color_mode,
        icon_id,
        is_smart: 0,
        cover_hash: None,
        cover_path: None,
        cover_is_auto: 1,
        position: 0,
        created_at: now,
        updated_at: now,
        track_count: 0,
        total_duration_ms: 0,
        placeholder_count: 0,
        smart_rules: None,
    })
}

/// Partial update — name/description/color/icon. Bumps `updated_at`.
#[tauri::command]
pub async fn update_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    input: UpdatePlaylistInput,
) -> AppResult<()> {
    let trimmed_name = input.name.as_ref().map(|s| s.trim().to_string());
    if input
        .color_mode
        .as_deref()
        .is_some_and(|mode| mode != "auto" && mode != "manual")
    {
        return Err(AppError::Other("invalid playlist color mode".into()));
    }
    if let Some(name) = &trimmed_name {
        if name.is_empty() {
            return Err(AppError::Other("playlist name cannot be empty".into()));
        }
    }

    let patch = PlaylistUpdate {
        name: trimmed_name.clone(),
        description: input.description.clone(),
        color_id: input.color_id.clone(),
        color_mode: input.color_mode.clone(),
        icon_id: input.icon_id.clone(),
    };

    // Atomic existence-check + update + per-field outbox ops in one
    // tx. `update_conn` now returns the rows-affected boolean so a
    // concurrent delete between the pre-1.f exists() probe and the
    // UPDATE can't slip an enqueue past — if no row was touched we
    // drop the tx (auto-rollback on drop) and return the same 404-
    // style error the previous shape did, but without the race
    // window. One Lamport bump per supplied field so the server can
    // replay them in order against concurrent updates from another
    // device.
    let pool = state.require_profile_pool().await?;
    let mut tx = pool.begin().await?;
    let updated = update_conn(&mut tx, playlist_id, &patch, now_millis()).await?;
    if !updated {
        return Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        )));
    }

    // Resolve the playlist's canonical id so the outbox row carries
    // the cross-device identifier rather than the local rowid. A
    // playlist without a canonical (pre-1.f.desktop.4b row that
    // dodged the migration backfill — shouldn't happen, but the
    // fallback keeps the tx atomic) gets one minted here.
    let entity_id = crate::sync::canonical::ensure_local_playlist(&mut tx, playlist_id).await?;
    let mut last_stamp: Option<crate::sync::hooks::EnqueuedStamp> = None;
    for (field, value) in [
        ("name", trimmed_name.map(serde_json::Value::String)),
        (
            "description",
            input.description.map(serde_json::Value::String),
        ),
        ("color_id", input.color_id.map(serde_json::Value::String)),
        (
            "color_mode",
            input.color_mode.map(serde_json::Value::String),
        ),
        ("icon_id", input.icon_id.map(serde_json::Value::String)),
    ] {
        if let Some(value) = value {
            let stamp = crate::sync::hooks::enqueue_op_in_tx(
                &mut tx,
                &crate::sync::hooks::PendingOpDraft {
                    entity: "playlist".into(),
                    entity_id: entity_id.clone(),
                    field: Some(field.into()),
                    op: "set".into(),
                    payload: Some(serde_json::json!({ "value": value })),
                },
            )
            .await?;
            if let Some(s) = stamp {
                last_stamp = Some(s);
            }
        }
    }
    // Phase B.0 — stamp once at the end with the latest queued op's
    // HLC, reading the canonical fields from the persisted row so the
    // hash reflects what's actually on disk (defence-in-depth against
    // future normalisation in `update_conn`).
    if let Some(stamp) = last_stamp {
        if let Some(fields) =
            crate::sync::payload::playlist::fields_from_row(&mut tx, playlist_id).await?
        {
            crate::sync::payload::playlist::stamp_in_tx(&mut tx, playlist_id, fields, stamp)
                .await?;
        }
    }
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    Ok(())
}

/// Delete a playlist. `ON DELETE CASCADE` on `playlist_track` removes the
/// track links, but the underlying `track` rows are preserved — they still
/// belong to their library.
#[tauri::command]
pub async fn delete_playlist(state: tauri::State<'_, AppState>, playlist_id: i64) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let mut tx = pool.begin().await?;
    // Resolve canonical BEFORE the DELETE — the row (and any
    // canonical_id column on it) is gone after delete_conn.
    let canonical = crate::sync::canonical::canonical_for_local(
        &mut tx,
        crate::sync::canonical::ENTITY_PLAYLIST,
        playlist_id,
    )
    .await?;
    if !delete_conn(&mut tx, playlist_id).await? {
        return Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        )));
    }
    // Drop the mapping row in the same tx so a future inbound op
    // referencing the same canonical doesn't get routed to a
    // dangling rowid. Falls back to the local rowid as `entity_id`
    // when the mapping was missing (pre-1.f.desktop.4b row that
    // dodged the migration backfill — shouldn't happen, but the
    // fallback keeps the tx shape consistent).
    let entity_id = if let Some(ref c) = canonical {
        crate::sync::canonical::drop_mapping(&mut tx, crate::sync::canonical::ENTITY_PLAYLIST, c)
            .await?;
        c.clone()
    } else {
        playlist_id.to_string()
    };
    let stamp = crate::sync::hooks::enqueue_op_in_tx(
        &mut tx,
        &crate::sync::hooks::PendingOpDraft {
            entity: "playlist".into(),
            entity_id,
            field: None,
            op: "delete".into(),
            payload: None,
        },
    )
    .await?;
    // Phase B.0 — bump the playlist digest counter on delete. No row
    // to stamp (the DELETE already ran), but the set member's removal
    // still has to be visible to the backfill protocol.
    if stamp.is_some() {
        crate::sync::payload::bump_digest_in_tx(&mut tx, "playlist").await?;
    }
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();
    tracing::info!(playlist_id, "playlist deleted");
    Ok(())
}

/// List every track of a playlist in its stored order. Mirrors the SELECT in
/// [`super::track::list_tracks`] with an extra `JOIN playlist_track` so the
/// ordering follows the user's arrangement (`pt.position ASC`) instead of
/// the alphabetical artist/album/disc/track sort.
#[tauri::command]
pub async fn list_playlist_tracks(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<crate::commands::track::ListTracksResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let rows = SqliteTrackRepository::new((*pool).clone())
        .list_in_playlist(playlist_id)
        .await?;

    // Same blocking-pool offload as `list_tracks` — large playlists
    // (the Liked Songs pseudo-playlist routinely runs 800+ rows on a
    // healthy library) would otherwise stall the runtime on per-row
    // `Path::exists` thumbnail probes.
    let artwork_dir_for_blocking = artwork_dir.clone();
    let items = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|row| {
                crate::commands::track::track_list_item_from_row(row, &artwork_dir_for_blocking)
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("list_playlist_tracks join: {e}")))?;

    Ok(crate::commands::track::ListTracksResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Return the IDs of every user playlist that currently contains `track_id`.
/// Smart playlists are excluded — their membership is computed on the fly
/// from rules and would be misleading to expose as a toggle target.
///
/// Used by the `+` popover to render a checkmark on rows the track is
/// already in (and to flip the click handler from "add" to "remove").
#[tauri::command]
pub async fn list_playlists_containing_track(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Vec<i64>> {
    Ok(playlist_repo(&state)
        .await?
        .list_user_playlists_containing(track_id)
        .await?)
}

/// Refuse to add tracks to a smart playlist. Its tracks are rebuilt from
/// its rules on every regeneration, so a hand-added track would be
/// accepted, shown, and then silently dropped. The menus no longer offer
/// smart playlists; this is the guard for every other way in.
async fn refuse_smart_playlist(
    conn: &mut sqlx::SqliteConnection,
    playlist_id: i64,
) -> AppResult<()> {
    let is_smart: Option<i64> = sqlx::query_scalar("SELECT is_smart FROM playlist WHERE id = ?")
        .bind(playlist_id)
        .fetch_optional(&mut *conn)
        .await?;
    if is_smart == Some(1) {
        return Err(AppError::Other(
            "tracks cannot be added to a smart playlist: it is rebuilt from its rules".into(),
        ));
    }
    Ok(())
}

/// Append a single track to the end of a playlist. Idempotent — if the track
/// is already in the playlist the existing row is preserved and `updated_at`
/// is still bumped so the UI reflects the user's intent.
#[tauri::command]
pub async fn add_track_to_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let now = now_millis();

    let mut tx = pool.begin().await?;
    refuse_smart_playlist(&mut tx, playlist_id).await?;
    append_track_conn(&mut tx, playlist_id, track_id, now).await?;
    let entity_id = crate::sync::canonical::ensure_local_playlist(&mut tx, playlist_id).await?;
    // Phase 1.j.b — fold per-track snapshots into the outbound
    // payload so the server's `playlist_track.snapshot_*` columns
    // land populated and the public share preview can render the
    // track without resolving the local-i64 id cross-device.
    let snapshots = crate::sync::track_snapshots::build_snapshots(&mut tx, &[track_id]).await?;
    crate::sync::hooks::enqueue_op_in_tx(
        &mut tx,
        &crate::sync::hooks::PendingOpDraft {
            entity: "playlist".into(),
            entity_id,
            field: Some("tracks".into()),
            op: "insert".into(),
            payload: Some(serde_json::json!({
                "track_ids": [track_id],
                "snapshots": snapshots,
            })),
        },
    )
    .await?;
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    // Cover regen runs OUTSIDE the tx — it does its own pool read +
    // a filesystem-level rasterise, neither of which belongs in the
    // atomic write window.
    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
    Ok(())
}

/// Bulk variant of [`add_track_to_playlist`]. Inserts every track one by one
/// (so positions stay contiguous even if some are duplicates) and returns
/// the count that were actually inserted.
#[tauri::command]
pub async fn add_tracks_to_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_ids: Vec<i64>,
) -> AppResult<u32> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let now = now_millis();

    let mut tx = pool.begin().await?;
    refuse_smart_playlist(&mut tx, playlist_id).await?;
    let inserted = append_tracks_conn(&mut tx, playlist_id, &track_ids, now).await?;
    let entity_id = crate::sync::canonical::ensure_local_playlist(&mut tx, playlist_id).await?;
    // Phase 1.j.b — per-track snapshots for the public share
    // preview. See [`add_track_to_playlist`] for the rationale.
    let snapshots = crate::sync::track_snapshots::build_snapshots(&mut tx, &track_ids).await?;
    // One coalesced op for the whole batch — emitting N ops would
    // cost N Lamport draws and bloat the queue without giving the
    // server side any extra signal.
    crate::sync::hooks::enqueue_op_in_tx(
        &mut tx,
        &crate::sync::hooks::PendingOpDraft {
            entity: "playlist".into(),
            entity_id,
            field: Some("tracks".into()),
            op: "insert".into(),
            payload: Some(serde_json::json!({
                "track_ids": track_ids,
                "snapshots": snapshots,
            })),
        },
    )
    .await?;
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
    Ok(inserted)
}

/// Remove a single track and renumber the tail so positions stay contiguous.
#[tauri::command]
pub async fn remove_track_from_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;

    let mut tx = pool.begin().await?;
    let removed = remove_track_conn(&mut tx, playlist_id, track_id, now_millis()).await?;
    // Only enqueue when the local DELETE actually touched a row —
    // otherwise a `remove` op against a track that wasn't in the
    // playlist (concurrent removal, double-click, stale UI state)
    // would replay on the server and drop a row that legitimately
    // belonged there. The tx still commits so the no-op stays
    // idempotent from the caller's POV.
    if removed {
        let entity_id = crate::sync::canonical::ensure_local_playlist(&mut tx, playlist_id).await?;
        crate::sync::hooks::enqueue_op_in_tx(
            &mut tx,
            &crate::sync::hooks::PendingOpDraft {
                entity: "playlist".into(),
                entity_id,
                field: Some("tracks".into()),
                op: "delete".into(),
                payload: Some(serde_json::json!({ "track_ids": [track_id] })),
            },
        )
        .await?;
    }
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    if removed {
        super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
            .await;
    }
    Ok(())
}

/// Move a track to a new absolute position inside a playlist, shifting
/// the surrounding rows so positions stay dense. Used by the
/// drag-and-drop UI. `new_position` is clamped to `[0, length - 1]`
/// so an out-of-range drop snaps to the nearest end instead of erroring.
///
/// `playlist_track.position` is non-UNIQUE (just an index for ORDER BY)
/// so the shift is a single bulk UPDATE per direction; no offset
/// gymnastics needed unlike the queue's UNIQUE-positioned variant.
#[tauri::command]
pub async fn reorder_playlist_track(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    track_id: i64,
    new_position: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;

    // Atomic move + outbox enqueue in one tx. `reorder_track_conn`
    // returns the effective position (the value after the repo's
    // internal clamp) so the sync payload matches the row's actual
    // new state — closes the divergence path #192 documented when
    // sending the raw `new_position` to the server.
    let mut tx = pool.begin().await?;
    let effective =
        reorder_track_conn(&mut tx, playlist_id, track_id, new_position, now_millis()).await?;
    let Some(position_for_sync) = effective else {
        return Err(AppError::Other(format!(
            "track {track_id} not in playlist {playlist_id}"
        )));
    };
    let entity_id = crate::sync::canonical::ensure_local_playlist(&mut tx, playlist_id).await?;
    crate::sync::hooks::enqueue_op_in_tx(
        &mut tx,
        &crate::sync::hooks::PendingOpDraft {
            entity: "playlist".into(),
            entity_id,
            field: Some("tracks".into()),
            op: "set".into(),
            payload: Some(serde_json::json!({
                "track_id": track_id,
                "position": position_for_sync,
            })),
        },
    )
    .await?;
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
    Ok(())
}

/// Add every available track matching a source (folder, album, artist) to a
/// playlist in one server-side transaction — avoids round-tripping thousands
/// of track IDs through the IPC bridge.
///
/// `source_type` must be one of `"folder"`, `"album"`, `"artist"`.
/// Returns the number of tracks actually inserted (duplicates are skipped).
#[tauri::command]
pub async fn add_source_to_playlist(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    source_type: String,
    source_id: i64,
) -> AppResult<u32> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;

    let source = match source_type.as_str() {
        "folder" => TrackSource::Folder(source_id),
        "album" => TrackSource::Album(source_id),
        "artist" => TrackSource::Artist(source_id),
        other => {
            return Err(AppError::Other(format!(
                "unknown source_type '{other}', expected folder/album/artist"
            )));
        }
    };
    let track_ids = SqliteTrackRepository::new(pool.clone())
        .list_ids_in_source(source)
        .await?;

    let mut tx = pool.begin().await?;
    refuse_smart_playlist(&mut tx, playlist_id).await?;
    let inserted = append_tracks_conn(&mut tx, playlist_id, &track_ids, now_millis()).await?;
    let entity_id = crate::sync::canonical::ensure_local_playlist(&mut tx, playlist_id).await?;
    // Phase 1.j.b — per-track snapshots for the public share
    // preview.
    let snapshots = crate::sync::track_snapshots::build_snapshots(&mut tx, &track_ids).await?;
    crate::sync::hooks::enqueue_op_in_tx(
        &mut tx,
        &crate::sync::hooks::PendingOpDraft {
            entity: "playlist".into(),
            entity_id,
            field: Some("tracks".into()),
            op: "insert".into(),
            payload: Some(serde_json::json!({
                "track_ids": track_ids,
                "snapshots": snapshots,
                "via_source": { "type": source_type, "id": source_id },
            })),
        },
    )
    .await?;
    tx.commit().await?;
    // Wake the drain task so a chatty user's edits don't wait the
    // full 30 s tick before reaching the server.
    state.drain.notify();

    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
    Ok(inserted)
}

// ── M3U / M3U8 export ───────────────────────────────────────────────
//
// Plain-text playlist exchange so users can move between WaveFlow and
// foobar2000 / VLC / Rekordbox / car stereos. Format:
//
//   #EXTM3U
//   #PLAYLIST:<name>
//   #EXTINF:<seconds>,<artist> - <title>
//   <absolute path>
//
// We always write UTF-8 (.m3u8). Importing — M3U and the other formats —
// lives in `playlist_import`. A placeholder has no file, so it has no line
// here: an M3U entry is a path.

/// Write the active playlist out as a UTF-8 .m3u8 file at `dest_path`.
/// Caller (frontend) is responsible for picking the destination via
/// the native save dialog and supplying an absolute path.
#[tauri::command]
pub async fn export_playlist_m3u(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
    dest_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let name = SqlitePlaylistRepository::new(pool.clone())
        .get_name(playlist_id)
        .await?
        .ok_or_else(|| {
            AppError::Other(format!(
                "playlist {playlist_id} not found in active profile"
            ))
        })?;

    // Custom projection for the export — small enough that it doesn't
    // earn its own repository method.
    #[derive(FromRow)]
    struct ExportRow {
        title: String,
        artist_name: Option<String>,
        duration_ms: i64,
        file_path: String,
    }

    let rows = sqlx::query_as::<_, ExportRow>(
        r#"
        SELECT t.title,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               t.duration_ms,
               t.file_path
          FROM playlist_track pt
          JOIN track t ON t.id = pt.track_id
         WHERE pt.playlist_id = ?
         ORDER BY pt.position ASC
        "#,
    )
    .bind(playlist_id)
    .fetch_all(&*pool)
    .await?;

    let mut out = String::with_capacity(rows.len() * 200 + 64);
    out.push_str("#EXTM3U\n");
    out.push_str(&format!("#PLAYLIST:{}\n", name.replace(['\r', '\n'], " ")));
    for row in &rows {
        let secs = (row.duration_ms / 1000).max(0);
        let artist = row.artist_name.as_deref().unwrap_or("").trim();
        let display = if artist.is_empty() {
            row.title.clone()
        } else {
            format!("{artist} - {}", row.title)
        };
        let display = display.replace(['\r', '\n'], " ");
        out.push_str(&format!("#EXTINF:{secs},{display}\n"));
        out.push_str(&row.file_path);
        out.push('\n');
    }

    // Through a partial file renamed into place, so overwriting an
    // existing playlist file that fails halfway (a full disk, a share
    // that drops) leaves the old one intact rather than truncated. On
    // the blocking pool: the destination is wherever the user picked,
    // network shares included, and the parent may need creating.
    let dest = std::path::PathBuf::from(&dest_path);
    {
        let dest = dest.clone();
        tokio::task::spawn_blocking(move || {
            waveflow_core::atomic_write::write_bytes(&dest, out.as_bytes())
        })
        .await
        .map_err(|e| AppError::Other(format!("write m3u join: {e}")))?
        .map_err(|e| AppError::Other(format!("write m3u file: {e}")))?;
    }

    tracing::info!(
        playlist_id,
        path = %dest.display(),
        tracks = rows.len(),
        "playlist exported as m3u8"
    );
    Ok(())
}
