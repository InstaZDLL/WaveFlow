//! Shuffle, album shuffle and the restore of the pre-shuffle order.

use chrono::Utc;
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

use super::{current_track, read_setting_i64, read_setting_string, write_setting_string};

/// Randomize the queue, keeping the currently-playing track at
/// position 0. Stashes the pre-shuffle ordering in
/// `profile_setting['queue.preshuffle']` (JSON array of track ids, in
/// their original order) so [`unshuffle`] can restore it.
///
/// Fisher–Yates on the slice after the current track; this is cheap
/// and deterministic given the crate-level RNG.
pub async fn shuffle(pool: &SqlitePool) -> AppResult<()> {
    // Read current ordering.
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT position, track_id FROM queue_item ORDER BY position")
            .fetch_all(pool)
            .await?;
    if rows.len() < 2 {
        return Ok(()); // nothing to shuffle
    }

    let current_index = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, rows.len() as i64 - 1) as usize;

    snapshot_preshuffle_order(pool, &rows.iter().map(|(_, id)| *id).collect::<Vec<_>>()).await?;

    // Build the new ordering: [current, ...shuffled rest].
    let mut ids: Vec<i64> = rows.iter().map(|(_, id)| *id).collect();
    let current_id = ids.remove(current_index);
    fisher_yates(&mut ids);
    let mut new_ids = Vec::with_capacity(ids.len() + 1);
    new_ids.push(current_id);
    new_ids.extend(ids);

    write_queue_order(pool, &new_ids, 0).await
}

/// Take the pre-shuffle snapshot, unless one is already held.
///
/// `queue.preshuffle` records the order the listener actually built,
/// so [`unshuffle`] can give it back. It must be taken **once**, on
/// the way into shuffle: re-taking it while already shuffled would
/// record the shuffled order as the original one, and turning shuffle
/// off would then restore the mess instead of undoing it.
///
/// That became reachable when shuffle grew a third state (#618) — you
/// can now go from grouping tracks to grouping albums without passing
/// through off. The queue-replacing paths clear the key, so "a
/// snapshot exists" means "we are already shuffled" and nothing else.
async fn snapshot_preshuffle_order(pool: &SqlitePool, ids: &[i64]) -> AppResult<()> {
    if read_setting_string(pool, "queue.preshuffle")
        .await?
        .is_some()
    {
        return Ok(());
    }
    let json =
        serde_json::to_string(ids).map_err(|e| AppError::Other(format!("preshuffle json: {e}")))?;
    write_setting_string(pool, "queue.preshuffle", &json).await
}

/// Shuffle whole records instead of individual tracks (#618).
///
/// Only the order of the albums is randomised. Inside each one the
/// tracks are put back into disc and track order, so a record that
/// arrived in the queue scrambled still plays the way it was pressed.
///
/// Two rules the plain track shuffle does not have to answer:
///
/// - **The record you are in continues.** The current track stays at
///   position 0, the rest of its album follows from the next track
///   onward, and the tracks before it come after the last one — a
///   rotation, so nothing is dropped and the album still plays in
///   order from where you are.
/// - **A track with no album is its own record.** Loose files then
///   shuffle exactly as they would have under track shuffle, instead
///   of being welded into one arbitrary block by a shared NULL.
pub async fn shuffle_by_album(pool: &SqlitePool) -> AppResult<()> {
    // `position` decides ties inside a record, so a pair of tracks that
    // both lack a track number keeps the order it already had rather
    // than depending on how the rows came back.
    let rows: Vec<QueuedAlbumTrack> = sqlx::query_as(
        "SELECT q.track_id, q.position, t.album_id, t.disc_number, t.track_number
           FROM queue_item q
           LEFT JOIN track t ON t.id = q.track_id
          ORDER BY q.position",
    )
    .fetch_all(pool)
    .await?;
    if rows.len() < 2 {
        return Ok(());
    }

    let current_index = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, rows.len() as i64 - 1) as usize;
    let current_id = rows[current_index].0;

    // The order the listener built, so turning shuffle off restores it
    // exactly as it does after a track shuffle.
    snapshot_preshuffle_order(pool, &rows.iter().map(|(id, ..)| *id).collect::<Vec<_>>()).await?;

    let mut groups = album_runs(&rows, current_id);
    // Only the order of the records is random; `album_runs` has
    // already settled everything inside them.
    let tail = &mut groups[1..];
    fisher_yates(tail);

    let ordered: Vec<i64> = groups.into_iter().flatten().collect();
    write_queue_order(pool, &ordered, 0).await
}

