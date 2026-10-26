//! Autoplay: keep playing related tracks when the queue runs out.
//!
//! With `profile_setting['playback.autoplay']` on (default off) and repeat
//! off, the step that would run past the last entry first appends a batch
//! of tracks from the library to the queue, then steps onto the first of
//! them like any other next. The tracks are real queue rows, shown in the
//! queue panel and removable there; nothing plays that is not in it.
//!
//! **One entry point, called by every producer that advances.** The Next
//! button (`player_next`), the non-frontend surfaces through
//! [`crate::player_actions::step`] (tray, media keys, taskbar buttons,
//! MPD), a track ending on its own and the gapless / crossfade prefetch
//! all call [`top_up`] under the engine's publish lock, before they peek
//! at the next entry. So every surface lands on the same track, and the
//! queue is never extended under a producer that is replacing it.
//!
//! **Selection reuses what the radios already know**, it does not rank
//! anything of its own. The seed is the last track of the queue. First its
//! primary artist and the artists the similar-artists cache lists for it
//! (`waveflow_core::similar`, read offline: a top-up never waits on the
//! network), at most [`SEED_ARTIST_CAP`] from the seed artist itself so the
//! batch moves on; then, if that is not enough, any other track of the
//! library, and more of the seed artist's only once nothing else is left.
//! Never a track already in the queue, and not one played in the last
//! [`RECENT_WINDOW_MS`] unless nothing else is left.
//!
//! Turning the option off stops the next top-up; tracks already added
//! stay in the queue, where the user can see and remove them.

use sqlx::SqlitePool;

use crate::{
    error::AppResult,
    queue::{self, RepeatMode},
};

/// The `profile_setting` key. A bool, default off.
pub const SETTING_KEY: &str = "playback.autoplay";

/// Tracks added per top-up. Enough that a top-up is rare, few enough that
/// the queue panel does not fill with a radio the user did not ask to see.
pub const BATCH: usize = 10;

/// At most this many of a batch come from the seed's own artist.
const SEED_ARTIST_CAP: usize = 3;

/// Similar artists read from the cache.
const SIMILAR_ARTIST_CAP: usize = 8;

/// A track played this recently is left for later, unless nothing else is
/// left in the library.
const RECENT_WINDOW_MS: i64 = 3 * 60 * 60 * 1000;

/// Whether the option is on for this profile. Missing or unreadable is off.
pub async fn is_enabled(pool: &SqlitePool) -> bool {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(SETTING_KEY)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    matches!(value.as_deref(), Some("true" | "1"))
}

/// A batch [`top_up`] added: the queue positions `[start, start + len)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopUp {
    start: i64,
    len: i64,
}

impl TopUp {
    /// Whether the queue position `index` is one of this batch's.
    pub fn covers(&self, index: i64) -> bool {
        (self.start..self.start + self.len).contains(&index)
    }

    /// Take the batch back out of the queue: the step it was added for
    /// lost its claim to a newer selection, and a dispatch that gives up
    /// leaves nothing behind. Logged, not returned: the step is already
    /// giving up.
    pub async fn undo(self, pool: &SqlitePool) {
        if let Err(err) = queue::remove_range(pool, self.start, self.start + self.len).await {
            tracing::warn!(%err, "autoplay: an abandoned batch could not be removed");
        }
    }
}

/// [`TopUp::undo`] for a step that gives up, whether or not it added a
/// batch: every early return past [`top_up`] goes through here, so none
/// of them leaves the batch behind.
pub async fn undo_if_added(batch: Option<TopUp>, pool: &SqlitePool) {
    if let Some(batch) = batch {
        batch.undo(pool).await;
    }
}

/// Append a batch to a queue whose cursor is on its last entry, when
/// autoplay is on and repeat is off. Returns what was added, if anything.
///
/// Callers hold the engine's publish lock and peek at the next entry
/// afterwards, so the step that follows lands on the first added track.
/// A caller that then fails to claim its dispatch hands the batch to
/// [`TopUp::undo`], and announces the queue change only once it has
/// claimed. An empty queue is left alone: there is no seed, and nothing
/// was playing to continue from.
pub async fn top_up(pool: &SqlitePool, repeat: RepeatMode) -> AppResult<Option<TopUp>> {
    if repeat != RepeatMode::Off || !is_enabled(pool).await {
        return Ok(None);
    }
    let length = queue::queue_length(pool).await?;
    if length == 0 || queue::current_index(pool).await + 1 < length {
        return Ok(None);
    }
    let Some(seed) = last_track_id(pool).await? else {
        return Ok(None);
    };
    let picked = pick(pool, seed, BATCH, now_ms()).await?;
    if picked.is_empty() {
        tracing::info!(seed, "autoplay: nothing left to add");
        return Ok(None);
    }
    // `radio`, the source the seed radio uses: these are generated
    // picks, and the queue's CHECK has no other name for them. Being a
    // source other than `manual` also keeps "Add to queue" ahead of them.
    queue::append(pool, &picked, "radio", None).await?;
    tracing::info!(seed, added = picked.len(), "autoplay: queue topped up");
    Ok(Some(TopUp {
        start: length,
        len: picked.len() as i64,
    }))
}

/// [`top_up`] for the producers, which must step whatever happens: a
/// failure is logged and reads as nothing added, so the step goes on and
/// finds the end of the queue as it did before.
pub async fn top_up_or_log(pool: &SqlitePool, repeat: RepeatMode, surface: &str) -> Option<TopUp> {
    top_up(pool, repeat).await.unwrap_or_else(|err| {
        tracing::warn!(%err, surface, "autoplay: top-up failed");
        None
    })
}

