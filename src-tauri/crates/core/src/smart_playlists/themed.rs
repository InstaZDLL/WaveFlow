//! Genre and decade mixes: one generated playlist per genre or decade the
//! user listens to most.
//!
//! The other families partition the library on tempo (Daily Mix) or on
//! recency (On Repeat); these cut it along axes of their own, which is
//! what makes them worth having beside the rest. Both families work the
//! same way and differ only in how a mix's tracks are found:
//!
//! 1. **Seeds.** The genres (or decades) with the most listening over the
//!    last [`LOOKBACK_DAYS`], the largest in the library breaking ties —
//!    so a library with no history yet still gets mixes, of what it holds
//!    most of. A seed needs [`MIN_TRACKS`] tracks by [`MIN_ARTISTS`]
//!    artists: below that a "mix" is one album on repeat.
//! 2. **Tracks.** Every available track of the seed, scored by how often
//!    it was played plus a jitter drawn from a per-day seed, so the mix
//!    leans on what the user plays and still brings back what they have
//!    not, and changes from one day to the next without moving within
//!    one. At most [`PER_ARTIST_CAP`] tracks per artist.
//! 3. **Rows.** Upserted by their rule, positioned after Daily Mix, and
//!    the family's rows that were not regenerated are removed: a genre
//!    that fell out of the top three takes its mix with it.
//!
//! Names come from the frontend, in the user's language
//! ([`MixLabels`]): the decade and the genre name are data, the words
//! around them are not.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde::Deserialize;
use sqlx::{FromRow, SqlitePool};

use super::{cover, generator, PathsContext, SmartPlaylistRules};
use crate::error::{CoreError, CoreResult};

/// Listening window the seeds are ranked on, as for Daily Mix.
pub const LOOKBACK_DAYS: i64 = 90;
/// Mixes per family.
pub const MIXES_PER_FAMILY: i64 = 3;
/// Tracks per mix, as for Daily Mix.
pub const TRACKS_PER_MIX: usize = 50;
/// A seed below this many available tracks is not a mix.
pub const MIN_TRACKS: i64 = 25;
/// …nor one held by fewer artists than this.
pub const MIN_ARTISTS: i64 = 4;
/// Tracks one artist may take in a mix: enough to be present, not enough
/// to turn a genre mix into that artist's best-of.
pub const PER_ARTIST_CAP: usize = 3;

/// Sort positions: after On Repeat (0) and the Daily Mix slots (1–3).
const GENRE_POSITION: i64 = 20;
const DECADE_POSITION: i64 = 30;

/// The words around a mix's name and description, in the user's language.
/// `{name}` stands for the genre's name and `{decade}` for the decade
/// ("1990").
#[derive(Debug, Clone, Deserialize)]
pub struct MixLabels {
    pub genre_name: String,
    pub genre_description: String,
    pub decade_name: String,
    pub decade_description: String,
}

/// A track a mix may take.
#[derive(Debug, Clone, FromRow)]
pub struct Candidate {
    pub track_id: i64,
    pub primary_artist: Option<i64>,
    pub plays: i64,
}

#[derive(FromRow)]
struct GenreSeed {
    id: i64,
    name: String,
}

/// Recent listening per track, for ranking seeds.
const RECENT: &str = "LEFT JOIN (SELECT pe.track_id, SUM(pe.listened_ms) AS ms
                         FROM play_event pe
                        WHERE pe.played_at >= ?
                        GROUP BY pe.track_id) recent ON recent.track_id = t.id";

/// The year a track is filed under, the bounds the Years tab uses.
const DECADE_OF: &str = "(t.year / 10) * 10";

fn cutoff_ms() -> i64 {
    Utc::now().timestamp_millis() - LOOKBACK_DAYS * 86_400_000
}

/// Day number, so a mix changes once a day and not between two visits.
fn today() -> u64 {
    (Utc::now().timestamp_millis() / 86_400_000).max(0) as u64
}

