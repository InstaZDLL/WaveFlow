use serde::Deserialize;
use std::path::Path;

use waveflow_core::{
    domain::track::TrackRow,
    repository::{
        sqlite::SqliteTrackRepository,
        track::{SortDirection, TrackListFilter, TrackRepository, TrackSort, TrackSortColumn},
    },
};

use crate::{error::AppResult, state::AppState};
// The plain-data DTOs (`Track`, `TrackListItem`, `ListTracksResponse`)
// + the joined `TrackRow` query target live in
// `waveflow_core::domain::track` since step 5.d. Re-exported here so
// existing call sites (`crate::commands::track::TrackRow`) keep
// resolving.
pub use waveflow_core::domain::track::{ListTracksResponse, Track, TrackListItem};

/// Build the slim row + check thumbnail existence on disk in one
/// pass. Shared between every bulk endpoint so the conversion stays
/// in lockstep.
pub fn track_list_item_from_row(row: TrackRow, artwork_dir: &Path) -> TrackListItem {
    let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
        Some(hash) => {
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, hash);
            (p1.is_some(), p2.is_some())
        }
        None => (false, false),
    };
    TrackListItem {
        id: row.id,
        library_id: row.library_id,
        title: row.title,
        album_id: row.album_id,
        album_title: row.album_title,
        artist_id: row.artist_id,
        artist_name: row.artist_name,
        artist_ids: row.artist_ids,
        duration_ms: row.duration_ms,
        track_number: row.track_number,
        disc_number: row.disc_number,
        year: row.year,
        bitrate: row.bitrate,
        sample_rate: row.sample_rate,
        channels: row.channels,
        bit_depth: row.bit_depth,
        codec: row.codec,
        musical_key: row.musical_key,
        file_path: row.file_path,
        file_size: row.file_size,
        added_at: row.added_at,
        artwork_hash: row.artwork_hash,
        artwork_format: row.artwork_format,
        artwork_has_1x,
        artwork_has_2x,
        rating: row.rating,
    }
}

/// Inflate a [`TrackRow`] into the frontend-facing [`Track`] by
/// resolving the artwork hash + format against the per-profile artwork
/// directory. Shared between every single-track endpoint
/// (`get_track`, `search_tracks`, `search_tracks_advanced`).
pub fn track_from_row(row: TrackRow, artwork_dir: &Path) -> Track {
    let (artwork_path, artwork_path_1x, artwork_path_2x) =
        match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
            (Some(hash), Some(format)) => {
                let full = artwork_dir
                    .join(format!("{}.{}", hash, format))
                    .to_string_lossy()
                    .to_string();
                let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, hash);
                (Some(full), p1, p2)
            }
            _ => (None, None, None),
        };
    Track {
        id: row.id,
        library_id: row.library_id,
        title: row.title,
        album_id: row.album_id,
        album_title: row.album_title,
        artist_id: row.artist_id,
        artist_name: row.artist_name,
        artist_ids: row.artist_ids,
        duration_ms: row.duration_ms,
        track_number: row.track_number,
        disc_number: row.disc_number,
        year: row.year,
        bitrate: row.bitrate,
        sample_rate: row.sample_rate,
        channels: row.channels,
        bit_depth: row.bit_depth,
        codec: row.codec,
        musical_key: row.musical_key,
        file_path: row.file_path,
        file_size: row.file_size,
        added_at: row.added_at,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
        rating: row.rating,
    }
}

/// Parse the wire-format `order_by` / `direction` strings into a
/// typed [`TrackSort`]. Unknown values fall back to the column /
/// direction defaults. Kept in app because the wire format is a
/// frontend contract.
fn parse_sort(order_by: Option<&str>, direction: Option<&str>) -> TrackSort {
    let column = match order_by {
        Some("title") => TrackSortColumn::Title,
        Some("artist") => TrackSortColumn::Artist,
        Some("album") => TrackSortColumn::Album,
        Some("duration_ms") => TrackSortColumn::DurationMs,
        Some("year") => TrackSortColumn::Year,
        Some("added_at") => TrackSortColumn::AddedAt,
        Some("rating") => TrackSortColumn::Rating,
        _ => TrackSortColumn::Default,
    };
    let direction = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => Some(SortDirection::Asc),
        Some(d) if d.eq_ignore_ascii_case("desc") => Some(SortDirection::Desc),
        _ => None,
    };
    TrackSort { column, direction }
}

