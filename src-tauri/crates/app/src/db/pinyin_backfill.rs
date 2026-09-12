//! Fill `pinyin` for the rows that predate the column (#579).
//!
//! The migration adds the column; it cannot fill it, because pinyin is
//! not derivable in SQL. New rows get theirs from the scanner and the
//! tag editor, so this pass exists for one reason: a library that was
//! already scanned when the feature landed. It runs once per profile
//! database and then finds nothing on every later launch.
//!
//! **`NULL` means "not computed", `''` means "computed, nothing to
//! romanise".** Writing the empty string is what makes the pass
//! terminate: most libraries hold no Han characters at all, and a sweep
//! keyed on "the blob is empty" would re-read every row on every
//! launch, forever.
//!
//! It is a background writer, so it follows the shape the invariant
//! requires of one: **park behind the scan, batch, retry on
//! `SQLITE_BUSY`**. Giving up is safe — the rows it did not reach are
//! still `NULL`, and the next launch picks them up.

use sqlx::SqlitePool;
use waveflow_core::scanner::pinyin_blob;

/// Rows per transaction. Small enough that the scanner never waits long
/// behind one, large enough that a big library is not thousands of
/// commits.
const BATCH: i64 = 500;

/// How long to wait before looking again while a scan holds the writer.
const PARK: std::time::Duration = std::time::Duration::from_millis(500);

/// Retries for one batch before this launch gives up on it.
const MAX_ATTEMPTS: usize = 4;

/// The three tables that carry a pinyin blob, with the text it is
/// derived from. Each is written where its `canonical_*` sibling is
/// written; this fills in the history.
const TABLES: [(&str, &str); 3] = [("track", "title"), ("album", "title"), ("artist", "name")];

/// Fill every `NULL` pinyin in this profile. Returns how many rows were
/// written, for the log line.
pub async fn run(pool: &SqlitePool) -> usize {
    let mut written = 0_usize;
    for (table, column) in TABLES {
        match fill_table(pool, table, column).await {
            Ok(count) => written += count,
            Err(err) => {
                // Not fatal, and not worth a user-facing error: search
                // still works on the text, and the next launch resumes
                // exactly where this one stopped.
                tracing::warn!(%err, table, "pinyin backfill stopped early");
                break;
            }
        }
    }
    if written > 0 {
        tracing::info!(rows = written, "pinyin backfill complete");
    }
    written
}

async fn fill_table(pool: &SqlitePool, table: &str, column: &str) -> Result<usize, sqlx::Error> {
    let select = format!("SELECT id, {column} FROM {table} WHERE pinyin IS NULL LIMIT ?");
    let update = format!("UPDATE {table} SET pinyin = ? WHERE id = ?");
    let mut written = 0_usize;

    loop {
        park_behind_scan().await;

        let rows: Vec<(i64, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(select.clone()))
            .bind(BATCH)
            .fetch_all(pool)
            .await?;
        if rows.is_empty() {
            return Ok(written);
        }

        let blobs: Vec<(i64, String)> = rows
            .into_iter()
            .map(|(id, text)| {
                // `unwrap_or_default` is the `''` marker: this row has
                // been looked at and holds nothing to romanise.
                let blob = text.as_deref().and_then(pinyin_blob).unwrap_or_default();
                (id, blob)
            })
            .collect();

        write_batch(pool, &update, &blobs).await?;
        written += blobs.len();
    }
}

/// Wait while a scan holds the single writer. The scanner is the one
/// pass a user is watching; this one has all the time in the world.
async fn park_behind_scan() {
    while crate::commands::scan::scan_in_flight() {
        tokio::time::sleep(PARK).await;
    }
}

/// One batch in one transaction, retried on a busy collision.
///
/// A `SQLITE_BUSY` here means somebody else is writing — a scan that
/// started between the park above and this commit, a play event — not a
/// schema fault, so it is worth waiting out. Exhausting the budget
/// returns the error, and the caller stops for this launch.
async fn write_batch(
    pool: &SqlitePool,
    update: &str,
    blobs: &[(i64, String)],
) -> Result<(), sqlx::Error> {
    let mut backoff = std::time::Duration::from_millis(50);
    for attempt in 1..=MAX_ATTEMPTS {
        match write_batch_once(pool, update, blobs).await {
            Ok(()) => return Ok(()),
            Err(err) if is_busy(&err) && attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("the loop returns on its last attempt")
}

async fn write_batch_once(
    pool: &SqlitePool,
    update: &str,
    blobs: &[(i64, String)],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for (id, blob) in blobs {
        sqlx::query(sqlx::AssertSqlSafe(update.to_owned()))
            .bind(blob)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Busy / locked, in any of SQLite's flavours: the primary result code
/// lives in the low byte, the high bits carry the extended detail.
fn is_busy(err: &sqlx::Error) -> bool {
    let sqlx::Error::Database(db) = err else {
        return false;
    };
    db.code()
        .and_then(|code| code.parse::<i32>().ok())
        .is_some_and(|code| matches!(code & 0xff, 5 | 6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_tells_computed_from_uncomputed() {
        // The whole reason the empty string is written rather than left
        // NULL: without it a Latin library re-reads every row on every
        // launch, because "no pinyin" and "not looked at yet" would be
        // the same value.
        assert_eq!(pinyin_blob("Dark Side"), None);
        let stored = pinyin_blob("Dark Side").unwrap_or_default();
        assert_eq!(stored, "");
    }

    #[test]
    fn every_table_carries_the_column_it_is_swept_on() {
        // The pairs are the ones the migration added `pinyin` to.
        assert_eq!(TABLES.len(), 3);
        assert!(TABLES.iter().any(|(t, c)| *t == "track" && *c == "title"));
        assert!(TABLES.iter().any(|(t, c)| *t == "artist" && *c == "name"));
    }
}
