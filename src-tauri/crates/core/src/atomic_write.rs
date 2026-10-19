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
//! Here the bytes go to a partial file beside the destination, reach
//! the disk, and only then replace it in a single `rename`. Any failure
//! removes the partial and leaves the destination as it was. Each write
//! gets a partial of its own, created exclusively: two writes aimed at
//! the same destination (an export run while a backup writes there)
//! would otherwise truncate each other's half-written file.
//!
//! Not for audio files: those go through [`crate::tagio`], which has to
//! keep the original's inode where it can and copies it before writing.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Appended to the destination's file name while it is being written.
/// Anything that lists a folder for finished files (the backup
/// retention, for one) must not take a name ending in it as one.
pub const PARTIAL_SUFFIX: &str = ".partial";

/// The partial for one write of `target`: `<stem>.<token>.<extension>`
/// followed by [`PARTIAL_SUFFIX`], or `<name>.<token>` and the suffix for
/// a name without an extension, in the destination's directory.
///
/// The extension stays last before the suffix so a folder listing can
/// still tell what the partial would have become: the backup retention
/// knows `….waveflow.partial` as an unfinished archive. Beside the
/// destination, never in the system temp directory: `rename` is only
/// atomic within a filesystem, and an export folder is routinely on a
/// different one than `/tmp`.
pub fn partial_path(target: &Path, token: &str) -> PathBuf {
    sibling_path(target, token, PARTIAL_SUFFIX)
}

/// Where the file a Windows replace moves out of the way goes for the
/// length of the swap: like [`partial_path`], but with this suffix, which
/// nothing sweeps. Should the swap fail in a way that leaves the old file
/// there, it may be the only copy of it, and it must not be taken for an
/// unfinished write and deleted a day later.
#[cfg(any(windows, test))]
pub const REPLACED_SUFFIX: &str = ".replaced";

/// `<stem>.<token>.<extension><suffix>`, or `<name>.<token><suffix>` for
/// a name without an extension, in the target's directory.
fn sibling_path(target: &Path, token: &str, suffix: &str) -> PathBuf {
    let mut name = OsString::new();
    match (target.file_stem(), target.extension()) {
        (Some(stem), Some(extension)) => {
            name.push(stem);
            name.push(".");
            name.push(token);
            name.push(".");
            name.push(extension);
        }
        _ => {
            name.push(target.file_name().unwrap_or_default());
            name.push(".");
            name.push(token);
        }
    }
    name.push(suffix);
    target.with_file_name(name)
}

/// A token no other write of this process, or of another process, uses:
/// the process id and a counter.
fn next_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Create a partial no one else holds. A name already taken (a leftover
/// from a crashed process that had the same id) moves on to the next
/// token rather than writing over it.
fn create_partial(target: &Path) -> io::Result<(PathBuf, File)> {
    let mut last_err = None;
    for _ in 0..16 {
        let path = partial_path(target, &next_token());
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => last_err = Some(err),
            Err(err) => return Err(err),
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("no free partial name")))
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

    let (path, mut file) = create_partial(target).map_err(E::from)?;
    let mut partial = Partial {
        path,
        committed: false,
    };
    // Replacing a file must not change who can read it: an archive the
    // user made private would otherwise come back with default rights
    // after the rename. So the destination's mode goes on the partial
    // before a byte is written, and the content is never more exposed
    // than the file it replaces. A new destination keeps the defaults a
    // plain create would have given it. Unix only: on Windows the partial
    // takes the folder's inherited ACL, as the destination did, and the
    // one mode bit there (read-only) would stop the write itself.
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(target) {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // Group bits only mean the same thing for the same group: a `640`
        // archive of the "music" group, copied onto a partial owned by the
        // user's own group, would let that other group read it. So the
        // partial takes the destination's group first, and when that is
        // refused (a group the user is not in), it keeps the owner's bits
        // alone.
        let same_group = std::os::unix::fs::fchown(&file, None, Some(meta.gid())).is_ok();
        // Permission and special bits only, not the file type.
        let mode = meta.permissions().mode() & 0o7777;
        let mode = if same_group { mode } else { mode & 0o700 };
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(E::from)?;
    }
    let value = {
        let value = body(&mut file)?;
        file.flush().map_err(E::from)?;
        // Before the rename, not after: once renamed, the file is the
        // one the user will restore from, and a rename that survives a
        // power cut over content that did not would be the very archive
        // this exists to prevent.
        file.sync_all().map_err(E::from)?;
        // Closed before the rename: Windows refuses to rename a file that
        // is still open.
        drop(file);
        value
    };

    replace(&partial.path, target).map_err(E::from)?;
    partial.committed = true;
    // Durability on top of a replacement that has already happened, so
    // its failure is not reported as a failed write.
    let _ = sync_directory(directory);
    Ok(value)
}

