//! "On Repeat" generation: top ~30 tracks by play count over a short
//! lookback window. Mirrors Spotify's daily-refreshed "On Repeat"
//! playlist — the songs the user can't stop replaying right now.
//!
//! Unlike [`crate::smart_playlists::generator`] (Daily Mix), this family
//! has no tempo bucketing and no shuffle: the playlist is the top-N tracks
//! in straight play-count order, so the user sees their #1 most-played
//! song first. That makes the page useful as a "current rotation" snapshot
//! and keeps the regen output deterministic without needing a seed.

use chrono::Utc;
use sqlx::{FromRow, SqlitePool};

use super::{cover, generator, SmartPlaylistRules};
use crate::error::AppResult;
use crate::paths::AppPaths;

/// Short lookback so "On Repeat" reflects what the user is actually
/// rotating right now — a 90-day window would let last quarter's binges
/// drown out the current week. 30 days matches Spotify's behaviour.
const LOOKBACK_DAYS: i64 = 30;

/// Track count target. Spotify's "On Repeat" sits around 30; this gives
/// roughly two hours of listening which is plenty for a "current
/// rotation" snapshot without diluting into half-forgotten plays.
const TRACKS_LIMIT: usize = 30;

/// Minimum number of distinct tracks the user has played in the window
/// before we materialize anything. Below this the playlist would be
/// "the same handful of songs you've already heard a lot" and isn't
/// adding value beyond the History view.
const MIN_TRACKS: usize = 8;

/// Position written to `playlist.position` so On Repeat sorts ahead of
/// every Daily Mix slot in the Home carousel. Daily Mix uses 1/2/3, so 0
/// keeps it first without colliding.
const PLAYLIST_POSITION: i64 = 0;

#[derive(Debug, FromRow)]
struct TrackPlayRow {
    track_id: i64,
    /// Surfaced by `COUNT(pe.id)`; used implicitly by `ORDER BY play_count
    /// DESC` in the SQL. Kept on the struct so the column reference can
    /// be logged if a future debug pass wants to see the histogram.
    #[allow(dead_code)]
    play_count: i64,
}

/// Regenerate the active profile's On Repeat playlist from the last
/// 30 days of `play_event` rows. Returns the playlist id (or `None`
/// when the window has too few distinct tracks to make a meaningful
/// playlist — in that case any previously-materialized row is removed
/// so a stale playlist doesn't linger after a quiet month).
///
/// Cover artwork is brand-rendered (no per-library imagery) so a
/// profile id is not required here; future per-track-art families
/// (Release Radar, Recently Added) will take a `profile_id` to resolve
/// the active library's artwork cache.
pub async fn regenerate_on_repeat(pool: &SqlitePool, paths: &AppPaths) -> AppResult<Option<i64>> {
    let cutoff_ms = Utc::now().timestamp_millis() - (LOOKBACK_DAYS * 86_400_000);
    let tracks = top_played_tracks(pool, cutoff_ms).await?;

    if tracks.len() < MIN_TRACKS {
        tracing::info!(
            count = tracks.len(),
            min = MIN_TRACKS,
            "smart playlists: not enough recent listening data for On Repeat, skipping"
        );
        delete_existing(pool).await?;
        return Ok(None);
    }

    let track_ids: Vec<i64> = tracks
        .iter()
        .take(TRACKS_LIMIT)
        .map(|t| t.track_id)
        .collect();

    // Cover: branded artwork (deep indigo → violet diagonal gradient
    // with a pink infinity loop motif) rendered deterministically by
    // [`cover::build_on_repeat_cover`]. This family has a fixed visual
    // identity — *not* a contact sheet of the user's library — which
    // matches how Spotify presents On Repeat and reads as a distinct
    // surface from Daily Mix's composite covers.
    let cover_hash = match cover::build_on_repeat_cover(&paths.metadata_artwork_dir) {
        Ok(h) => {
            tracing::info!(hash = %h, "smart cover (on repeat) rendered");
            Some(h)
        }
        Err(err) => {
            tracing::warn!(?err, "smart cover (on repeat) render failed");
            None
        }
    };

    let rules = SmartPlaylistRules::OnRepeat;
    let id = generator::upsert_smart_playlist(
        pool,
        "On Repeat",
        "Tes morceaux les plus écoutés ces 30 derniers jours",
        // Needle matches the JSON shape produced by `SmartPlaylistRules`
        // serde encoding for the unit variant. Stable as long as the
        // serde rename rule on the enum stays kebab/snake-case.
        "\"kind\":\"on_repeat\"",
        PLAYLIST_POSITION,
        cover_hash.as_deref(),
        &rules.to_json(),
        &track_ids,
    )
    .await?;
    Ok(Some(id))
}

async fn top_played_tracks(pool: &SqlitePool, cutoff_ms: i64) -> AppResult<Vec<TrackPlayRow>> {
    Ok(sqlx::query_as::<_, TrackPlayRow>(
        r#"
        SELECT pe.track_id        AS track_id,
               COUNT(pe.id)       AS play_count
          FROM play_event pe
          JOIN track t ON t.id = pe.track_id
         WHERE pe.played_at >= ?
           AND t.is_available  = 1
         GROUP BY pe.track_id
        HAVING play_count > 0
         ORDER BY play_count DESC, MAX(pe.played_at) DESC
         LIMIT 60
        "#,
    )
    .bind(cutoff_ms)
    .fetch_all(pool)
    .await?)
}

async fn delete_existing(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query("DELETE FROM playlist WHERE is_smart = 1 AND smart_rules LIKE ?")
        .bind("%\"kind\":\"on_repeat\"%")
        .execute(pool)
        .await?;
    Ok(())
}

/// Guard the JSON shape of the [`SmartPlaylistRules::OnRepeat`] variant
/// since the upsert / delete needles `LIKE` on its raw text. A serde
/// rename here would silently break refresh-in-place and stack up
/// duplicate playlists on every regen.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_json_carries_on_repeat_kind() {
        let json = SmartPlaylistRules::OnRepeat.to_json();
        assert!(
            json.contains("\"kind\":\"on_repeat\""),
            "OnRepeat serialized incorrectly: {json}"
        );
    }
}
