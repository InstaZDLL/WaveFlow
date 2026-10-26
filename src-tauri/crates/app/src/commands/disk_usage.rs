//! What WaveFlow keeps on disk, item by item, for Settings → Storage.
//!
//! One figure for "the caches" (`storage::get_cache_location`) told the
//! user how much, not what — and not which part could go. Here each item
//! is measured apart and says whether it rebuilds itself:
//!
//! - **rebuildable** — a cache filled from somewhere else, emptied without
//!   losing anything: the animated-cover and Canvas caches (plugins fetch
//!   them again) and, for a server account, its cover thumbnails and the
//!   streams kept from playing (the server still has them). Settings
//!   offers to empty these, through the command each already had;
//! - **kept** — what the user would have to redo: the library's own
//!   covers (the database points at them), downloaded artist pictures,
//!   the clips and covers set by hand, the offline copies asked of a
//!   server, the databases and the copies taken before an upgrade, the
//!   backups, the logs, the plugins. Shown, with no button: the place to
//!   act on each is its own card, where the consequence is explained.
//!
//! Per-profile items are the active profile's; shared ones cover every
//! profile. Measured on the blocking pool — a populated artwork tree is
//! tens of thousands of files — and only when asked.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{error::AppResult, state::AppState};

/// One item on the Storage page.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageEntry {
    /// Stable id; the frontend names it and knows how to empty it.
    pub id: &'static str,
    pub bytes: u64,
    pub files: u64,
    /// Emptied without loss: filled again from where it came from.
    pub rebuildable: bool,
    /// A folder that exists could not be read whole (permissions, a
    /// drive gone mid-walk): the figure is a floor, not the size.
    pub incomplete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskUsage {
    pub entries: Vec<UsageEntry>,
    pub total_bytes: u64,
}

/// Bytes and files under `root`, those `keep` accepts, in its
/// subdirectories too when `recursive`. Symlinks are not followed: a link
/// into the music library would count the library.
///
/// The third value is false when a folder that exists could not be read:
/// a cache that is not there yet is empty, one that cannot be listed is
/// not.
fn walk(root: &Path, keep: &dyn Fn(&Path) -> bool, recursive: bool) -> (u64, u64, bool) {
    let (mut bytes, mut files, mut complete) = (0u64, 0u64, true);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    complete = false;
                }
                continue;
            }
        };
        // Any entry that cannot be read makes the figure a floor; one gone
        // between the listing and the read is a cache doing its job.
        let mut missed = |err: std::io::Error| {
            if err.kind() != std::io::ErrorKind::NotFound {
                complete = false;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    missed(err);
                    continue;
                }
            };
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => {
                    if recursive {
                        stack.push(entry.path());
                    }
                }
                Ok(ft) if ft.is_file() => {
                    if !keep(&entry.path()) {
                        continue;
                    }
                    match entry.metadata() {
                        Ok(meta) => {
                            bytes = bytes.saturating_add(meta.len());
                            files += 1;
                        }
                        Err(err) => missed(err),
                    }
                }
                Ok(_) => {}
                Err(err) => missed(err),
            }
        }
    }
    (bytes, files, complete)
}

