//! Shuffle, album shuffle and the restore of the pre-shuffle order.

use chrono::Utc;
use std::collections::{HashMap, HashSet, VecDeque};

use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

use super::{read_setting_i64, read_setting_string, write_setting_string};

/// Randomize the queue, keeping the currently-playing track at
/// position 0. Stashes the pre-shuffle ordering in
/// `profile_setting['queue.preshuffle']` (JSON array of track ids, in
/// their original order) so [`unshuffle`] can restore it.
///
/// The tracks queued by hand and still to come are not shuffled: they
/// follow the current track in the order they were queued, and the
/// rest is shuffled behind them (#1168). "Add to queue" means "play
/// these next", whether or not the rest is in order — and it is what
/// lets a fill that kept them be shuffled at once without scattering
/// them.
///
/// Fisher–Yates on the slice after them; this is cheap and
/// deterministic given the crate-level RNG.
pub async fn shuffle(pool: &SqlitePool) -> AppResult<()> {
    // Read current ordering.
    let rows: Vec<QueuedRow> =
        sqlx::query_as("SELECT id, track_id, queued_by_hand FROM queue_item ORDER BY position")
            .fetch_all(pool)
            .await?;
    if rows.len() < 2 {
        return Ok(()); // nothing to shuffle
    }

    let current_index = read_setting_i64(pool, "queue.current_index")
        .await?
        .unwrap_or(0)
        .clamp(0, rows.len() as i64 - 1) as usize;

    snapshot_preshuffle_order(pool, &rows.iter().map(|row| row.1).collect::<Vec<_>>()).await?;

    // Build the new ordering: [current, ...waiting, ...shuffled rest].
    let (current, waiting, mut rest) = split_user_queue(&rows, current_index);
    fisher_yates(&mut rest);
    let mut ordered = Vec::with_capacity(rows.len());
    ordered.push(current);
    ordered.extend(waiting);
    ordered.extend(rest);

    write_row_order(pool, &ordered, current).await
}

/// One queue row as a shuffle sees it: `(queue_item.id, track_id,
/// queued_by_hand)`.
pub(super) type QueuedRow = (i64, i64, bool);