/// Move the finished partial over `target`.
///
/// On Windows an existing destination is replaced with `ReplaceFileW`,
/// which keeps its ACL and attributes the way rewriting it in place did.
/// A plain rename would leave the partial's inherited ACL in their place,
/// and an explicit one the user set would be gone. When they cannot be
/// kept, the write fails rather than replacing the file without them. A
/// destination that does not exist is renamed into place.
///
/// The replaced file goes to a backup name of ours during the swap
/// ([`REPLACED_SUFFIX`]). On one failure,
/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT_2`, Windows has already moved it
/// there without putting the new file in its place: it is moved back, and
/// if even that fails the error says where it is, so neither copy is lost
/// to the cleanup. Nothing sweeps that name, since it may then hold the
/// only copy of the old file.
#[cfg(windows)]
fn replace(partial: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
    };
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    if std::fs::symlink_metadata(target).is_err() {
        return std::fs::rename(partial, target);
    }
    let wide = |path: &Path| -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let backup = sibling_path(target, &next_token(), REPLACED_SUFFIX);
    let replaced = wide(target);
    let replacement = wide(partial);
    let backup_name = wide(&backup);
    // SAFETY: the three paths are NUL-terminated UTF-16 buffers that
    // outlive the call; the two reserved pointers are null and the flags
    // 0, as the API documents.
    let ok = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            backup_name.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok != 0 {
        let _ = std::fs::remove_file(&backup);
        return Ok(());
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error().map(|code| code as u32) {
        // The destination went away between the check and the call.
        Some(ERROR_FILE_NOT_FOUND) => std::fs::rename(partial, target),
        // The old file is at `backup` and the new one is not in place.
        Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2) => match std::fs::rename(&backup, target) {
            Ok(()) => Err(err),
            Err(_) => Err(io::Error::other(format!(
                "{err}; the previous file is at {}",
                backup.display()
            ))),
        },
        // Nothing was moved; a backup is only made once the swap starts.
        _ => {
            let _ = std::fs::remove_file(&backup);
            Err(err)
        }
    }
}

#[cfg(not(windows))]
fn replace(partial: &Path, target: &Path) -> io::Result<()> {
    std::fs::rename(partial, target)
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

    /// Partials, and a Windows replace's backups, left in `dir`.
    fn partials(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.ends_with(PARTIAL_SUFFIX) || name.ends_with(REPLACED_SUFFIX)
            })
            .count()
    }

    #[test]
    fn the_partial_sits_beside_the_target_and_keeps_its_extension_last() {
        assert_eq!(
            partial_path(Path::new("/backups/Main-2026.waveflow"), "7-1"),
            Path::new("/backups/Main-2026.7-1.waveflow.partial")
        );
        assert_eq!(
            partial_path(Path::new("/exports/list"), "7-2"),
            Path::new("/exports/list.7-2.partial")
        );
    }

    /// A Windows replace's backup must not be swept as an unfinished
    /// write: it can hold the only copy of the old file.
    #[test]
    fn the_replaced_backup_is_not_a_partial() {
        let backup = sibling_path(
            Path::new("/backups/Main-2026.waveflow"),
            "7-3",
            REPLACED_SUFFIX,
        );
        assert_eq!(
            backup,
            Path::new("/backups/Main-2026.7-3.waveflow.replaced")
        );
        assert!(!backup.to_string_lossy().ends_with(PARTIAL_SUFFIX));
    }

    #[test]
    fn every_write_gets_its_own_partial() {
        assert_ne!(next_token(), next_token());
    }

    #[test]
    fn a_complete_write_replaces_the_target_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("list.m3u8");
        std::fs::write(&target, b"old").unwrap();

        write_bytes(&target, b"new").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(partials(dir.path()), 0);
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
        assert_eq!(partials(dir.path()), 0);
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
        assert_eq!(partials(dir.path()), 0);
    }

    /// A partial someone else holds is neither truncated nor removed: it
    /// may be another write in progress.
    #[test]
    fn another_partial_of_the_same_target_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("list.m3u8");
        let other = partial_path(&target, "1-999");
        std::fs::write(&other, b"someone else's write").unwrap();

        write_bytes(&target, b"fresh").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
        assert_eq!(std::fs::read(&other).unwrap(), b"someone else's write");
    }

    /// A replaced file keeps the permissions it had, so a private archive
    /// does not come back readable by everyone.
    #[cfg(unix)]
    #[test]
    fn a_replaced_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("private.waveflow");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_bytes(&target, b"new").unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("new").join("folder").join("list.m3u8");

        write_bytes(&target, b"x").unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"x");
    }
}
