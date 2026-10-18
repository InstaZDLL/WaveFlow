//! Replace a file only once its new content is complete.
//!
//! For files WaveFlow writes whole — a profile archive, a playlist
//! export — where the previous copy is worth more than a half-written
//! new one. Writing straight to the destination truncates it first, so
//! a full disk, a share that drops or a crash halfway leaves the user
//! with neither: an export over last week's archive destroys last
//! week's archive, and an automatic backup cut short sits in the backup
//! folder looking like the newest good one.
//!
//! Here the bytes go to `<name>.partial` beside the destination, reach
//! the disk, and only then replace it in a single `rename`. Any failure
//! removes the partial and leaves the destination as it was.
//!
//! Not for audio files: those go through [`crate::tagio`], which has to
//! keep the original's inode where it can and copies it before writing.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Appended to the destination's file name while it is being written.
/// Anything that lists a folder for finished files (the backup
/// retention, for one) must not take a name ending in it as one.
pub const PARTIAL_SUFFIX: &str = ".partial";

/// `<destination file name>.partial`, in the destination's directory.
///
/// Beside it, never in the system temp directory: `rename` is only
/// atomic within a filesystem, and an export folder is routinely on a
/// different one than `/tmp`.
pub fn partial_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(PARTIAL_SUFFIX);
    target.with_file_name(name)
}

/// Removes the partial unless it was renamed into place, so every early
/// return between creating it and committing it cleans up after itself.
struct Partial {
    path: PathBuf,
    committed: bool,
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Write `target` through a sibling partial file.
///
/// `body` writes the complete new content into the handle it is given
/// (opened for reading and writing, empty, positioned at the start). The
/// content is flushed to the disk, then renamed over `target`. When
/// `body` or any step after it fails, `target` is untouched — still
/// absent, or still holding what it held before — and the partial is
/// gone.
///
/// The parent directory is created if missing. Blocking: call it from a
/// blocking thread, never an async worker.
pub fn write_with<T, E>(target: &Path, body: impl FnOnce(&mut File) -> Result<T, E>) -> Result<T, E>
where
    E: From<io::Error>,
{
    let directory = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory).map_err(E::from)?;

    let mut partial = Partial {
        path: partial_path(target),
        committed: false,
    };
    // `create` rather than `create_new`: a partial already there is the
    // leftover of a write that crashed before it could clean up, and the
    // name is ours, so it is simply started over.
    let value = {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial.path)
            .map_err(E::from)?;
        let value = body(&mut file)?;
        file.flush().map_err(E::from)?;
        // Before the rename, not after: once renamed, the file is the
        // one the user will restore from, and a rename that survives a
        // power cut over content that did not would be the very archive
        // this exists to prevent.
        file.sync_all().map_err(E::from)?;
        value
        // The handle closes here: Windows refuses to rename a file that
        // is still open.
    };

    std::fs::rename(&partial.path, target).map_err(E::from)?;
    partial.committed = true;
    // Durability on top of a replacement that has already happened, so
    // its failure is not reported as a failed write.
    let _ = sync_directory(directory);
    Ok(value)
}

/// [`write_with`] for content already in memory.
pub fn write_bytes(target: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with(target, |file| file.write_all(bytes))
}

/// Make the rename durable on Unix, where it is a separate write to the
/// directory that a crash can lose. Windows has no directory handle to
/// sync, and opening one fails there, so this is Unix-only.
fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_partial_sits_beside_the_target() {
        let target = Path::new("/backups/Main-2026.waveflow");
        assert_eq!(
            partial_path(target),
            Path::new("/backups/Main-2026.waveflow.partial")
        );
    }

    #[test]
    fn a_complete_write_replaces_the_target_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("list.m3u8");
        std::fs::write(&target, b"old").unwrap();

        write_bytes(&target, b"new").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!partial_path(&target).exists());
    }

    /// The case this exists for: a write that fails halfway must not
    /// have truncated the copy the user already had.
    #[test]
    fn a_failed_write_keeps_the_previous_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("archive.waveflow");
        std::fs::write(&target, b"last week's archive").unwrap();

        let result: io::Result<()> = write_with(&target, |file| {
            file.write_all(b"half of a new one")?;
            Err(io::Error::other("disk full"))
        });

        assert!(result.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"last week's archive");
        assert!(!partial_path(&target).exists());
    }

    /// And with nothing there before, a failure leaves nothing at all
    /// -- not an archive-shaped file a listing would take for a backup.
    #[test]
    fn a_failed_first_write_leaves_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("archive.waveflow");

        let result: io::Result<()> = write_with(&target, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("cancelled"))
        });

        assert!(result.is_err());
        assert!(!target.exists());
        assert!(!partial_path(&target).exists());
    }

    #[test]
    fn a_leftover_partial_is_started_over() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("list.m3u8");
        std::fs::write(partial_path(&target), b"crashed write, much longer").unwrap();

        write_bytes(&target, b"fresh").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("new").join("folder").join("list.m3u8");

        write_bytes(&target, b"x").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"x");
    }
}
