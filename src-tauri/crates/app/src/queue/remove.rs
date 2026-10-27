//! Removing from the queue, and keeping the cursor on the right row.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::AppResult;

/// Where the cursor lands after `[start, end)` (which held `removed` rows) is
/// deleted from the queue and positions are compacted, so the **same track
/// keeps playing**:
///   - deletions entirely before the cursor shift it down by `removed`;
///   - a deletion that spans the cursor lands it on whatever now occupies
///     `start` (the track that fell into the gap);
///   - deletions after the cursor leave it put.
///
/// The result is clamped into the shrunk queue `[0, new_len)`.
///
/// Pure so the cursor arithmetic is unit-testable without a database — the
/// bug it guards (only clamping, so a deletion before the cursor silently
/// advanced to the next track) isn't visible from the clamp alone.
pub(super) fn cursor_after_removal(
    current: i64,
    start: i64,
    end: i64,
    removed: i64,
    new_len: i64,
) -> i64 {
    let adjusted = if current >= end {
        current - removed
    } else if current >= start {
        start
    } else {
        current
    };
    adjusted.clamp(0, (new_len - 1).max(0))
}

/// Persist the [`cursor_after_removal`] adjustment inside the open removal
/// transaction. Whenever rows leave the queue the cursor can dangle past the
/// end (and [`advance`] would then have nothing to step from) or, worse,
/// silently point at a different track; this keeps it on the same one.
async fn adjust_current_index_after_removal(
    tx: &mut sqlx::SqliteConnection,
    start: i64,
    end: i64,
    removed: i64,
    new_len: i64,
) -> AppResult<()> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
            .fetch_optional(&mut *tx)
            .await?;
    let current = raw.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let next = cursor_after_removal(current, start, end, removed, new_len);
    if next != current {
        let now = Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE profile_setting SET value = ?, updated_at = ?
              WHERE key = 'queue.current_index'",
        )
        .bind(next.to_string())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// Empty the queue and reset the cursor.
///
/// Playback is deliberately left alone — the decoder keeps whatever it
/// already loaded. MPD's `clear` behaves the same way: it empties the
/// queue, and stopping is a separate `stop`.
pub async fn clear(pool: &SqlitePool) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting SET value = '0', updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // The pre-shuffle snapshot describes a queue that no longer exists.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Remove the half-open position range `[start, end)` and compact the
/// positions above it. Returns the number of rows removed.
///
/// `position` carries a UNIQUE constraint and SQLite enforces it per
/// row, so the surviving rows cannot simply be decremented in place —
/// a row moving down onto a slot whose occupant has not moved yet would
/// collide. Same `PARK` detour [`reorder`] uses.
pub async fn remove_range(pool: &SqlitePool, start: i64, end: i64) -> AppResult<u64> {
    const PARK: i64 = 10_000_000;
    let mut tx = pool.begin().await?;

    // Read the length INSIDE the transaction so the clamp, the delete range and
    // `new_len` all derive from the same consistent snapshot the DELETE acts
    // on — a concurrent queue write between a pre-transaction count and the
    // delete would otherwise skew the cursor adjustment.
    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    let start = start.clamp(0, len);
    let end = end.clamp(0, len);
    if end <= start {
        return Ok(0);
    }
    let span = end - start;

    let removed = sqlx::query("DELETE FROM queue_item WHERE position >= ? AND position < ?")
        .bind(start)
        .bind(end)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
        .bind(PARK)
        .bind(end)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE queue_item SET position = position - ? - ? WHERE position >= ?")
        .bind(PARK)
        .bind(span)
        .bind(PARK)
        .execute(&mut *tx)
        .await?;

    adjust_current_index_after_removal(&mut tx, start, end, removed as i64, len - removed as i64)
        .await?;
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(removed)
}

/// Remove one entry by its `queue_item.id`.
///
/// Keyed on the row id rather than the position because that is the
/// identity MPD's `deleteid` carries, and it stays valid even if the
/// queue was reordered between the client reading it and acting on it.
/// Returns `false` when no such row exists.
pub async fn remove_by_queue_id(pool: &SqlitePool, queue_id: i64) -> AppResult<bool> {
    let position: Option<i64> = sqlx::query_scalar("SELECT position FROM queue_item WHERE id = ?")
        .bind(queue_id)
        .fetch_optional(pool)
        .await?;
    let Some(position) = position else {
        return Ok(false);
    };
    Ok(remove_range(pool, position, position + 1).await? > 0)
}

/// Look up a queue row's position from its id.
pub async fn position_of_queue_id(pool: &SqlitePool, queue_id: i64) -> AppResult<Option<i64>> {
    let position: Option<i64> = sqlx::query_scalar("SELECT position FROM queue_item WHERE id = ?")
        .bind(queue_id)
        .fetch_optional(pool)
        .await?;
    Ok(position)
}
