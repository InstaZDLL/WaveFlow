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
//! ## Track is deferred
//!
//! Same reason as [`super::push`]: composite canonical +
//! album/artist relations need a dedicated upsert path.

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
        let outcome =
            pull_one(state, client, pool, entity, profile_canonical_id, &member.canonical_id).await;
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
    row.fields.get(key).and_then(|v| v.as_str()).map(str::to_string)
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

