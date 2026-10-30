//! The workshop's journal: the batches it wrote and, for every field it
//! rewrote in a file, the value that field had before.
//!
//! A file is journaled **before** it is written — its old and new values,
//! status `pending` — and confirmed after, with the hash of the bytes the
//! write left. So an interrupted batch leaves a record of every file it
//! may have touched, and an undo can tell a file it wrote from one that
//! changed since: the hash no longer matches.
//!
//! Plain reads and writes on one connection; the caller owns the
//! transaction and the retry on a busy database.

use sqlx::SqliteConnection;

use crate::error::CoreResult;

use super::{Field, FieldDiff};

/// Where a journaled file stands. See the migration
/// `20261030180000_tag_workshop_journal.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Pending,
    Written,
    Failed,
    Undone,
}

impl FileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FileStatus::Pending => "pending",
            FileStatus::Written => "written",
            FileStatus::Failed => "failed",
            FileStatus::Undone => "undone",
        }
    }

    fn parse(value: &str) -> FileStatus {
        match value {
            "written" => FileStatus::Written,
            "failed" => FileStatus::Failed,
            "undone" => FileStatus::Undone,
            // An unknown value is treated as the least trusted one: an
            // undo refuses a pending file rather than guess.
            _ => FileStatus::Pending,
        }
    }
}

/// Open a batch. Returns its id.
pub async fn create_batch(
    conn: &mut SqliteConnection,
    label: &str,
    release_mbid: Option<&str>,
    now: i64,
) -> CoreResult<i64> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO tag_workshop_batch (created_at, label, release_mbid)
         VALUES (?, ?, ?)
         RETURNING id",
    )
    .bind(now)
    .bind(label)
    .bind(release_mbid)
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

/// Record that `path` is about to be written with `diffs`. Returns the
/// file's journal id, for [`mark_written`] / [`mark_failed`].
pub async fn journal_file(
    conn: &mut SqliteConnection,
    batch_id: i64,
    track_id: Option<i64>,
    path: &str,
    diffs: &[FieldDiff],
) -> CoreResult<i64> {
    let file_id: i64 = sqlx::query_scalar(
        "INSERT INTO tag_workshop_file (batch_id, track_id, file_path, status)
         VALUES (?, ?, ?, 'pending')
         RETURNING id",
    )
    .bind(batch_id)
    .bind(track_id)
    .bind(path)
    .fetch_one(&mut *conn)
    .await?;
    for diff in diffs {
        sqlx::query(
            "INSERT INTO tag_workshop_field (file_id, field, old_value, new_value)
             VALUES (?, ?, ?, ?)",
        )
        .bind(file_id)
        .bind(diff.field.as_str())
        .bind(Some(diff.old.as_str()).filter(|v| !v.is_empty()))
        .bind(&diff.new)
        .execute(&mut *conn)
        .await?;
    }
    Ok(file_id)
}