/// Whether the step onto queue position `index` leaves a session for the
/// picks autoplay added after it: the row is a `radio` one and the track
/// that just ended was not playing as `radio`.
///
/// The natural end of a track used to hand its own source to the next
/// load, which across that boundary played a pick as part of the album
/// before it — album gain on a lone track, and the play credited to the
/// album. The Next button and the gapless / crossfade prefetch load the
/// pick as `manual`; the end of a track does the same when this, or the
/// batch it has just added, says it is reaching one.
pub async fn reaches_picks(pool: &SqlitePool, index: i64, finished_source: &str) -> bool {
    if finished_source == "radio" {
        return false;
    }
    let source: Option<String> =
        sqlx::query_scalar("SELECT source_type FROM queue_item WHERE position = ?")
            .bind(index)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    source.as_deref() == Some("radio")
}

async fn last_track_id(pool: &SqlitePool) -> AppResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT track_id FROM queue_item ORDER BY position DESC LIMIT 1")
            .fetch_optional(pool)
            .await?,
    )
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Up to `n` available tracks to follow `seed`, none of them already in the
/// queue. See the module comment for the order the tiers are tried in.
async fn pick(pool: &SqlitePool, seed: i64, n: usize, now: i64) -> AppResult<Vec<i64>> {
    let recent_since = now - RECENT_WINDOW_MS;
    let seed_artist: Option<i64> =
        sqlx::query_scalar("SELECT primary_artist FROM track WHERE id = ?")
            .bind(seed)
            .fetch_optional(pool)
            .await?
            .flatten();

    let mut out: Vec<i64> = Vec::with_capacity(n);
    if let Some(artist) = seed_artist {
        let own = by_artists(pool, &[artist], SEED_ARTIST_CAP.min(n), recent_since, &out).await?;
        out.extend(own);
        // The cache needs `app` attached, as every profile pool has it; a
        // read that fails is no similar artist, not a failed top-up.
        let similar = waveflow_core::similar::cached_similar_library_ids(pool, artist)
            .await
            .unwrap_or_else(|err| {
                tracing::debug!(%err, "autoplay: similar artists unreadable");
                Vec::new()
            });
        let similar: Vec<i64> = similar.into_iter().take(SIMILAR_ARTIST_CAP).collect();
        if out.len() < n {
            let more = by_artists(pool, &similar, n - out.len(), recent_since, &out).await?;
            out.extend(more);
        }
    }
    // The rest of the library keeps the seed artist's cap: only when
    // nothing else is left does a batch take more of that artist. The
    // three-hour rule gives way last: more of the seed artist comes
    // before a track the user has just heard.
    for (recent, skip_artist) in [
        (Some(recent_since), seed_artist),
        (Some(recent_since), None),
        (None, seed_artist),
        (None, None),
    ] {
        if out.len() >= n {
            break;
        }
        let more = anywhere(pool, n - out.len(), recent, skip_artist, &out).await?;
        out.extend(more);
    }
    Ok(out)
}

/// The SQL that every tier shares: available, not queued, not already
/// picked, and — when `recent_since` is given — not played since then.
fn exclusions(picked: &[i64], recent: bool) -> String {
    let mut sql = String::from(
        " AND t.is_available = 1
          AND t.id NOT IN (SELECT track_id FROM queue_item)",
    );
    if recent {
        // `NOT EXISTS`, not `NOT IN`: a play whose track was deleted keeps
        // its row with a NULL `track_id`, and one NULL in a `NOT IN` list
        // makes the test NULL for every track.
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM play_event pe
                               WHERE pe.track_id = t.id AND pe.played_at >= ?)",
        );
    }
    if !picked.is_empty() {
        let marks = vec!["?"; picked.len()].join(",");
        sql.push_str(&format!(" AND t.id NOT IN ({marks})"));
    }
    sql
}

async fn by_artists(
    pool: &SqlitePool,
    artists: &[i64],
    limit: usize,
    recent_since: i64,
    picked: &[i64],
) -> AppResult<Vec<i64>> {
    if artists.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; artists.len()].join(",");
    let sql = format!(
        "SELECT t.id FROM track t WHERE t.primary_artist IN ({marks}){} ORDER BY RANDOM() LIMIT ?",
        exclusions(picked, true)
    );
    let mut q = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    for a in artists {
        q = q.bind(*a);
    }
    q = q.bind(recent_since);
    for p in picked {
        q = q.bind(*p);
    }
    Ok(q.bind(limit as i64).fetch_all(pool).await?)
}

/// Any track of the library, except `skip_artist`'s when one is given.
async fn anywhere(
    pool: &SqlitePool,
    limit: usize,
    recent_since: Option<i64>,
    skip_artist: Option<i64>,
    picked: &[i64],
) -> AppResult<Vec<i64>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let skip = if skip_artist.is_some() {
        " AND (t.primary_artist IS NULL OR t.primary_artist <> ?)"
    } else {
        ""
    };
    let sql = format!(
        "SELECT t.id FROM track t WHERE 1 = 1{}{skip} ORDER BY RANDOM() LIMIT ?",
        exclusions(picked, recent_since.is_some())
    );
    let mut q = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql));
    if let Some(since) = recent_since {
        q = q.bind(since);
    }
    for p in picked {
        q = q.bind(*p);
    }
    if let Some(artist) = skip_artist {
        q = q.bind(artist);
    }
    Ok(q.bind(limit as i64).fetch_all(pool).await?)
}

#[cfg(test)]
mod tests;
