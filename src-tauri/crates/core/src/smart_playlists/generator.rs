//! Daily Mix generation: pick the user's most-listened artists, split them
//! into tempo buckets, and materialize one playlist per bucket.
//!
//! The grouping signal is BPM (from `track_analysis`) because it gives a
//! musically coherent split with zero external services and degrades
//! gracefully when only some tracks are analysed: artists with no analysed
//! tracks fall into the medium bucket. A future iteration could swap this
//! for a co-occurrence graph mined from `play_event` sessions, but at the
//! single-user / few-thousand-event scale BPM produces something that
//! actually feels like distinct mixes ("calm", "groove", "energy") rather
//! than three random shuffles of the same listening history.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use sqlx::{FromRow, SqlitePool};

use super::cover;
use super::SmartPlaylistRules;
use crate::error::CoreResult;
use crate::smart_playlists::PathsContext;

/// How far back the generator looks for listening events. 90 days strikes a
/// balance between staleness (a one-off binge from last summer shouldn't
/// dominate) and sample size (a casual listener may only have a few weeks
/// of meaningful data).
const LOOKBACK_DAYS: i64 = 90;

/// Maximum artists per bucket. The cover composes up to 3 strips so we keep
/// enough top artists for the picture even after some are filtered out for
/// missing pictures.
const ARTISTS_PER_BUCKET: usize = 12;

/// Tracks per generated mix. 50 ≈ 2-3 hours of listening, matching the user
/// reference (Spotify's Daily Mix is ~50 tracks).
const TRACKS_PER_MIX: usize = 50;

/// Minimum number of distinct top artists required before we attempt to
/// generate any mix at all. Below this the buckets degenerate into "the
/// same 5 tracks shuffled three different ways", which is worse than no
/// mix at all.
const MIN_ARTISTS: usize = 6;

/// Deterministic shuffle seed prefix — XOR'd with the bucket slot so each
/// regen produces the same order on the same input set (no UI flicker when
/// the user reopens the same mix), but different ordering across buckets.
/// Arbitrary 64-bit constant; the only requirement is that it's nonzero so
/// the xorshift step inside [`shuffle_with_seed`] doesn't degenerate.
const SHUFFLE_SEED: u64 = 0xDA17_1A1A_DEAD_BEEF;

#[derive(Debug, FromRow)]
struct ArtistListenRow {
    artist_id: i64,
    /// Kept on the row so `ORDER BY listened_ms DESC` in the SQL maps to a
    /// real Rust value — also handy for future debug logging when a bucket
    /// looks underweight.
    #[allow(dead_code)]
    listened_ms: i64,
    median_bpm: Option<f64>,
    picture_hash: Option<String>,
}

#[derive(Debug, FromRow)]
struct TrackPickRow {
    track_id: i64,
    /// Surfaced by `COUNT(pe.id)`, used to drive `ORDER BY play_count DESC`
    /// in SQL. Not read in Rust but kept on the row so the column can be
    /// referenced safely if/when we want to log the cluster's top track.
    #[allow(dead_code)]
    play_count: i64,
}

/// Identifier for one tempo bucket. Ordering matches the slot numbers shown
/// to the user (Daily Mix 1 = calm, 2 = groove, 3 = energy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bucket {
    Calm,
    Groove,
    Energy,
}

impl Bucket {
    const ALL: [Bucket; 3] = [Bucket::Calm, Bucket::Groove, Bucket::Energy];

    fn slot(self) -> u8 {
        match self {
            Bucket::Calm => 1,
            Bucket::Groove => 2,
            Bucket::Energy => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Bucket::Calm => "Daily Mix 1",
            Bucket::Groove => "Daily Mix 2",
            Bucket::Energy => "Daily Mix 3",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Bucket::Calm => "Vibes apaisées tirées de tes écoutes récentes",
            Bucket::Groove => "Le cœur de tes playlists du moment",
            Bucket::Energy => "Tempo élevé, énergie haute",
        }
    }

    /// Inclusive lower / exclusive upper BPM bounds. Tracks with no BPM are
    /// routed to `Groove` so missing analysis doesn't black-hole an artist.
    fn matches(self, bpm: f64) -> bool {
        match self {
            Bucket::Calm => bpm < 95.0,
            Bucket::Groove => (95.0..130.0).contains(&bpm),
            Bucket::Energy => bpm >= 130.0,
        }
    }
}

