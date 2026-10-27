//! What happens to a pair once it is linked: confirming, rejecting and
//! removing links, and the favourite, rating and playback preference
//! carried across one.

use std::path::PathBuf;

use sqlx::{Row, SqliteConnection, SqlitePool};

use crate::error::{AppError, AppResult};

use super::{
    now_ms, playlist_link_freshness, valid_full_hash, PlaylistLinkProof, PreferredLocalPlayback,
    ReconciliationLink,
};

/// Confirm one exact pair from an ambiguous group. The local file is re-read
/// immediately before the transaction so a stale UI cannot confirm old bytes.
pub async fn confirm_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT t.file_path, t.file_size, t.file_modified,
                rt.size AS remote_size, rt.full_hash
           FROM track t
           JOIN remote_track rt ON rt.remote_id = ?
          WHERE t.id = ? AND t.is_available = 1",
    )
    .bind(remote_track_id)
    .bind(local_track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other("local or remote track is unavailable".into()))?;

    let file_path: String = row.try_get("file_path")?;
    let local_size: i64 = row.try_get("file_size")?;
    let local_modified: i64 = row.try_get("file_modified")?;
    let remote_size: i64 = row.try_get("remote_size")?;
    let remote_hash: String = row.try_get("full_hash")?;
    if local_size != remote_size || !valid_full_hash(&remote_hash) {
        return Err(AppError::Other(
            "tracks do not share a valid exact-content candidate".into(),
        ));
    }

    // Reads the file, deliberately: this is a verification immediately before an
    // irreversible write, and the cache exists to avoid re-reading during
    // *discovery*. Going through `hashing::verify` rather than hashing directly
    // means the fresh reading also repairs whatever the cache held.
    let local_hash =
        super::super::hashing::verify(pool, local_track_id, &file_path, local_size, local_modified)
            .await?
            .ok_or_else(|| AppError::Other("local file could not be read".into()))?;
    if !local_hash.eq_ignore_ascii_case(&remote_hash) {
        return Err(AppError::Other("track contents no longer match".into()));
    }

    link_exact(pool, local_track_id, remote_track_id, &remote_hash).await
}

