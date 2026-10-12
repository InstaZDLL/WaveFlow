//! Genre, decade and artist mixes: one generated playlist per genre,
//! decade or artist the user listens to most.
//!
//! The artist mix is the odd one out, described at
//! [`regenerate_artist_mixes`]; what follows is the genre and decade
//! families.
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
use crate::scanner::canonical_name;

/// Listening window the seeds are ranked on, as for Daily Mix.
pub const LOOKBACK_DAYS: i64 = 90;
/// Mixes per family.
pub const MIXES_PER_FAMILY: i64 = 3;
/// Genre and decade seeds read per regen: the files a seed was admitted
/// on can merge into fewer songs than a mix needs, and a seed that falls
/// short must leave its place to the next one rather than cost a mix.
const SEED_CANDIDATES: i64 = MIXES_PER_FAMILY * 3;

/// The songs of a seed, copies merged, or `None` when they are too few
/// for a mix. Counted before the per-artist cap: a genre of 25 songs by 4
/// artists is a mix, of 12 tracks. [`pick`] merges the copies again,
/// which costs nothing here and covers the artist mixes, which do not
/// come through this check.
fn enough_songs(candidates: Vec<Candidate>) -> Option<Vec<Candidate>> {
    let songs = dedupe(candidates);
    ((songs.len() as i64) >= MIN_TRACKS).then_some(songs)
}
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
const ARTIST_POSITION: i64 = 10;
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
    /// `{name}` stands for the artist's name.
    pub artist_name: String,
    pub artist_description: String,
}

/// A track a mix may take.
#[derive(Debug, Clone, FromRow)]
pub struct Candidate {
    pub track_id: i64,
    pub primary_artist: Option<i64>,
    pub plays: i64,
    /// With the artist, what tells two files of one song apart from two
    /// songs ([`dedupe`]).
    pub title: String,
    pub bit_depth: Option<i64>,
    pub sample_rate: Option<i64>,
}

/// One candidate per song: a library holding the same track twice (an MP3
/// and its Hi-Res re-buy) would otherwise put it in a mix twice. Same
/// artist and same title under [`canonical_name`] is one song; the best
/// copy stands for it (bit depth, then sample rate, then the lowest id),
/// with the plays of every copy, so a song heard on two files is not
/// ranked as half as loved.
pub fn dedupe(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut songs: HashMap<(Option<i64>, String), Candidate> = HashMap::new();
    let mut order = Vec::new();
    for c in candidates {
        // Without an artist a title says nothing ("Track 1"): such a
        // track is only ever itself. `#` cannot start a canonical title,
        // which drops punctuation, so the two kinds of key never meet.
        // So does one whose title is all punctuation ("???"), which
        // folds to nothing.
        let title = canonical_name(&c.title);
        let key = match c.primary_artist {
            Some(_) if !title.is_empty() => (c.primary_artist, title),
            _ => (c.primary_artist, format!("#{}", c.track_id)),
        };
        match songs.get_mut(&key) {
            Some(kept) => {
                let plays = kept.plays + c.plays;
                let better = (c.bit_depth, c.sample_rate, std::cmp::Reverse(c.track_id))
                    > (
                        kept.bit_depth,
                        kept.sample_rate,
                        std::cmp::Reverse(kept.track_id),
                    );
                if better {
                    *kept = c;
                }
                kept.plays = plays;
            }
            None => {
                order.push(key.clone());
                songs.insert(key, c);
            }
        }
    }
    order.into_iter().filter_map(|k| songs.remove(&k)).collect()
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
        .bind(SEED_CANDIDATES)
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
        .bind(SEED_CANDIDATES)
        .fetch_all(pool)
        .await?)
}