/// One queue row, as [`album_runs`] needs it: the track, where it sits
/// now, and what the library knows about its place on a record.
pub(super) type QueuedAlbumTrack = (i64, i64, Option<i64>, Option<i64>, Option<i64>);

/// Split a queue into records, each in the order it was pressed in,
/// with the record being listened to first and rotated onto the
/// current track.
///
/// Pure so the grouping and the rotation are testable without a
/// database — the same reason [`cursor_after_removal`] is. Everything
/// random about album shuffle is the caller's single `fisher_yates`
/// over the returned tail; nothing here depends on chance, so a
/// failure is reproducible.
///
/// The first run is always the one holding `current_id`, so callers
/// can shuffle `[1..]` and leave the current track at position 0.
pub(super) fn album_runs(rows: &[QueuedAlbumTrack], current_id: i64) -> Vec<Vec<i64>> {
    // First-appearance order, so the grouping itself is deterministic.
    let mut groups: Vec<Vec<&QueuedAlbumTrack>> = Vec::new();
    let mut index_of: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for row in rows {
        match row.2 {
            Some(album) => match index_of.get(&album) {
                Some(&at) => groups[at].push(row),
                None => {
                    index_of.insert(album, groups.len());
                    groups.push(vec![row]);
                }
            },
            // A track with no album is its own record. Grouping them
            // all under one NULL would weld unrelated loose files into
            // a single block that always plays together.
            None => groups.push(vec![row]),
        }
    }

    for group in &mut groups {
        group.sort_by_key(|(_, position, _, disc, track_no)| {
            // A missing disc or track number sorts after every numbered
            // track rather than before it: an untagged bonus track
            // belongs at the end, not in front of track 1. `position`
            // breaks the remaining ties, so two equally untagged tracks
            // keep the order they already had instead of depending on
            // how the rows came back.
            (
                disc.unwrap_or(i64::MAX),
                track_no.unwrap_or(i64::MAX),
                *position,
            )
        });
    }

    let current_group = groups
        .iter()
        .position(|group| group.iter().any(|(id, ..)| *id == current_id))
        .unwrap_or(0);
    let mut ordered: Vec<Vec<i64>> = Vec::with_capacity(groups.len());
    let mut first: Vec<i64> = groups
        .remove(current_group)
        .into_iter()
        .map(|(id, ..)| *id)
        .collect();
    // Carry on from where the listener is, then wrap to the start of
    // the record. Nothing is dropped — a shuffle is a permutation —
    // and the album still plays in order from the current track.
    if let Some(at) = first.iter().position(|id| *id == current_id) {
        first.rotate_left(at);
    }
    ordered.push(first);
    ordered.extend(
        groups
            .into_iter()
            .map(|group| group.into_iter().map(|(id, ..)| *id).collect()),
    );
    ordered
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
    // Where each track came from, kept across the reorder. Writing
    // every row back as 'manual' would throw away two things that are
    // read later: the source a play_event is attributed to, and the
    // boundary `fill_queue` uses to tell queued-up "play next" items
    // from the source queue they were dropped into.
    //
    // Keyed by track id with one entry per occurrence, in position
    // order, so a queue holding the same track twice hands each copy
    // back its own source rather than the first one's.
    let mut sources: std::collections::HashMap<
        i64,
        std::collections::VecDeque<(String, Option<i64>)>,
    > = std::collections::HashMap::new();
    //
    // Read *inside* the transaction that rewrites them, so the rows
    // put back describe the queue being replaced rather than one that
    // changed in between.
    let mut tx = pool.begin().await?;
    let existing: Vec<(i64, String, Option<i64>)> =
        sqlx::query_as("SELECT track_id, source_type, source_id FROM queue_item ORDER BY position")
            .fetch_all(&mut *tx)
            .await?;
    for (track_id, source_type, source_id) in existing {
        sources
            .entry(track_id)
            .or_default()
            .push_back((source_type, source_id));
    }

    sqlx::query("DELETE FROM queue_item")
        .execute(&mut *tx)
        .await?;
    let now = Utc::now().timestamp_millis();
    for (pos, track_id) in ordered_ids.iter().enumerate() {
        // 'manual' only for a track the old queue did not hold, which
        // is not something any caller here does today.
        let (source_type, source_id) = sources
            .get_mut(track_id)
            .and_then(|queued| queued.pop_front())
            .unwrap_or_else(|| ("manual".to_string(), None));
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