/// The write went through and left the file hashing to `hash_after`.
pub async fn mark_written(
    conn: &mut SqliteConnection,
    file_id: i64,
    hash_after: &str,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE tag_workshop_file SET status = 'written', hash_after = ?, error = NULL
          WHERE id = ?",
    )
    .bind(hash_after)
    .bind(file_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The write was refused; the file is as it was.
pub async fn mark_failed(conn: &mut SqliteConnection, file_id: i64, error: &str) -> CoreResult<()> {
    sqlx::query("UPDATE tag_workshop_file SET status = 'failed', error = ? WHERE id = ?")
        .bind(error)
        .bind(file_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The old values are back in the file.
pub async fn mark_undone(conn: &mut SqliteConnection, file_id: i64) -> CoreResult<()> {
    sqlx::query("UPDATE tag_workshop_file SET status = 'undone', error = NULL WHERE id = ?")
        .bind(file_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Close the undo of a batch: it counts as undone once none of its files
/// is still written or unconfirmed. `true` when it is.
pub async fn settle_undo(conn: &mut SqliteConnection, batch_id: i64, now: i64) -> CoreResult<bool> {
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tag_workshop_file
          WHERE batch_id = ? AND status IN ('written', 'pending')",
    )
    .bind(batch_id)
    .fetch_one(&mut *conn)
    .await?;
    if open > 0 {
        return Ok(false);
    }
    sqlx::query("UPDATE tag_workshop_batch SET undone_at = COALESCE(undone_at, ?) WHERE id = ?")
        .bind(now)
        .bind(batch_id)
        .execute(&mut *conn)
        .await?;
    Ok(true)
}

/// Drop a batch that wrote nothing: every write refused, or the batch
/// stopped before its first file. `true` when it was dropped.
pub async fn discard_if_empty(conn: &mut SqliteConnection, batch_id: i64) -> CoreResult<bool> {
    let kept: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tag_workshop_file
          WHERE batch_id = ? AND status IN ('written', 'pending')",
    )
    .bind(batch_id)
    .fetch_one(&mut *conn)
    .await?;
    if kept > 0 {
        return Ok(false);
    }
    sqlx::query("DELETE FROM tag_workshop_batch WHERE id = ?")
        .bind(batch_id)
        .execute(&mut *conn)
        .await?;
    Ok(true)
}

/// One batch, as the list of batches shows it.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct BatchSummary {
    pub id: i64,
    pub created_at: i64,
    pub label: String,
    pub release_mbid: Option<String>,
    pub undone_at: Option<i64>,
    pub files: i64,
    pub written: i64,
    pub pending: i64,
    pub failed: i64,
    pub undone: i64,
}

/// The newest batches first.
pub async fn batches(conn: &mut SqliteConnection, limit: i64) -> CoreResult<Vec<BatchSummary>> {
    Ok(sqlx::query_as(
        "SELECT b.id, b.created_at, b.label, b.release_mbid, b.undone_at,
                COUNT(f.id) AS files,
                COALESCE(SUM(f.status = 'written'), 0) AS written,
                COALESCE(SUM(f.status = 'pending'), 0) AS pending,
                COALESCE(SUM(f.status = 'failed'), 0) AS failed,
                COALESCE(SUM(f.status = 'undone'), 0) AS undone
           FROM tag_workshop_batch b
           LEFT JOIN tag_workshop_file f ON f.batch_id = b.id
          GROUP BY b.id
          ORDER BY b.created_at DESC, b.id DESC
          LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?)
}

/// One field a journaled file had rewritten.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JournaledField {
    pub field: Field,
    /// `None` when the file did not carry the field.
    pub old: Option<String>,
    pub new: String,
}

/// One journaled file and what was written into it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JournaledFile {
    pub id: i64,
    pub track_id: Option<i64>,
    pub file_path: String,
    pub status: FileStatus,
    pub hash_after: Option<String>,
    pub error: Option<String>,
    pub fields: Vec<JournaledField>,
}

/// Every file of a batch, in the order it was written.
pub async fn batch_files(
    conn: &mut SqliteConnection,
    batch_id: i64,
) -> CoreResult<Vec<JournaledFile>> {
    type FileRow = (
        i64,
        Option<i64>,
        String,
        String,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<FileRow> = sqlx::query_as(
        "SELECT id, track_id, file_path, status, hash_after, error
           FROM tag_workshop_file
          WHERE batch_id = ?
          ORDER BY id",
    )
    .bind(batch_id)
    .fetch_all(&mut *conn)
    .await?;
    let fields: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(
        "SELECT fd.file_id, fd.field, fd.old_value, fd.new_value
           FROM tag_workshop_field fd
           JOIN tag_workshop_file f ON f.id = fd.file_id
          WHERE f.batch_id = ?",
    )
    .bind(batch_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out: Vec<JournaledFile> = rows
        .into_iter()
        .map(
            |(id, track_id, file_path, status, hash_after, error)| JournaledFile {
                id,
                track_id,
                file_path,
                status: FileStatus::parse(&status),
                hash_after,
                error,
                fields: Vec::new(),
            },
        )
        .collect();
    let index: std::collections::HashMap<i64, usize> =
        out.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
    for (file_id, field, old, new) in fields {
        // A field name this build does not know came from a newer one;
        // leaving it out of an undo is safer than writing it blind.
        let Some(field) = Field::parse(&field) else {
            continue;
        };
        if let Some(&at) = index.get(&file_id) {
            out[at].fields.push(JournaledField { field, old, new });
        }
    }
    for file in &mut out {
        file.fields.sort_by_key(|f| f.field);
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