/// Regenerate every Daily Mix slot from the active profile's listening
/// history. Returns the playlist ids that were created or refreshed, in slot
/// order, so the caller can navigate the user straight to the first one.
///
/// `profile_id` is needed to resolve the per-profile artwork directory
/// (`<root>/profiles/<id>/artwork/<hash>.<format>`) used as the cover-image
/// fallback when none of the cluster's top artists have a Deezer picture in
/// the shared cache.
pub async fn regenerate_daily_mixes(
    pool: &SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
) -> CoreResult<Vec<i64>> {
    let cutoff_ms = Utc::now().timestamp_millis() - (LOOKBACK_DAYS * 86_400_000);

    // Top artists by total listened time over the lookback window. We join
    // out to `track_analysis` to pull the median BPM per artist (used for
    // bucketing) and to the shared `app.deezer_artist` cache for the cover
    // picture hash. The cross-database join is done in two steps because
    // sqlx with sqlite can't transparently span attached schemas.
    let artists = top_artists_with_bpm(pool, paths, cutoff_ms).await?;
    if artists.len() < MIN_ARTISTS {
        tracing::info!(
            count = artists.len(),
            min = MIN_ARTISTS,
            "smart playlists: not enough listening data, skipping regen"
        );
        return Ok(vec![]);
    }

    // Bucket the artists. Each artist lands in exactly one bucket; ties
    // (e.g. an artist with no analysis whose tracks straddle two buckets)
    // are resolved to Groove so the medium bucket stays well-populated.
    let mut by_bucket: HashMap<Bucket, Vec<&ArtistListenRow>> = HashMap::new();
    for art in &artists {
        let bucket = match art.median_bpm {
            Some(bpm) if bpm.is_finite() => Bucket::ALL
                .iter()
                .copied()
                .find(|b| b.matches(bpm))
                .unwrap_or(Bucket::Groove),
            _ => Bucket::Groove,
        };
        by_bucket.entry(bucket).or_default().push(art);
    }

    let mut created: Vec<i64> = Vec::with_capacity(Bucket::ALL.len());
    for bucket in Bucket::ALL {
        let bucket_artists = by_bucket.get(&bucket).cloned().unwrap_or_default();
        if bucket_artists.is_empty() {
            // Skip empty buckets without leaving a stale playlist behind —
            // an "Energy" mix with no tracks would be confusing.
            delete_existing_slot(pool, bucket.slot()).await?;
            continue;
        }
        // `generate_one_mix` returns `None` when the bucket's listening
        // history yields no playable tracks (rare but happens on a
        // user whose only matching artists are now unavailable). Skip
        // such slots instead of pushing a sentinel `0` into the result
        // vector — the Home view would otherwise try to navigate to
        // playlist id 0.
        if let Some(id) = generate_one_mix(pool, paths, profile_id, bucket, &bucket_artists).await?
        {
            created.push(id);
        }
    }
    Ok(created)
}

/// Build (or refresh) the playlist for a single bucket and return its id.
/// Returns `None` when no playable tracks could be picked for the bucket —
/// any stale playlist row for the slot is removed in that case.
async fn generate_one_mix(
    pool: &SqlitePool,
    paths: &PathsContext,
    profile_id: i64,
    bucket: Bucket,
    artists: &[&ArtistListenRow],
) -> CoreResult<Option<i64>> {
    let top_artist_ids: Vec<i64> = artists
        .iter()
        .take(ARTISTS_PER_BUCKET)
        .map(|a| a.artist_id)
        .collect();

    let tracks = pick_tracks_for_artists(pool, &top_artist_ids).await?;
    if tracks.is_empty() {
        delete_existing_slot(pool, bucket.slot()).await?;
        return Ok(None);
    }

    // Deterministic shuffle so the same input set produces the same listening
    // order — no flicker when the user revisits the playlist mid-session.
    let mut shuffled: Vec<i64> = tracks.iter().map(|t| t.track_id).collect();
    shuffle_with_seed(&mut shuffled, SHUFFLE_SEED ^ bucket.slot() as u64);
    shuffled.truncate(TRACKS_PER_MIX);

    // Cover image source priority:
    //  1. Top 3 artists' Deezer pictures (shared metadata cache) — looks
    //     best because they're consistent portrait crops.
    //  2. Fallback: album artwork of the first 3 shuffled tracks (per-profile
    //     local cache) — always present for any track with embedded art,
    //     so this guarantees we ship a real cover even when the cluster
    //     has no Deezer-enriched artists (lots of niche / soundtrack libs).
    let mut cover_paths: Vec<PathBuf> = artists
        .iter()
        .take(3)
        .filter_map(|a| {
            let hash = a.picture_hash.as_deref()?;
            crate::artwork::metadata::existing_path(&paths.metadata_artwork_dir, hash)
                .map(PathBuf::from)
        })
        .collect();
    let deezer_pics = cover_paths.len();
    if cover_paths.is_empty() {
        cover_paths = first_track_artwork_paths(pool, paths, profile_id, &shuffled, 3).await;
    }
    tracing::info!(
        slot = bucket.slot(),
        deezer_pics,
        fallback_album_arts = if deezer_pics == 0 {
            cover_paths.len()
        } else {
            0
        },
        total_paths = cover_paths.len(),
        "smart cover image sources resolved"
    );
    let cover_hash = if cover_paths.is_empty() {
        tracing::warn!(
            slot = bucket.slot(),
            "smart cover: no image sources available — playlist will use gradient fallback"
        );
        None
    } else {
        match cover::build_daily_mix_cover(&cover_paths, &paths.metadata_artwork_dir) {
            Ok(h) => {
                tracing::info!(slot = bucket.slot(), hash = %h, "smart cover rendered");
                Some(h)
            }
            Err(err) => {
                tracing::warn!(?err, "smart cover render failed, falling back to gradient");
                None
            }
        }
    };

    let rules = SmartPlaylistRules::DailyMix {
        slot: bucket.slot(),
    };
    let rules_json = rules
        .to_json()
        .map_err(|e| crate::error::CoreError::Audio(format!("smart rules serialize: {e}")))?;
    let needle = format!("\"slot\":{}", bucket.slot());
    let id = upsert_smart_playlist(
        pool,
        bucket.label(),
        bucket.description(),
        &needle,
        bucket.slot() as i64,
        cover_hash.as_deref(),
        &rules_json,
        &shuffled,
    )
    .await?;
    Ok(Some(id))
}