/// List tracks. When `library_id` is `Some`, only tracks from that library
/// are returned. When `None`, tracks across **all** libraries are shown —
/// the "Ma musique" mode where the concept of multiple libraries is hidden
/// from the user.
///
/// `order_by` and `direction` map to a whitelisted `ORDER BY` clause via
/// [`parse_sort`] + `repository::sqlite::track::order_clause`.
#[tauri::command]
pub async fn list_tracks(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListTracksResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let sort = parse_sort(order_by.as_deref(), direction.as_deref());
    let rows = SqliteTrackRepository::new((*pool).clone())
        .list(TrackListFilter { library_id }, sort)
        .await?;

    // Each row triggers 2 synchronous `Path::exists` probes for the
    // thumbnail variants — at 1k+ tracks that's enough to stall the
    // tokio runtime. Hand the batch off to the blocking pool in one
    // hop rather than spawning per row.
    let artwork_dir_for_blocking = artwork_dir.clone();
    let items = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|row| track_list_item_from_row(row, &artwork_dir_for_blocking))
            .collect()
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("list_tracks join: {e}")))?;

    Ok(ListTracksResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Fetch a single track by id with the same joined shape as
/// `list_tracks`. Used by the Properties modal to refresh its local
/// state after a tag / cover edit so the user sees the change without
/// closing the dialog. Returns `None` when the id was deleted between
/// the open and the refetch (race-tolerant).
#[tauri::command]
pub async fn get_track(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<Track>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let row = SqliteTrackRepository::new((*pool).clone()).get(track_id).await?;
    Ok(row.map(|row| track_from_row(row, &artwork_dir)))
}

/// Full-text search via the `track_fts` FTS5 virtual table (kept in sync
/// by triggers). Returns up to 50 matching tracks, ranked by relevance.
/// The query is sanitized: double-quotes are stripped and a trailing `*`
/// is appended for prefix matching so "moon" finds "Moonlight".
#[tauri::command]
pub async fn search_tracks(
    state: tauri::State<'_, AppState>,
    query: String,
) -> AppResult<Vec<Track>> {
    let trimmed = query.trim().replace('"', "");
    if trimmed.is_empty() {
        return Ok(vec![]);
    }
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // Build FTS5 query: split words and add * for prefix matching.
    let fts_query = trimmed
        .split_whitespace()
        .map(|w| format!("{w}*"))
        .collect::<Vec<_>>()
        .join(" ");

    let rows = SqliteTrackRepository::new((*pool).clone())
        .search_fts(&fts_query, 50)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| track_from_row(row, &artwork_dir))
        .collect())
}

/// Optional multi-criteria filters layered on top of the FTS5 search.
///
/// Every field is `Option`: when `None`, the corresponding clause is
/// omitted entirely. The `query` field is itself optional so the command
/// doubles as a pure-filter browse when the search box is empty (the
/// user just wants to filter the whole library by genre/year/format).
///
/// All filters are AND-combined. Within a multi-value filter
/// (`genre_ids`, `formats`) the values are OR-combined (at least one
/// must match).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SearchFilters {
    pub query: Option<String>,
    pub genre_ids: Option<Vec<i64>>,
    pub year_min: Option<i64>,
    pub year_max: Option<i64>,
    pub bpm_min: Option<f64>,
    pub bpm_max: Option<f64>,
    pub duration_min_ms: Option<i64>,
    pub duration_max_ms: Option<i64>,
    pub formats: Option<Vec<String>>,
    pub min_sample_rate: Option<i64>,
    pub min_bit_depth: Option<i64>,
    /// Convenience flag: equivalent to `min_sample_rate >= 48000 AND
    /// min_bit_depth >= 24`. Applied in addition to (and intersected
    /// with) the explicit min_* fields if both are set.
    pub hi_res_only: Option<bool>,
    pub liked_only: Option<bool>,
}

