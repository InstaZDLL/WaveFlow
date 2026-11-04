//! Recognising a file that moved: a row whose file is gone and a new file
//! with the same bytes are one track, which keeps its rating, its likes, its
//! plays and its places in playlists instead of starting over.
//!
//! Only when there is one way to read it. Two copies of a vanished file, or
//! two vanished rows and one file, could be either; they stay what a scan
//! makes of them without this, new rows beside a hidden one, which loses
//! nothing and can still be put right by hand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use waveflow_core::network_locator::is_locator;
use waveflow_core::scanner::{hash_file, hash_file_full};

use super::walk::drive_is_away;
use crate::error::{AppError, AppResult};

/// A file found at a new place, and the row that follows it there.
pub(super) struct MovedFile {
    pub(super) track_id: i64,
    pub(super) old_path: String,
    pub(super) new_path: String,
    pub(super) file_modified: i64,
    pub(super) file_hash: String,
    pub(super) added_at: i64,
    pub(super) album_id: Option<i64>,
}

/// A row that might be the old place of a new file.
#[derive(Clone)]
struct Candidate {
    track_id: i64,
    path: String,
    size: i64,
    hash: String,
    file_modified: i64,
    added_at: i64,
    album_id: Option<i64>,
    /// The library folder holding it. A folder that does not answer says
    /// nothing about its files.
    root: String,
    /// Was that folder a mount point when it last held files?
    was_mount: bool,
}

/// Point the rows of the files in `new_paths` at them, and say which moved.
///
/// `new_paths` are the files this scan found with no row. The same library
/// is searched and no further, every folder of it: a move between two folders
/// is seen whichever is scanned first, so a candidate may still be marked
/// available, its own folder not having been scanned yet. What makes it one
/// is that its file is gone, asked of the disk.
///
/// Run before the extraction, so the file then goes down the ordinary path
/// for a row that exists: the unchanged branch, or the rewriting one when the
/// row holds the editor's hash, each of which keeps what the user set.
pub(super) async fn relink_moved_files(
    pool: &SqlitePool,
    library_id: i64,
    folder_id: i64,
    new_paths: Vec<PathBuf>,
) -> AppResult<Vec<MovedFile>> {
    let found = tokio::task::spawn_blocking(move || with_sizes(new_paths))
        .await
        .map_err(|e| AppError::Other(format!("move detection failed: {e}")))?;
    if found.is_empty() {
        return Ok(Vec::new());
    }

    // SQLite caps bound parameters at 999, as for the scan's probe.
    const SIZE_CHUNK: usize = 900;
    let mut sizes: Vec<i64> = found.iter().map(|(_, size)| *size).collect();
    sizes.sort_unstable();
    sizes.dedup();
    let mut candidates: Vec<Candidate> = Vec::new();
    for chunk in sizes.chunks(SIZE_CHUNK) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT t.id, t.file_path, t.file_size, t.file_hash, t.file_modified, t.added_at,
                    t.album_id, f.path, m.value
               FROM track t
               JOIN library_folder f ON f.id = t.folder_id
               LEFT JOIN profile_setting m ON m.key = 'scan.root_was_mount.' || f.id
              WHERE t.library_id = ? AND t.file_size IN ({placeholders})"
        );
        type Row = (
            i64,
            String,
            i64,
            String,
            i64,
            i64,
            Option<i64>,
            String,
            Option<String>,
        );
        let mut query = sqlx::query_as::<_, Row>(sqlx::AssertSqlSafe(sql)).bind(library_id);
        for size in chunk {
            query = query.bind(size);
        }
        for (track_id, path, size, hash, file_modified, added_at, album_id, root, was_mount) in
            query.fetch_all(pool).await?
        {
            // A share's file is not on this disk by construction, which
            // would read as gone.
            if !is_locator(&path) && !is_locator(&root) {
                candidates.push(Candidate {
                    track_id,
                    path,
                    size,
                    hash,
                    file_modified,
                    added_at,
                    album_id,
                    root,
                    was_mount: was_mount.as_deref() == Some("1"),
                });
            }
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let moved = tokio::task::spawn_blocking(move || match_moves(found, candidates))
        .await
        .map_err(|e| AppError::Other(format!("move detection failed: {e}")))?;
    if moved.is_empty() {
        return Ok(moved);
    }

    // Only from where it was read. Nothing stops two folders being scanned at
    // once, and a row the other scan has just taken is not this one's: its file
    // is then a new track, as it would be without any of this.
    let mut tx = pool.begin().await?;
    let mut relinked = Vec::with_capacity(moved.len());
    for file in moved {
        let done = sqlx::query(
            "UPDATE track SET file_path = ?, folder_id = ?, is_available = 1
              WHERE id = ? AND file_path = ?",
        )
        .bind(&file.new_path)
        .bind(folder_id)
        .bind(file.track_id)
        .bind(&file.old_path)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 1 {
            relinked.push(file);
        }
    }
    tx.commit().await?;
    let moved = relinked;
    for file in &moved {
        tracing::info!(
            track_id = file.track_id,
            old_path = %file.old_path,
            new_path = %file.new_path,
            "a file moved; its row follows it"
        );
    }
    Ok(moved)
}