async fn top_artists_with_bpm(
    pool: &SqlitePool,
    paths: &PathsContext,
    cutoff_ms: i64,
) -> CoreResult<Vec<ArtistListenRow>> {
    // Step 1: aggregate listened_ms per artist + median BPM, all in the
    // profile DB (the cross-DB picture hash is added in step 2).
    #[derive(FromRow)]
    struct Step1 {
        artist_id: i64,
        listened_ms: i64,
        median_bpm: Option<f64>,
        deezer_id: Option<i64>,
    }
    let step1: Vec<Step1> = sqlx::query_as(
        r#"
        SELECT a.id                     AS artist_id,
               SUM(pe.listened_ms)      AS listened_ms,
               -- SQLite doesn't ship a true median; AVG over BPMs is good
               -- enough for bucket assignment and avoids a window function
               -- (compatibility with older builds).
               AVG(ta.bpm)              AS median_bpm,
               a.deezer_id              AS deezer_id
          FROM play_event pe
          JOIN track t           ON t.id  = pe.track_id
          JOIN track_artist ta2  ON ta2.track_id = t.id AND ta2.position = 0
          JOIN artist a          ON a.id  = ta2.artist_id
          LEFT JOIN track_analysis ta ON ta.track_id = t.id
         WHERE pe.played_at >= ?
           AND t.is_available = 1
         GROUP BY a.id
        HAVING listened_ms > 0
         ORDER BY listened_ms DESC
         LIMIT 60
        "#,
    )
    .bind(cutoff_ms)
    .fetch_all(pool)
    .await?;

    if step1.is_empty() {
        return Ok(vec![]);
    }

    // Step 2: look up the picture hash for any deezer_id we collected. We
    // open a short-lived connection to app.db rather than ATTACH because
    // ATTACH would require routing through the same pool the deezer
    // commands use — easier to stay connection-local here.
    let app_db_path = &paths.app_db_path;
    let pool_app = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", app_db_path.display()))
        .await
        .ok();
    let pictures: HashMap<i64, String> = if let Some(app_pool) = pool_app {
        let ids: Vec<i64> = step1.iter().filter_map(|s| s.deezer_id).collect();
        let mut map = HashMap::new();
        if !ids.is_empty() {
            let placeholders = std::iter::repeat("?")
                .take(ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT deezer_id, picture_hash FROM deezer_artist
                 WHERE deezer_id IN ({placeholders}) AND picture_hash IS NOT NULL"
            );
            #[derive(FromRow)]
            struct PRow {
                deezer_id: i64,
                picture_hash: String,
            }
            let rows: Vec<PRow> = {
                let mut q = sqlx::query_as::<_, PRow>(sqlx::AssertSqlSafe(sql));
                for id in &ids {
                    q = q.bind(*id);
                }
                q.fetch_all(&app_pool).await.unwrap_or_default()
            };
            for r in rows {
                map.insert(r.deezer_id, r.picture_hash);
            }
        }
        map
    } else {
        HashMap::new()
    };

    Ok(step1
        .into_iter()
        .map(|s| ArtistListenRow {
            artist_id: s.artist_id,
            listened_ms: s.listened_ms,
            median_bpm: s.median_bpm,
            picture_hash: s.deezer_id.and_then(|id| pictures.get(&id).cloned()),
        })
        .collect())
}

