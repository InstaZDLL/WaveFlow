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
    sqlite::SqlitePlaylistRepository,
};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};
// `Playlist` + input DTOs moved to `waveflow_core::domain::playlist` in
// the Phase 1.a refactor. Re-exported so existing call sites
// (`crate::commands::playlist::Playlist`) keep resolving.
pub use waveflow_core::domain::playlist::{CreatePlaylistInput, Playlist, UpdatePlaylistInput};

fn now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

async fn playlist_repo(state: &AppState) -> AppResult<SqlitePlaylistRepository> {
    Ok(SqlitePlaylistRepository::new(
        state.require_profile_pool().await?,
    ))
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
    let icon_id = input.icon_id.unwrap_or_else(|| "music".to_string());
    let now = now_millis();

    let draft = PlaylistDraft {
        name: name.clone(),
        description: input.description.clone(),
        color_id: color_id.clone(),
        icon_id: icon_id.clone(),
        now_ms: now,
    };
    let id = playlist_repo(&state).await?.insert_custom(&draft).await?;

    Ok(Playlist {
        id,
        name,
        description: input.description,
        color_id,
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
    let repo = playlist_repo(&state).await?;

    // Precise error for missing id instead of a silent "0 rows updated".
    if !repo.exists(playlist_id).await? {
        return Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        )));
    }

    let trimmed_name = input.name.as_ref().map(|s| s.trim().to_string());
    if let Some(name) = &trimmed_name {
        if name.is_empty() {
            return Err(AppError::Other("playlist name cannot be empty".into()));
        }
    }

    let patch = PlaylistUpdate {
        name: trimmed_name,
        description: input.description,
        color_id: input.color_id,
        icon_id: input.icon_id,
    };
    repo.update(playlist_id, &patch, now_millis()).await?;
    Ok(())
}

