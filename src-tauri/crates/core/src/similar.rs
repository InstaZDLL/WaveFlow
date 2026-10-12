//! Artists near another one, from what is already on this machine: the
//! similar-artists cache and the user's own listening. Nothing here goes
//! to the network; filling the cache is the app's job
//! (`commands::similar::ensure_similar_cached`).

use std::collections::HashMap;

use chrono::Utc;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::error::CoreResult;
use crate::scanner::canonical_name;

#[derive(Debug, Deserialize, Default)]
struct RawSimilar {
    name: String,
}

/// The library artists the `app.lastfm_similar` cache lists as similar to
/// `artist_id`, most similar first, the artist itself left out. Stale
/// entries (past `expires_at`) are used anyway: a slightly outdated list
/// beats none. Empty when the artist was never looked up.
///
/// Needs the `app` database attached, as every profile pool has it.
pub async fn cached_similar_library_ids(pool: &SqlitePool, artist_id: i64) -> CoreResult<Vec<i64>> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM artist WHERE id = ?")
        .bind(artist_id)
        .fetch_optional(pool)
        .await?;
    let Some(name) = name else {
        return Ok(Vec::new());
    };
    let canonical = canonical_name(&name);
    if canonical.is_empty() {
        return Ok(Vec::new());
    }

    let payload: Option<String> =
        sqlx::query_scalar("SELECT payload FROM app.lastfm_similar WHERE name_canonical = ?")
            .bind(&canonical)
            .fetch_optional(pool)
            .await?;
    let Some(payload) = payload else {
        return Ok(Vec::new());
    };
    let raw: Vec<RawSimilar> = serde_json::from_str(&payload).unwrap_or_default();
    if raw.is_empty() {
        return Ok(Vec::new());
    }

    // One query for every name, then put them back in the cache's order
    // so the closest matches stay first.
    let canonicals: Vec<String> = raw.iter().map(|r| canonical_name(&r.name)).collect();
    let placeholders = canonicals.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql =
        format!("SELECT id, canonical_name FROM artist WHERE canonical_name IN ({placeholders})");
    let mut q = sqlx::query_as::<_, (i64, String)>(sqlx::AssertSqlSafe(sql));
    for c in &canonicals {
        q = q.bind(c);
    }
    let lookup: HashMap<String, i64> = q
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|(id, c)| (c, id))
        .collect();

    let mut out = Vec::with_capacity(canonicals.len());
    for c in canonicals {
        if let Some(&id) = lookup.get(&c) {
            if id != artist_id && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// How far apart two plays may be and still count as one listening
/// session for [`co_listened_artists`].
pub const SESSION_WINDOW_MS: i64 = 30 * 60 * 1000;

/// How far back [`co_listened_artists`] looks.
pub const CO_LISTEN_LOOKBACK_DAYS: i64 = 365;

/// The artists the user plays around `artist_id`, with how often: those
/// with a play within [`SESSION_WINDOW_MS`] of one of its plays over the
/// last year, most often first. The neighbours a cache cannot know — the
/// user's own pairings — and the ones available offline, for an artist
/// nobody has looked up. The count is what tells a pairing from a chance
/// meeting in a long shuffled session; the caller decides where that
/// line is.
pub async fn co_listened_artists(
    pool: &SqlitePool,
    artist_id: i64,
    limit: usize,
) -> CoreResult<Vec<(i64, i64)>> {
    let since = Utc::now().timestamp_millis() - CO_LISTEN_LOOKBACK_DAYS * 86_400_000;
    Ok(sqlx::query_as(
        "SELECT other.primary_artist, COUNT(DISTINCT b.id)
           FROM play_event a
           JOIN track seed  ON seed.id = a.track_id AND seed.primary_artist = ?
           JOIN play_event b
             ON b.played_at BETWEEN a.played_at - ? AND a.played_at + ?
           JOIN track other ON other.id = b.track_id
          WHERE a.played_at >= ?
            AND other.is_available = 1
            AND other.primary_artist IS NOT NULL
            AND other.primary_artist <> ?
          GROUP BY other.primary_artist
          ORDER BY COUNT(DISTINCT b.id) DESC, other.primary_artist
          LIMIT ?",
    )
    .bind(artist_id)
    .bind(SESSION_WINDOW_MS)
    .bind(SESSION_WINDOW_MS)
    .bind(since)
    .bind(artist_id)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?)
}
