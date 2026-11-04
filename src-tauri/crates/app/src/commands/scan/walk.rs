//! Walking a library folder for the files a scan reads.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use waveflow_core::scanner::folders::{relation, FolderRelation};
use waveflow_core::scanner::is_scannable_audio;

/// What a walk of a library folder found.
pub(super) struct FolderWalk {
    pub(super) files: Vec<PathBuf>,
    /// Where the walk could not look: a directory it may not enter, or the
    /// root itself when the error names no path. Says nothing about the
    /// files under it, so the scan's sweep leaves them alone.
    pub(super) unreadable: Vec<PathBuf>,
}

/// Did a walk that could not look in `unreadable` have no way to know about
/// the file at `path`?
pub(super) fn missed(unreadable: &[PathBuf], path: &Path) -> bool {
    unreadable.iter().any(|dir| {
        matches!(
            relation(path, dir),
            Some(FolderRelation::Same | FolderRelation::Inside)
        )
    })
}

/// Is this folder a mount point, another device than its parent's? Always
/// `false` where the platform has no such notion: Windows names a missing
/// drive through the walk's own error.
pub(super) fn is_mount_point(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let device = |path: &Path| std::fs::metadata(path).ok().map(|meta| meta.dev());
        match (device(path), path.parent().and_then(device)) {
            (Some(own), Some(parent)) => own != parent,
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Is this folder the empty directory a drive left behind? It answers like any
/// other, so what gives it away is `was_mount`, what it was when it last held
/// files. The device number itself would not do: a drive plugged back in can
/// get another one. A folder that is a mount point now is whatever that drive
/// holds, however empty.
pub(super) fn drive_is_away(root: &Path, was_mount: bool) -> bool {
    was_mount && !is_mount_point(root)
}

/// Every file under `root` the library should index.
///
/// **The reserved directory is pruned as the walk meets it**, so the whole
/// branch is skipped rather than each file in it being tested and refused
/// -- and so WaveFlow's own media (issue #695) never reaches
/// `is_scannable_audio`, which would accept a Canvas clip on sight: `mp4`
/// is an audio extension here, legitimately, and only `ogg` / `oga` pay
/// for a header read before the scanner believes the extension.
///
/// The root is never pruned, whatever it is called: a library folder the
/// user happens to have named `.waveflow` is still their library, and
/// scanning none of it would be a defect with no symptom to follow.
#[cfg(test)]
pub(super) fn collect_scannable_files(root: &Path) -> Vec<PathBuf> {
    collect_scannable_files_with(root, |_, _| {}, || false)
        .ok()
        .flatten()
        .map(|walk| walk.files)
        .unwrap_or_default()
}

/// Entries the walk visits between two reports and two looks at the
/// stop flag. Small, because what is slow on a network share is opening
/// a directory, not the entries in it, so a handful of entries can stand
/// for a long wait; the reports themselves are throttled downstream.
const LISTING_CHECK_EVERY: usize = 32;

/// [`collect_scannable_files`], reporting as it goes and stoppable.
///
/// Listing used to be a silent phase: on a NAS the walk alone can take
/// minutes, and the scan's row sat at zero with no folder named while
/// its stop button did nothing, since nothing looked at it until the
/// list was complete. Every [`LISTING_CHECK_EVERY`] entries this hands
/// `on_progress` the count of files found so far and the directory it is
/// in, and asks `should_stop`.
///
/// `Ok(None)` when stopped. A partial list is not returned on purpose: the
/// scan treats the list as the folder's contents, and every track missing
/// from it would be swept as gone.
///
/// `Err`, with the reason, when the root itself cannot be read: a drive that is not plugged
/// in, a folder that was removed, a permission taken away. That is not an
/// empty folder, and answering with an empty list would hide every track of
/// it. A directory further down that cannot be read does not end the walk,
/// and is reported in [`FolderWalk::unreadable`].
pub(super) fn collect_scannable_files_with(
    root: &Path,
    mut on_progress: impl FnMut(usize, &Path),
    should_stop: impl Fn() -> bool,
) -> Result<Option<FolderWalk>, String> {
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !waveflow_core::scanner::is_reserved_dir_name(entry.file_name())
        });
    let mut walk = FolderWalk {
        files: Vec::new(),
        unreadable: Vec::new(),
    };
    for (visited, entry) in walker.enumerate() {
        let periodic = visited % LISTING_CHECK_EVERY == 0;
        // Before every directory as well: yielding one is what makes the
        // walk open it next, and opening a directory is the slow part on
        // a share — a stop must not wait out one more of them.
        let is_dir = entry.as_ref().is_ok_and(|e| e.file_type().is_dir());
        if (periodic || is_dir) && should_stop() {
            return Ok(None);
        }
        if periodic {
            if let Ok(entry) = &entry {
                let dir = if entry.file_type().is_dir() {
                    entry.path()
                } else {
                    entry.path().parent().unwrap_or(root)
                };
                on_progress(walk.files.len(), dir);
            }
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) if err.depth() == 0 => {
                return Err(err
                    .io_error()
                    .map_or_else(|| err.to_string(), ToString::to_string));
            }
            Err(err) => {
                tracing::debug!(error = %err, "could not read part of a library folder");
                // An error reading a directory's entries names no path, and
                // nothing says which one it was: the whole folder is unsure.
                walk.unreadable
                    .push(err.path().unwrap_or(root).to_path_buf());
                continue;
            }
        };
        // A root that is a plain file walks without an error, as one entry.
        // Asked of the path, since a root that is a link is not a directory
        // to `file_type`, and is walked all the same.
        if entry.depth() == 0 && !entry.path().is_dir() {
            return Err("it is not a folder".to_string());
        }
        if entry.file_type().is_file() && is_scannable_audio(entry.path()) {
            walk.files.push(entry.path().to_path_buf());
        }
    }
    Ok(Some(walk))
}