/// Play counts are all-time: the window ranks the seeds, but a track
/// loved two years ago still belongs at the front of its genre's mix.
async fn genre_candidates(pool: &SqlitePool, genre_id: i64) -> CoreResult<Vec<Candidate>> {
    Ok(sqlx::query_as::<_, Candidate>(
        "SELECT t.id AS track_id, t.primary_artist, COUNT(pe.id) AS plays,
                t.title, t.bit_depth, t.sample_rate
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
        "SELECT t.id AS track_id, t.primary_artist, COUNT(pe.id) AS plays,
                t.title, t.bit_depth, t.sample_rate
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
    pick_with(candidates, seed, |_| PER_ARTIST_CAP)
}

/// [`pick`] with a cap of its own per artist.
fn pick_with(
    candidates: Vec<Candidate>,
    seed: u64,
    cap: impl Fn(Option<i64>) -> usize,
) -> Vec<i64> {
    let mut state = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    let mut scored: Vec<(f64, Candidate)> = dedupe(candidates)
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
        if *taken >= cap(c.primary_artist) {
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
    track_ids: Vec<i64>,
) -> CoreResult<Option<i64>> {
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
    for seed in genre_seeds(pool).await? {
        if created.len() as i64 >= MIXES_PER_FAMILY {
            break;
        }
        let i = created.len();
        let mix = Mix {
            rules: SmartPlaylistRules::GenreMix { genre_id: seed.id },
            name: labels.genre_name.replace("{name}", &seed.name),
            description: labels.genre_description.replace("{name}", &seed.name),
            position: GENRE_POSITION + i as i64,
            seed: day_seed(0x6E4E_5E00, seed.id),
        };
        let Some(songs) = enough_songs(genre_candidates(pool, seed.id).await?) else {
            continue;
        };
        let tracks = pick(songs, mix.seed);
        if let Some(id) = write_mix(pool, paths, profile_id, &mix, tracks).await? {
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
    for decade in decade_seeds(pool).await? {
        if created.len() as i64 >= MIXES_PER_FAMILY {
            break;
        }
        let i = created.len();
        let shown = decade.to_string();
        let mix = Mix {
            rules: SmartPlaylistRules::DecadeMix { decade },
            name: labels.decade_name.replace("{decade}", &shown),
            description: labels.decade_description.replace("{decade}", &shown),
            position: DECADE_POSITION + i as i64,
            seed: day_seed(0xDEC4_DE00, decade),
        };
        let Some(songs) = enough_songs(decade_candidates(pool, decade).await?) else {
            continue;
        };
        let tracks = pick(songs, mix.seed);
        if let Some(id) = write_mix(pool, paths, profile_id, &mix, tracks).await? {
            created.push(id);
        }
    }
    sweep(pool, "decade_mix", &created).await?;
    Ok(created)
}

/// Tracks the artist itself may take in its mix: the centre of it, still
/// under a third.
pub const SEED_ARTIST_CAP: usize = 15;
/// Neighbours an artist mix draws from, and tracks each may take.
pub const NEIGHBOURS: usize = 8;
pub const NEIGHBOUR_CAP: usize = 4;
/// An artist with fewer available tracks than this is not the centre of
/// a mix.
pub const MIN_SEED_TRACKS: i64 = 8;
/// Songs an artist mix needs. Lower than a genre's [`MIN_TRACKS`]: the mix
/// is mostly the artist, and its neighbours are often in a library by a
/// single or two — measured, eight of them brought 24 songs once copies
/// were merged, one short of the genre floor.
pub const MIN_ARTIST_MIX_TRACKS: i64 = 20;
/// Neighbours a mix needs: with one, it is a duet, not the artist's world.
pub const MIN_NEIGHBOURS: usize = 2;
/// A session neighbour needs this many shared plays…
pub const MIN_CO_LISTENS: i64 = 3;
/// …and this share of the strongest one's: past it, the list is artists
/// that happened to come up in the same shuffle, not the artist's company.
/// Measured on a real library: 45 plays of one lo-fi artist's closest
/// neighbour came within a session of it, and 5 of the eighth, a K-pop act.
pub const CO_LISTEN_SHARE: f64 = 0.25;

async fn artist_seeds(pool: &SqlitePool) -> CoreResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT ar.id, ar.name
           FROM play_event pe
           JOIN track t   ON t.id = pe.track_id AND t.is_available = 1
           JOIN artist ar ON ar.id = t.primary_artist
          WHERE pe.played_at >= ?
            AND (SELECT COUNT(*) FROM track own
                  WHERE own.primary_artist = ar.id AND own.is_available = 1) >= ?
          GROUP BY ar.id
          ORDER BY SUM(pe.listened_ms) DESC, ar.id
          LIMIT ?",
    )
    .bind(cutoff_ms())
    .bind(MIN_SEED_TRACKS)
    .bind(MIXES_PER_FAMILY)
    .fetch_all(pool)
    .await?)
}

async fn artists_candidates(pool: &SqlitePool, artist_ids: &[i64]) -> CoreResult<Vec<Candidate>> {
    if artist_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = artist_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT t.id AS track_id, t.primary_artist, COUNT(pe.id) AS plays,
                t.title, t.bit_depth, t.sample_rate
           FROM track t
           LEFT JOIN play_event pe ON pe.track_id = t.id
          WHERE t.is_available = 1 AND t.primary_artist IN ({placeholders})
          GROUP BY t.id"
    );
    let mut q = sqlx::query_as::<_, Candidate>(sqlx::AssertSqlSafe(sql));
    for id in artist_ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all(pool).await?)
}

