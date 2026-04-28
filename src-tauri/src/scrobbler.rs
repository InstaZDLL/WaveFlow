//! Last.fm scrobble worker.
//!
//! Drains the per-profile `scrobble_queue` table by signing and POSTing
//! `track.scrobble` against Last.fm. Successful scrobbles are deleted;
//! transient failures (network, 5xx, rate-limit) bump `retry_count` and
//! schedule the next attempt with exponential backoff. Permanent
//! failures (invalid signature, unknown track…) are deleted so the
//! queue never gets clogged by a poison item.
//!
//! Scrobbles are enqueued from [`crate::audio::analytics`] when a
//! `play_event` row is inserted that meets Last.fm's eligibility
//! rules (track ≥ 30 s, listened ≥ 50 % of duration or ≥ 4 minutes).
//! The user's API key + secret + session key live in
//! [`crate::commands::integration`] state.
//!
//! Lifecycle: a single tokio task is spawned at startup. Profile
//! switches keep it running — the task always reads the *active*
//! profile's pool via `AppState::require_profile_pool` on each tick,
//! so the new profile's queue takes over automatically without any
//! restart.

use std::time::Duration;

use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};
use tokio::time;

use crate::{
    commands::integration::read_lastfm_credentials,
    lastfm::{is_permanent_error, LastfmClient, LastfmError},
    state::AppState,
};

/// How often the worker wakes up to look at the queue. 30 s is a
/// gentle balance between "scrobbles appear quickly on the user's
/// profile" and "we don't pummel Last.fm with empty polls".
const TICK: Duration = Duration::from_secs(30);

/// Cap on the per-tick batch so a backlog of thousands doesn't burn
/// the rate limit (5 req/s per Last.fm ToS) or block the task for
/// minutes. Anything beyond this rolls over to the next tick.
const MAX_PER_TICK: i64 = 50;

/// Spawn the scrobble worker on the tauri runtime. Cheap to call —
/// the loop sleeps for 30 s between work and short-circuits when no
/// credentials are configured.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Stagger the first tick a touch so it doesn't compete with
        // boot-time DB / scanner work.
        time::sleep(Duration::from_secs(5)).await;
        let mut interval = time::interval(TICK);
        // The first tick fires immediately (default). We want the
        // pacing to be steady regardless of how long a tick takes,
        // so use Burst behaviour.
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(err) = run_once(&app).await {
                tracing::warn!(%err, "scrobble worker tick failed");
            }
        }
    });
}