async fn genre_seeds(pool: &SqlitePool) -> CoreResult<Vec<GenreSeed>> {
    let sql = format!(
        "SELECT g.id, g.name
           FROM genre g
           JOIN track_genre tg ON tg.genre_id = g.id
           JOIN track t        ON t.id = tg.track_id AND t.is_available = 1
           {RECENT}
          GROUP BY g.id
         HAVING COUNT(DISTINCT t.id) >= ? AND COUNT(DISTINCT t.primary_artist) >= ?
          ORDER BY COALESCE(SUM(recent.ms), 0) DESC, COUNT(DISTINCT t.id) DESC, g.id
          LIMIT ?"
    );
    Ok(sqlx::query_as::<_, GenreSeed>(sqlx::AssertSqlSafe(sql))
        .bind(cutoff_ms())
        .bind(MIN_TRACKS)
        .bind(MIN_ARTISTS)
        .bind(MIXES_PER_FAMILY)
        .fetch_all(pool)
        .await?)
}

async fn decade_seeds(pool: &SqlitePool) -> CoreResult<Vec<i64>> {
    let sql = format!(
        "SELECT {DECADE_OF} AS decade
           FROM track t
           {RECENT}
          WHERE t.is_available = 1 AND t.year BETWEEN 1 AND 9999
          GROUP BY decade
         HAVING COUNT(t.id) >= ? AND COUNT(DISTINCT t.primary_artist) >= ?
          ORDER BY COALESCE(SUM(recent.ms), 0) DESC, COUNT(t.id) DESC, decade DESC
          LIMIT ?"
    );
    Ok(sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
        .bind(cutoff_ms())
        .bind(MIN_TRACKS)
        .bind(MIN_ARTISTS)
        .bind(MIXES_PER_FAMILY)
        .fetch_all(pool)
        .await?)
}

/// Play counts are all-time: the window ranks the seeds, but a track
/// loved two years ago still belongs at the front of its genre's mix.
async fn genre_candidates(pool: &SqlitePool, genre_id: i64) -> CoreResult<Vec<Candidate>> {
    Ok(sqlx::query_as::<_, Candidate>(
        "SELECT t.id AS track_id, t.primary_artist, COUNT(pe.id) AS plays
           FROM track t
           JOIN track_genre tg ON tg.track_id = t.id AND tg.genre_id = ?
           LEFT JOIN play_event pe ON pe.track_id = t.id
          WHERE t.is_available = 1
          GROUP BY t.id",
    )
    .bind(genre_id)
    .fetch_all(pool)
    .await?)
}

async fn decade_candidates(pool: &SqlitePool, decade: i64) -> CoreResult<Vec<Candidate>> {
    Ok(sqlx::query_as::<_, Candidate>(
        "SELECT t.id AS track_id, t.primary_artist, COUNT(pe.id) AS plays
           FROM track t
           LEFT JOIN play_event pe ON pe.track_id = t.id
          WHERE t.is_available = 1 AND t.year BETWEEN ? AND ?
          GROUP BY t.id",
    )
    .bind(decade)
    .bind(decade + 9)
    .fetch_all(pool)
    .await?)
}

/// xorshift64: the shuffles here are not security-critical, and a seeded
/// generator makes a day's mix reproducible.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn unit(state: &mut u64) -> f64 {
    (next(state) >> 11) as f64 / (1u64 << 53) as f64
}