/// Write one exact link, in its own transaction.
///
/// Shared with the import path, which reaches this point having *just written*
/// the bytes and so already knowing the digest — the one case where an exact
/// link costs nothing. Keeping the write here rather than copying it is what
/// stops the two callers from drifting on the guard below: a link is refused
/// when either side is already linked elsewhere, because `remote_track_link`
/// is one-to-one in both directions and an `ON CONFLICT` on `local_track_id`
/// alone would silently steal a remote track from another local row.
pub(in crate::remote) async fn link_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
    full_hash: &str,
) -> AppResult<()> {
    if !valid_full_hash(full_hash) {
        return Err(AppError::Other("link proof is not a full digest".into()));
    }
    let normalized_hash = full_hash.to_ascii_lowercase();
    let now = now_ms();
    let mut tx = pool.begin().await?;
    let conflict: Option<(i64, String)> = sqlx::query_as(
        "SELECT local_track_id, remote_track_id FROM remote_track_link
          WHERE (local_track_id = ? AND remote_track_id != ?)
             OR (remote_track_id = ? AND local_track_id != ?)",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(remote_track_id)
    .bind(local_track_id)
    .fetch_optional(&mut *tx)
    .await?;
    if conflict.is_some() {
        return Err(AppError::Other(
            "one of these tracks is already linked elsewhere".into(),
        ));
    }

    sqlx::query(
        "INSERT INTO remote_track_link
            (local_track_id, remote_track_id, method, verified_full_hash,
             status, playback_preference, confirmed_at, verified_at)
         VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', ?, ?)
         ON CONFLICT(local_track_id) DO UPDATE SET
             remote_track_id = excluded.remote_track_id,
             method = excluded.method,
             verified_full_hash = excluded.verified_full_hash,
             status = excluded.status,
             verified_at = excluded.verified_at",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(&normalized_hash)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM remote_track_match_rejection
          WHERE local_track_id = ? AND remote_track_id = ?
            AND proof_kind = 'exact_full_hash' AND proof = ?",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(&normalized_hash)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Hide an exact candidate until its proof changes.
pub async fn reject_exact(
    pool: &SqlitePool,
    local_track_id: i64,
    remote_track_id: &str,
) -> AppResult<()> {
    let row = sqlx::query(
        "SELECT rt.full_hash,
                EXISTS(SELECT 1 FROM track t WHERE t.id = ?) AS local_exists,
                EXISTS(SELECT 1 FROM remote_track_link l
                        WHERE l.local_track_id = ? OR l.remote_track_id = ?) AS linked
           FROM remote_track rt WHERE rt.remote_id = ?",
    )
    .bind(local_track_id)
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::Other("remote track is unavailable".into()))?;
    let local_exists = row.try_get::<i64, _>("local_exists")? != 0;
    let linked = row.try_get::<i64, _>("linked")? != 0;
    let proof: String = row.try_get("full_hash")?;
    if !local_exists || !valid_full_hash(&proof) {
        return Err(AppError::Other("candidate is unavailable".into()));
    }
    if linked {
        return Err(AppError::Other("linked tracks cannot be rejected".into()));
    }

    sqlx::query(
        "INSERT OR IGNORE INTO remote_track_match_rejection
            (local_track_id, remote_track_id, proof_kind, proof, rejected_at)
         VALUES (?, ?, 'exact_full_hash', ?, ?)",
    )
    .bind(local_track_id)
    .bind(remote_track_id)
    .bind(proof.to_ascii_lowercase())
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn links(pool: &SqlitePool) -> AppResult<Vec<ReconciliationLink>> {
    let rows = sqlx::query(
        "SELECT l.local_track_id, l.remote_track_id, t.title AS local_title,
                rt.title AS remote_title, l.method, l.verified_full_hash,
                l.status, l.playback_preference, l.confirmed_at, l.verified_at,
                EXISTS(SELECT 1 FROM liked_track f WHERE f.track_id = t.id) AS local_favorite,
                EXISTS(SELECT 1 FROM remote_favorite f
                        WHERE f.entity_type = 'track' AND f.entity_id = l.remote_track_id) AS remote_favorite,
                t.rating AS local_rating,
                (SELECT rating FROM remote_rating r
                  WHERE r.entity_type = 'track' AND r.entity_id = l.remote_track_id) AS remote_rating,
                (SELECT count(*) FROM play_event p WHERE p.track_id = t.id) AS local_plays,
                (SELECT count(*) FROM remote_history h
                  WHERE h.track_remote_id = l.remote_track_id) AS remote_plays
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id
           LEFT JOIN remote_track rt ON rt.remote_id = l.remote_track_id
          ORDER BY lower(t.title), l.local_track_id",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let local_plays = row.try_get("local_plays")?;
            let remote_plays = row.try_get("remote_plays")?;
            Ok(ReconciliationLink {
                local_track_id: row.try_get("local_track_id")?,
                remote_track_id: row.try_get("remote_track_id")?,
                local_title: row.try_get("local_title")?,
                remote_title: row.try_get("remote_title")?,
                method: row.try_get("method")?,
                verified_full_hash: row.try_get("verified_full_hash")?,
                status: row.try_get("status")?,
                playback_preference: row.try_get("playback_preference")?,
                confirmed_at: row.try_get("confirmed_at")?,
                verified_at: row.try_get("verified_at")?,
                local_favorite: row.try_get::<i64, _>("local_favorite")? != 0,
                remote_favorite: row.try_get::<i64, _>("remote_favorite")? != 0,
                local_rating: row.try_get("local_rating")?,
                remote_rating: row.try_get("remote_rating")?,
                local_plays,
                remote_plays,
                combined_plays: local_plays + remote_plays,
            })
        })
        .collect()
}

async fn confirmed_link_on(
    conn: &mut SqliteConnection,
    local_track_id: i64,
) -> AppResult<(String, bool, bool, Option<i64>, Option<i64>)> {
    let row = sqlx::query(
        "SELECT l.remote_track_id, t.file_path, l.verified_full_hash,
                rt.full_hash AS remote_full_hash,
                EXISTS(SELECT 1 FROM liked_track f WHERE f.track_id = t.id) AS local_favorite,
                EXISTS(SELECT 1 FROM remote_favorite f
                        WHERE f.entity_type = 'track' AND f.entity_id = l.remote_track_id) AS remote_favorite,
                t.rating AS local_rating,
                (SELECT rating FROM remote_rating r
                  WHERE r.entity_type = 'track' AND r.entity_id = l.remote_track_id) AS remote_rating
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id AND t.is_available = 1
           JOIN remote_track rt ON rt.remote_id = l.remote_track_id
          WHERE l.local_track_id = ? AND l.status = 'confirmed'",
    )
    .bind(local_track_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::Other("confirmed visible reconciliation link not found".into()))?;
    let freshness = playlist_link_freshness(vec![PlaylistLinkProof {
        local_track_id,
        file_path: row.try_get("file_path")?,
        verified_full_hash: row.try_get("verified_full_hash")?,
        remote_full_hash: row.try_get("remote_full_hash")?,
    }])
    .await?;
    if freshness.get(&local_track_id) != Some(&true) {
        return Err(AppError::Other("reconciliation link is stale".into()));
    }
    Ok((
        row.try_get("remote_track_id")?,
        row.try_get::<i64, _>("local_favorite")? != 0,
        row.try_get::<i64, _>("remote_favorite")? != 0,
        row.try_get("local_rating")?,
        row.try_get("remote_rating")?,
    ))
}

pub async fn copy_favorite(
    pool: &SqlitePool,
    local_track_id: i64,
    direction: &str,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let (remote_id, local, remote, _, _) = confirmed_link_on(&mut tx, local_track_id).await?;
    match direction {
        "local_to_server" => {
            crate::remote::write::set_favorite_in_tx(&mut tx, "track", &remote_id, local).await?
        }
        "server_to_local" => {
            if remote {
                sqlx::query(
                    "INSERT OR REPLACE INTO liked_track (track_id, liked_at) VALUES (?, ?)",
                )
                .bind(local_track_id)
                .bind(now_ms())
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query("DELETE FROM liked_track WHERE track_id = ?")
                    .bind(local_track_id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        _ => return Err(AppError::Other("invalid user-data copy direction".into())),
    }
    tx.commit().await?;
    Ok(())
}

pub async fn copy_rating(pool: &SqlitePool, local_track_id: i64, direction: &str) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    let (remote_id, _, _, local, remote) = confirmed_link_on(&mut tx, local_track_id).await?;
    match direction {
        "local_to_server" => {
            let stars = local
                .map(|value| ((value.clamp(0, 255) * 5 + 127) / 255).clamp(1, 5) as u8)
                .unwrap_or(0);
            crate::remote::write::set_rating_in_tx(&mut tx, "track", &remote_id, stars).await?;
        }
        "server_to_local" => {
            // Reconciliation copies user data, never audio tags. A later local
            // edit may deliberately write the rating through the normal track
            // command, but this directional copy stays database-only.
            let popm = remote.map(|value| (value.clamp(1, 5) * 255 + 2) / 5);
            sqlx::query("UPDATE track SET rating = ? WHERE id = ?")
                .bind(popm)
                .bind(local_track_id)
                .execute(&mut *tx)
                .await?;
        }
        _ => return Err(AppError::Other("invalid user-data copy direction".into())),
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_playback_preference(
    pool: &SqlitePool,
    local_track_id: i64,
    preference: &str,
) -> AppResult<()> {
    if !matches!(preference, "local_first" | "server_first") {
        return Err(AppError::Other("invalid playback preference".into()));
    }
    let result = sqlx::query(
        "UPDATE remote_track_link SET playback_preference = ?
          WHERE local_track_id = ?",
    )
    .bind(preference)
    .bind(local_track_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::Other("reconciliation link not found".into()));
    }
    Ok(())
}

/// Resolve a remote track to the confirmed, available local file selected by
/// the user's playback preference. A missing file is treated like no local
/// candidate so callers can immediately fall back to the server.
/// The server track a local one was proved to be, for playing it when its
/// file is gone.
///
/// The mirror image of [`preferred_local_playback`], and the direction that
/// did not exist: server-to-local has always resolved, local-to-server never
/// did, so a library track whose file vanished simply failed. Only a
/// `confirmed` link qualifies — a stale one is a guess, and guessing which
/// recording to play instead is worse than saying the file is missing.
pub async fn linked_remote_track(
    pool: &SqlitePool,
    local_track_id: i64,
) -> AppResult<Option<String>> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT remote_track_id FROM remote_track_link
          WHERE local_track_id = ? AND status = 'confirmed'
          LIMIT 1",
    )
    .bind(local_track_id)
    .fetch_optional(pool)
    .await?)
}

pub async fn preferred_local_playback(
    pool: &SqlitePool,
    remote_track_id: &str,
) -> AppResult<Option<PreferredLocalPlayback>> {
    let row = sqlx::query(
        "SELECT t.id, t.file_path, t.duration_ms
           FROM remote_track_link l
           JOIN track t ON t.id = l.local_track_id
          WHERE l.remote_track_id = ?
            AND l.status = 'confirmed'
            AND l.playback_preference = 'local_first'
            AND t.is_available = 1
          LIMIT 1",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let path = PathBuf::from(row.try_get::<String, _>("file_path")?);
    if !path.is_file() {
        return Ok(None);
    }
    let duration_ms = row.try_get::<i64, _>("duration_ms")?.max(0) as u64;
    Ok(Some(PreferredLocalPlayback {
        track_id: row.try_get("id")?,
        path,
        duration_ms,
    }))
}

pub async fn remove_link(pool: &SqlitePool, local_track_id: i64) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    // Capture the link's matching evidence before deleting it and record a
    // rejection for the same pair — otherwise the next `discover` would just
    // auto-recreate the exact link the user manually removed. Same proof
    // representation as `reject_exact`, so `reconcile` honours it.
    if let Some(row) = sqlx::query(
        "SELECT remote_track_id, method, verified_full_hash
           FROM remote_track_link WHERE local_track_id = ?",
    )
    .bind(local_track_id)
    .fetch_optional(&mut *tx)
    .await?
    {
        let remote_track_id: String = row.try_get("remote_track_id")?;
        let method: String = row.try_get("method")?;
        let verified_full_hash: Option<String> = row.try_get("verified_full_hash")?;
        // Only an exact-hash link carries a full-hash proof to suppress.
        if method == "exact_full_hash" {
            if let Some(proof) = verified_full_hash.filter(|value| valid_full_hash(value)) {
                sqlx::query(
                    "INSERT OR IGNORE INTO remote_track_match_rejection
                        (local_track_id, remote_track_id, proof_kind, proof, rejected_at)
                     VALUES (?, ?, 'exact_full_hash', ?, ?)",
                )
                .bind(local_track_id)
                .bind(&remote_track_id)
                .bind(proof.to_ascii_lowercase())
                .bind(now_ms())
                .execute(&mut *tx)
                .await?;
            }
        }
    }

    sqlx::query("DELETE FROM remote_track_link WHERE local_track_id = ?")
        .bind(local_track_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
