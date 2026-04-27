//! Persistent playback queue.
//!
//! The queue lives in the per-profile `data.db` via the `queue_item`
//! table (populated by [`fill_queue`]) and the `queue.current_index`
//! row of `profile_setting` (pointer to the active slot).
//!
//! Playing a track from the library view fills the whole queue with
//! the current view, sets `current_index` to the clicked row, and the
//! audio engine's auto-advance task walks forward through the queue
//! as each track ends. Shuffle / repeat behaviour is applied here,
//! not in the decoder thread.
//!
//! None of these functions touch the audio engine — they're pure DB
//! operations that return [`QueueTrack`]s for the caller to feed into
//! `AudioCmd::LoadAndPlay`.
//!
//! `dead_code` is tolerated module-wide because shuffle / unshuffle /
//! restore_state / persist_resume_point are consumed by later
//! checkpoints (12 = shuffle+repeat, 13 = startup restore).

#![allow(dead_code)]

use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};

use crate::{
    commands::player::PlayerStateSnapshot,
    error::{AppError, AppResult},
};

/// Minimum track shape needed to hand off to the decoder thread. Kept
/// narrower than [`crate::commands::track::Track`] because playback
/// doesn't need the full metadata block; anything the UI wants on top
/// is fetched via `list_tracks`.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct QueueTrack {
    pub id: i64,
    pub file_path: String,
    pub duration_ms: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_title: Option<String>,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
}

impl QueueTrack {
    /// Return the absolute filesystem path the decoder should open.
    pub fn as_path(&self) -> PathBuf {
        PathBuf::from(&self.file_path)
    }
}

/// Direction arguments for [`advance`].
#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Next,
    Previous,
}

/// Repeat mode. Mirrors the `player.repeat_mode` profile_setting
/// string ('off' / 'all' / 'one').
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn from_str(s: &str) -> Self {
        match s {
            "all" => Self::All,
            "one" => Self::One,
            _ => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

// ---------------------------------------------------------------------
// Helpers: typed wrappers around profile_setting string values
// ---------------------------------------------------------------------

async fn read_setting_string(pool: &SqlitePool, key: &str) -> AppResult<Option<String>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT value FROM profile_setting WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

async fn read_setting_i64(pool: &SqlitePool, key: &str) -> AppResult<Option<i64>> {
    match read_setting_string(pool, key).await? {
        Some(s) => Ok(s.parse::<i64>().ok()),
        None => Ok(None),
    }
}

async fn write_setting_i64(pool: &SqlitePool, key: &str, value: i64) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = ?",
    )
    .bind(value.to_string())
    .bind(now)
    .bind(key)
    .execute(pool)
    .await?;
    Ok(())
}

async fn write_setting_string(pool: &SqlitePool, key: &str, value: &str) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------
// Read helpers used by player commands + analytics
// ---------------------------------------------------------------------

pub async fn read_repeat_mode(pool: &SqlitePool) -> RepeatMode {
    match read_setting_string(pool, "player.repeat_mode").await {
        Ok(Some(s)) => RepeatMode::from_str(&s),
        _ => RepeatMode::Off,
    }
}

pub async fn read_shuffle(pool: &SqlitePool) -> bool {
    matches!(
        read_setting_string(pool, "player.shuffle").await,
        Ok(Some(ref s)) if s == "true"
    )
}