async fn run_once(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let creds = read_lastfm_credentials(&state)
        .await
        .map_err(|e| format!("read credentials: {e}"))?;
    let Some((api_key, api_secret, session_key, _username)) = creds else {
        // Not configured / not logged in: nothing to do.
        return Ok(());
    };
    let pool = match state.require_profile_pool().await {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };

    let pending = fetch_due(&pool).await.map_err(|e| format!("fetch: {e}"))?;
    if pending.is_empty() {
        return Ok(());
    }

    let client = LastfmClient::new();
    for item in pending {
        let outcome = client
            .scrobble(
                &api_key,
                &api_secret,
                &session_key,
                &item.artist_name,
                &item.title,
                item.album_title.as_deref(),
                item.track_number,
                item.duration_s,
                item.played_at_unix_s,
            )
            .await;
        match outcome {
            Ok(()) => {
                tracing::info!(item_id = item.id, track_id = item.track_id, "scrobbled");
                if let Err(e) = delete_item(&pool, item.id).await {
                    tracing::warn!(?e, item_id = item.id, "delete after scrobble failed");
                }
            }
            Err(LastfmError::Api { code, ref message }) if is_permanent_error(code) => {
                tracing::warn!(
                    item_id = item.id,
                    code,
                    message = %message,
                    "permanent scrobble error, dropping"
                );
                let _ = delete_item(&pool, item.id).await;
            }
            Err(err) => {
                tracing::warn!(item_id = item.id, %err, "scrobble retry");
                if let Err(e) =
                    bump_retry(&pool, item.id, item.retry_count + 1, err.to_string()).await
                {
                    tracing::warn!(?e, item_id = item.id, "bump retry failed");
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PendingScrobble {
    id: i64,
    track_id: i64,
    title: String,
    artist_name: String,
    album_title: Option<String>,
    track_number: Option<i64>,
    duration_s: Option<i64>,
    played_at_unix_s: i64,
    retry_count: i64,
}

/// Pull the next batch of scrobbles whose `next_retry_at` is past
/// (or null = first attempt) and that haven't burned all their
/// retries. Joined with the track row so the worker doesn't need a
/// second query per item to assemble the API payload.
async fn fetch_due(pool: &SqlitePool) -> Result<Vec<PendingScrobble>, sqlx::Error> {
    let now = chrono::Utc::now().timestamp_millis();
    let rows: Vec<(
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        i64,
        i64,
        i64,
    )> = sqlx::query_as(
        r#"
        SELECT q.id, q.track_id,
               t.title,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                   SELECT ar.name FROM track_artist ta
                   JOIN artist ar ON ar.id = ta.artist_id
                   WHERE ta.track_id = t.id
                   ORDER BY ta.position
               )) AS artist_name,
               al.title AS album_title,
               t.track_number,
               t.duration_ms,
               q.played_at,
               q.retry_count
          FROM scrobble_queue q
          JOIN track t        ON t.id = q.track_id
          LEFT JOIN album al  ON al.id = t.album_id
         WHERE q.provider = 'lastfm'
           AND q.retry_count < 10
           AND (q.next_retry_at IS NULL OR q.next_retry_at <= ?)
         ORDER BY q.id
         LIMIT ?
        "#,
    )
    .bind(now)
    .bind(MAX_PER_TICK)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let (id, track_id, title, artist, album, track_no, duration_ms, played_at_ms, retry) =
                row;
            // Last.fm requires an artist; if the joined GROUP_CONCAT
            // yielded nothing, drop the scrobble — re-enqueueing it
            // wouldn't help.
            let artist_name = artist?.trim().to_string();
            if artist_name.is_empty() {
                return None;
            }
            Some(PendingScrobble {
                id,
                track_id,
                title,
                artist_name,
                album_title: album.filter(|s| !s.is_empty()),
                track_number: track_no.filter(|n| *n > 0),
                duration_s: if duration_ms > 0 { Some(duration_ms / 1000) } else { None },
                played_at_unix_s: played_at_ms / 1000,
                retry_count: retry,
            })
        })
        .collect())
}

async fn delete_item(pool: &SqlitePool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM scrobble_queue WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn bump_retry(
    pool: &SqlitePool,
    id: i64,
    new_count: i64,
    last_error: String,
) -> Result<(), sqlx::Error> {
    // Exponential backoff: 1 min, 2, 4, 8, 16, 32, 64, 128, 256, 512.
    // Capped at retry_count = 10 by the WHERE filter, so the queue
    // never holds more than ~17 hours of pending retries per row.
    let delay_min = 1u64 << (new_count - 1).min(9) as u64;
    let next_retry_at = chrono::Utc::now().timestamp_millis() + (delay_min as i64) * 60_000;
    sqlx::query(
        "UPDATE scrobble_queue
            SET retry_count = ?, next_retry_at = ?, last_error = ?
          WHERE id = ?",
    )
    .bind(new_count)
    .bind(next_retry_at)
    .bind(last_error)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert a scrobble candidate into `scrobble_queue`. Called from
/// [`crate::audio::analytics`] right after a `play_event` row is
/// written. Eligibility is checked before this call — by the time we
/// land here the listen is known to qualify.
pub async fn enqueue(
    pool: &SqlitePool,
    track_id: i64,
    played_at_ms: i64,
    listened_ms: i64,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO scrobble_queue
            (provider, track_id, played_at, listened_ms,
             retry_count, next_retry_at, last_error, created_at)
         VALUES ('lastfm', ?, ?, ?, 0, NULL, NULL, ?)",
    )
    .bind(track_id)
    .bind(played_at_ms)
    .bind(listened_ms)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Last.fm eligibility check applied at enqueue time: track must be
/// at least 30 s long, and the user must have listened to at least
/// half its duration or 4 minutes — whichever comes first. Tracks
/// without a known duration are skipped (we have no basis to judge).
pub fn is_eligible(duration_ms: i64, listened_ms: i64) -> bool {
    if duration_ms < 30_000 {
        return false;
    }
    listened_ms >= duration_ms / 2 || listened_ms >= 240_000
}