/// Resolve the on-disk artwork path for the first `take` tracks of the
/// mix's shuffled order. Used as the cover-image fallback when none of the
/// cluster's top artists have a Deezer picture cached. Tracks without
/// embedded artwork are silently skipped — we walk the input until `take`
/// hits or the source is exhausted.
pub(super) async fn first_track_artwork_paths(
    pool: &SqlitePool,
    paths: &PathsContext,
    // `profile_id` was previously required to resolve the per-profile
    // artwork directory; that path is now carried by `PathsContext`
    // itself. The argument is kept for the public symmetry with the
    // other `generator::*` helpers — easier to spot at the call site
    // which profile a job is running for.
    _profile_id: i64,
    track_ids: &[i64],
    take: usize,
) -> Vec<PathBuf> {
    if track_ids.is_empty() {
        return vec![];
    }
    // Fetch artwork hashes for every candidate in one round-trip; we walk
    // the shuffled order client-side so the strip layout matches the
    // playlist's first-track ordering.
    let placeholders = std::iter::repeat("?")
        .take(track_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    // Track artwork is reached through the album: `track.album_id`
    // → `album.artwork_id` → `artwork.hash` + `artwork.format`. There's
    // no direct `track.artwork_id` column.
    let sql = format!(
        r#"
        SELECT t.id          AS track_id,
               aw.hash       AS hash,
               aw.format     AS format
          FROM track t
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.id IN ({placeholders})
           AND aw.hash IS NOT NULL
        "#
    );
    #[derive(FromRow)]
    struct Row {
        track_id: i64,
        hash: String,
        format: String,
    }
    let mut q = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql));
    for id in track_ids {
        q = q.bind(*id);
    }
    let rows = match q.fetch_all(pool).await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(?err, "smart cover: artwork fallback query failed");
            return vec![];
        }
    };
    // Re-order by the shuffled track sequence so the strips reflect "the
    // first N tracks of the mix" rather than whatever order SQLite chose.
    let by_id: HashMap<i64, (String, String)> = rows
        .into_iter()
        .map(|r| (r.track_id, (r.hash, r.format)))
        .collect();
    let artwork_dir = paths.profile_artwork_dir.clone();
    let mut out = Vec::with_capacity(take);
    for id in track_ids {
        if out.len() >= take {
            break;
        }
        let Some((hash, format)) = by_id.get(id) else {
            continue;
        };
        let p = artwork_dir.join(format!("{hash}.{format}"));
        if p.exists() {
            out.push(p);
        }
    }
    out
}

async fn pick_tracks_for_artists(
    pool: &SqlitePool,
    artist_ids: &[i64],
) -> CoreResult<Vec<TrackPickRow>> {
    if artist_ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = std::iter::repeat("?")
        .take(artist_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        r#"
        SELECT t.id                  AS track_id,
               COUNT(pe.id)          AS play_count
          FROM track t
          JOIN track_artist ta ON ta.track_id = t.id AND ta.position = 0
          LEFT JOIN play_event pe ON pe.track_id = t.id
         WHERE ta.artist_id IN ({placeholders})
           AND t.is_available = 1
         GROUP BY t.id
         ORDER BY play_count DESC, t.id ASC
         LIMIT 200
        "#,
    );
    let mut q = sqlx::query_as::<_, TrackPickRow>(sqlx::AssertSqlSafe(sql));
    for id in artist_ids {
        q = q.bind(*id);
    }
    Ok(q.fetch_all(pool).await?)
}