/// Split a queue for a shuffle into the current row, the rows queued
/// by hand past it (in order: the user queue, which a shuffle leaves in
/// front) and every other row (in order: what the shuffle mixes). Row
/// ids, not track ids, so a track queued twice is two rows.
///
/// Pure, like [`album_runs`], so what is pinned is testable without a
/// database or chance. `current_index` must be inside `rows`.
pub(super) fn split_user_queue(
    rows: &[QueuedRow],
    current_index: usize,
) -> (i64, Vec<i64>, Vec<i64>) {
    let mut waiting = Vec::new();
    let mut rest = Vec::with_capacity(rows.len());
    for (index, &(row_id, _, by_hand)) in rows.iter().enumerate() {
        if index == current_index {
            continue;
        }
        if by_hand && index > current_index {
            waiting.push(row_id);
        } else {
            rest.push(row_id);
        }
    }
    (rows[current_index].0, waiting, rest)
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
///
/// And the one it shares with it: the tracks queued by hand and still
/// to come belong to no record here. They follow the current track, in
/// their order, ahead of the rest of its album (#1168).
pub async fn shuffle_by_album(pool: &SqlitePool) -> AppResult<()> {
    // `position` decides ties inside a record, so a pair of tracks that
    // both lack a track number keeps the order it already had rather
    // than depending on how the rows came back.
    let rows: Vec<AlbumShuffleRow> = sqlx::query_as(
        "SELECT q.id, q.track_id, q.queued_by_hand,
                q.position, t.album_id, t.disc_number, t.track_number
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
    snapshot_preshuffle_order(pool, &rows.iter().map(|row| row.1).collect::<Vec<_>>()).await?;

    let shuffled: Vec<QueuedRow> = rows
        .iter()
        .map(|&(id, track_id, by_hand, ..)| (id, track_id, by_hand))
        .collect();
    let (_, waiting, _) = split_user_queue(&shuffled, current_index);
    let pinned: HashSet<i64> = waiting.iter().copied().collect();
    // Grouped by row id rather than track id: `album_runs` only ever
    // compares the first field, and a row id names one entry even when
    // the same track is queued twice.
    let records: Vec<QueuedAlbumTrack> = rows
        .iter()
        .filter(|row| !pinned.contains(&row.0))
        .map(|&(id, _, _, position, album, disc, track_no)| (id, position, album, disc, track_no))
        .collect();

    let mut groups = album_runs(&records, current_id);
    // Only the order of the records is random; `album_runs` has
    // already settled everything inside them.
    let tail = &mut groups[1..];
    fisher_yates(tail);

    let mut ordered: Vec<i64> = groups.into_iter().flatten().collect();
    // The current track opens the first run; the user queue follows it.
    ordered.splice(1..1, waiting);
    write_row_order(pool, &ordered, current_id).await
}

/// A queue row as album shuffle reads it: `(queue_item.id, track_id,
/// queued_by_hand, position, album_id, disc_number, track_number)`.
type AlbumShuffleRow = (i64, i64, bool, i64, Option<i64>, Option<i64>, Option<i64>);

/// One queue row, as [`album_runs`] needs it: the entry (any id that
/// names one entry — the shuffle passes `queue_item.id`), where it sits
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
/// the cursor onto the currently-playing entry's position in that
/// restored ordering.
///
/// The snapshot holds track ids; each is matched to an entry of the
/// queue, one per occurrence in queue order, so a track queued twice
/// puts both entries back. A track no longer in the queue — removed
/// with its folder, which cascades without touching the snapshot — is
/// skipped rather than queued again.
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

    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT id, track_id FROM queue_item ORDER BY position")
            .fetch_all(pool)
            .await?;
    if !rows.is_empty() {
        let current_index = read_setting_i64(pool, "queue.current_index")
            .await?
            .unwrap_or(0)
            .clamp(0, rows.len() as i64 - 1) as usize;
        let mut entries: HashMap<i64, VecDeque<i64>> = HashMap::new();
        for (row_id, track_id) in &rows {
            entries.entry(*track_id).or_default().push_back(*row_id);
        }
        let ordered: Vec<i64> = original
            .iter()
            .filter_map(|track_id| entries.get_mut(track_id).and_then(VecDeque::pop_front))
            .collect();
        write_row_order(pool, &ordered, rows[current_index].0).await?;
    }
    sqlx::query("DELETE FROM profile_setting WHERE key = 'queue.preshuffle'")
        .execute(pool)
        .await?;
    Ok(())
}

/// Put the queue's entries in the order of `ordered` (`queue_item.id`s)
/// and the cursor on `current_row`, wherever it lands. Runs in a
/// transaction.
///
/// The rows are moved, not rewritten: each keeps everything it was
/// written with — the source a `play_event` is attributed to, whether
/// it was queued by hand (which a fill keeps and "Add to queue" stacks
/// behind), when it was added, and its id, which MPD clients hold as
/// the entry's `Id`. Writing every row back as `'manual'`, as this once
/// did, lost the first two.
///
/// `ordered` was read before this transaction, so it is checked against
/// the queue as it is now: an entry gone since is skipped, and one
/// added since goes after the others, in its own order. Either way the
/// positions stay dense, which every reader of the queue relies on.
async fn write_row_order(pool: &SqlitePool, ordered: &[i64], current_row: i64) -> AppResult<()> {
    // The write lock is taken first: this reads before it writes, and a
    // deferred transaction's first write fails outright when another writer
    // (a scan) committed since its read (`db::lock_waiters::begin_write`).
    let mut tx = crate::db::lock_waiters::begin_write(pool).await?;
    let existing: Vec<i64> = sqlx::query_scalar("SELECT id FROM queue_item ORDER BY position")
        .fetch_all(&mut *tx)
        .await?;
    let present: HashSet<i64> = existing.iter().copied().collect();
    let mut placed: HashSet<i64> = HashSet::with_capacity(existing.len());
    let mut order: Vec<i64> = Vec::with_capacity(existing.len());
    for id in ordered.iter().chain(existing.iter()) {
        if present.contains(id) && placed.insert(*id) {
            order.push(*id);
        }
    }

    // Parked above every real position first, so no update collides
    // with a row not yet moved on UNIQUE(position), which SQLite checks
    // per row.
    const PARK: i64 = 10_000_000;
    sqlx::query("UPDATE queue_item SET position = position + ?")
        .bind(PARK)
        .execute(&mut *tx)
        .await?;
    for (position, id) in order.iter().enumerate() {
        sqlx::query("UPDATE queue_item SET position = ? WHERE id = ?")
            .bind(position as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }

    let cursor = order.iter().position(|id| *id == current_row).unwrap_or(0);
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE profile_setting
            SET value = ?, updated_at = ?
          WHERE key = 'queue.current_index'",
    )
    .bind((cursor as i64).to_string())
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
