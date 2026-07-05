//! Backfill pull direction — fetch rows the server has but the
//! desktop doesn't, write them locally without triggering the
//! outbox (RFC-003 Phase B.2).
//!
//! For each `canonical_id` flagged `missing_locally`, hit
//! [`crate::sync::digest::entity_client::fetch_remote_entity`]
//! and apply the row directly via SQL — bypassing the
//! [`crate::sync::apply`] pipeline so:
//!
//! - The exact `(hlc_wall, hlc_logical, origin_device_id,
//!   payload_hash)` the server returned stamps the row, so the
//!   next digest sweep computes the same set_hash as the server.
//!   The standard apply path would draw fresh stamps locally,
//!   leaving the row in a state that disagrees with the server's
//!   digest until the next CRUD touched it.
//! - No outbox echo. The apply path already gates this on
//!   `device_id == local_device_id`, but bypassing it removes
//!   the need to fabricate a "phantom" device id.
//! - Reconciliation skips lamport bumping. Backfill is offline-
//!   state reconciliation, not a wire op; bumping the lamport
//!   floor over the entity set's max would make every next local
//!   write slot above an arbitrarily-old peer write.
//!
use chrono::Utc;
use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{AppError, AppResult};
use crate::server_client::WaveflowServerClient;
use crate::state::AppState;
use crate::sync::canonical;
use crate::sync::digest::client::RemoteMember;
use crate::sync::digest::entity_client::{self, RemoteEntityRow};

/// Counters returned to the orchestrator.
#[derive(Debug, Default)]
pub struct PullStats {
    pub pulled: u32,
    pub failed: u32,
}