/// Insert or refresh a smart playlist row keyed by a `smart_rules` JSON
/// needle. Idempotent: a previous row matching the same needle is wiped
/// (tracks + metadata) before the new contents are written, so repeated
/// regens never stack up duplicate playlists.
///
/// `needle` is the substring used in the `LIKE '%...%'` lookup against
/// `smart_rules`. Pass a fragment unique to the family (e.g. `"slot":2`
/// for Daily Mix slot 2, or `"kind":"on_repeat"` for the On Repeat
/// family) so the upsert can't accidentally match a sibling row.
pub(super) async fn upsert_smart_playlist(
    pool: &SqlitePool,
    name: &str,
    description: &str,
    needle: &str,
    position: i64,
    cover_hash: Option<&str>,
    rules_json: &str,
    track_ids: &[i64],
) -> CoreResult<i64> {
    let now = Utc::now().timestamp_millis();
    let mut tx = pool.begin().await?;

    // SQLite has no first-class JSON ops here so we LIKE on the raw
    // string — the caller-supplied needle must be specific enough to
    // identify the family + slot unambiguously.
    let existing: Option<(i64,)> = sqlx::query_as(
        r#"
        SELECT id FROM playlist
         WHERE is_smart = 1
           AND smart_rules LIKE ?
         LIMIT 1
        "#,
    )
    .bind(format!("%{needle}%"))
    .fetch_optional(&mut *tx)
    .await?;

    let playlist_id = match existing {
        Some((id,)) => {
            // Refresh metadata + clear out the old tracks before re-inserting.
            // `position` is included so a family that shifts its sort order
            // between releases (e.g. moving On Repeat from 1 to 0 to land
            // ahead of Daily Mix) actually re-anchors existing rows instead
            // of silently keeping the stale position from the first regen.
            sqlx::query(
                r#"
                UPDATE playlist
                   SET name        = ?,
                       description = ?,
                       cover_hash  = ?,
                       smart_rules = ?,
                       position    = ?,
                       updated_at  = ?
                 WHERE id = ?
                "#,
            )
            .bind(name)
            .bind(description)
            .bind(cover_hash)
            .bind(rules_json)
            .bind(position)
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM playlist_track WHERE playlist_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            id
        }
        None => {
            let res = sqlx::query(
                r#"
                INSERT INTO playlist
                    (name, description, color_id, icon_id, is_smart,
                     smart_rules, cover_hash, position, created_at, updated_at)
                VALUES (?, ?, 'violet', 'sparkles', 1, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(name)
            .bind(description)
            .bind(rules_json)
            .bind(cover_hash)
            .bind(position) // caller decides sort order vs sibling smart playlists
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            res.last_insert_rowid()
        }
    };

    // Bulk insert the tracks. `position` follows the shuffled order so the
    // PlaylistView reads the mix in the intended sequence.
    for (pos, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(playlist_id, track_id) DO UPDATE SET position = excluded.position
            "#,
        )
        .bind(playlist_id)
        .bind(*track_id)
        .bind(pos as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(playlist_id)
}

async fn delete_existing_slot(pool: &SqlitePool, slot: u8) -> CoreResult<()> {
    let needle = format!("\"slot\":{slot}");
    sqlx::query("DELETE FROM playlist WHERE is_smart = 1 AND smart_rules LIKE ?")
        .bind(format!("%{needle}%"))
        .execute(pool)
        .await?;
    Ok(())
}

/// In-place Fisher–Yates with a seeded xorshift RNG. Mirrors the helper in
/// [`crate::queue`] (we deliberately avoid pulling in a heavier RNG crate
/// for shuffling — these aren't security-critical operations) but takes a
/// caller-supplied seed so the order is reproducible across regens.
fn shuffle_with_seed<T>(slice: &mut [T], seed: u64) {
    // xorshift requires a nonzero seed; fall back to a magic constant so a
    // zero coming in from the caller doesn't produce a degenerate "no
    // shuffle" output.
    let mut state: u64 = if seed == 0 {
        0x9E37_79B9_7F4A_7C15
    } else {
        seed
    };
    for i in (1..slice.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let j = (state % (i as u64 + 1)) as usize;
        slice.swap(i, j);
    }
}

/// Round-trip the rules JSON shape so a bad serializer change is caught at
/// compile/test time rather than at runtime when the regenerator can't find
/// its own previously-written rows.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_json_contains_slot_for_lookup() {
        let json = SmartPlaylistRules::DailyMix { slot: 2 }
            .to_json()
            .expect("serialize");
        // The upsert query LIKEs on `"slot":N` — guard the format here so
        // a serde rename doesn't silently break refresh-in-place behaviour.
        assert!(
            json.contains("\"slot\":2"),
            "slot serialized incorrectly: {json}"
        );
    }

    #[test]
    fn bucket_thresholds_partition_full_bpm_range() {
        // Every plausible BPM should match exactly one bucket.
        for bpm_int in 30..=220 {
            let bpm = bpm_int as f64;
            let matches: Vec<_> = Bucket::ALL.iter().filter(|b| b.matches(bpm)).collect();
            assert_eq!(
                matches.len(),
                1,
                "bpm {bpm} matched {} buckets",
                matches.len()
            );
        }
    }
}