/// The mix: scored, capped per artist, then shuffled, all from `seed`.
///
/// A play is worth `ln(1 + plays)` and the jitter up to 1, so one play
/// already outweighs most of the draw while a track never played can
/// still beat one played once: the mix leans on what the user plays
/// without being made of it alone.
pub fn pick(candidates: Vec<Candidate>, seed: u64) -> Vec<i64> {
    let mut state = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    let mut scored: Vec<(f64, Candidate)> = candidates
        .into_iter()
        .map(|c| ((c.plays.max(0) as f64).ln_1p() + unit(&mut state), c))
        .collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.track_id.cmp(&b.1.track_id))
    });
    let mut per_artist: HashMap<Option<i64>, usize> = HashMap::new();
    let mut out = Vec::with_capacity(TRACKS_PER_MIX);
    for (_, c) in scored {
        if out.len() >= TRACKS_PER_MIX {
            break;
        }
        let taken = per_artist.entry(c.primary_artist).or_insert(0);
        if *taken >= PER_ARTIST_CAP {
            continue;
        }
        *taken += 1;
        out.push(c.track_id);
    }
    // Ranked best-first, the mix would play its favourites up front and
    // its discoveries last; shuffled, they are spread through it.
    for i in (1..out.len()).rev() {
        let j = (next(&mut state) % (i as u64 + 1)) as usize;
        out.swap(i, j);
    }
    out
}

/// One family's seed, resolved to what its row needs.
struct Mix {
    rules: SmartPlaylistRules,
    name: String,
    description: String,
    position: i64,
    seed: u64,
}

/// The `LIKE` needle of a rule: its JSON without the opening brace. The
/// closing one stays, so genre 1 cannot match genre 12.
fn needle(rules_json: &str) -> &str {
    rules_json.strip_prefix('{').unwrap_or(rules_json)
}

/// Write one mix, or `None` when its seed yielded no playable track.
async fn write_mix(
    pool: &SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
    mix: &Mix,
    candidates: Vec<Candidate>,
) -> CoreResult<Option<i64>> {
    let track_ids = pick(candidates, mix.seed);
    if track_ids.is_empty() {
        return Ok(None);
    }
    let cover_paths =
        generator::first_track_artwork_paths(pool, paths, profile_id, &track_ids, 4).await;
    let cover_hash = if cover_paths.is_empty() {
        None
    } else {
        match cover::build_composite_cover(&cover_paths, &paths.metadata_artwork_dir) {
            Ok(hash) => Some(hash),
            Err(err) => {
                tracing::warn!(?err, "mix cover render failed, falling back to gradient");
                None
            }
        }
    };
    let rules_json = mix
        .rules
        .to_json()
        .map_err(|e| CoreError::Other(format!("smart rules serialize: {e}")))?;
    let id = generator::upsert_smart_playlist(
        pool,
        &mix.name,
        &mix.description,
        needle(&rules_json),
        mix.position,
        cover_hash.as_deref(),
        &rules_json,
        &track_ids,
    )
    .await?;
    Ok(Some(id))
}