fn everything(_: &Path) -> bool {
    true
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// A database or one of its journals: `app.db`, `data.db-wal`, …
fn is_database(path: &Path) -> bool {
    let name = file_name(path);
    ["app.db", "data.db"].iter().any(|db| {
        name == *db
            || name
                .strip_prefix(db)
                .is_some_and(|rest| rest.starts_with('-'))
    })
}

/// A copy taken before a schema upgrade: `<db>.pre-<version>.bak`
/// (`db::pre_migration_backup`).
fn is_database_snapshot(path: &Path) -> bool {
    let name = file_name(path);
    (name.starts_with("app.db.pre-") || name.starts_with("data.db.pre-")) && name.ends_with(".bak")
}

/// A finished backup archive or one being written (`backup.rs`).
fn is_backup_archive(path: &Path) -> bool {
    let name = file_name(path);
    name.ends_with(".waveflow")
        || name
            .strip_suffix(waveflow_core::atomic_write::PARTIAL_SUFFIX)
            .is_some_and(|archive| archive.ends_with(".waveflow"))
}

/// One item and where it lives, resolved before the walk so the walk
/// itself needs nothing but paths.
struct Item {
    id: &'static str,
    roots: Vec<PathBuf>,
    keep: fn(&Path) -> bool,
    /// Whole trees for the caches; one level for the databases and the
    /// backups, which sit side by side in a folder — the backup folder
    /// can be the user's Documents, and a walk of that would count it.
    recursive: bool,
    rebuildable: bool,
}

impl Item {
    fn tree(id: &'static str, roots: Vec<PathBuf>, rebuildable: bool) -> Self {
        Self {
            id,
            roots,
            keep: everything,
            recursive: true,
            rebuildable,
        }
    }

    fn files_in(id: &'static str, roots: Vec<PathBuf>, keep: fn(&Path) -> bool) -> Self {
        Self {
            id,
            roots,
            keep,
            recursive: false,
            rebuildable: false,
        }
    }
}

fn measure(items: Vec<Item>) -> DiskUsage {
    let entries: Vec<UsageEntry> = items
        .into_iter()
        .map(|item| {
            let (mut bytes, mut files, mut complete) = (0u64, 0u64, true);
            for root in &item.roots {
                let (b, f, c) = walk(root, &item.keep, item.recursive);
                bytes = bytes.saturating_add(b);
                files += f;
                complete &= c;
            }
            UsageEntry {
                id: item.id,
                bytes,
                files,
                rebuildable: item.rebuildable,
                incomplete: !complete,
            }
        })
        .collect();
    let total_bytes = entries.iter().map(|e| e.bytes).sum();
    DiskUsage {
        entries,
        total_bytes,
    }
}

/// Measure every item. A few seconds at worst on a large library.
#[tauri::command]
pub async fn disk_usage(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<DiskUsage> {
    let paths = state.paths.clone();
    let profile = state
        .require_profile_snapshot()
        .await
        .ok()
        .map(|(_, id)| id);
    let backup_folder = crate::backup::read_config(&state, &app)
        .await
        .ok()
        .map(|c| {
            if c.folder.trim().is_empty() {
                PathBuf::from(c.default_folder)
            } else {
                PathBuf::from(c.folder)
            }
        });
    let plugins = paths.plugin_paths();

    // The databases sit in the app-data root (`app.db`) and in each
    // profile's folder (`data.db`), with their pre-upgrade copies.
    let mut db_dirs = vec![paths.root.clone()];
    if let Ok(entries) = std::fs::read_dir(&paths.profiles_dir) {
        db_dirs.extend(
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.path()),
        );
    }

    let mut items: Vec<Item> = Vec::new();
    if let Some(id) = profile {
        items.push(Item::tree(
            "artwork",
            vec![paths.profile_artwork_dir(id)],
            false,
        ));
    }
    items.push(Item::tree(
        "metadataArtwork",
        vec![paths.metadata_artwork_dir.clone()],
        false,
    ));
    items.push(Item::tree(
        "motionCache",
        vec![paths.motion_cache_dir.clone()],
        true,
    ));
    items.push(Item::tree(
        "canvasCache",
        vec![paths.canvas_cache_dir.clone()],
        true,
    ));
    if let Some(id) = profile {
        #[cfg(feature = "sync_v2")]
        {
            items.push(Item::tree(
                "remoteArtwork",
                vec![paths.profile_remote_artwork_dir(id)],
                true,
            ));
            items.push(Item::tree(
                "remoteStream",
                vec![paths.profile_remote_stream_dir(id)],
                true,
            ));
            items.push(Item::tree(
                "remoteDownloads",
                vec![paths.profile_remote_download_dir(id)],
                false,
            ));
        }
        items.push(Item::tree(
            "manualMedia",
            vec![paths.profile_motion_dir(id), paths.profile_canvas_dir(id)],
            false,
        ));
    }
    items.push(Item::files_in("databases", db_dirs.clone(), is_database));
    items.push(Item::files_in(
        "databaseSnapshots",
        db_dirs,
        is_database_snapshot,
    ));
    if let Some(folder) = backup_folder {
        items.push(Item::files_in("backups", vec![folder], is_backup_archive));
    }
    if let Some(dir) = crate::logging::log_dir() {
        items.push(Item::tree("logs", vec![dir.clone()], false));
    }
    items.push(Item::tree(
        "plugins",
        vec![plugins.plugins_root.clone(), plugins.data_root.clone()],
        false,
    ));

    let usage = tokio::task::spawn_blocking(move || measure(items))
        .await
        .map_err(|e| crate::error::AppError::Other(format!("disk usage task failed: {e}")))?;
    Ok(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, len: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; len]).unwrap();
    }

    #[test]
    fn databases_snapshots_and_backups_are_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("app.db"), 10);
        write(&root.join("app.db-wal"), 5);
        write(&root.join("app.db.pre-20260101000000.bak"), 7);
        write(&root.join("profiles/1/data.db"), 20);
        write(&root.join("profiles/1/data.db-shm"), 3);
        write(&root.join("profiles/1/data.db.pre-20260101000000.bak"), 11);
        write(&root.join("profiles/1/artwork/cover.jpg"), 100);
        write(&root.join("backups/Main-2026.waveflow"), 50);
        write(&root.join("backups/notes.txt"), 9);

        let db_dirs = vec![root.to_path_buf(), root.join("profiles/1")];
        let usage = measure(vec![
            Item::files_in("databases", db_dirs.clone(), is_database),
            Item::files_in("snapshots", db_dirs, is_database_snapshot),
            Item::files_in("backups", vec![root.join("backups")], is_backup_archive),
            Item::tree("cache", vec![root.join("missing")], true),
            Item::tree("artwork", vec![root.join("profiles")], false),
        ]);
        let get = |id: &str| usage.entries.iter().find(|e| e.id == id).unwrap();
        assert_eq!((get("databases").bytes, get("databases").files), (38, 4));
        assert_eq!((get("snapshots").bytes, get("snapshots").files), (18, 2));
        // A file of the user's beside the archives is not counted.
        assert_eq!((get("backups").bytes, get("backups").files), (50, 1));
        // A directory that is not there is empty, not an error.
        assert_eq!((get("cache").bytes, get("cache").files), (0, 0));
        assert!(!get("cache").incomplete);
        // A whole tree, by contrast, counts everything under it.
        assert_eq!(get("artwork").files, 4);
        assert_eq!(usage.total_bytes, 38 + 18 + 50 + (20 + 3 + 11 + 100));
    }
}
