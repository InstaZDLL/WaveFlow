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
mod tests {
    use super::*;
    use crate::queue::{self, Direction, RepeatMode};

    /// The repo's own migrations, both databases, with `app` attached the
    /// way every profile pool has it and `foreign_keys` on.
    async fn pool(dir: &std::path::Path) -> SqlitePool {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let opts_for = |path: &std::path::Path| {
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
                .foreign_keys(true)
        };
        let app_path = dir.join("app.db");
        let app_pool = SqlitePool::connect_with(opts_for(&app_path)).await.unwrap();
        sqlx::migrate!("../../migrations/app")
            .run(&app_pool)
            .await
            .unwrap();
        app_pool.close().await;
        let attach = format!(
            "ATTACH DATABASE '{}' AS app",
            app_path.display().to_string().replace('\'', "''")
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(move |conn, _meta| {
                let attach = attach.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(attach))
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(opts_for(&dir.join("profile.db")))
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        pool
    }

    /// Artists 1 (the seed's), 2 (listed as similar) and 3 (unrelated):
    /// tracks 1-5 by artist 1, 6-10 by artist 2, 11-30 by artist 3.
    async fn library(pool: &SqlitePool) {
        sqlx::raw_sql(
            "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO artist (id, name, canonical_name)
                  VALUES (1, 'Seed', 'seed'), (2, 'Near', 'near'), (3, 'Far', 'far');
             INSERT INTO app.lastfm_similar (name_canonical, payload, source, fetched_at, expires_at)
                  VALUES ('seed', '[{\"name\":\"Near\"}]', 'lastfm', 0, 0);",
        )
        .execute(pool)
        .await
        .unwrap();
        for id in 1..=30_i64 {
            let artist = match id {
                1..=5 => 1,
                6..=10 => 2,
                _ => 3,
            };
            sqlx::query(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, primary_artist, duration_ms, added_at)
                 VALUES (?, 1, ?, ?, 1, 0, 't', ?, 1000, 0)",
            )
            .bind(id)
            .bind(format!("/l/{id}.flac"))
            .bind(format!("h{id}"))
            .bind(artist)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    async fn enable(pool: &SqlitePool, on: bool) {
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, ?, 'bool', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(SETTING_KEY)
        .bind(if on { "true" } else { "false" })
        .execute(pool)
        .await
        .unwrap();
    }

    async fn queued(pool: &SqlitePool) -> Vec<i64> {
        sqlx::query_scalar("SELECT track_id FROM queue_item ORDER BY position")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// A queue of `ids` with the cursor on its last entry.
    async fn at_end(pool: &SqlitePool, ids: &[i64]) {
        queue::fill_queue(pool, "album", None, ids, ids.len() - 1, false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn off_by_default_and_the_queue_still_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        at_end(&pool, &[1, 2]).await;
        assert!(!is_enabled(&pool).await);
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
        assert_eq!(queued(&pool).await, vec![1, 2]);
        assert!(queue::peek_step(&pool, Direction::Next, RepeatMode::Off)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn an_exhausted_queue_is_topped_up_and_the_next_step_lands_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        enable(&pool, true).await;
        at_end(&pool, &[1, 2]).await;
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_some());
        let ids = queued(&pool).await;
        assert_eq!(ids.len(), 2 + BATCH);
        let added = &ids[2..];
        let mut unique = added.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), BATCH, "a track added twice: {added:?}");
        assert!(!added.contains(&1) && !added.contains(&2), "{added:?}");
        let (index, track) = queue::peek_step(&pool, Direction::Next, RepeatMode::Off)
            .await
            .unwrap()
            .expect("the step past the old end lands on an added track");
        assert_eq!(index, 2);
        assert_eq!(track.id, added[0]);
    }

    #[tokio::test]
    async fn related_artists_come_first_and_the_seed_artist_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        // Seed = track 1 (artist 1). Artist 1 has 4 more, artist 2 five.
        at_end(&pool, &[1]).await;
        let picked = pick(&pool, 1, BATCH, 0).await.unwrap();
        let seed_artist = picked.iter().filter(|id| (2..=5).contains(*id)).count();
        let similar = picked.iter().filter(|id| (6..=10).contains(*id)).count();
        assert_eq!(seed_artist, SEED_ARTIST_CAP, "{picked:?}");
        assert_eq!(
            similar, 5,
            "every similar track before the rest: {picked:?}"
        );
        assert_eq!(picked.len(), BATCH);
        assert!(!picked.contains(&1), "{picked:?}");
    }

    #[tokio::test]
    async fn nothing_queued_or_just_played_is_picked_while_something_else_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        at_end(&pool, &[1, 6, 7]).await;
        // Tracks 8-10 were played a minute ago; 2 four hours ago.
        let now = 10 * 60 * 60 * 1000_i64;
        for (track, at) in [
            (8, now - 60_000),
            (9, now - 60_000),
            (10, now - 60_000),
            (2, now - 4 * 60 * 60 * 1000),
        ] {
            sqlx::query(
                "INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (?, ?, 1000)",
            )
            .bind(track)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }
        // A play whose track was deleted since: its row stays, with no
        // track. It must not empty the "not played lately" tiers.
        sqlx::query(
            "INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (NULL, ?, 1000)",
        )
        .bind(now - 60_000)
        .execute(&pool)
        .await
        .unwrap();
        let picked = pick(&pool, 7, 40, now).await.unwrap();
        for id in [1, 6, 7] {
            assert!(!picked.contains(&id), "{id} is queued: {picked:?}");
        }
        // 27 tracks are not queued; 3 of them were just played and come
        // last, once everything else is in.
        assert_eq!(picked.len(), 27, "{picked:?}");
        let tail: Vec<i64> = picked[24..].to_vec();
        for id in [8, 9, 10] {
            assert!(tail.contains(&id), "{id} should come last: {picked:?}");
        }
        assert!(picked[..24].contains(&2), "played hours ago is fair game");
    }

    #[tokio::test]
    async fn only_the_end_of_a_queue_with_repeat_off_is_topped_up() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        enable(&pool, true).await;
        // Repeat all / one already have a next entry.
        at_end(&pool, &[1, 2]).await;
        assert!(top_up(&pool, RepeatMode::All).await.unwrap().is_none());
        assert!(top_up(&pool, RepeatMode::One).await.unwrap().is_none());
        // A cursor before the end has a next entry too.
        queue::fill_queue(&pool, "album", None, &[1, 2, 3], 0, false)
            .await
            .unwrap();
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
        assert_eq!(queued(&pool).await, vec![1, 2, 3]);
        // An empty queue has nothing to continue from.
        queue::clear(&pool).await.unwrap();
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
        assert!(queued(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn switched_off_it_adds_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        enable(&pool, true).await;
        at_end(&pool, &[1]).await;
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_some());
        let len = queued(&pool).await.len() as i64;
        queue::commit_index(&pool, len - 1).await.unwrap();
        enable(&pool, false).await;
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
        assert_eq!(queued(&pool).await.len() as i64, len);
    }

    #[tokio::test]
    async fn a_batch_taken_back_leaves_the_queue_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        enable(&pool, true).await;
        at_end(&pool, &[1, 2]).await;
        let batch = top_up(&pool, RepeatMode::Off).await.unwrap().unwrap();
        assert!(batch.covers(2) && batch.covers(1 + BATCH as i64) && !batch.covers(1));
        batch.undo(&pool).await;
        assert_eq!(queued(&pool).await, vec![1, 2]);
        assert_eq!(queue::current_index(&pool).await, 1);
    }