/// Remove the family's rows that this regen did not write.
async fn sweep(pool: &SqlitePool, kind: &str, kept: &[i64]) -> CoreResult<()> {
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM playlist WHERE is_smart = 1 AND smart_rules LIKE ?")
            .bind(format!("%\"kind\":\"{kind}\"%"))
            .fetch_all(pool)
            .await?;
    let kept: HashSet<i64> = kept.iter().copied().collect();
    for id in ids.into_iter().filter(|id| !kept.contains(id)) {
        sqlx::query("DELETE FROM playlist WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// A per-family, per-seed, per-day seed for [`pick`].
fn day_seed(family: u64, key: i64) -> u64 {
    family
        ^ (key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ today().wrapping_mul(0xD1B5_4A32_D192_ED03)
}

/// Regenerate the genre mixes. Returns their ids, best seed first.
pub async fn regenerate_genre_mixes(
    pool: &SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
    labels: &MixLabels,
) -> CoreResult<Vec<i64>> {
    let mut created = Vec::new();
    for (i, seed) in genre_seeds(pool).await?.into_iter().enumerate() {
        let mix = Mix {
            rules: SmartPlaylistRules::GenreMix { genre_id: seed.id },
            name: labels.genre_name.replace("{name}", &seed.name),
            description: labels.genre_description.replace("{name}", &seed.name),
            position: GENRE_POSITION + i as i64,
            seed: day_seed(0x6E4E_5E00, seed.id),
        };
        let candidates = genre_candidates(pool, seed.id).await?;
        if let Some(id) = write_mix(pool, paths, profile_id, &mix, candidates).await? {
            created.push(id);
        }
    }
    sweep(pool, "genre_mix", &created).await?;
    Ok(created)
}

/// Regenerate the decade mixes. Returns their ids, best seed first.
pub async fn regenerate_decade_mixes(
    pool: &SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
    labels: &MixLabels,
) -> CoreResult<Vec<i64>> {
    let mut created = Vec::new();
    for (i, decade) in decade_seeds(pool).await?.into_iter().enumerate() {
        let shown = decade.to_string();
        let mix = Mix {
            rules: SmartPlaylistRules::DecadeMix { decade },
            name: labels.decade_name.replace("{decade}", &shown),
            description: labels.decade_description.replace("{decade}", &shown),
            position: DECADE_POSITION + i as i64,
            seed: day_seed(0xDEC4_DE00, decade),
        };
        let candidates = decade_candidates(pool, decade).await?;
        if let Some(id) = write_mix(pool, paths, profile_id, &mix, candidates).await? {
            created.push(id);
        }
    }
    sweep(pool, "decade_mix", &created).await?;
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates(artists: usize, per_artist: usize) -> Vec<Candidate> {
        let mut out = Vec::new();
        for a in 0..artists {
            for t in 0..per_artist {
                out.push(Candidate {
                    track_id: (a * 100 + t) as i64,
                    primary_artist: Some(a as i64),
                    plays: 0,
                });
            }
        }
        out
    }

    #[test]
    fn no_artist_takes_more_than_the_cap() {
        let picked = pick(candidates(30, 10), 7);
        assert_eq!(picked.len(), TRACKS_PER_MIX);
        let mut per_artist: HashMap<i64, usize> = HashMap::new();
        for id in &picked {
            *per_artist.entry(id / 100).or_default() += 1;
        }
        assert!(per_artist.values().all(|n| *n <= PER_ARTIST_CAP));
    }

    #[test]
    fn the_same_seed_gives_the_same_mix_and_another_seed_another() {
        assert_eq!(pick(candidates(30, 10), 7), pick(candidates(30, 10), 7));
        assert_ne!(pick(candidates(30, 10), 7), pick(candidates(30, 10), 8));
    }

    #[test]
    fn played_tracks_lead_without_filling_the_mix() {
        // One heavily played track per artist among unplayed ones: every
        // one of them makes the mix, and the rest is still drawn.
        let mut pool = candidates(30, 10);
        for c in pool.iter_mut().filter(|c| c.track_id % 100 == 0) {
            c.plays = 40;
        }
        let picked = pick(pool, 3);
        let favourites = picked.iter().filter(|id| *id % 100 == 0).count();
        assert_eq!(favourites, 30);
        assert_eq!(picked.len(), TRACKS_PER_MIX);
    }

    #[test]
    fn needles_are_unique_per_seed_and_per_family() {
        let genre = |id| {
            SmartPlaylistRules::GenreMix { genre_id: id }
                .to_json()
                .unwrap()
        };
        let one = genre(1);
        let twelve = genre(12);
        assert_eq!(one, r#"{"kind":"genre_mix","genre_id":1}"#);
        assert!(
            !twelve.contains(needle(&one)),
            "genre 1 must not match genre 12"
        );
        let decade = SmartPlaylistRules::DecadeMix { decade: 1990 }
            .to_json()
            .unwrap();
        assert_eq!(decade, r#"{"kind":"decade_mix","decade":1990}"#);
        // Neither family matches the other ones' needles, nor theirs.
        let daily = SmartPlaylistRules::DailyMix {
            slot: 1,
            album_mode: false,
        }
        .to_json()
        .unwrap();
        for other in [&daily, &decade] {
            assert!(!other.contains(needle(&one)));
        }
        assert!(!one.contains("\"slot\":"));
        assert!(!decade.contains("\"slot\":"));
    }
}