/// Advanced search combining FTS5 full-text matching with structured
/// filters (genre, year, BPM, duration, format, Hi-Res, …).
///
/// Returns up to 200 rows (vs. 50 for the simple `search_tracks`)
/// because users often want to browse the result of a filter-only
/// query. Ordering: FTS rank when a query is supplied, otherwise the
/// canonical "Artist → Album → Disc → Track" order.
///
/// SQL is built dynamically here (still app-side) — the shape is too
/// client-specific to commit to a stable core trait method before the
/// future server defines its own filter language.
#[tauri::command]
pub async fn search_tracks_advanced(
    state: tauri::State<'_, AppState>,
    filters: SearchFilters,
) -> AppResult<Vec<Track>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // Build the FTS query string. Empty/whitespace → pure-filter mode.
    let fts_query: Option<String> = filters
        .query
        .as_deref()
        .map(|q| q.trim().replace('"', ""))
        .filter(|q| !q.is_empty())
        .map(|q| {
            q.split_whitespace()
                .map(|w| format!("{w}*"))
                .collect::<Vec<_>>()
                .join(" ")
        });

    let mut sql = String::with_capacity(1024);
    sql.push_str(
        "SELECT t.id, t.library_id, t.title,\n\
                t.album_id,\n\
                al.title AS album_title,\n\
                t.primary_artist AS artist_id,\n\
                (SELECT GROUP_CONCAT(name, ', ') FROM (\n\
                   SELECT ar2.name FROM track_artist ta2\n\
                   JOIN artist ar2 ON ar2.id = ta2.artist_id\n\
                   WHERE ta2.track_id = t.id\n\
                   ORDER BY ta2.position\n\
                )) AS artist_name,\n\
                (SELECT GROUP_CONCAT(id, ',') FROM (\n\
                   SELECT ta2.artist_id AS id FROM track_artist ta2\n\
                   WHERE ta2.track_id = t.id\n\
                   ORDER BY ta2.position\n\
                )) AS artist_ids,\n\
                t.duration_ms, t.track_number, t.disc_number, t.year,\n\
                t.bitrate, t.sample_rate, t.channels,\n\
                t.bit_depth, t.codec, t.musical_key,\n\
                t.file_path, t.file_size, t.added_at,\n\
                aw.hash   AS artwork_hash,\n\
                aw.format AS artwork_format,\n\
                t.rating  AS rating\n",
    );

    if fts_query.is_some() {
        sql.push_str("FROM track_fts fts JOIN track t ON t.id = fts.rowid\n");
    } else {
        sql.push_str("FROM track t\n");
    }
    sql.push_str(
        "LEFT JOIN album   al ON al.id = t.album_id\n\
         LEFT JOIN artist  ar ON ar.id = t.primary_artist\n\
         LEFT JOIN artwork aw ON aw.id = al.artwork_id\n",
    );

    // Bind values are pushed in the same order as their `?` placeholders
    // appear in the SQL string. We use sqlx::Any-style binds via
    // `query_as::<_, TrackRow>` and `.bind(...)` chain at the end.
    enum Bind {
        Str(String),
        Int(i64),
        Real(f64),
    }
    let mut binds: Vec<Bind> = Vec::new();

    sql.push_str("WHERE t.is_available = 1\n");
    if let Some(q) = &fts_query {
        sql.push_str("  AND track_fts MATCH ?\n");
        binds.push(Bind::Str(q.clone()));
    }

    if let Some(ids) = filters.genre_ids.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = vec!["?"; ids.len()].join(",");
        sql.push_str(&format!(
            "  AND EXISTS (SELECT 1 FROM track_genre tg WHERE tg.track_id = t.id AND tg.genre_id IN ({placeholders}))\n"
        ));
        for id in ids {
            binds.push(Bind::Int(*id));
        }
    }

    if let Some(y) = filters.year_min {
        sql.push_str("  AND COALESCE(t.year, al.year) >= ?\n");
        binds.push(Bind::Int(y));
    }
    if let Some(y) = filters.year_max {
        sql.push_str("  AND COALESCE(t.year, al.year) <= ?\n");
        binds.push(Bind::Int(y));
    }

    if filters.bpm_min.is_some() || filters.bpm_max.is_some() {
        // BPM is in track_analysis; require the row to exist when the
        // user filters by tempo.
        sql.push_str("  AND EXISTS (SELECT 1 FROM track_analysis ta WHERE ta.track_id = t.id\n");
        if let Some(b) = filters.bpm_min {
            sql.push_str("           AND ta.bpm >= ?\n");
            binds.push(Bind::Real(b));
        }
        if let Some(b) = filters.bpm_max {
            sql.push_str("           AND ta.bpm <= ?\n");
            binds.push(Bind::Real(b));
        }
        sql.push_str("       )\n");
    }

    if let Some(d) = filters.duration_min_ms {
        sql.push_str("  AND t.duration_ms >= ?\n");
        binds.push(Bind::Int(d));
    }
    if let Some(d) = filters.duration_max_ms {
        sql.push_str("  AND t.duration_ms <= ?\n");
        binds.push(Bind::Int(d));
    }

    if let Some(fmts) = filters.formats.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = vec!["?"; fmts.len()].join(",");
        sql.push_str(&format!(
            "  AND UPPER(COALESCE(t.codec, '')) IN ({placeholders})\n"
        ));
        for f in fmts {
            binds.push(Bind::Str(f.to_uppercase()));
        }
    }

    if let Some(sr) = filters.min_sample_rate {
        sql.push_str("  AND t.sample_rate >= ?\n");
        binds.push(Bind::Int(sr));
    }
    if let Some(bd) = filters.min_bit_depth {
        sql.push_str("  AND t.bit_depth >= ?\n");
        binds.push(Bind::Int(bd));
    }
    if filters.hi_res_only.unwrap_or(false) {
        sql.push_str("  AND t.sample_rate >= 48000 AND t.bit_depth >= 24\n");
    }

    if filters.liked_only.unwrap_or(false) {
        sql.push_str("  AND EXISTS (SELECT 1 FROM liked_track lt WHERE lt.track_id = t.id)\n");
    }

    if fts_query.is_some() {
        sql.push_str("ORDER BY rank\n");
    } else {
        sql.push_str(
            "ORDER BY ar.canonical_name COLLATE NOCASE,\n\
                      al.canonical_title COLLATE NOCASE,\n\
                      t.disc_number,\n\
                      t.track_number,\n\
                      t.title COLLATE NOCASE\n",
        );
    }
    sql.push_str("LIMIT 200");

    let mut q = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql));
    for b in binds {
        q = match b {
            Bind::Str(s) => q.bind(s),
            Bind::Int(i) => q.bind(i),
            Bind::Real(r) => q.bind(r),
        };
    }
    let rows = q.fetch_all(&*pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| track_from_row(row, &artwork_dir))
        .collect())
}

