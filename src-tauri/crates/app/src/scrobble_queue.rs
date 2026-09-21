use sqlx::SqlitePool;

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