fn with_sizes(paths: Vec<PathBuf>) -> Vec<(PathBuf, i64)> {
    paths
        .into_iter()
        .filter_map(|path| {
            let size = std::fs::metadata(&path).ok()?.len() as i64;
            Some((path, size))
        })
        .collect()
}

/// Is the file a candidate's row points at really gone, as far as anything can
/// tell from here? Not when its folder does not answer, or is the empty mount
/// point of a drive that is away, and not when the question itself fails: that
/// is a file out of reach, which comes back. A
/// path that still resolves may be the new file itself, renamed in case alone
/// on a file system that ignores it.
fn is_gone(
    candidate: &Candidate,
    new_path: &Path,
    root_answers: &mut HashMap<String, bool>,
) -> bool {
    let answers = *root_answers
        .entry(candidate.root.clone())
        .or_insert_with(|| {
            let root = Path::new(&candidate.root);
            root.is_dir() && !drive_is_away(root, candidate.was_mount)
        });
    if !answers {
        return false;
    }
    match Path::new(&candidate.path).try_exists() {
        Ok(false) => true,
        Ok(true) => same_file(Path::new(&candidate.path), new_path),
        Err(_) => false,
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The pairs with one reading: a file only one gone row could be, and a row
/// only one file could be.
///
/// `file_hash` holds one of two digests and nothing says which: the scanner's
/// over the length, the head and the tail, or the editor's (and an older
/// scanner's) over the whole file. Each is computed from the new file for
/// what it is, never one compared against the other. The whole-file one reads
/// every byte, so it waits until a row of that size still has not matched.
fn match_moves(found: Vec<(PathBuf, i64)>, candidates: Vec<Candidate>) -> Vec<MovedFile> {
    let mut by_size: HashMap<i64, Vec<&Candidate>> = HashMap::new();
    for candidate in &candidates {
        by_size.entry(candidate.size).or_default().push(candidate);
    }
    let mut root_answers: HashMap<String, bool> = HashMap::new();
    let mut claims: HashMap<i64, usize> = HashMap::new();
    let mut matches: Vec<(String, Vec<&Candidate>)> = Vec::new();
    for (path, size) in &found {
        let gone: Vec<&Candidate> = by_size
            .get(size)
            .into_iter()
            .flatten()
            .copied()
            .filter(|candidate| is_gone(candidate, path, &mut root_answers))
            .collect();
        if gone.is_empty() {
            continue;
        }
        let Ok(scanner) = hash_file(path) else {
            continue;
        };
        let whole = if gone.iter().any(|c| c.hash != scanner) {
            hash_file_full(path).ok()
        } else {
            None
        };
        let same: Vec<&Candidate> = gone
            .into_iter()
            .filter(|c| c.hash == scanner || whole.as_deref() == Some(c.hash.as_str()))
            .collect();
        for candidate in &same {
            *claims.entry(candidate.track_id).or_default() += 1;
        }
        matches.push((path.to_string_lossy().into_owned(), same));
    }

    matches
        .into_iter()
        .filter_map(|(new_path, same)| match same.as_slice() {
            [one] if claims.get(&one.track_id) == Some(&1) => Some(MovedFile {
                track_id: one.track_id,
                old_path: one.path.clone(),
                new_path,
                file_modified: one.file_modified,
                file_hash: one.hash.clone(),
                added_at: one.added_at,
                album_id: one.album_id,
            }),
            _ => None,
        })
        .collect()
}
