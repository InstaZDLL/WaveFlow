//! Filling and editing the queue: a fresh fill, appends, play-next
//! inserts and drag-reorders.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

use super::{queue_length, read_setting_i64, write_album_ordered};

/// Clear the queue and insert new rows, one per track, with positions
/// 0..n. Also sets `queue.current_index` to `start_index`. Runs in a
/// single transaction so the UI never sees a partial state.
///
/// `album_ordered` says whether this list is whole records in their own
/// order — what a generator produces in album mode, and what no
/// `source_type` can express (#647). Persisted alongside the queue
/// rather than held in memory only, for the same reason the shuffle
/// mode is: a session survives a restart, and the gain decision has to
/// survive with it.
///
/// **Every replacement writes it**, including the ones that write
/// `false`. This is the single path that replaces the queue, which is
/// exactly what makes it the place a stale flag cannot outlive its
/// session.
pub async fn fill_queue(
    pool: &SqlitePool,
    source_type: &str,
    source_id: Option<i64>,
    track_ids: &[i64],
    start_index: usize,
    album_ordered: bool,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Err(AppError::Other(
            "cannot fill queue with empty track list".into(),
        ));
    }
    if start_index >= track_ids.len() {
        return Err(AppError::Other(format!(
            "start_index {start_index} out of range (queue length {})",
            track_ids.len()
        )));
    }

    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;

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

    write_album_ordered(&mut tx, album_ordered, now).await?;

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
    let max_pos: Option<i64> = sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    let start = max_pos.map(|p| p + 1).unwrap_or(0);
    let now = Utc::now().timestamp_millis();
    for (offset, id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(start + offset as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    // Order changed → shuffle snapshot is no longer reusable.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Append `track_ids` to the **user queue** — the contiguous block of
/// `source_type = 'manual'` items sitting between the current track
/// and the context tail (whatever album / playlist / smart-playlist
/// fill seeded the queue). Matches Spotify semantics: "Add to queue"
/// stacks tracks at the *bottom* of the user queue, not at the
/// absolute end past every remaining album track, so the user can
/// keep playing the album they started while their manual picks fire
/// one by one in between.
///
/// Boundary = `MIN(position)` among items past the current cursor
/// whose `source_type != 'manual'`. NULL boundary means the entire
/// tail is already manual (or there's nothing past the cursor) — that
/// degenerates into [`append`]. Empty queue still degenerates into
/// [`fill_queue`] so the first "Add to queue" click on a fresh
/// session starts playback.
/// Returns whether the queue was **replaced** rather than added to —
/// true only on the empty-queue branch, which builds a new queue and so
/// decides afresh what it is. The caller mirrors that into the engine
/// (#647).
pub async fn append_to_user_queue(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_id: Option<i64>,
) -> AppResult<bool> {
    if track_ids.is_empty() {
        return Ok(false);
    }

    // Single transaction wraps the boundary lookup AND the insert so the
    // decision can't be invalidated between reads and writes by a
    // concurrent advance / fill_queue / shuffle (SQLite WAL allows
    // many readers but only one writer, and our reads were happening
    // outside that single-writer envelope before this commit). Every
    // SQL inside the function now runs against `&mut *tx`.
    let mut tx = pool.begin().await?;
    let now = Utc::now().timestamp_millis();

    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;

    // Empty queue — replicate `fill_queue` inline so it stays in the
    // same tx. Same shape, just bound to "manual" since we know the
    // origin.
    if len == 0 {
        for (pos, id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
                 VALUES (?, ?, 'manual', ?, ?)",
            )
            .bind(id)
            .bind(pos as i64)
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
        .bind("0")
        .bind(now)
        .execute(&mut *tx)
        .await?;
        // A queue built out of picks the listener stacked by hand is not
        // a session of records, whatever the last one was.
        write_album_ordered(&mut tx, false, now).await?;
        sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(true);
    }

    let current_raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
            .fetch_optional(&mut *tx)
            .await?;
    let current = current_raw
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, len - 1);

    let boundary: Option<i64> = sqlx::query_scalar(
        "SELECT MIN(position) FROM queue_item
          WHERE position > ?
            AND source_type != 'manual'",
    )
    .bind(current)
    .fetch_one(&mut *tx)
    .await?;

    // NULL boundary = no context tail past the cursor (entire tail is
    // already manual, or current is at the very end). Same end shape as
    // `append`: drop the new rows at MAX(position) + 1.
    let insert_at = match boundary {
        Some(p) => p,
        None => {
            let max_pos: Option<i64> = sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
                .fetch_one(&mut *tx)
                .await?;
            max_pos.map(|p| p + 1).unwrap_or(0)
        }
    };
    let needs_shift = boundary.is_some();

    let count = track_ids.len() as i64;

    if needs_shift {
        // Park-shift detour — same trick as `insert_after_current`.
        // SQLite checks UNIQUE(position) per row so a direct
        // `position + N` collides mid-update; bump the affected rows
        // to a high range, then bring them back down past the freshly
        // inserted block.
        const OFFSET: i64 = 10_000_000;
        sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
            .bind(OFFSET)
            .bind(insert_at)
            .execute(&mut *tx)
            .await?;

        for (offset, id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
                 VALUES (?, ?, 'manual', ?, ?)",
            )
            .bind(id)
            .bind(insert_at + offset as i64)
            .bind(source_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE queue_item SET position = position - ? + ? WHERE position >= ?")
            .bind(OFFSET)
            .bind(count)
            .bind(insert_at + OFFSET)
            .execute(&mut *tx)
            .await?;
    } else {
        // No shift needed — append directly past the last row.
        for (offset, id) in track_ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO queue_item (track_id, position, source_type, source_id, added_at)
                 VALUES (?, ?, 'manual', ?, ?)",
            )
            .bind(id)
            .bind(insert_at + offset as i64)
            .bind(source_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
    }

    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(false)
}

/// Insert `track_ids` immediately after the current cursor position.
/// Existing items past the cursor are pushed down to keep the queue
/// dense. The cursor itself doesn't move, so the currently-playing
/// track keeps playing.
///
/// Returns whether the queue was **replaced** rather than inserted
/// into: an empty one is filled instead, which is a new queue and a new
/// answer to what it is made of. The caller mirrors that into the
/// engine (#647).
pub async fn insert_after_current(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
) -> AppResult<bool> {
    if track_ids.is_empty() {
        return Ok(false);
    }
    let len = queue_length(pool).await?;
    if len == 0 {
        // No queue yet — fall back to filling it and starting at 0.
        // An empty queue filled by "play next" is a hand-built list, not
        // a session of records — whatever the tracks happen to be.
        fill_queue(pool, source_type, source_id, track_ids, 0, false).await?;
        return Ok(true);
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
    sqlx::query("UPDATE queue_item SET position = position - ? + ? WHERE position >= ?")
        .bind(OFFSET)
        .bind(count)
        .bind(insert_at + OFFSET)
        .execute(&mut *tx)
        .await?;

    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    // The queue was not replaced, so what it is made of has not
    // changed. The inserted rows say `'manual'`, and a `'manual'` row
    // is never read as a record playing through whatever session it
    // landed in — see `SharedPlayback::listening_to`. Clearing the flag
    // here instead would take the album gain from every record still
    // queued behind the one track the listener slipped in.
    Ok(false)
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
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
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
