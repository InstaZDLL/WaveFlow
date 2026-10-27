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
            // The profile was closed under the pass — the app quitting, or
            // a switch to another profile. That is the pass being told to
            // stop, not failing: the next time this profile opens, it
            // resumes on the rows still `NULL` (#763).
            Err(sqlx::Error::PoolClosed) => {
                tracing::debug!(table, "pinyin backfill stopped: profile closed");
                break;
            }
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

/// The write, as a function so a test can run the very statement the
/// pass runs.
///
/// `AND pinyin IS NULL` because this pass reads, then computes, then
/// writes: a scan or a tag edit landing in that gap has already written
/// a blob for a title this pass never saw, and an unguarded write would
/// replace it with the stale romanisation.
fn update_sql(table: &str) -> String {
    format!("UPDATE {table} SET pinyin = ? WHERE id = ? AND pinyin IS NULL")
}

async fn fill_table(pool: &SqlitePool, table: &str, column: &str) -> Result<usize, sqlx::Error> {
    let select = format!("SELECT id, {column} FROM {table} WHERE pinyin IS NULL LIMIT ?");
    let update = update_sql(table);
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

        // The count comes back from the database, not from the batch:
        // since the write is guarded on `pinyin IS NULL`, a row somebody
        // else filled in the meantime is skipped, and reporting it as
        // written would make the log line a guess.
        written += write_batch(pool, &update, &blobs).await? as usize;
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
) -> Result<u64, sqlx::Error> {
    let mut backoff = std::time::Duration::from_millis(50);
    for attempt in 1..=MAX_ATTEMPTS {
        match write_batch_once(pool, update, blobs).await {
            Ok(written) => return Ok(written),
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
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut written = 0_u64;
    for (id, blob) in blobs {
        written += sqlx::query(sqlx::AssertSqlSafe(update.to_owned()))
            .bind(blob)
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    tx.commit().await?;
    Ok(written)
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
mod tests;
