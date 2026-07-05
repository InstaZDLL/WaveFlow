//! Per-artist manual overrides for bio + similar artists (issue #323).
//!
//! Offline-first users want the same manual control over an artist's
//! biography and similar list that they already have over the picture
//! (local `artist.jpg` sidecar). Both overrides are per-profile, live
//! in the profile DB, and take precedence at read time:
//!   - `artist.custom_bio` short-circuits [`enrich_artist_deezer`](super::deezer::enrich_artist_deezer)
//!   - `artist_similar_custom` short-circuits [`get_similar_artists`](super::similar::get_similar_artists)
//!
//! An enrichment pass writes the shared `app.metadata_artist` cache, so
//! it never clobbers these per-profile rows.

use serde::Serialize;

use crate::{error::AppResult, metadata_artwork, state::AppState};

/// Upper bound on a curated similar list — generous vs the 12-item
/// display cap, but stops a pathological payload from bloating the DB.
const MAX_SIMILAR: usize = 50;

/// One entry of a curated similar-artist list, resolved for the editor
/// chips (name + best available picture).
#[derive(Debug, Clone, Serialize)]
pub struct ArtistOverrideSimilar {
    pub artist_id: i64,
    pub name: String,
    pub picture_url: Option<String>,
    pub picture_path: Option<String>,
}

/// Current override state for one artist, used to pre-fill the editor.
#[derive(Debug, Clone, Serialize)]
pub struct ArtistOverrides {
    /// `None` when no bio override is set (the fetched bio is used).
    pub custom_bio: Option<String>,
    /// Empty when no similar override is set (the online list is used).
    pub similar: Vec<ArtistOverrideSimilar>,
}

/// Read the override state for one artist (bio text + curated similar
/// list resolved to names + pictures) so the editor modal can pre-fill.
#[tauri::command]
pub async fn get_artist_overrides(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<ArtistOverrides> {
    let pool = state.require_profile_pool().await?;
    let artwork_dir = &state.paths.metadata_artwork_dir;

    let custom_bio: Option<String> =
        sqlx::query_scalar("SELECT custom_bio FROM artist WHERE id = ?")
            .bind(artist_id)
            .fetch_optional(&pool)
            .await?
            .flatten();

    let rows: Vec<(i64, String, Option<String>, Option<String>)> = sqlx::query_as(
        r#"
        SELECT a.id, a.name, ma.picture_url, ma.picture_hash
          FROM artist_similar_custom sc
          JOIN artist a ON a.id = sc.similar_artist_id
          LEFT JOIN app.metadata_artist ma ON ma.deezer_id = a.deezer_id
         WHERE sc.artist_id = ?
         ORDER BY sc.position
        "#,
    )
    .bind(artist_id)
    .fetch_all(&pool)
    .await?;

    let similar = rows
        .into_iter()
        .map(
            |(id, name, picture_url, picture_hash)| ArtistOverrideSimilar {
                artist_id: id,
                name,
                picture_path: picture_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(artwork_dir, h)),
                picture_url,
            },
        )
        .collect();

    Ok(ArtistOverrides {
        custom_bio,
        similar,
    })
}

/// Set (or clear) **both** overrides for one artist in a single
/// transaction so a failure can't leave a half-applied state (e.g. the
/// bio saved but the similar list not). Pass `null`/blank `bio` to drop
/// the bio override; pass `null`/empty `similar_ids` to drop the similar
/// override. For similar, self-references and duplicates are dropped,
/// first-seen order is preserved and becomes the stored `position`, and
/// the list is capped at [`MAX_SIMILAR`].
#[tauri::command]
pub async fn set_artist_metadata_overrides(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
    bio: Option<String>,
    similar_ids: Option<Vec<i64>>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    let trimmed_bio = bio.map(|b| b.trim().to_string()).filter(|b| !b.is_empty());

    // Normalise similar: drop self + duplicates, keep first-seen order, cap.
    let mut seen = std::collections::HashSet::new();
    let cleaned: Vec<i64> = similar_ids
        .unwrap_or_default()
        .into_iter()
        .filter(|id| *id != artist_id && seen.insert(*id))
        .take(MAX_SIMILAR)
        .collect();

    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE artist SET custom_bio = ? WHERE id = ?")
        .bind(trimmed_bio)
        .bind(artist_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM artist_similar_custom WHERE artist_id = ?")
        .bind(artist_id)
        .execute(&mut *tx)
        .await?;
    for (position, similar_id) in cleaned.iter().enumerate() {
        sqlx::query(
            "INSERT INTO artist_similar_custom (artist_id, similar_artist_id, position)
             VALUES (?, ?, ?)",
        )
        .bind(artist_id)
        .bind(similar_id)
        .bind(position as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
