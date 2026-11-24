//! Filling and editing the queue: a fresh fill, appends, play-next
//! inserts and drag-reorders.

use chrono::Utc;
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{AppError, AppResult};

use super::{queue_length, write_album_ordered};

/// What a fresh fill does with the tracks the listener queued by hand
/// and has not heard yet: the rows marked `queued_by_hand` past the
/// cursor, which "Add to queue" and "Play next" write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserQueue {
    /// They play right after the track the fill starts on, in the order
    /// they were queued, ahead of the rest of the new list (#1168).
    /// Starting something else is not asking for them to go; emptying
    /// the queue is.
    Keep,
    /// The new list is the whole queue: a queue another player handed
    /// over, whose order is what was handed over.
    Drop,
}

/// A row queued by hand, carried across a fill as it was written.
#[derive(sqlx::FromRow)]
struct HandQueued {
    track_id: i64,
    source_type: String,
    source_id: Option<i64>,
    added_at: i64,
}

/// Replace the queue with one row per track and set
/// `queue.current_index` to `start_index`. Runs in a single
/// transaction so the UI never sees a partial state.
///
/// With [`UserQueue::Keep`], the tracks the listener queued by hand and
/// has not heard yet survive the replacement: they are written right
/// after the track at `start_index`, in their order, and the rest of
/// `track_ids` follows them — the order "Add to queue" gives them in a
/// queue that was not replaced. The cursor still lands on the track at
/// `start_index`, which keeps its index. One of them that is the very
/// track starting is dropped, since it is the one playing now; only
/// one, as a track queued twice was asked for twice. What was queued by
/// hand and already played (at or before the cursor) goes with the old
/// queue.
///
/// `album_ordered` says whether this list is whole records in their own
/// order — what a generator produces in album mode, and what no
/// `source_type` can express (#647). Persisted alongside the queue
/// rather than held in memory only, for the same reason the shuffle
/// mode is: a session survives a restart, and the gain decision has to
/// survive with it. It describes the new list, not the kept rows, and
/// that stays true with them in it: they say `'manual'`, which is never
/// read as a record playing through (`SharedPlayback::listening_to`).
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
    user_queue: UserQueue,
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

    // The write lock is taken first: keeping the user queue reads the
    // queue before replacing it, and a deferred transaction's first write
    // fails outright when another writer (a scan) committed since its
    // read (`db::lock_waiters::begin_write`).
    let mut tx = crate::db::lock_waiters::begin_write(pool).await?;

    let mut kept = match user_queue {
        UserQueue::Keep => pending_by_hand(&mut tx).await?,
        UserQueue::Drop => Vec::new(),
    };
    let starting = track_ids[start_index];
    if let Some(at) = kept.iter().position(|row| row.track_id == starting) {
        kept.remove(at);
    }

    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;

    let now = Utc::now().timestamp_millis();
    let (up_to_start, after_start) = track_ids.split_at(start_index + 1);
    insert_rows(&mut tx, 0, up_to_start, source_type, source_id, false, now).await?;
    let mut position = up_to_start.len() as i64;
    for row in &kept {
        sqlx::query(
            "INSERT INTO queue_item
                    (track_id, position, source_type, source_id, added_at, queued_by_hand)
             VALUES (?, ?, ?, ?, ?, 1)",
        )
        .bind(row.track_id)
        .bind(position)
        .bind(&row.source_type)
        .bind(row.source_id)
        .bind(row.added_at)
        .execute(&mut *tx)
        .await?;
        position += 1;
    }
    insert_rows(
        &mut tx,
        position,
        after_start,
        source_type,
        source_id,
        false,
        now,
    )
    .await?;

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

/// The rows queued by hand past the cursor, in queue order. The cursor
/// is clamped into the queue as every reader clamps it, so a stale one
/// left past the end keeps nothing rather than everything.
async fn pending_by_hand(tx: &mut SqliteConnection) -> AppResult<Vec<HandQueued>> {
    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    if len == 0 {
        return Ok(Vec::new());
    }
    let cursor = read_cursor(tx, len).await?;
    Ok(sqlx::query_as(
        "SELECT track_id, source_type, source_id, added_at
           FROM queue_item
          WHERE queued_by_hand = 1 AND position > ?
          ORDER BY position",
    )
    .bind(cursor)
    .fetch_all(&mut *tx)
    .await?)
}

/// `queue.current_index` read inside `tx`, clamped into `[0, len)`.
/// `len` must be positive.
async fn read_cursor(tx: &mut SqliteConnection, len: i64) -> AppResult<i64> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = 'queue.current_index'")
            .fetch_optional(&mut *tx)
            .await?;
    Ok(raw
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, len - 1))
}

