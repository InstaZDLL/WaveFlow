//! User-driven "split this artist" action (issue #396).
//!
//! The scanner deliberately splits a multi-artist `ARTIST` tag on `"; "`
//! only — a comma can be part of a real name (`Tyler, The Creator`,
//! `Earth, Wind & Fire`), so splitting on `", "` at scan time silently
//! fragments those into bogus rows. The documented remedy is to re-tag
//! with `"; "`, but that doesn't scale to a comma-joined library and
//! leaves the track stranded on a dead-end **phantom** artist (named
//! after the whole comma string) while the real, enrichable artists
//! already exist next to it.
//!
//! This command is the explicit, user-driven escape hatch: the user
//! clicks "Split" on a phantom, and we re-link every track that credits
//! it to the individual comma-separated artists, **reusing existing rows
//! by canonical name** (so the tracks immediately point at the already-
//! enriched artist), then delete the now-orphaned phantom. No file is
//! re-tagged. The scanner's skip-branch has a matching guard
//! ([`commands/scan.rs`](super::scan)) so an unchanged-file rescan doesn't
//! collapse the split back into the phantom.

use serde::Serialize;

use waveflow_core::scanner::upsert_artist;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// One artist produced by a split, in tag order.
#[derive(Debug, Clone, Serialize)]
pub struct SplitArtist {
    pub id: i64,
    pub name: String,
}

/// Outcome of [`split_artist`], returned so the UI can navigate to the
/// new primary artist and toast the result.
#[derive(Debug, Clone, Serialize)]
pub struct SplitArtistResult {
    /// The resolved individual artists, in tag order. The first is the
    /// new primary artist.
    pub artists: Vec<SplitArtist>,
    /// How many tracks were re-linked away from the phantom.
    pub tracks_relinked: usize,
    /// Whether the now-orphaned phantom row was removed.
    pub phantom_deleted: bool,
}

/// Split a comma-joined phantom artist into its individual artists and
/// re-link every track that credits it. See the module docs for the why.
///
/// Fails with a clear message when the name has no comma-separated parts
/// (nothing to split) or when every part canonicalises back to the
/// phantom itself.
#[tauri::command]
pub async fn split_artist(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<SplitArtistResult> {
    let pool = state.require_profile_pool().await?;

    let name: String = sqlx::query_scalar("SELECT name FROM artist WHERE id = ?")
        .bind(artist_id)
        .fetch_optional(&*pool)
        .await?
        .ok_or_else(|| AppError::Other(format!("artist {artist_id} not found")))?;

    // Comma-split — the deliberate opposite of the scanner's `"; "`-only
    // policy, gated behind this explicit user action so a real name like
    // "Tyler, The Creator" is never fragmented without intent.
    let parts: Vec<String> = name
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if parts.len() < 2 {
        return Err(AppError::Other(
            "this artist name has no comma-separated parts to split".into(),
        ));
    }

    let mut tx = pool.begin().await?;

    // Resolve each fragment, reusing an existing row by canonical name so
    // a track split off "A, B, C" lands on the already-enriched A / B / C
    // rows; skip the phantom itself in case a fragment canonicalises back
    // to it.
    let mut new_ids: Vec<i64> = Vec::new();
    for part in &parts {
        if let Some(id) = upsert_artist(&mut tx, part).await? {
            if id != artist_id && !new_ids.contains(&id) {
                new_ids.push(id);
            }
        }
    }
    if new_ids.is_empty() {
        return Err(AppError::Other(
            "splitting this artist produced no distinct artists".into(),
        ));
    }

    // Re-link every track that credits the phantom. Read the whole credit
    // list per track so co-credited artists and their order survive, and
    // expand the phantom in place into the split fragments (role `main`).
    let track_ids: Vec<i64> =
        sqlx::query_scalar("SELECT DISTINCT track_id FROM track_artist WHERE artist_id = ?")
            .bind(artist_id)
            .fetch_all(&mut *tx)
            .await?;

    for tid in &track_ids {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT artist_id, role FROM track_artist WHERE track_id = ? ORDER BY position",
        )
        .bind(tid)
        .fetch_all(&mut *tx)
        .await?;

        let mut rebuilt: Vec<(i64, String)> = Vec::new();
        let mut seen: std::collections::HashSet<(i64, String)> = std::collections::HashSet::new();
        for (aid, role) in rows {
            if aid == artist_id {
                for &nid in &new_ids {
                    let key = (nid, "main".to_string());
                    if seen.insert(key.clone()) {
                        rebuilt.push(key);
                    }
                }
            } else if seen.insert((aid, role.clone())) {
                rebuilt.push((aid, role));
            }
        }

        sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
            .bind(tid)
            .execute(&mut *tx)
            .await?;
        for (position, (aid, role)) in rebuilt.iter().enumerate() {
            sqlx::query(
                "INSERT INTO track_artist (track_id, artist_id, role, position)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(tid)
            .bind(aid)
            .bind(role)
            .bind(position as i64)
            .execute(&mut *tx)
            .await?;
        }
    }

    // Repoint any track whose primary artist was the phantom to the first
    // fragment — matches the scanner's own re-normalisation rule.
    let primary = new_ids[0];
    sqlx::query("UPDATE track SET primary_artist = ? WHERE primary_artist = ?")
        .bind(primary)
        .bind(artist_id)
        .execute(&mut *tx)
        .await?;

    // Clean up the phantom only when nothing references it any more —
    // guarded on every FK into `artist` so we never SET NULL an album's
    // artist or CASCADE a curated similar list out from under the user.
    let remaining_ta: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track_artist WHERE artist_id = ?")
            .bind(artist_id)
            .fetch_one(&mut *tx)
            .await?;
    let remaining_album: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album WHERE artist_id = ?")
        .bind(artist_id)
        .fetch_one(&mut *tx)
        .await?;
    let remaining_primary: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE primary_artist = ?")
            .bind(artist_id)
            .fetch_one(&mut *tx)
            .await?;
    let remaining_curated: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM artist_similar_custom
          WHERE artist_id = ? OR similar_artist_id = ?",
    )
    .bind(artist_id)
    .bind(artist_id)
    .fetch_one(&mut *tx)
    .await?;
    let phantom_deleted =
        remaining_ta == 0 && remaining_album == 0 && remaining_primary == 0 && remaining_curated == 0;
    if phantom_deleted {
        sqlx::query("DELETE FROM artist WHERE id = ?")
            .bind(artist_id)
            .execute(&mut *tx)
            .await?;
    }

    // Resolve display names for the response before the pool closes.
    let mut artists = Vec::with_capacity(new_ids.len());
    for &id in &new_ids {
        let nm: String = sqlx::query_scalar("SELECT name FROM artist WHERE id = ?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        artists.push(SplitArtist { id, name: nm });
    }

    tx.commit().await?;

    Ok(SplitArtistResult {
        artists,
        tracks_relinked: track_ids.len(),
        phantom_deleted,
    })
}