pub async fn write_repeat_mode(pool: &SqlitePool, mode: RepeatMode) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'player.repeat_mode'",
    )
    .bind(mode.as_str())
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn write_shuffle(pool: &SqlitePool, shuffle: bool) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'player.shuffle'",
    )
    .bind(if shuffle { "true" } else { "false" })
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the persisted player volume (`player.volume` key, stored as
/// an integer 0-100 in `profile_setting`) and convert to the
/// `f32 in [0.0, 1.0]` range used by the audio engine. Returns
/// `None` if the row is missing or not parseable.
pub async fn read_player_volume(pool: &SqlitePool) -> Option<f32> {
    let raw = read_setting_string(pool, "player.volume").await.ok()??;
    raw.parse::<i64>()
        .ok()
        .map(|v| (v.clamp(0, 100) as f32) / 100.0)
}

// ---------------------------------------------------------------------
// Core queue operations
// ---------------------------------------------------------------------

/// Clear the queue and insert new rows, one per track, with positions
/// 0..n. Also sets `queue.current_index` to `start_index`. Runs in a
/// single transaction so the UI never sees a partial state.
pub async fn fill_queue(
    pool: &SqlitePool,
    source_type: &str,
    source_id: Option<i64>,
    track_ids: &[i64],
    start_index: usize,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Err(AppError::Other("cannot fill queue with empty track list".into()));
    }
    if start_index >= track_ids.len() {
        return Err(AppError::Other(format!(
            "start_index {start_index} out of range (queue length {})",
            track_ids.len()
        )));
    }

    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM queue_item").execute(&mut *tx).await?;

    let now = Utc::now().timestamp_millis();
    for (pos, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(track_id)
        .bind(pos as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind((start_index as i64).to_string())
    .bind(now)
    .execute(&mut *tx)
    .await?;

    // New queue invalidates any previous shuffle snapshot.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Append `track_ids` to the end of the queue without disturbing the
/// current cursor. Used by the "Add to queue" context menu action so
/// the user can stack tracks without losing what's currently playing.
pub async fn append(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    let max_pos: Option<i64> =
        sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
            .fetch_one(&mut *tx)
            .await?;
    let mut next = max_pos.map(|p| p + 1).unwrap_or(0);
    let now = Utc::now().timestamp_millis();
    for id in track_ids {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(next)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        next += 1;
    }
    // Order changed → shuffle snapshot is no longer reusable.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Insert `track_ids` immediately after the current cursor position.
/// Existing items past the cursor are pushed down to keep the queue
/// dense. Returns nothing — the cursor itself doesn't move so the
/// currently-playing track keeps playing.
pub async fn insert_after_current(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Ok(());
    }
    let len = queue_length(pool).await?;
    if len == 0 {
        // No queue yet — fall back to filling it and starting at 0.
        return fill_queue(pool, source_type, source_id, track_ids, 0).await;
    }
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, len - 1);
    let insert_at = current + 1;
    let count = track_ids.len() as i64;
    let mut tx = pool.begin().await?;

    // Push existing items down to make room. SQLite checks the
    // UNIQUE(position) constraint per row, so a direct
    // `position = position + N` would collide mid-update. Bump the
    // affected rows into a high range first, then bring them back
    // down past the inserted block. The 10_000_000 offset is well
    // above any realistic queue length.
    const OFFSET: i64 = 10_000_000;
    sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
        .bind(OFFSET)
        .bind(insert_at)
        .execute(&mut *tx)
        .await?;

    let now = Utc::now().timestamp_millis();
    for (offset, id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(insert_at + offset as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    // Bring the bumped rows back down past the freshly-inserted block.
    sqlx::query(
        "UPDATE queue_item SET position = position - ? + ? WHERE position >= ?",
    )
    .bind(OFFSET)
    .bind(count)
    .bind(insert_at + OFFSET)
    .execute(&mut *tx)
    .await?;

    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Move the queue item at `from` to the slot at `to`, shifting the
/// items between them in the opposite direction so positions stay
/// dense and unique. Also adjusts `queue.current_index` so the
/// playing track keeps playing — only the user's drag of the very
/// item that's playing should change which position is "current".
///
/// `from` / `to` are clamped to `[0, len-1]`; out-of-range drops
/// snap to the nearest end. SQLite's UNIQUE(position) is honored by
/// parking the moved row at a high offset before the shift, then
/// dropping it back to the target slot.
pub async fn reorder(pool: &SqlitePool, from: i64, to: i64) -> AppResult<()> {
    let len = queue_length(pool).await?;
    if len == 0 {
        return Ok(());
    }
    let from = from.clamp(0, len - 1);
    let to = to.clamp(0, len - 1);
    if from == to {
        return Ok(());
    }

    const PARK: i64 = 10_000_000;
    let mut tx = pool.begin().await?;

    // 1. Park the moved row out of the way.
    sqlx::query("UPDATE queue_item SET position = ? WHERE position = ?")
        .bind(PARK)
        .bind(from)
        .execute(&mut *tx)
        .await?;

    // 2. Shift the affected range, again via PARK detour to keep the
    //    UNIQUE constraint from firing mid-update on a contiguous
    //    range — SQLite checks per row, so a direct
    //    `position = position ± 1` would collide on the first step.
    if to > from {
        // Items in (from, to] shift down by 1.
        sqlx::query(
            "UPDATE queue_item SET position = position + ?
              WHERE position > ? AND position <= ?",
        )
        .bind(PARK)
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE queue_item SET position = position - ? - 1
              WHERE position > ? AND position <= ?",
        )
        .bind(PARK)
        .bind(from + PARK)
        .bind(to + PARK)
        .execute(&mut *tx)
        .await?;
    } else {
        // Items in [to, from) shift up by 1.
        sqlx::query(
            "UPDATE queue_item SET position = position + ?
              WHERE position >= ? AND position < ?",
        )
        .bind(PARK)
        .bind(to)
        .bind(from)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE queue_item SET position = position - ? + 1
              WHERE position >= ? AND position < ?",
        )
        .bind(PARK)
        .bind(to + PARK)
        .bind(from + PARK)
        .execute(&mut *tx)
        .await?;
    }

    // 3. Drop the parked row at its target.
    sqlx::query("UPDATE queue_item SET position = ? WHERE position = ?")
        .bind(to)
        .bind(PARK)
        .execute(&mut *tx)
        .await?;

    // 4. Adjust the cursor so the currently-playing track keeps
    //    pointing at itself even when we shifted rows around it.
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value FROM profile_setting WHERE key = 'queue.current_index'",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let current = raw.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let new_current = if current == from {
        to
    } else if from < to && current > from && current <= to {
        current - 1
    } else if to < from && current >= to && current < from {
        current + 1
    } else {
        current
    };
    if new_current != current {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE profile_setting SET value = ?, updated_at = ?
              WHERE key = 'queue.current_index'",
        )
        .bind(new_current.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    // Reordering invalidates the pre-shuffle snapshot — the original
    // order can't be reconstructed from a manually-tweaked queue.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Count of rows in `queue_item`. Used by [`advance`] to bound the
/// cursor when the queue length shrinks (e.g. a track is deleted).
pub async fn queue_length(pool: &SqlitePool) -> AppResult<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

/// Fetch the full queue as an ordered `Vec<QueueTrack>`. Joined
/// with track / album / artist / artwork so the frontend doesn't
/// have to issue N extra queries to render the panel.
pub async fn list_queue(pool: &SqlitePool) -> AppResult<Vec<QueueTrack>> {
    let rows = sqlx::query_as::<_, QueueTrack>(
        r#"
        SELECT t.id,
               t.file_path,
               t.duration_ms,
               t.title,
               t.primary_artist AS artist_id,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               (SELECT GROUP_CONCAT(id, ',') FROM (
                  SELECT ta2.artist_id AS id FROM track_artist ta2
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_ids,
               al.title AS album_title,
               aw.hash  AS artwork_hash,
               aw.format AS artwork_format
          FROM queue_item q
          JOIN track t       ON t.id = q.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         ORDER BY q.position
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Fetch the track at a specific position in the queue.
async fn track_at_position(pool: &SqlitePool, position: i64) -> AppResult<Option<QueueTrack>> {
    let row = sqlx::query_as::<_, QueueTrack>(
        r#"
        SELECT t.id,
               t.file_path,
               t.duration_ms,
               t.title,
               t.primary_artist AS artist_id,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                  SELECT ar2.name FROM track_artist ta2
                  JOIN artist ar2 ON ar2.id = ta2.artist_id
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_name,
               (SELECT GROUP_CONCAT(id, ',') FROM (
                  SELECT ta2.artist_id AS id FROM track_artist ta2
                  WHERE ta2.track_id = t.id
                  ORDER BY ta2.position
               )) AS artist_ids,
               al.title AS album_title,
               aw.hash  AS artwork_hash,
               aw.format AS artwork_format
          FROM queue_item q
          JOIN track t       ON t.id = q.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE q.position = ?
         LIMIT 1
        "#,
    )
    .bind(position)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Return the track currently pointed at by `queue.current_index`.
pub async fn current_track(pool: &SqlitePool) -> AppResult<Option<QueueTrack>> {
    let Some(idx) = read_setting_i64(pool, "queue.current_index").await? else {
        return Ok(None);
    };
    track_at_position(pool, idx).await
}

/// Move the cursor to an arbitrary position in the existing queue
/// and return the track there. Used when the user double-clicks a
/// row in the QueuePanel to jump.
pub async fn jump_to(pool: &SqlitePool, position: i64) -> AppResult<Option<QueueTrack>> {
    let length = queue_length(pool).await?;
    if length == 0 {
        return Ok(None);
    }
    let clamped = position.clamp(0, length - 1);
    write_setting_i64(pool, "queue.current_index", clamped).await?;
    track_at_position(pool, clamped).await
}

/// Apply a next / previous step to the queue cursor respecting the
/// repeat mode. Returns the newly-current track, or `None` if the
/// queue is empty / the step runs off the end with `RepeatMode::Off`.
///
/// The cursor is clamped to `[0, queue_length - 1]` before writing.
pub async fn advance(
    pool: &SqlitePool,
    direction: Direction,
    repeat: RepeatMode,
) -> AppResult<Option<QueueTrack>> {
    let length = queue_length(pool).await?;
    if length == 0 {
        return Ok(None);
    }
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0);

    let new_index = match (direction, repeat) {
        // Repeat-one re-plays the same slot regardless of direction.
        (_, RepeatMode::One) => current,
        (Direction::Next, RepeatMode::Off) => {
            if current + 1 >= length {
                return Ok(None);
            }
            current + 1
        }
        (Direction::Next, RepeatMode::All) => (current + 1) % length,
        (Direction::Previous, RepeatMode::Off) => (current - 1).max(0),
        (Direction::Previous, RepeatMode::All) => {
            if current == 0 {
                length - 1
            } else {
                current - 1
            }
        }
    };

    write_setting_i64(pool, "queue.current_index", new_index).await?;
    track_at_position(pool, new_index).await
}

/// Non-mutating sibling of [`advance`]: returns what the next track
/// *would* be without moving the cursor. Used by the crossfade
/// prefetcher so it can hand the decoder a candidate without
/// committing to a queue advance — the cursor is bumped only when
/// the crossfade actually starts.
pub async fn peek_next(
    pool: &SqlitePool,
    repeat: RepeatMode,
) -> AppResult<Option<QueueTrack>> {
    let length = queue_length(pool).await?;
    if length == 0 {
        return Ok(None);
    }
    let current = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0);

    let next_index = match repeat {
        RepeatMode::One => current,
        RepeatMode::Off => {
            if current + 1 >= length {
                return Ok(None);
            }
            current + 1
        }
        RepeatMode::All => (current + 1) % length,
    };
    track_at_position(pool, next_index).await
}

/// Startup restore: return the track + position the UI should show at
/// mount, without starting playback. Priority:
///
/// 1. `player.last_track_id` + `player.last_position_ms` if the track
///    still exists and is available,
/// 2. otherwise the current queue track at offset 0 ms,
/// 3. otherwise `None`.
pub async fn restore_state(pool: &SqlitePool) -> AppResult<Option<(QueueTrack, u64)>> {
    if let Some(last_id) = read_setting_i64(pool, "player.last_track_id").await? {
        if last_id > 0 {
            let row = sqlx::query_as::<_, QueueTrack>(
                r#"
                SELECT t.id, t.file_path, t.duration_ms, t.title,
                       ar.name AS artist_name,
                       al.title AS album_title,
                       aw.hash AS artwork_hash,
                       aw.format AS artwork_format
                  FROM track t
                  LEFT JOIN album al ON al.id = t.album_id
                  LEFT JOIN artist ar ON ar.id = t.primary_artist
                  LEFT JOIN artwork aw ON aw.id = al.artwork_id
                 WHERE t.id = ? AND t.is_available = 1
                "#,
            )
            .bind(last_id)
            .fetch_optional(pool)
            .await?;
            if let Some(track) = row {
                let pos = read_setting_i64(pool, "player.last_position_ms")
                    .await?
                    .unwrap_or(0)
                    .max(0) as u64;
                return Ok(Some((track, pos)));
            }
        }
    }
    match current_track(pool).await? {
        Some(t) => Ok(Some((t, 0))),
        None => Ok(None),
    }
}

/// Persist the last-playing track id + position so the next app
/// launch can resume from where the user stopped.
pub async fn persist_resume_point(
    pool: &SqlitePool,
    track_id: i64,
    position_ms: u64,
) -> AppResult<()> {
    write_setting_i64(pool, "player.last_track_id", track_id).await?;
    write_setting_i64(pool, "player.last_position_ms", position_ms as i64).await?;
    Ok(())
}

// ---------------------------------------------------------------------
// Shuffle / unshuffle
// ---------------------------------------------------------------------

/// Randomize the queue, keeping the currently-playing track at
/// position 0. Stashes the pre-shuffle ordering in
/// `profile_setting['queue.preshuffle']` (JSON array of track ids, in
/// their original order) so [`unshuffle`] can restore it.
///
/// Fisher–Yates on the slice after the current track; this is cheap
/// and deterministic given the crate-level RNG.
pub async fn shuffle(pool: &SqlitePool) -> AppResult<()> {
    // Read current ordering.
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT position, track_id FROM queue_item ORDER BY position",
    )
    .fetch_all(pool)
    .await?;
    if rows.len() < 2 {
        return Ok(()); // nothing to shuffle
    }

    let current_index = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, rows.len() as i64 - 1) as usize;

    // Snapshot the pre-shuffle order for unshuffle.
    let preshuffle_json: String =
        serde_json::to_string(&rows.iter().map(|(_, id)| *id).collect::<Vec<_>>())
            .map_err(|e| AppError::Other(format!("preshuffle json: {e}")))?;
    write_setting_string(pool, "queue.preshuffle", &preshuffle_json).await?;

    // Build the new ordering: [current, ...shuffled rest].
    let mut ids: Vec<i64> = rows.iter().map(|(_, id)| *id).collect();
    let current_id = ids.remove(current_index);
    fisher_yates(&mut ids);
    let mut new_ids = Vec::with_capacity(ids.len() + 1);
    new_ids.push(current_id);
    new_ids.extend(ids);

    write_queue_order(pool, &new_ids, 0).await
}

/// Restore the pre-shuffle order from `queue.preshuffle` and re-home
/// the cursor onto the currently-playing track's position in that
/// restored ordering.
pub async fn unshuffle(pool: &SqlitePool) -> AppResult<()> {
    let json = match read_setting_string(pool, "queue.preshuffle").await? {
        Some(s) => s,
        None => return Ok(()),
    };
    let original: Vec<i64> = serde_json::from_str(&json)
        .map_err(|e| AppError::Other(format!("preshuffle parse: {e}")))?;
    if original.is_empty() {
        return Ok(());
    }

    // Find the currently-playing track in the restored order.
    let current = current_track(pool).await?;
    let new_index = match current {
        Some(t) => original.iter().position(|&id| id == t.id).unwrap_or(0),
        None => 0,
    };

    write_queue_order(pool, &original, new_index).await?;
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(pool)
        .await?;
    Ok(())
}

/// Rewrite `queue_item` with the given ordering and update the
/// current index pointer. Runs in a transaction.
async fn write_queue_order(
    pool: &SqlitePool,
    ordered_ids: &[i64],
    new_current: usize,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM queue_item").execute(&mut *tx).await?;
    let now = Utc::now().timestamp_millis();
    for (pos, track_id) in ordered_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, 'manual', NULL, ?)",
        )
        .bind(track_id)
        .bind(pos as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind((new_current as i64).to_string())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// In-place Fisher–Yates using [`fastrand`] for simplicity.
///
/// We deliberately don't pull in a full RNG crate here — shuffling the
/// queue is not a security-critical operation and `fastrand` is ~20
/// lines of code and already a transitive dep via some other crate.
fn fisher_yates<T>(slice: &mut [T]) {
    // Simple linear congruential RNG seeded from the clock. Good
    // enough for shuffling a music queue.
    let mut seed: u64 = Utc::now().timestamp_millis() as u64;
    for i in (1..slice.len()).rev() {
        // xorshift step
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = (seed % (i as u64 + 1)) as usize;
        slice.swap(i, j);
    }
}

// Re-export the player state snapshot type here so the analytics task
// can keep its imports minimal. Not a real runtime dependency.
#[allow(dead_code)]
fn _type_check() -> Option<PlayerStateSnapshot> {
    None
}