/// One row per track at consecutive positions from `first`, all from
/// the same source and all queued by hand or none.
async fn insert_rows(
    tx: &mut SqliteConnection,
    first: i64,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
    queued_by_hand: bool,
    now: i64,
) -> AppResult<()> {
    for (offset, id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO queue_item
                    (track_id, position, source_type, source_id, added_at, queued_by_hand)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(first + offset as i64)
        .bind(source_type)
        .bind(source_id)
        .bind(now)
        .bind(queued_by_hand)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// Build a queue out of tracks queued by hand where there was none: the
/// first one is current, and the rest are the user queue behind it. A
/// new queue, so a new answer to what it is made of — a hand-built list
/// is not a session of records, whatever the last one was (#647).
async fn fill_by_hand(
    tx: &mut SqliteConnection,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
    now: i64,
) -> AppResult<()> {
    insert_rows(tx, 0, track_ids, source_type, source_id, true, now).await?;
    sqlx::query(
        "UPDATE profile_setting
            SET value = '0', updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind(now)
    .execute(&mut *tx)
    .await?;
    write_album_ordered(tx, false, now).await?;
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Move every row at or past `from` down by `count` positions, making
/// room for an insert there. SQLite checks the UNIQUE(position)
/// constraint per row, so a direct `position = position + N` would
/// collide mid-update: the rows are parked in a high range first, then
/// brought back down past the gap. The 10_000_000 offset is well above
/// any realistic queue length.
async fn open_gap(tx: &mut SqliteConnection, from: i64, count: i64) -> AppResult<()> {
    const OFFSET: i64 = 10_000_000;
    sqlx::query("UPDATE queue_item SET position = position + ? WHERE position >= ?")
        .bind(OFFSET)
        .bind(from)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE queue_item SET position = position - ? + ? WHERE position >= ?")
        .bind(OFFSET)
        .bind(count)
        .bind(from + OFFSET)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Append `track_ids` to the end of the queue without disturbing the
/// current cursor. Used by autoplay to add its picks after everything
/// else; they are not queued by hand.
pub async fn append(
    pool: &SqlitePool,
    track_ids: &[i64],
    source_type: &str,
    source_id: Option<i64>,
) -> AppResult<()> {
    if track_ids.is_empty() {
        return Ok(());
    }
    // The write lock is taken first: this reads before it writes, and a
    // deferred transaction's first write fails outright when another writer
    // (a scan) committed since its read (`db::lock_waiters::begin_write`).
    let mut tx = crate::db::lock_waiters::begin_write(pool).await?;
    let max_pos: Option<i64> = sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    let start = max_pos.map(|p| p + 1).unwrap_or(0);
    let now = Utc::now().timestamp_millis();
    insert_rows(
        &mut tx,
        start,
        track_ids,
        source_type,
        source_id,
        false,
        now,
    )
    .await?;
    // Order changed → shuffle snapshot is no longer reusable.
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Append `track_ids` to the **user queue** — the rows queued by hand
/// (`queued_by_hand = 1`) sitting between the current track and the
/// context tail (whatever album / playlist / smart-playlist fill seeded
/// the queue). Matches Spotify semantics: "Add to queue" stacks tracks
/// at the *bottom* of the user queue, not at the absolute end past
/// every remaining album track, so the user can keep playing the album
/// they started while their manual picks fire one by one in between.
///
/// Boundary = `MIN(position)` among items past the current cursor that
/// were not queued by hand. Not `source_type != 'manual'`: a selection,
/// the history or files opened from the system fill the queue as
/// `'manual'` too, and those are a context like an album. NULL boundary
/// means the entire tail is already queued by hand (or there's nothing
/// past the cursor) — the new rows then go after the last one. Empty
/// queue still builds a queue so the first "Add to queue" click on a
/// fresh session starts playback.
///
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
    // The write lock is taken first: this reads before it writes, and a
    // deferred transaction's first write fails outright when another writer
    // (a scan) committed since its read (`db::lock_waiters::begin_write`).
    let mut tx = crate::db::lock_waiters::begin_write(pool).await?;
    let now = Utc::now().timestamp_millis();

    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;

    if len == 0 {
        fill_by_hand(&mut tx, track_ids, "manual", source_id, now).await?;
        tx.commit().await?;
        return Ok(true);
    }

    let current = read_cursor(&mut tx, len).await?;

    let boundary: Option<i64> = sqlx::query_scalar(
        "SELECT MIN(position) FROM queue_item
          WHERE position > ?
            AND queued_by_hand = 0",
    )
    .bind(current)
    .fetch_one(&mut *tx)
    .await?;

    // NULL boundary = no context tail past the cursor (entire tail is
    // already queued by hand, or current is at the very end). Same end
    // shape as `append`: drop the new rows at MAX(position) + 1.
    let insert_at = match boundary {
        Some(p) => {
            open_gap(&mut tx, p, track_ids.len() as i64).await?;
            p
        }
        None => {
            let max_pos: Option<i64> = sqlx::query_scalar("SELECT MAX(position) FROM queue_item")
                .fetch_one(&mut *tx)
                .await?;
            max_pos.map(|p| p + 1).unwrap_or(0)
        }
    };
    insert_rows(
        &mut tx, insert_at, track_ids, "manual", source_id, true, now,
    )
    .await?;

    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(false)
}

/// Insert `track_ids` immediately after the current cursor position —
/// "Play next", so the rows are queued by hand, at the top of the user
/// queue. Existing items past the cursor are pushed down to keep the
/// queue dense. The cursor itself doesn't move, so the currently-playing
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
    // The write lock is taken first: this reads the cursor before it
    // writes (`db::lock_waiters::begin_write`).
    let mut tx = crate::db::lock_waiters::begin_write(pool).await?;
    let now = Utc::now().timestamp_millis();
    let len: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM queue_item")
        .fetch_one(&mut *tx)
        .await?;
    if len == 0 {
        // No queue yet — the picks become one, starting at 0.
        fill_by_hand(&mut tx, track_ids, source_type, source_id, now).await?;
        tx.commit().await?;
        return Ok(true);
    }
    let insert_at = read_cursor(&mut tx, len).await? + 1;
    open_gap(&mut tx, insert_at, track_ids.len() as i64).await?;
    insert_rows(
        &mut tx,
        insert_at,
        track_ids,
        source_type,
        source_id,
        true,
        now,
    )
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
