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
//! A write that never reaches its cleanup (a crash, a power cut, the app
//! closed in the middle of an export) leaves its partial behind. The next
//! write to the same destination removes those that are a day old.
//!
//! Caches named by the hash of their content (the artwork folders) go
//! through [`store_content_addressed`], the same partial-and-rename
//! without the flushes to disk.
//!
//! Not for audio files: those go through [`crate::tagio`], which has to
//! keep the original's inode where it can and copies it before writing.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Appended to the destination's file name while it is being written.
/// Anything that lists a folder for finished files (the backup
/// retention, for one) must not take a name ending in it as one.
pub const PARTIAL_SUFFIX: &str = ".partial";

/// How old a partial has to be before it is taken for one whose write
/// will never finish. Far longer than any write takes, so it is never
/// one in progress: a write touches its partial with every block.
pub const STALE_PARTIAL_AGE: Duration = Duration::from_secs(24 * 60 * 60);

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

/// Whether `name` is the file name of a partial of `target`, left by any
/// process: [`partial_path`] with a `<pid>-<n>` token. The token is
/// matched whole, so a partial of another destination whose name starts
/// the same way (`Main.old.waveflow` beside `Main.waveflow`) is never
/// taken for one of this destination's. Names are compared as they are
/// on disk, so a destination whose name is not valid UTF-8 still has its
/// partials found; only the token has to be text.
fn is_partial_of(target: &Path, name: &OsStr) -> bool {
    let (head, tail) = match (target.file_stem(), target.extension()) {
        (Some(stem), Some(extension)) => {
            let mut tail = OsString::from(".");
            tail.push(extension);
            tail.push(PARTIAL_SUFFIX);
            (stem, tail)
        }
        _ => match target.file_name() {
            Some(name) => (name, OsString::from(PARTIAL_SUFFIX)),
            None => return false,
        },
    };
    let Some(token) = name
        .as_encoded_bytes()
        .strip_prefix(head.as_encoded_bytes())
        .and_then(|rest| rest.strip_prefix(b"."))
        .and_then(|rest| rest.strip_suffix(tail.as_encoded_bytes()))
        .and_then(|token| std::str::from_utf8(token).ok())
    else {
        return false;
    };
    let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    token
        .split_once('-')
        .is_some_and(|(pid, n)| number(pid) && number(n))
}

/// Remove the partials of `target` that earlier writes left behind and
/// that are older than [`STALE_PARTIAL_AGE`]. One another write is still
/// filling is younger than that, and one whose age cannot be read is
/// kept. Best effort: what cannot be listed or removed stays, and the
/// write goes ahead.
fn sweep_stale_partials(target: &Path, directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        if !is_partial_of(target, &entry.file_name()) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_PARTIAL_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
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
/// The parent directory is created if missing. Partials of `target` that
/// earlier writes left there are removed once a day old, before this one
/// starts, so the space they hold is free for it. Blocking: call it from
/// a blocking thread, never an async worker.
pub fn write_with<T, E>(target: &Path, body: impl FnOnce(&mut File) -> Result<T, E>) -> Result<T, E>
where
    E: From<io::Error>,
{
    let directory = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory).map_err(E::from)?;
    sweep_stale_partials(target, directory);
    write_through_partial(target, directory, Durability::Synced, body)
}

/// Whether a write waits for its bytes to reach the disk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Durability {
    /// Flushed before the rename, and the rename made durable after it.
    Synced,
    /// Left to the OS: for a cache the next write repairs.
    Cached,
}

/// The partial, the body, the rename: [`write_with`] once its directory
/// exists and its stale partials are gone.
fn write_through_partial<T, E>(
    target: &Path,
    directory: &Path,
    durability: Durability,
    body: impl FnOnce(&mut File) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<io::Error>,
{
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
        if durability == Durability::Synced {
            file.sync_all().map_err(E::from)?;
        }
        // Closed before the rename: Windows refuses to rename a file that
        // is still open.
        drop(file);
        value
    };

    replace(&partial.path, target).map_err(E::from)?;
    partial.committed = true;
    // Durability on top of a replacement that has already happened, so
    // its failure is not reported as a failed write.
    if durability == Durability::Synced {
        let _ = sync_directory(directory);
    }
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

/// Store `bytes` at `target` in a cache whose file names are the hash of
/// their content (the artwork caches), unless it already holds them.
/// Returns whether it wrote.
///
/// A name stands for one content there, so a file already at `target` is
/// kept when its length is that of `bytes`, and written again when it is
/// not: a copy cut short before writes went through a partial, or one a
/// power cut emptied, is shorter than its name says, and an `exists()`
/// check kept it forever. The write itself goes through a partial and a
/// rename like [`write_bytes`], so a cut-short one never shows under the
/// final name, but without the flushes to disk: what a crash costs here
/// is a cache entry the next write puts back, and a scan storing one
/// cover per album should not wait on the disk for each. Nor does it
/// sweep the folder for stale partials, which would list a folder of
/// thousands of covers on every write.
///
/// When the rename fails but `target` meanwhile holds the right length —
/// another writer stored the same image first, and Windows refuses to
/// replace a file a reader has open — the content is there and that is
/// a success. Blocking.
pub fn store_content_addressed(target: &Path, bytes: &[u8]) -> io::Result<bool> {
    let complete = |path: &Path| {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == bytes.len() as u64)
    };
    if complete(target) {
        return Ok(false);
    }
    let directory = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory)?;
    match write_through_partial(target, directory, Durability::Cached, |file: &mut File| {
        file.write_all(bytes)
    }) {
        Ok(()) => Ok(true),
        Err(_) if complete(target) => Ok(false),
        Err(err) => Err(err),
    }
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
mod tests;
