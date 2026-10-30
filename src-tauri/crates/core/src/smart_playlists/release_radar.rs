//! "Release Radar": the recent releases of the user's artists that the
//! library already holds, as a playlist.
//!
//! The releases come from the radar's own tables ([`crate::release_radar`]),
//! filled by a scan of MusicBrainz; this pass reads them and the library,
//! and never goes to the network, so a regeneration offline gives the
//! same playlist. Only a release matched to local tracks contributes: a
//! release the radar merely discovered is in the Release Radar view, with
//! its link, and never here.

use chrono::Utc;

use super::{cover, generator, PathsContext, SmartPlaylistRules};
use crate::error::{CoreError, CoreResult};
use crate::release_radar::{self, store, LocalMatch};

/// Tracks the playlist holds at most.
pub const TRACKS_LIMIT: usize = 50;

/// Tracks one release brings at most: a whole new album would otherwise
/// fill the playlist alone, and the point of the radar is the spread of
/// what came out.
pub const PER_RELEASE_CAP: usize = 5;

/// After On Repeat (0) and the Daily Mix slots (1–3), before the artist,
/// genre and decade mixes (10+).
const PLAYLIST_POSITION: i64 = 4;

/// The `LIKE` needle of the family's row.
const NEEDLE: &str = "\"kind\":\"release_radar\"";

/// The tracks of the playlist, from the releases newest first and each
/// release's own matches in their play order: at most
/// [`PER_RELEASE_CAP`] per release, [`TRACKS_LIMIT`] in all, and a track
/// two releases share (a single, then the album it is on) once, under
/// the newer one.
pub fn pick_tracks(
    releases_newest_first: &[String],
    matches: &std::collections::HashMap<String, LocalMatch>,
) -> Vec<i64> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for id in releases_newest_first {
        let Some(found) = matches.get(id) else {
            continue;
        };
        let mut taken = 0;
        for track_id in &found.track_ids {
            if taken == PER_RELEASE_CAP || out.len() == TRACKS_LIMIT {
                break;
            }
            if seen.insert(*track_id) {
                out.push(*track_id);
                taken += 1;
            }
        }
        if out.len() == TRACKS_LIMIT {
            break;
        }
    }
    out
}

/// Rewrite the profile's Release Radar playlist from the stored releases
/// and the library. `None` when no recent release is in the library, in
/// which case the row, if there was one, is removed: an empty radar
/// playlist says nothing the view does not say better.
pub async fn regenerate_release_radar(
    pool: &sqlx::SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
    description: &str,
) -> CoreResult<Option<i64>> {
    let cutoff = release_radar::cutoff_date(Utc::now(), release_radar::RECENT_DAYS);
    let releases = store::recent_releases(pool, &cutoff).await?;
    let refs: Vec<_> = releases.iter().map(|r| r.as_ref()).collect();
    let tracks = store::local_tracks(pool).await?;
    let matches = release_radar::match_releases(&refs, &tracks);
    let order: Vec<String> = releases.iter().map(|r| r.id.clone()).collect();
    let track_ids = pick_tracks(&order, &matches);

    if track_ids.is_empty() {
        sqlx::query("DELETE FROM playlist WHERE is_smart = 1 AND smart_rules LIKE ?")
            .bind(format!("%{NEEDLE}%"))
            .execute(pool)
            .await?;
        return Ok(None);
    }

    // The 2×2 composite of the first tracks' album art, as the genre and
    // decade mixes: the releases are the user's own files, and their
    // covers are what tells one radar from the next.
    let cover_paths =
        generator::first_track_artwork_paths(pool, paths, profile_id, &track_ids, 4).await;
    let cover_hash = if cover_paths.is_empty() {
        None
    } else {
        match cover::build_composite_cover(&cover_paths, &paths.metadata_artwork_dir) {
            Ok(hash) => Some(hash),
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "release radar cover render failed, falling back to gradient"
                );
                None
            }
        }
    };

    let rules_json = SmartPlaylistRules::ReleaseRadar
        .to_json()
        .map_err(|e| CoreError::Other(format!("smart rules serialize: {e}")))?;
    let id = generator::upsert_smart_playlist(
        pool,
        // The family's name, the same in every language, as Daily Mix
        // and On Repeat.
        "Release Radar",
        description,
        NEEDLE,
        PLAYLIST_POSITION,
        cover_hash.as_deref(),
        &rules_json,
        &track_ids,
    )
    .await?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests;