/// Delete a playlist. `ON DELETE CASCADE` on `playlist_track` removes the
/// track links, but the underlying `track` rows are preserved — they still
/// belong to their library.
#[tauri::command]
pub async fn delete_playlist(state: tauri::State<'_, AppState>, playlist_id: i64) -> AppResult<()> {
    if !playlist_repo(&state).await?.delete(playlist_id).await? {
        return Err(AppError::Other(format!(
            "playlist {playlist_id} not found in active profile"
        )));
    }
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

    // Stays inline until step 5.d migrates `TrackRow` into core/repository
    // alongside the rest of the TrackRepository.
    let rows = sqlx::query_as::<_, crate::commands::track::TrackRow>(
        r#"
        SELECT t.id, t.library_id, t.title,
               t.album_id,
               al.title AS album_title,
               t.primary_artist AS artist_id,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               (SELECT GROUP_CONCAT(id, ',') FROM (
                  SELECT ta2.artist_id AS id FROM track_artist ta2
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_ids,
               t.duration_ms, t.track_number, t.disc_number, t.year,
               t.bitrate, t.sample_rate, t.channels,
               t.bit_depth, t.codec, t.musical_key,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format,
               t.rating  AS rating
          FROM playlist_track pt
          JOIN track   t  ON t.id  = pt.track_id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE pt.playlist_id = ? AND t.is_available = 1
         ORDER BY pt.position ASC
        "#,
    )
    .bind(playlist_id)
    .fetch_all(&pool)
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

    SqlitePlaylistRepository::new(pool.clone())
        .append_track(playlist_id, track_id, now)
        .await?;

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

    let inserted = SqlitePlaylistRepository::new(pool.clone())
        .append_tracks(playlist_id, &track_ids, now)
        .await?;

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

    SqlitePlaylistRepository::new(pool.clone())
        .remove_track(playlist_id, track_id, now_millis())
        .await?;

    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
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

    let moved = SqlitePlaylistRepository::new(pool.clone())
        .reorder_track(playlist_id, track_id, new_position, now_millis())
        .await?;
    if !moved {
        return Err(AppError::Other(format!(
            "track {track_id} not in playlist {playlist_id}"
        )));
    }

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

    // Source-resolution still uses inline SQL — moves into the future
    // `TrackRepository::list_ids_for_source` in step 5.d.
    let track_ids: Vec<i64> = match source_type.as_str() {
        "folder" => {
            sqlx::query_scalar(
                "SELECT id FROM track WHERE folder_id = ? AND is_available = 1
                 ORDER BY disc_number, track_number, title COLLATE NOCASE",
            )
            .bind(source_id)
            .fetch_all(&pool)
            .await?
        }
        "album" => {
            sqlx::query_scalar(
                "SELECT id FROM track WHERE album_id = ? AND is_available = 1
                 ORDER BY disc_number, track_number, title COLLATE NOCASE",
            )
            .bind(source_id)
            .fetch_all(&pool)
            .await?
        }
        "artist" => {
            sqlx::query_scalar(
                "SELECT id FROM track WHERE primary_artist = ? AND is_available = 1
                 ORDER BY title COLLATE NOCASE",
            )
            .bind(source_id)
            .fetch_all(&pool)
            .await?
        }
        other => {
            return Err(AppError::Other(format!(
                "unknown source_type '{other}', expected folder/album/artist"
            )));
        }
    };

    let inserted = SqlitePlaylistRepository::new(pool.clone())
        .append_tracks(playlist_id, &track_ids, now_millis())
        .await?;

    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
    Ok(inserted)
}

// ── M3U / M3U8 import + export ──────────────────────────────────────
//
// Plain-text playlist exchange so users can move between WaveFlow and
// foobar2000 / VLC / Rekordbox / car stereos. Format:
//
//   #EXTM3U
//   #PLAYLIST:<name>
//   #EXTINF:<seconds>,<artist> - <title>
//   <absolute path>
//
// We always write UTF-8 (.m3u8). On import we accept both encodings —
// UTF-8 first, lossy latin-1 fallback for older players' .m3u dumps.

#[derive(Debug, serde::Serialize)]
pub struct ImportPlaylistResult {
    pub playlist_id: i64,
    pub imported: i64,
    pub missing: i64,
    /// Up to 20 unmatched paths so the UI can surface them to the
    /// user. Truncated server-side to keep the IPC payload bounded
    /// even when a user imports a 10 k-line broken playlist.
    pub missing_paths: Vec<String>,
}

/// Build a comparable key from a filesystem path. We canonicalize
/// when possible (resolves symlinks, fixes case, tightens drive
/// letters) then strip the `\\?\` and `\\?\UNC\` extended-length
/// prefixes Windows' `canonicalize` adds. Falls back to the input
/// path when canonicalize fails so library-relative .m3u entries can
/// still match scanned tracks even if the file isn't currently
/// mounted.
///
/// Uses a byte-level prefix match instead of `str::strip_prefix(r"...")` —
/// the raw-string version was silently dropping a backslash in transit
/// through some shells, leaving keys diverging between candidates and
/// DB rows.
fn canonical_path_key(p: &std::path::Path) -> String {
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = canon.to_string_lossy().to_string();
    let bytes = s.as_bytes();
    let has_verbatim = bytes.len() >= 4
        && bytes[0] == b'\\'
        && bytes[1] == b'\\'
        && bytes[2] == b'?'
        && bytes[3] == b'\\';
    let has_unc = has_verbatim
        && bytes.len() >= 8
        && (bytes[4] == b'U' || bytes[4] == b'u')
        && (bytes[5] == b'N' || bytes[5] == b'n')
        && (bytes[6] == b'C' || bytes[6] == b'c')
        && bytes[7] == b'\\';
    if has_unc {
        // \\?\UNC\server\share\... → \\server\share\...
        format!("\\\\{}", &s[8..]).to_lowercase()
    } else if has_verbatim {
        // \\?\C:\... → C:\...
        s[4..].to_lowercase()
    } else {
        s.to_lowercase()
    }
}

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
    .fetch_all(&pool)
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

    let dest = std::path::PathBuf::from(&dest_path);
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::Other(format!("create parent dir: {e}")))?;
        }
    }
    std::fs::write(&dest, out).map_err(|e| AppError::Other(format!("write m3u file: {e}")))?;

    tracing::info!(
        playlist_id,
        path = %dest.display(),
        tracks = rows.len(),
        "playlist exported as m3u8"
    );
    Ok(())
}

