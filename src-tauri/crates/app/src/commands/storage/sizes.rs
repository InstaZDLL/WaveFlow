//! Measuring the caches: their folders, sizes and file counts.

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::{error::AppResult, paths::AppPaths};

/// Recursive byte total, ignoring anything unreadable.
///
/// Iterative rather than recursive so a pathological tree cannot blow
/// the stack, and errors are skipped rather than propagated: this feeds
/// a number in a settings card, and one unreadable file should not turn
/// the whole reading into an error message.
fn dir_size(root: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(ft) if ft.is_file() => {
                    if let Ok(meta) = entry.metadata() {
                        total = total.saturating_add(meta.len());
                    }
                }
                // Symlinks are not followed: a link into the music
                // library would report the library's size as cache.
                _ => {}
            }
        }
    }
    total
}

/// Total bytes of a set of directories, off the async runtime.
///
/// A populated artwork tree is tens of thousands of small files, and
/// walking it in place would hold the runtime for the whole read —
/// which on the Settings page means every other command queued behind
/// a number nobody is waiting on.
pub(super) async fn measure(dirs: Vec<(String, PathBuf)>) -> u64 {
    tokio::task::spawn_blocking(move || dirs.iter().map(|(_, path)| dir_size(path)).sum::<u64>())
        .await
        .unwrap_or(0)
}

/// Every cache directory that currently exists, app-wide and per
/// profile.
///
/// Built from [`AppPaths`] rather than from a second hand-written list,
/// so a directory added to the layout cannot be left out of the move or
/// the size reading.
/// The profile enumeration is **propagated**, not swallowed: a move
/// that silently saw no profiles would copy the shared caches, persist
/// the new location, and let the startup pass delete every profile's
/// artwork from the old one — none of it ever having been copied.
pub(super) async fn cache_dirs(
    paths: &AppPaths,
    app_db: &SqlitePool,
) -> AppResult<Vec<(String, PathBuf)>> {
    let mut dirs: Vec<(String, PathBuf)> = paths
        .shared_cache_dirs()
        .iter()
        .map(|(name, path)| ((*name).to_string(), (*path).clone()))
        .collect();

    let profile_ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM profile ORDER BY id")
        .fetch_all(app_db)
        .await?;

    for id in profile_ids {
        let rel = format!("profiles/{id}");
        dirs.push((format!("{rel}/artwork"), paths.profile_artwork_dir(id)));
        dirs.push((
            format!("{rel}/remote-artwork"),
            paths.profile_remote_artwork_dir(id),
        ));
        #[cfg(feature = "sync_v2")]
        dirs.push((
            format!("{rel}/remote-stream"),
            paths.profile_remote_stream_dir(id),
        ));
    }
    Ok(dirs)
}

/// Files under `root`, counted the way [`copy_tree`] copies them: regular
/// files, symlinks left out. What a move's progress is measured against.
pub(super) fn count_files(root: &Path) -> u64 {
    let mut files = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => stack.push(entry.path()),
                Ok(ft) if ft.is_file() => files += 1,
                _ => {}
            }
        }
    }
    files
}
