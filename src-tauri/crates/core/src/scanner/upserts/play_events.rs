//! Re-attach the play events a vanished track left behind.

use sqlx::SqlitePool;

use crate::error::CoreResult;

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
///    tell a live version from the studio one.
///
/// Each step only claims events the previous one left behind, so a strong
/// match is never overwritten by a weaker one. Matching is restricted to
/// available tracks — an unavailable row is a file that vanished, and
/// attaching history to it would just orphan it again on the next pass.
///
/// Returns the number of events re-attached.
pub async fn reattach_orphaned_play_events(pool: &SqlitePool) -> CoreResult<u32> {
    // Cheap probe first: on a healthy library there are no orphans at all,
    // and this keeps the common case to one indexed lookup instead of
    // three correlated UPDATEs.
    let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM play_event WHERE track_id IS NULL")
        .fetch_one(pool)
        .await?;
    if orphans == 0 {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    let mut reattached: u64 = 0;

    // `ORDER BY t.id` rather than a bare LIMIT 1 so a duplicate file
    // resolves to the same track on every run — otherwise the history
    // could hop between copies from one scan to the next.
    for (key, sql) in [
        (
            "file_hash",
            "UPDATE play_event
                SET track_id = (
                    SELECT t.id FROM track t
                     WHERE t.file_hash = play_event.snapshot_hash
                       AND t.is_available = 1
                     ORDER BY t.id LIMIT 1
                )
              WHERE track_id IS NULL
                AND snapshot_hash IS NOT NULL
                AND EXISTS (
                    SELECT 1 FROM track t
                     WHERE t.file_hash = play_event.snapshot_hash
                       AND t.is_available = 1
                )",
        ),
        (
            "file_path",
            "UPDATE play_event
                SET track_id = (
                    SELECT t.id FROM track t
                     WHERE t.file_path = play_event.snapshot_path
                       AND t.is_available = 1
                     ORDER BY t.id LIMIT 1
                )
              WHERE track_id IS NULL
                AND snapshot_path IS NOT NULL
                AND EXISTS (
                    SELECT 1 FROM track t
                     WHERE t.file_path = play_event.snapshot_path
                       AND t.is_available = 1
                )",
        ),
        (
            "artist_title",
            "UPDATE play_event
                SET track_id = (
                    SELECT t.id FROM track t
                     LEFT JOIN artist ar ON ar.id = t.primary_artist
                     WHERE t.title = play_event.snapshot_title COLLATE NOCASE
                       AND ar.name = play_event.snapshot_artist COLLATE NOCASE
                       AND t.is_available = 1
                     ORDER BY t.id LIMIT 1
                )
              WHERE track_id IS NULL
                AND snapshot_title IS NOT NULL
                AND snapshot_artist IS NOT NULL
                AND EXISTS (
                    SELECT 1 FROM track t
                     LEFT JOIN artist ar ON ar.id = t.primary_artist
                     WHERE t.title = play_event.snapshot_title COLLATE NOCASE
                       AND ar.name = play_event.snapshot_artist COLLATE NOCASE
                       AND t.is_available = 1
                )",
        ),
    ] {
        let affected = sqlx::query(sql).execute(&mut *tx).await?.rows_affected();
        if affected > 0 {
            tracing::info!(key, affected, "re-attached orphaned play events");
        }
        reattached += affected;
    }

    tx.commit().await?;
    Ok(reattached.min(u32::MAX as u64) as u32)
}

#[cfg(test)]
mod tests;
