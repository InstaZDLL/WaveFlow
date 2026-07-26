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
use sqlx::SqlitePool;

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
    split_artist_inner(&pool, artist_id).await
}

/// DB-only core of [`split_artist`], split out so integration tests can
/// drive it against a migrated in-memory profile DB without a Tauri
/// `AppState`.
pub(crate) async fn split_artist_inner(
    pool: &SqlitePool,
    artist_id: i64,
) -> AppResult<SplitArtistResult> {
    let name: String = sqlx::query_scalar("SELECT name FROM artist WHERE id = ?")
        .bind(artist_id)
        .fetch_optional(pool)
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
                // Inherit the phantom's own role (a phantom credited as a
                // `feature` splits into featured artists, not main ones).
                for &nid in &new_ids {
                    let key = (nid, role.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use waveflow_core::scanner::canonical_name;

    /// In-memory profile DB migrated with the real schema (FKs on) so the
    /// relink + cleanup guards are exercised against the same constraints
    /// production uses — a stripped fixture would hide a broken FK guard.
    async fn pool() -> SqlitePool {
        let opts = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        // One library so tracks satisfy the NOT NULL `library_id` FK.
        sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn seed_artist(pool: &SqlitePool, name: &str) -> i64 {
        sqlx::query("INSERT INTO artist (name, canonical_name) VALUES (?, ?)")
            .bind(name)
            .bind(canonical_name(name))
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    async fn seed_track(pool: &SqlitePool, id: i64, primary: Option<i64>) -> i64 {
        sqlx::query(
            "INSERT INTO track
                (id, library_id, file_path, file_hash, file_size, file_modified,
                 title, primary_artist, duration_ms, added_at)
             VALUES (?, 1, ?, 'h', 0, 0, 't', ?, 0, 0)",
        )
        .bind(id)
        .bind(format!("/f/{id}.flac"))
        .bind(primary)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn link(pool: &SqlitePool, track: i64, artist: i64, role: &str, position: i64) {
        sqlx::query(
            "INSERT INTO track_artist (track_id, artist_id, role, position) VALUES (?, ?, ?, ?)",
        )
        .bind(track)
        .bind(artist)
        .bind(role)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn credits(pool: &SqlitePool, track: i64) -> Vec<(i64, String, i64)> {
        sqlx::query_as(
            "SELECT artist_id, role, position FROM track_artist
              WHERE track_id = ? ORDER BY position",
        )
        .bind(track)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn primary_of(pool: &SqlitePool, track: i64) -> Option<i64> {
        sqlx::query_scalar("SELECT primary_artist FROM track WHERE id = ?")
            .bind(track)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn artist_exists(pool: &SqlitePool, id: i64) -> bool {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artist WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
        n == 1
    }

    #[tokio::test]
    async fn splits_a_phantom_reusing_existing_rows() {
        let pool = pool().await;
        // Two of the three fragments already have enriched rows.
        let a = seed_artist(&pool, "Tibeauthetraveler").await;
        let b = seed_artist(&pool, "Nogymx").await;
        let phantom = seed_artist(&pool, "Tibeauthetraveler, Nogymx, Osaki").await;
        let t = seed_track(&pool, 10, Some(phantom)).await;
        link(&pool, t, phantom, "main", 0).await;

        let res = split_artist_inner(&pool, phantom).await.unwrap();

        // Existing rows reused; "Osaki" created fresh.
        assert_eq!(res.artists.len(), 3);
        assert_eq!(res.artists[0].id, a, "first fragment reuses existing row");
        assert_eq!(res.artists[1].id, b, "second fragment reuses existing row");
        let osaki = res.artists[2].id;
        assert!(osaki != phantom && osaki != a && osaki != b);
        assert_eq!(res.tracks_relinked, 1);
        assert!(res.phantom_deleted);

        // track_artist rebuilt to the three, in order, role `main`.
        assert_eq!(
            credits(&pool, t).await,
            vec![
                (a, "main".into(), 0),
                (b, "main".into(), 1),
                (osaki, "main".into(), 2),
            ]
        );
        // Primary repointed to the first fragment; phantom row gone.
        assert_eq!(primary_of(&pool, t).await, Some(a));
        assert!(!artist_exists(&pool, phantom).await);
    }

    #[tokio::test]
    async fn deduplicates_repeated_fragments() {
        let pool = pool().await;
        let phantom = seed_artist(&pool, "A, B, A").await;
        let t = seed_track(&pool, 1, Some(phantom)).await;
        link(&pool, t, phantom, "main", 0).await;

        let res = split_artist_inner(&pool, phantom).await.unwrap();

        // "A, B, A" collapses to two distinct artists.
        let names: Vec<&str> = res.artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["A", "B"]);
        assert_eq!(credits(&pool, t).await.len(), 2);
    }

    #[tokio::test]
    async fn keeps_the_phantom_when_still_referenced() {
        let pool = pool().await;
        let phantom = seed_artist(&pool, "A, B").await;
        let t = seed_track(&pool, 1, Some(phantom)).await;
        link(&pool, t, phantom, "main", 0).await;
        // An album still credits the phantom as its artist → the cleanup
        // guard must leave the row alone (deleting it would SET NULL the
        // album's artist).
        sqlx::query(
            "INSERT INTO album (id, title, canonical_title, artist_id) VALUES (1, 'Al', 'al', ?)",
        )
        .bind(phantom)
        .execute(&pool)
        .await
        .unwrap();

        let res = split_artist_inner(&pool, phantom).await.unwrap();

        assert!(!res.phantom_deleted);
        assert!(artist_exists(&pool, phantom).await);
        // ...but the track is still relinked away from the phantom.
        assert!(credits(&pool, t)
            .await
            .iter()
            .all(|(aid, _, _)| *aid != phantom));
    }

    #[tokio::test]
    async fn preserves_cocredit_order_and_roles() {
        let pool = pool().await;
        let x = seed_artist(&pool, "X").await;
        let y = seed_artist(&pool, "Y").await;
        let phantom = seed_artist(&pool, "A, B").await;
        let t = seed_track(&pool, 1, Some(x)).await;
        // X(main,0), phantom(main,1), Y(feature,2).
        link(&pool, t, x, "main", 0).await;
        link(&pool, t, phantom, "main", 1).await;
        link(&pool, t, y, "feature", 2).await;

        let res = split_artist_inner(&pool, phantom).await.unwrap();
        let a = res.artists[0].id;
        let b = res.artists[1].id;

        // Phantom expanded in place; co-credits + roles kept, positions
        // renumbered contiguously.
        assert_eq!(
            credits(&pool, t).await,
            vec![
                (x, "main".into(), 0),
                (a, "main".into(), 1),
                (b, "main".into(), 2),
                (y, "feature".into(), 3),
            ]
        );
        // Primary was X (not the phantom) → untouched. Phantom had no
        // other references → deleted.
        assert_eq!(primary_of(&pool, t).await, Some(x));
        assert!(res.phantom_deleted);
    }

    #[tokio::test]
    async fn inherits_the_phantoms_role_when_not_main() {
        let pool = pool().await;
        let x = seed_artist(&pool, "X").await;
        let phantom = seed_artist(&pool, "A, B").await;
        let t = seed_track(&pool, 1, Some(x)).await;
        // X is the main artist; the phantom is credited as a `feature`.
        link(&pool, t, x, "main", 0).await;
        link(&pool, t, phantom, "feature", 1).await;

        let res = split_artist_inner(&pool, phantom).await.unwrap();
        let a = res.artists[0].id;
        let b = res.artists[1].id;

        // Both fragments inherit the phantom's `feature` role, not `main`.
        assert_eq!(
            credits(&pool, t).await,
            vec![
                (x, "main".into(), 0),
                (a, "feature".into(), 1),
                (b, "feature".into(), 2),
            ]
        );
    }

    #[tokio::test]
    async fn refuses_a_name_without_a_comma() {
        let pool = pool().await;
        let solo = seed_artist(&pool, "Solo").await;
        assert!(split_artist_inner(&pool, solo).await.is_err());
    }
}