/// Parse an .m3u / .m3u8 file at `source_path`, match each entry
/// against the active profile's library, and create a new playlist
/// holding the tracks that resolved. Unmatched entries are returned
/// (truncated to 20) so the UI can warn the user.
#[tauri::command]
pub async fn import_playlist_m3u(
    state: tauri::State<'_, AppState>,
    source_path: String,
) -> AppResult<ImportPlaylistResult> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let src = std::path::PathBuf::from(&source_path);

    let raw = std::fs::read(&src).map_err(|e| AppError::Other(format!("read m3u file: {e}")))?;
    // UTF-8 (.m3u8) first; fall back to byte→char lossy decode so legacy
    // .m3u files in latin-1 / cp1252 still produce readable paths.
    let text = match std::str::from_utf8(&raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|b| *b as char).collect::<String>(),
    };

    let parent = src.parent().unwrap_or_else(|| std::path::Path::new(""));

    // Collect candidate paths in playlist order, resolving relatives
    // against the m3u's own directory (matches what every desktop
    // player does and what users intuitively expect).
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for raw_line in text.lines() {
        // BOMs sneak in on Windows-edited m3u8 files; strip them once.
        let line = raw_line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let p = std::path::PathBuf::from(line);
        let resolved = if p.is_absolute() { p } else { parent.join(&p) };
        candidates.push(resolved);
    }

    // Build two lowercase lookups over every available track in the
    // active library — one full table scan, O(1) per candidate. The
    // canonical lookup is the primary match (handles different drive
    // mounts, symlinks, case differences). The basename lookup is a
    // last-resort fallback when the .m3u was authored on a machine
    // whose absolute paths don't resolve here — common when sharing
    // playlists across libraries with the same filename layout.
    #[derive(FromRow)]
    struct PathRow {
        id: i64,
        file_path: String,
    }
    let all =
        sqlx::query_as::<_, PathRow>("SELECT id, file_path FROM track WHERE is_available = 1")
            .fetch_all(&pool)
            .await?;
    let mut by_canonical: std::collections::HashMap<String, i64> =
        std::collections::HashMap::with_capacity(all.len());
    let mut by_basename: std::collections::HashMap<String, i64> =
        std::collections::HashMap::with_capacity(all.len());
    for r in all {
        let p = std::path::Path::new(&r.file_path);
        by_canonical.insert(canonical_path_key(p), r.id);
        if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
            // Last-write-wins on basename collisions — that's fine,
            // the user can still curate the playlist after import.
            by_basename.insert(stem.to_lowercase(), r.id);
        }
    }

    let mut matched: Vec<i64> = Vec::with_capacity(candidates.len());
    let mut missing: Vec<String> = Vec::new();
    for path in &candidates {
        let key = canonical_path_key(path);
        if let Some(id) = by_canonical.get(&key) {
            matched.push(*id);
            continue;
        }
        if let Some(stem) = path.file_name().and_then(|s| s.to_str()) {
            if let Some(id) = by_basename.get(&stem.to_lowercase()) {
                matched.push(*id);
                continue;
            }
        }
        missing.push(path.to_string_lossy().to_string());
    }
    if matched.is_empty() && !candidates.is_empty() {
        // Surface the first few resolved keys + a peek at the
        // library's stored basenames so the user can immediately tell
        // whether the divergence is path-shape or "the tracks just
        // aren't scanned in this profile".
        let sample: Vec<String> = candidates
            .iter()
            .take(3)
            .map(|p| canonical_path_key(p))
            .collect();
        let library_sample: Vec<String> = by_basename.keys().take(3).cloned().collect();
        tracing::warn!(
            ?sample,
            library_sample = ?library_sample,
            library_size = by_basename.len(),
            total = candidates.len(),
            "m3u import: no entries matched the active library"
        );
    }

    let name = src
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported playlist".to_string());

    let now = now_millis();
    let draft = PlaylistDraft {
        name,
        description: None,
        color_id: "violet".to_string(),
        icon_id: "music".to_string(),
        now_ms: now,
    };
    let (new_id, imported_u32) = SqlitePlaylistRepository::new(pool.clone())
        .create_with_tracks(&draft, &matched)
        .await?;
    let imported = i64::from(imported_u32);

    let missing_count = missing.len() as i64;
    tracing::info!(
        playlist_id = new_id,
        path = %src.display(),
        imported,
        missing = missing_count,
        "playlist imported from m3u"
    );

    let missing_paths: Vec<String> = missing.into_iter().take(20).collect();

    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, new_id).await;

    Ok(ImportPlaylistResult {
        playlist_id: new_id,
        imported,
        missing: missing_count,
        missing_paths,
    })
}