    #[tokio::test]
    async fn the_end_of_a_session_reaching_a_pick_is_told_so() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        enable(&pool, true).await;
        at_end(&pool, &[1, 2]).await;
        top_up(&pool, RepeatMode::Off).await.unwrap().unwrap();
        assert!(reaches_picks(&pool, 2, "album").await);
        assert!(!reaches_picks(&pool, 1, "album").await, "an album row");
        assert!(
            !reaches_picks(&pool, 2, "radio").await,
            "a radio carries on"
        );
    }

    #[tokio::test]
    async fn the_seed_artist_goes_past_its_cap_only_when_nothing_else_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        // Everything but artist 1 is queued: its four other tracks are all
        // that is left, and all of them are taken.
        let others: Vec<i64> = (1..=1).chain(6..=30).collect();
        at_end(&pool, &others).await;
        let mut picked = pick(&pool, 1, BATCH, 0).await.unwrap();
        picked.sort_unstable();
        assert_eq!(picked, vec![2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn more_of_the_seed_artist_comes_before_a_track_just_heard() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        // Artist 2 and half of artist 3 are queued; the other half of
        // artist 3 (21-30) was played a minute ago. Only the seed artist
        // has tracks left that were not heard lately, one past its cap.
        let queued_ids: Vec<i64> = (1..=1).chain(6..=20).collect();
        at_end(&pool, &queued_ids).await;
        let now = 10 * 60 * 60 * 1000_i64;
        for track in 21..=30_i64 {
            sqlx::query(
                "INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (?, ?, 1000)",
            )
            .bind(track)
            .bind(now - 60_000)
            .execute(&pool)
            .await
            .unwrap();
        }
        let picked = pick(&pool, 1, BATCH, now).await.unwrap();
        for id in 2..=5 {
            assert!(
                picked.contains(&id),
                "{id} was not heard lately: {picked:?}"
            );
        }
        assert_eq!(picked.len(), BATCH);
    }

    #[tokio::test]
    async fn a_library_with_nothing_left_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        library(&pool).await;
        enable(&pool, true).await;
        let all: Vec<i64> = (1..=30).collect();
        at_end(&pool, &all).await;
        assert!(top_up(&pool, RepeatMode::Off).await.unwrap().is_none());
        assert_eq!(queued(&pool).await.len(), 30);
    }
}
