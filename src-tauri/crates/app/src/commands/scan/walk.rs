//! Walking a library folder for the files a scan reads.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use waveflow_core::scanner::is_scannable_audio;

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
    collect_scannable_files_with(root, |_, _| {}, || false).unwrap_or_default()
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
/// `None` when stopped. A partial list is not returned on purpose: the
/// scan treats the list as the folder's contents, and every track missing
/// from it would be swept as gone.
pub(super) fn collect_scannable_files_with(
    root: &Path,
    mut on_progress: impl FnMut(usize, &Path),
    should_stop: impl Fn() -> bool,
) -> Option<Vec<PathBuf>> {
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !waveflow_core::scanner::is_reserved_dir_name(entry.file_name())
        });
    let mut found = Vec::new();
    for (visited, entry) in walker.enumerate() {
        let periodic = visited % LISTING_CHECK_EVERY == 0;
        // Before every directory as well: yielding one is what makes the
        // walk open it next, and opening a directory is the slow part on
        // a share — a stop must not wait out one more of them.
        let is_dir = entry.as_ref().is_ok_and(|e| e.file_type().is_dir());
        if (periodic || is_dir) && should_stop() {
            return None;
        }
        if periodic {
            if let Ok(entry) = &entry {
                let dir = if entry.file_type().is_dir() {
                    entry.path()
                } else {
                    entry.path().parent().unwrap_or(root)
                };
                on_progress(found.len(), dir);
            }
        }
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() && is_scannable_audio(entry.path()) {
            found.push(entry.path().to_path_buf());
        }
    }
    Some(found)
}