/// An artist's neighbours: the similar-artists cache first, in its order,
/// then the artists the user plays in the same sessions — those played
/// beside it at least [`MIN_CO_LISTENS`] times and [`CO_LISTEN_SHARE`] as
/// often as the closest — up to [`NEIGHBOURS`]. Only artists with an available track count, so a
/// neighbour that would bring nothing does not take a place.
async fn neighbours(pool: &SqlitePool, artist_id: i64) -> CoreResult<Vec<i64>> {
    // The cache is read through the attached `app` database; a pool that
    // has none (a test, a tool) gets the listening half alone.
    let cached = crate::similar::cached_similar_library_ids(pool, artist_id)
        .await
        .unwrap_or_else(|err| {
            tracing::debug!(?err, artist_id, "artist mix: no similar-artists cache");
            Vec::new()
        });
    let mut out = Vec::new();
    for id in cached {
        let has_tracks: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM track WHERE primary_artist = ? AND is_available = 1)",
        )
        .bind(id)
        .fetch_one(pool)
        .await?;
        if has_tracks && !out.contains(&id) {
            out.push(id);
        }
        if out.len() >= NEIGHBOURS {
            return Ok(out);
        }
    }
    let co = crate::similar::co_listened_artists(pool, artist_id, NEIGHBOURS * 2).await?;
    let strongest = co.first().map_or(0, |(_, n)| *n);
    let floor = MIN_CO_LISTENS.max((strongest as f64 * CO_LISTEN_SHARE).ceil() as i64);
    for (id, shared) in co {
        if shared < floor {
            break;
        }
        if !out.contains(&id) {
            out.push(id);
        }
        if out.len() >= NEIGHBOURS {
            break;
        }
    }
    Ok(out)
}