/// Pull every member of `missing_locally` for the given entity.
pub async fn pull_missing_locally(
    state: &AppState,
    client: &WaveflowServerClient,
    pool: &SqlitePool,
    entity: &str,
    profile_canonical_id: Option<&str>,
    missing: &[RemoteMember],
) -> AppResult<PullStats> {
    let mut stats = PullStats::default();
    for member in missing {
        let outcome = pull_one(
            state,
            client,
            pool,
            entity,
            profile_canonical_id,
            &member.canonical_id,
        )
        .await;
        match outcome {
            Ok(true) => stats.pulled += 1,
            Ok(false) => {
                tracing::debug!(
                    entity,
                    canonical_id = %member.canonical_id,
                    "backfill pull: server returned 404 / row absent, skipping"
                );
            }
            Err(err) => {
                tracing::warn!(
                    entity,
                    canonical_id = %member.canonical_id,
                    error = %err,
                    "backfill pull failed for row"
                );
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

/// Fetch + apply one row. Pub so [`super::lww`] reuses it on the
/// "remote wins" branch — same end state: server's view replaces
/// local.
pub async fn pull_one(
    _state: &AppState,
    client: &WaveflowServerClient,
    pool: &SqlitePool,
    entity: &str,
    profile_canonical_id: Option<&str>,
    canonical_id: &str,
) -> AppResult<bool> {
    let Some(row) =
        entity_client::fetch_remote_entity(client, entity, canonical_id, profile_canonical_id)
            .await?
    else {
        return Ok(false);
    };
    let mut tx = pool.begin().await?;
    apply_remote_row(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(true)
}

/// Direct-SQL apply of a [`RemoteEntityRow`] dispatched on
/// `row.entity`. Bypasses [`crate::sync::apply`] so the row's
/// stamp matches the server byte-exact. `pub(super)` so the LWW
/// resolver's "remote wins" branch reuses the same path.
pub(super) async fn apply_remote_row(
    conn: &mut SqliteConnection,
    row: &RemoteEntityRow,
) -> AppResult<()> {
    match row.entity.as_str() {
        "library" => apply_library(conn, row).await,
        "playlist" => apply_playlist(conn, row).await,
        "track" => apply_track(conn, row).await,
        "liked_track" => apply_liked_track(conn, row).await,
        "track_rating" => apply_track_rating(conn, row).await,
        other => Err(AppError::Other(format!(
            "pull: unsupported entity '{other}'",
        ))),
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn opt_str(row: &RemoteEntityRow, key: &str) -> Option<String> {
    row.fields
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn str_required(row: &RemoteEntityRow, key: &str) -> AppResult<String> {
    row.fields
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Other(format!("pull {}: missing field '{key}'", row.entity)))
}

fn origin_string(row: &RemoteEntityRow) -> Option<String> {
    row.origin_device_id.map(|u| u.to_string())
}

/// Apply a library row pulled from the server. UPSERT on
/// `canonical_id` so a stale row (e.g. one the user just deleted
/// locally) gets resurrected to match the server's view.
async fn apply_library(conn: &mut SqliteConnection, row: &RemoteEntityRow) -> AppResult<()> {
    let name = str_required(row, "name")?;
    let description = opt_str(row, "description");
    let color_id = opt_str(row, "color_id").unwrap_or_else(|| "emerald".into());
    let icon_id = opt_str(row, "icon_id").unwrap_or_else(|| "library".into());
    let now = now_ms();
    let payload_hash = hex::decode(&row.payload_hash)
        .map_err(|err| AppError::Other(format!("library payload_hash hex: {err}")))?;
    let origin = origin_string(row);

    // UPSERT lands the row + carries the server's exact stamp.
    // A new INSERT mints a fresh local rowid; a conflict on the
    // canonical id refreshes the metadata + stamp.
    sqlx::query(
        "INSERT INTO library
            (name, description, color_id, icon_id, created_at, updated_at,
             canonical_id, hlc_wall, hlc_logical, origin_device_id, payload_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(canonical_id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            color_id = excluded.color_id,
            icon_id = excluded.icon_id,
            updated_at = excluded.updated_at,
            hlc_wall = excluded.hlc_wall,
            hlc_logical = excluded.hlc_logical,
            origin_device_id = excluded.origin_device_id,
            payload_hash = excluded.payload_hash",
    )
    .bind(&name)
    .bind(description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(now)
    .bind(now)
    .bind(&row.canonical_id)
    .bind(row.hlc.wall)
    .bind(row.hlc.logical)
    .bind(origin.as_deref())
    .bind(&payload_hash[..])
    .execute(&mut *conn)
    .await?;

    // Make sure sync_id_map round-trips for the inbound apply
    // path's reverse lookup.
    let local_id = ensure_canonical(conn, "library", &row.canonical_id).await?;
    let _ = local_id;
    bump_digest(conn, "library").await
}

async fn apply_playlist(conn: &mut SqliteConnection, row: &RemoteEntityRow) -> AppResult<()> {
    let name = str_required(row, "name")?;
    let description = opt_str(row, "description");
    let color_id = opt_str(row, "color_id").unwrap_or_else(|| "violet".into());
    let icon_id = opt_str(row, "icon_id").unwrap_or_else(|| "music".into());
    let now = now_ms();
    let payload_hash = hex::decode(&row.payload_hash)
        .map_err(|err| AppError::Other(format!("playlist payload_hash hex: {err}")))?;
    let origin = origin_string(row);

    sqlx::query(
        "INSERT INTO playlist
            (name, description, color_id, icon_id, created_at, updated_at,
             canonical_id, hlc_wall, hlc_logical, origin_device_id, payload_hash)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(canonical_id) DO UPDATE SET
            name = excluded.name,
            description = excluded.description,
            color_id = excluded.color_id,
            icon_id = excluded.icon_id,
            updated_at = excluded.updated_at,
            hlc_wall = excluded.hlc_wall,
            hlc_logical = excluded.hlc_logical,
            origin_device_id = excluded.origin_device_id,
            payload_hash = excluded.payload_hash",
    )
    .bind(&name)
    .bind(description.as_deref())
    .bind(&color_id)
    .bind(&icon_id)
    .bind(now)
    .bind(now)
    .bind(&row.canonical_id)
    .bind(row.hlc.wall)
    .bind(row.hlc.logical)
    .bind(origin.as_deref())
    .bind(&payload_hash[..])
    .execute(&mut *conn)
    .await?;

    let local_id = ensure_canonical(conn, "playlist", &row.canonical_id).await?;
    let _ = local_id;
    bump_digest(conn, "playlist").await
}

/// `liked_track` canonical is the file_hash. We need a local
/// `track.id` matching the hash; rows for files we haven't
/// scanned locally yet are skipped (the row materialises once
/// the user imports the matching file). The skip surfaces as
/// `Ok(())` so the orchestrator counts it as pulled — same end
/// state as "successfully applied" because the absence of the
/// track row means the like can't bind anyway.
async fn apply_liked_track(conn: &mut SqliteConnection, row: &RemoteEntityRow) -> AppResult<()> {
    let file_hash = &row.canonical_id;
    let track_id: Option<i64> = sqlx::query_scalar("SELECT id FROM track WHERE file_hash = ?")
        .bind(file_hash)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(track_id) = track_id else {
        tracing::debug!(
            file_hash,
            "backfill pull liked_track: no local track for hash, deferring",
        );
        return Ok(());
    };
    let payload_hash = hex::decode(&row.payload_hash)
        .map_err(|err| AppError::Other(format!("liked_track payload_hash hex: {err}")))?;
    let origin = origin_string(row);
    let now = now_ms();

    sqlx::query(
        "INSERT INTO liked_track
            (track_id, liked_at, hlc_wall, hlc_logical, origin_device_id, payload_hash)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(track_id) DO UPDATE SET
            liked_at = excluded.liked_at,
            hlc_wall = excluded.hlc_wall,
            hlc_logical = excluded.hlc_logical,
            origin_device_id = excluded.origin_device_id,
            payload_hash = excluded.payload_hash",
    )
    .bind(track_id)
    .bind(now)
    .bind(row.hlc.wall)
    .bind(row.hlc.logical)
    .bind(origin.as_deref())
    .bind(&payload_hash[..])
    .execute(&mut *conn)
    .await?;
    bump_digest(conn, "liked_track").await
}

/// Apply a `track` row pulled from the server. The composite
/// canonical `<library_canonical_id>\u{1F}<file_path>` is already
/// split into `library_canonical_id` + `file_path` top-level
/// fields on the response. We:
///
/// 1. Resolve the local `library_id` from the canonical id. If
///    absent → defer (library backfill hasn't happened yet; the
///    orchestrator re-runs in order).
/// 2. Upsert `album` + every contributor `artist` via the core
///    scanner helpers (same path the scanner uses) so the
///    foreign keys resolve identically regardless of whether the
///    row originated from a local scan or a remote pull.
/// 3. UPSERT the `track` row on `(library_id, file_path)`. When
///    the file isn't locally available yet (no scanner pass has
///    seen it), `folder_id` stays NULL and `is_available = 0` —
///    the next scanner pass flips the flag + populates folder_id.
/// 4. Re-link `track_artist` to match the server's order.
/// 5. Stamp the server's exact `(hlc, origin, payload_hash)`.
async fn apply_track(conn: &mut SqliteConnection, row: &RemoteEntityRow) -> AppResult<()> {
    use waveflow_core::scanner::upserts;

    let library_canonical = row.library_canonical_id.as_deref().ok_or_else(|| {
        AppError::Other("track pull: missing library_canonical_id on response".into())
    })?;
    let file_path = row
        .file_path
        .as_deref()
        .ok_or_else(|| AppError::Other("track pull: missing file_path on response".into()))?;

    let library_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM library WHERE canonical_id = ?")
            .bind(library_canonical)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(library_id) = library_id else {
        tracing::debug!(
            library_canonical,
            file_path,
            "backfill pull track: no local library for canonical, deferring",
        );
        return Ok(());
    };

    let title = str_required(row, "title")?;
    let file_hash = str_required(row, "file_hash")?;
    let file_size = row
        .fields
        .get("file_size")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Other("track pull: missing 'file_size'".into()))?;
    let duration_ms = row
        .fields
        .get("duration_ms")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Other("track pull: missing 'duration_ms'".into()))?;
    let track_number = row.fields.get("track_number").and_then(Value::as_i64);
    let disc_number = row.fields.get("disc_number").and_then(Value::as_i64);
    let year = row.fields.get("year").and_then(Value::as_i64);
    let bitrate = row.fields.get("bitrate").and_then(Value::as_i64);
    let sample_rate = row.fields.get("sample_rate").and_then(Value::as_i64);
    let channels = row.fields.get("channels").and_then(Value::as_i64);
    let bit_depth = row.fields.get("bit_depth").and_then(Value::as_i64);
    let codec = opt_str(row, "codec");
    let musical_key = opt_str(row, "musical_key");
    let added_at = row
        .fields
        .get("added_at")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Other("track pull: missing 'added_at'".into()))?;
    let album_title = opt_str(row, "album_title");
    let album_artist_name = opt_str(row, "album_artist_name");
    let is_compilation = row
        .fields
        .get("is_compilation")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let artists: Vec<String> = row
        .fields
        .get("artists")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // Upsert every contributor artist via the per-artist helper
    // (the multi-artist `upsert_artist_list` takes a raw `"A; B"`
    // string + splits internally; we already have the split form
    // from the server, so call per-name to skip the parse).
    let mut artist_ids: Vec<i64> = Vec::with_capacity(artists.len());
    for name in &artists {
        if let Some(id) = upserts::upsert_artist(&mut *conn, name)
            .await
            .map_err(|err| AppError::Other(format!("upsert_artist: {err}")))?
        {
            artist_ids.push(id);
        }
    }
    let primary_artist_id = artist_ids.first().copied();
    let album_id = match album_title.as_deref() {
        Some(title) => upserts::upsert_album(
            &mut *conn,
            title,
            album_artist_name.as_deref(),
            is_compilation,
            primary_artist_id,
            year,
        )
        .await
        .map_err(|err| AppError::Other(format!("upsert_album: {err}")))?,
        None => None,
    };
    let _ = added_at; // bound below in the INSERT

    let payload_hash = hex::decode(&row.payload_hash)
        .map_err(|err| AppError::Other(format!("track payload_hash hex: {err}")))?;
    let origin = origin_string(row);

    // UPSERT on `(library_id, file_path)`. Folder_id stays NULL
    // when this is a fresh pull — the scanner fills it in when
    // the file materialises on this device. Is_available = 0 for
    // the same reason: the UI shouldn't try to play a row backed
    // by no local audio file. The scanner's `(mtime, size)`
    // fast-path will flip is_available + folder_id on its next
    // pass over the library folder.
    sqlx::query(
        "INSERT INTO track (
            library_id, folder_id, file_path, file_hash, file_size, file_modified,
            title, album_id, primary_artist,
            track_number, disc_number, year,
            duration_ms, bitrate, sample_rate, channels,
            bit_depth, codec, musical_key,
            added_at, is_available,
            hlc_wall, hlc_logical, origin_device_id, payload_hash
         ) VALUES (?, NULL, ?, ?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?, ?)
         ON CONFLICT(library_id, file_path) DO UPDATE SET
            file_hash       = excluded.file_hash,
            file_size       = excluded.file_size,
            title           = excluded.title,
            album_id        = excluded.album_id,
            primary_artist  = excluded.primary_artist,
            track_number    = excluded.track_number,
            disc_number     = excluded.disc_number,
            year            = excluded.year,
            duration_ms     = excluded.duration_ms,
            bitrate         = excluded.bitrate,
            sample_rate     = excluded.sample_rate,
            channels        = excluded.channels,
            bit_depth       = excluded.bit_depth,
            codec           = excluded.codec,
            musical_key     = excluded.musical_key,
            added_at        = excluded.added_at,
            hlc_wall        = excluded.hlc_wall,
            hlc_logical     = excluded.hlc_logical,
            origin_device_id = excluded.origin_device_id,
            payload_hash    = excluded.payload_hash",
    )
    .bind(library_id)
    .bind(file_path)
    .bind(&file_hash)
    .bind(file_size)
    .bind(&title)
    .bind(album_id)
    .bind(primary_artist_id)
    .bind(track_number)
    .bind(disc_number)
    .bind(year)
    .bind(duration_ms)
    .bind(bitrate)
    .bind(sample_rate)
    .bind(channels)
    .bind(bit_depth)
    .bind(codec.as_deref())
    .bind(musical_key.as_deref())
    .bind(added_at)
    .bind(row.hlc.wall)
    .bind(row.hlc.logical)
    .bind(origin.as_deref())
    .bind(&payload_hash[..])
    .execute(&mut *conn)
    .await?;

    // Re-link `track_artist` to match the server's order. DELETE
    // first because positions can shift on a re-tag — INSERT OR
    // IGNORE would leave stale positions behind.
    let track_id: i64 =
        sqlx::query_scalar("SELECT id FROM track WHERE library_id = ? AND file_path = ?")
            .bind(library_id)
            .bind(file_path)
            .fetch_one(&mut *conn)
            .await?;
    sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    for (position, aid) in artist_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO track_artist (track_id, artist_id, role, position)
             VALUES (?, ?, 'main', ?)",
        )
        .bind(track_id)
        .bind(aid)
        .bind(position as i64)
        .execute(&mut *conn)
        .await?;
    }

    bump_digest(conn, "track").await
}

async fn apply_track_rating(conn: &mut SqliteConnection, row: &RemoteEntityRow) -> AppResult<()> {
    let file_hash = &row.canonical_id;
    let track_id: Option<i64> = sqlx::query_scalar("SELECT id FROM track WHERE file_hash = ?")
        .bind(file_hash)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(track_id) = track_id else {
        tracing::debug!(
            file_hash,
            "backfill pull track_rating: no local track for hash, deferring",
        );
        return Ok(());
    };
    let rating = row
        .fields
        .get("rating")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::Other("track_rating pull: missing 'rating' field".into()))?;
    if !(0..=255).contains(&rating) {
        return Err(AppError::Other(format!(
            "track_rating pull: rating {rating} out of 0..=255 range",
        )));
    }
    let payload_hash = hex::decode(&row.payload_hash)
        .map_err(|err| AppError::Other(format!("track_rating payload_hash hex: {err}")))?;
    let origin = origin_string(row);

    sqlx::query(
        "UPDATE track
            SET rating = ?,
                rating_hlc_wall = ?,
                rating_hlc_logical = ?,
                rating_origin_device_id = ?,
                rating_payload_hash = ?
          WHERE id = ?",
    )
    .bind(rating)
    .bind(row.hlc.wall)
    .bind(row.hlc.logical)
    .bind(origin.as_deref())
    .bind(&payload_hash[..])
    .bind(track_id)
    .execute(&mut *conn)
    .await?;
    bump_digest(conn, "track_rating").await
}

/// Plant a `sync_id_map` row pointing at the local rowid we just
/// inserted/updated so the inbound apply path's reverse lookup
/// resolves. UPSERT shape mirrors
/// [`crate::sync::canonical::set_canonical_*`]: dropping any
/// prior `(entity, local_id)` row pointing at a different
/// canonical keeps "exactly one mapping per local_id" intact.
async fn ensure_canonical(
    conn: &mut SqliteConnection,
    entity: &str,
    canonical_id: &str,
) -> AppResult<i64> {
    // The entity row exists post-UPSERT — its id is what we want
    // to map. Re-read by canonical to handle both INSERT and
    // ON CONFLICT branches.
    let local_id: i64 = match entity {
        "library" => {
            sqlx::query_scalar("SELECT id FROM library WHERE canonical_id = ?")
                .bind(canonical_id)
                .fetch_one(&mut *conn)
                .await?
        }
        "playlist" => {
            sqlx::query_scalar("SELECT id FROM playlist WHERE canonical_id = ?")
                .bind(canonical_id)
                .fetch_one(&mut *conn)
                .await?
        }
        other => {
            return Err(AppError::Other(format!(
                "ensure_canonical: unsupported entity '{other}'",
            )))
        }
    };
    match entity {
        "library" => {
            canonical::set_canonical_library(&mut *conn, local_id, canonical_id).await?;
        }
        "playlist" => {
            canonical::set_canonical_playlist(&mut *conn, local_id, canonical_id).await?;
        }
        _ => {}
    }
    Ok(local_id)
}

/// Bump `metadata_digest_version` for the entity. Mirrors the
/// existing CRUD path: every state-changing apply increments the
/// counter so the next digest endpoint reflects the new state.
async fn bump_digest(conn: &mut SqliteConnection, entity: &str) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO metadata_digest_version (entity, version) VALUES (?, 1)
         ON CONFLICT(entity) DO UPDATE
            SET version = metadata_digest_version.version + 1",
    )
    .bind(entity)
    .execute(conn)
    .await?;
    Ok(())
}
