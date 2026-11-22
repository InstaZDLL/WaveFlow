//! Re-attach the play events a vanished track left behind.

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use crate::error::CoreResult;

/// Available tracks read per query while looking for the orphans' tracks.
const TRACK_PAGE: i64 = 2_000;

/// Events re-attached per transaction.
const WRITE_BATCH: usize = 500;

/// Re-attach listening history that lost its track row.
///
/// `play_event.track_id` is `ON DELETE SET NULL`, so removing a folder or
/// a library orphans the history instead of destroying it (issue #367).
/// This pass runs after a scan and gives those events their track back,
/// matching on the snapshot each one carries.
///
/// The three keys are tried in order, strongest first, because they fail
/// in different circumstances:
///
/// 1. **`file_hash`** — the same bytes, moved or re-added. Exact, but a
///    tag edit rewrites the file through lofty, so the blake3 changes
///    even though nothing about the music did.
/// 2. **`file_path`** — catches precisely that case: same file on disk,
///    different hash. Fails when the user reorganised their folders.
/// 3. **artist + title** — a re-rip, a different encoding, a library
///    rebuilt from scratch. Loosest, and deliberately last: it cannot
///    tell a live version from the studio one. Compared as SQLite's
///    `NOCASE` does, ASCII letters alone folded.
///
/// An event takes the strongest key that finds a track, so a strong match
/// is never overwritten by a weaker one, and within a key the lowest track
/// id, so a duplicate file resolves to the same track on every run instead
/// of the history hopping between copies. Matching is restricted to
/// available tracks — an unavailable row is a file that vanished, and
/// attaching history to it would just orphan it again on the next pass.
///
/// The matching is read, never written under the lock. An orphan whose
/// file never comes back stays an orphan and is looked for again after
/// every scan; done as three correlated `UPDATE`s, that meant a scan of
/// the whole `track` table per orphan for the path and the name (neither
/// is indexed alone), all in one transaction: 500 orphans on 20 000
/// tracks held the write lock for 9 s, and a click on a track at the end
/// of the scan failed on its busy timeout. The tracks are now read once,
/// a page at a time, keeping only the keys some orphan is waiting for,
/// and only the events that found a track are written, in short
/// transactions that start with their write. Each write checks that the
/// event is still an orphan and its track still available, since both
/// can change between the read and the write.
///
/// Returns the number of events re-attached.
pub async fn reattach_orphaned_play_events(pool: &SqlitePool) -> CoreResult<u32> {
    type Orphan = (
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    // On a healthy library there are no orphans at all, and this keeps the
    // common case to one indexed lookup.
    let orphans: Vec<Orphan> = sqlx::query_as(
        "SELECT id, snapshot_hash, snapshot_path, snapshot_artist, snapshot_title
           FROM play_event
          WHERE track_id IS NULL
          ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    if orphans.is_empty() {
        return Ok(0);
    }

    // What the orphans could be matched on, so the walk below keeps only the
    // tracks one of them is waiting for.
    let hashes: HashSet<&str> = orphans.iter().filter_map(|o| o.1.as_deref()).collect();
    let paths: HashSet<&str> = orphans.iter().filter_map(|o| o.2.as_deref()).collect();
    let names: HashSet<(String, String)> = orphans
        .iter()
        .filter_map(|o| Some(name_key(o.3.as_deref()?, o.4.as_deref()?)))
        .collect();

    // Key → lowest available track id. Pages come in id order, so the first
    // track a key meets is its lowest.
    let mut by_hash: HashMap<String, i64> = HashMap::new();
    let mut by_path: HashMap<String, i64> = HashMap::new();
    let mut by_name: HashMap<(String, String), i64> = HashMap::new();
    let mut after = i64::MIN;
    loop {
        // `ar.name` is NULL for a track with no primary artist, which no
        // name matches.
        let page: Vec<(i64, String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT t.id, t.file_hash, t.file_path, t.title, ar.name
               FROM track t
               LEFT JOIN artist ar ON ar.id = t.primary_artist
              WHERE t.is_available = 1 AND t.id > ?
              ORDER BY t.id
              LIMIT ?",
        )
        .bind(after)
        .bind(TRACK_PAGE)
        .fetch_all(pool)
        .await?;
        let Some(last) = page.last() else {
            break;
        };
        after = last.0;
        for (id, hash, path, title, artist) in page {
            if hashes.contains(hash.as_str()) {
                by_hash.entry(hash).or_insert(id);
            }
            if paths.contains(path.as_str()) {
                by_path.entry(path).or_insert(id);
            }
            if let Some(artist) = artist {
                let key = name_key(&artist, &title);
                if names.contains(&key) {
                    by_name.entry(key).or_insert(id);
                }
            }
        }
    }

    let found: Vec<(i64, i64, &'static str)> = orphans
        .iter()
        .filter_map(|(event, hash, path, artist, title)| {
            let by_hash = hash
                .as_deref()
                .and_then(|h| by_hash.get(h))
                .map(|&t| (t, "file_hash"));
            let by_path = || {
                path.as_deref()
                    .and_then(|p| by_path.get(p))
                    .map(|&t| (t, "file_path"))
            };
            let by_name = || {
                let key = name_key(artist.as_deref()?, title.as_deref()?);
                by_name.get(&key).map(|&t| (t, "artist_title"))
            };
            let (track, key) = by_hash.or_else(by_path).or_else(by_name)?;
            Some((*event, track, key))
        })
        .collect();

    let mut reattached: u64 = 0;
    let mut per_key: HashMap<&'static str, u64> = HashMap::new();
    for batch in found.chunks(WRITE_BATCH) {
        // The first statement writes, so the transaction waits for the lock
        // instead of failing on a snapshot another writer made stale.
        let mut tx = pool.begin().await?;
        for &(event, track, key) in batch {
            let affected = sqlx::query(
                "UPDATE play_event
                    SET track_id = ?
                  WHERE id = ?
                    AND track_id IS NULL
                    AND EXISTS (SELECT 1 FROM track WHERE id = ? AND is_available = 1)",
            )
            .bind(track)
            .bind(event)
            .bind(track)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            reattached += affected;
            *per_key.entry(key).or_default() += affected;
        }
        tx.commit().await?;
    }
    for (key, affected) in per_key {
        if affected > 0 {
            tracing::info!(key, affected, "re-attached orphaned play events");
        }
    }

    Ok(reattached.min(u32::MAX as u64) as u32)
}

/// An artist and a title as SQLite's `NOCASE` compares them: ASCII letters
/// folded, nothing else.
fn name_key(artist: &str, title: &str) -> (String, String) {
    (artist.to_ascii_lowercase(), title.to_ascii_lowercase())
}

#[cfg(test)]
mod tests;