/// Regenerate the artist mixes. Returns their ids, best seed first.
///
/// One per artist the user listened to most over the last
/// [`LOOKBACK_DAYS`] that has [`MIN_SEED_TRACKS`] available tracks: the
/// artist's own tracks (up to [`SEED_ARTIST_CAP`]) among those of its
/// neighbours (up to [`NEIGHBOUR_CAP`] each), scored and shuffled as the
/// other mixes are. The neighbours come from the similar-artists cache,
/// then from the user's own sessions — never from the network, so a
/// regeneration offline gives the same mixes. An artist with fewer than
/// [`MIN_NEIGHBOURS`] neighbours bringing tracks, or a mix under
/// [`MIN_ARTIST_MIX_TRACKS`], gets none: that is the artist's page shuffled, not a
/// mix. Unlike the genre and decade mixes there is no fallback without
/// history: the family is "the artists you play".
pub async fn regenerate_artist_mixes(
    pool: &SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
    labels: &MixLabels,
) -> CoreResult<Vec<i64>> {
    let mut created = Vec::new();
    for (i, (artist_id, name)) in artist_seeds(pool).await?.into_iter().enumerate() {
        let near = neighbours(pool, artist_id).await?;
        let mut members = vec![artist_id];
        members.extend(near.iter().copied());
        let candidates = artists_candidates(pool, &members).await?;
        let mix = Mix {
            rules: SmartPlaylistRules::ArtistMix { artist_id },
            name: labels.artist_name.replace("{name}", &name),
            description: labels.artist_description.replace("{name}", &name),
            position: ARTIST_POSITION + i as i64,
            seed: day_seed(0xA271_5700, artist_id),
        };
        let tracks = pick_with(candidates, mix.seed, |artist| {
            if artist == Some(artist_id) {
                SEED_ARTIST_CAP
            } else {
                NEIGHBOUR_CAP
            }
        });
        let neighbours_present = {
            let seen: HashSet<i64> = artist_of(pool, &tracks).await?;
            seen.iter().filter(|a| **a != artist_id).count()
        };
        if neighbours_present < MIN_NEIGHBOURS || (tracks.len() as i64) < MIN_ARTIST_MIX_TRACKS {
            continue;
        }
        if let Some(id) = write_mix(pool, paths, profile_id, &mix, tracks).await? {
            created.push(id);
        }
    }
    sweep(pool, "artist_mix", &created).await?;
    Ok(created)
}

/// The primary artists of `track_ids`.
async fn artist_of(pool: &SqlitePool, track_ids: &[i64]) -> CoreResult<HashSet<i64>> {
    if track_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let placeholders = track_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT DISTINCT primary_artist FROM track
          WHERE id IN ({placeholders}) AND primary_artist IS NOT NULL"
    );
    let mut q = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    for id in track_ids {
        q = q.bind(id);
    }
    Ok(q.fetch_all(pool).await?.into_iter().collect())
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
                    title: format!("Song {a}-{t}"),
                    bit_depth: None,
                    sample_rate: None,
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
    fn one_song_on_two_files_is_one_candidate_the_best_copy() {
        let copy = |id, title: &str, bit_depth, plays| Candidate {
            track_id: id,
            primary_artist: Some(1),
            plays,
            title: title.to_string(),
            bit_depth,
            sample_rate: Some(44_100),
        };
        let kept = dedupe(vec![
            copy(1, "Heaven", None, 3),
            copy(2, "heaven!", Some(24), 2),
            copy(3, "Meltdown", None, 0),
        ]);
        assert_eq!(kept.len(), 2);
        // The Hi-Res copy stands for the song, with both copies' plays.
        assert_eq!((kept[0].track_id, kept[0].plays), (2, 5));
        // The same title by another artist is another song.
        let other = Candidate {
            primary_artist: Some(2),
            ..copy(4, "Heaven", None, 0)
        };
        assert_eq!(dedupe(vec![copy(1, "Heaven", None, 0), other]).len(), 2);
        // Two tracks with no artist are two tracks, whatever their title.
        let anonymous = |id| Candidate {
            primary_artist: None,
            ..copy(id, "Track 1", None, 0)
        };
        assert_eq!(dedupe(vec![anonymous(5), anonymous(6)]).len(), 2);
        // Nor do two by one artist whose titles fold to nothing.
        assert_eq!(
            dedupe(vec![copy(7, "???", None, 0), copy(8, "!!!", None, 0)]).len(),
            2
        );
    }

    #[test]
    fn the_artist_keeps_its_own_larger_cap() {
        // Artist 0 is the seed: up to 15 of its tracks, 4 of anyone else.
        let picked = pick_with(candidates(12, 20), 5, |artist| {
            if artist == Some(0) {
                SEED_ARTIST_CAP
            } else {
                NEIGHBOUR_CAP
            }
        });
        let mut per_artist: HashMap<i64, usize> = HashMap::new();
        for id in &picked {
            *per_artist.entry(id / 100).or_default() += 1;
        }
        assert!(per_artist.get(&0).copied().unwrap_or(0) <= SEED_ARTIST_CAP);
        assert!(per_artist
            .iter()
            .filter(|(a, _)| **a != 0)
            .all(|(_, n)| *n <= NEIGHBOUR_CAP));
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