/// Set or clear a track's rating. The value is the raw POPM byte (0-255);
/// passing `None` clears the rating.
///
/// Writes the rating to:
/// 1. the audio file's tag (POPM frame for ID3v2 — MP3/WAV/AAC/AIFF —
///    or `RATING=0-100` text for Vorbis / MP4 / APE), so the rating
///    survives a re-scan or import on another machine, and
/// 2. the per-profile `track.rating` column, so the UI updates without
///    waiting for a folder scan.
///
/// File write is best-effort: containers lofty can't open (DSD, exotic
/// formats) keep the DB-only rating. The pause-if-playing handshake
/// mirrors [`crate::commands::edit::update_track_tags`] so a Windows
/// rename doesn't fight the audio engine's open handle. Emits
/// `track:updated` so every open view refreshes without polling.
#[tauri::command]
pub async fn set_track_rating(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, std::sync::Arc<crate::audio::AudioEngine>>,
    app: tauri::AppHandle,
    track_id: i64,
    rating: Option<u8>,
) -> AppResult<()> {
    use tauri::Emitter;

    let pool = state.require_profile_pool().await?;
    let repo = SqliteTrackRepository::new(pool.clone());

    // 1. Resolve the file path so we can write the POPM frame back.
    let file_path = repo.get_file_path(track_id).await?;

    // 2. Pause playback if the engine has this track open — required on
    //    Windows so lofty's atomic rename can take an exclusive handle.
    if let Some(ref path_str) = file_path {
        let active = engine
            .shared()
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        if active == track_id {
            let _ = engine.send(crate::audio::AudioCmd::Pause);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // 3. Write the tag. Failures are logged + non-fatal: DSD files
        //    have no writable rating frame, and we don't want a popup
        //    every time a user rates a DSF.
        let path = std::path::PathBuf::from(path_str);
        let mut tag_written = true;
        if let Err(err) = write_rating_to_file(&path, rating) {
            tracing::warn!(track_id, ?err, "rating tag write failed — DB-only");
            tag_written = false;
        }
        // 3b. If the tag write actually mutated the file, recompute its
        //     blake3 hash so the scanner's fast path keeps recognising
        //     the file on the next pass and the per-hash caches stay
        //     addressable. The pool inside SqliteTrackRepository is the
        //     same one we'd update through anyway, so go via the helper
        //     in `edit` to keep the rehash + DB write in one place.
        if tag_written {
            // Propagate any rehash error — if we wrote to the file
            // but can't read it back to recompute the hash, the
            // scanner would mis-detect the row as new on its next
            // pass. Better to surface the failure to the user (and
            // skip the DB rating update below) than silently drift.
            super::edit::rehash_track_file(&pool, track_id, &path).await?;
        }
    }

    // 4. DB update + outbox enqueue in a single tx so a peer device
    //    sees the rating change at the same cross-device key
    //    (file_hash). The set_rating call above used the repo pool;
    //    we drop down to raw SQL for the tx so the enqueue is
    //    atomic with the rating write.
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE track SET rating = ? WHERE id = ?")
        .bind(rating.map(i64::from))
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    let file_hash = crate::sync::canonical::file_hash_for_local_track(&mut tx, track_id).await?;
    if let Some(hash) = file_hash {
        let stamp = crate::sync::hooks::enqueue_op_in_tx(
            &mut tx,
            &crate::sync::hooks::PendingOpDraft {
                entity: "track_rating".into(),
                entity_id: hash,
                field: None,
                op: if rating.is_some() { "set" } else { "delete" }.into(),
                payload: rating.map(|r| serde_json::json!({ "value": r })),
            },
        )
        .await?;
        // Phase B.0 — stamp the rating_* mirror columns on the
        // same track row. The metadata sub-entity ([`super::track`])
        // owns the regular hlc_*/payload_hash columns; rating runs
        // in parallel under its own §2 tuple so a metadata edit
        // and a rating change don't clobber each other on
        // read-modify-write.
        if let Some(stamp) = stamp {
            if let Some(value) = rating {
                crate::sync::payload::track_rating::stamp_set_in_tx(
                    &mut tx,
                    track_id,
                    i64::from(value),
                    stamp,
                )
                .await?;
            } else {
                crate::sync::payload::track_rating::stamp_delete_in_tx(&mut tx, track_id, stamp)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    state.drain.notify();

    // Drop the legacy repo call now that the tx above wrote the row
    // — kept the binding alive only for its other call sites.
    let _ = repo;

    let _ = app.emit("track:updated", track_id);
    Ok(())
}

/// Write the rating into the file's primary tag. For ID3v2 (MP3, WAV,
/// AAC, AIFF) the raw POPM frame body is built directly because lofty
/// 0.24's generic `Tag` interface stores POPM as `ItemValue::Binary`
/// without round-tripping the typed `PopularimeterFrame`. For every
/// other container (Vorbis / MP4 / APE / WavPack) the rating is stored
/// as plain text `RATING=0-100` under [`ItemKey::Popularimeter`], which
/// is the same key the scanner reads back via `get_string`.
///
/// `rating = None` removes any existing POPM/Rating tag.
fn write_rating_to_file(
    path: &std::path::Path,
    rating: Option<u8>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};

    let mut tagged = lofty::read_from_path(path)?;
    if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
        let preferred = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(preferred));
    }
    let tag = if tagged.primary_tag().is_some() {
        tagged.primary_tag_mut().expect("checked primary_tag")
    } else {
        tagged.first_tag_mut().ok_or("no tag after insert")?
    };

    // Always start from a clean slate so a previously-written rating
    // (possibly with a different email) doesn't shadow the new one.
    tag.remove_key(ItemKey::Popularimeter);

    if let Some(r) = rating {
        match tag.tag_type() {
            TagType::Id3v2 => {
                // POPM body: <email>\0<rating:u8><counter:u32-be>.
                // Empty email is allowed by the ID3v2 spec and means
                // "anonymous user" — readers (foobar2000, Mp3tag,
                // MusicBee) accept it. Counter = 0; we don't track it.
                let bytes: Vec<u8> = std::iter::once(0u8)
                    .chain(std::iter::once(r))
                    .chain([0u8; 4])
                    .collect();
                tag.insert(TagItem::new(
                    ItemKey::Popularimeter,
                    ItemValue::Binary(bytes),
                ));
            }
            _ => {
                // Vorbis / MP4 / APE / WavPack: text rating on the
                // 0-100 scale, same shape the scanner reads.
                let as_100 = ((r as u16) * 100 / 255) as u8;
                tag.insert_text(ItemKey::Popularimeter, as_100.to_string());
            }
        }
    }

    tagged.save_to_path(path, lofty::config::WriteOptions::default())?;
    Ok(())
}

/// Toggle the liked state of a track. If already liked → unlike (DELETE),
/// if not → like (INSERT). Returns `true` if the track is now liked.
#[tauri::command]
pub async fn toggle_like_track(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;
    let mut tx = pool.begin().await?;

    // Resolve the file hash once so both branches enqueue against
    // the same cross-device key. A track without a hash (shouldn't
    // happen post-scan, but handle defensively) silently skips the
    // outbox enqueue — the local DB still moves so the UI heart
    // animation lands.
    let file_hash = crate::sync::canonical::file_hash_for_local_track(&mut tx, track_id).await?;

    let now = chrono::Utc::now().timestamp_millis();
    // Inline the like-state check + flip against the open tx so the
    // read + write land atomically without a separate
    // SqliteTrackRepository acquire/release pair.
    let was_liked: Option<i64> = sqlx::query_scalar("SELECT 1 FROM liked_track WHERE track_id = ?")
        .bind(track_id)
        .fetch_optional(&mut *tx)
        .await?;
    // Both branches derive the post-state from `rows_affected()`
    // rather than assuming the action landed. `INSERT OR IGNORE`
    // silently no-ops on an FK violation (the scanner removed the
    // track between the UI render and this command, or a future
    // UNIQUE constraint trips), and we don't want to return
    // `now_liked = true` to a UI rendering a heart against a row
    // that doesn't exist anymore. A DELETE that matched 0 rows
    // means a concurrent unlike already happened — the post-state
    // is still "not liked".
    let (now_liked, did_change) = if was_liked.is_some() {
        let res = sqlx::query("DELETE FROM liked_track WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        (false, res.rows_affected() > 0)
    } else {
        let res =
            sqlx::query("INSERT OR IGNORE INTO liked_track (track_id, liked_at) VALUES (?, ?)")
                .bind(track_id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        let actually_inserted = res.rows_affected() > 0;
        (actually_inserted, actually_inserted)
    };

    // Only enqueue when the local DB actually moved — emitting a
    // phantom op for a no-op INSERT wastes a Lamport draw and
    // bloats the queue with a like the peer can't resolve anyway.
    if did_change {
        if let Some(hash) = file_hash {
            let stamp = crate::sync::hooks::enqueue_op_in_tx(
                &mut tx,
                &crate::sync::hooks::PendingOpDraft {
                    entity: "liked_track".into(),
                    entity_id: hash,
                    field: None,
                    op: if now_liked { "insert" } else { "delete" }.into(),
                    payload: None,
                },
            )
            .await?;
            // Phase B.0 — stamp the liked_track row's payload_hash
            // on insert; on delete the row is already gone, so bump
            // the digest counter directly.
            if let Some(stamp) = stamp {
                if now_liked {
                    crate::sync::payload::liked_track::stamp_in_tx(&mut tx, track_id, stamp)
                        .await?;
                } else {
                    crate::sync::payload::liked_track::bump_for_delete_in_tx(&mut tx).await?;
                }
            }
        }
    }
    tx.commit().await?;
    state.drain.notify();
    Ok(now_liked)
}

/// Return the set of liked track IDs so the frontend can render hearts
/// without N+1 queries. Cheap because `liked_track` is indexed.
#[tauri::command]
pub async fn list_liked_track_ids(state: tauri::State<'_, AppState>) -> AppResult<Vec<i64>> {
    let pool = state.require_profile_pool().await?;
    Ok(SqliteTrackRepository::new((*pool).clone()).liked_ids().await?)
}

/// List every liked track with full metadata, ordered by most recently
/// liked first. Used by the LikedView.
#[tauri::command]
pub async fn list_liked_tracks(state: tauri::State<'_, AppState>) -> AppResult<ListTracksResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let rows = SqliteTrackRepository::new((*pool).clone()).list_liked().await?;

    // Same blocking-pool offload as `list_tracks`.
    let artwork_dir_for_blocking = artwork_dir.clone();
    let items = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|row| track_list_item_from_row(row, &artwork_dir_for_blocking))
            .collect()
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("list_liked_tracks join: {e}")))?;

    Ok(ListTracksResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}
