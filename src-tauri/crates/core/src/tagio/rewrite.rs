//! The rewrite through a temporary sibling file, for an edit that cannot
//! be written in place: the new file is complete on disk before it
//! replaces the original.

use std::io;
use std::path::Path;

use super::{
    is_transient_lock, lock_for, open_for_write, ReadOnlyGuard, LOCK_BACKOFF, LOCK_RETRIES,
};

/// A scratch file that deletes itself unless it was renamed into place.
///
/// Without this, every early return between the copy and the rename
/// leaves a `.wf-tmp` beside the user's music — next to the original, in
/// their library folder, where the scanner will eventually find it.
struct TempFile {
    path: std::path::PathBuf,
    committed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Rewrite `path` through a copy, so an interruption cannot destroy it
/// (#598).
///
/// For the writes that cannot be done in place. It matters most for
/// ID3v2, whose rewrite in lofty reads the whole file into memory,
/// **truncates the original to zero**, and writes it back: a crash, a
/// full disk or a killed process anywhere in there leaves a truncated
/// audio file. Here the original is untouched until a single `rename`
/// replaces it, and the bytes are on the disk before that happens.
///
/// The cost is a second copy of the file, which is why it is the
/// fallback: the in-place paths (#590) mean most edits never come here.
///
/// What a rename cannot carry across is worth naming. The file keeps its
/// contents and its permission bits, but it is a new inode: hard links
/// to the old one still point at the old content, and ACLs or extended
/// attributes beyond the permission bits do not follow. That is the
/// trade against losing the file altogether, and it is only paid on the
/// path that would otherwise have rewritten the file in place anyway.
pub fn rewrite_via_temp<E>(
    path: &Path,
    body: impl FnOnce(&mut std::fs::File) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<io::Error>,
{
    // One rewrite of this file at a time: see [`lock_for`]. Taken before
    // anything is created, so the copy, the write, the rename and the
    // cleanup are one sequence rather than four interleavable steps.
    let serialised = lock_for(path);
    let _serialised = serialised
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Lifted first so it is restored last — after the rename, so the
    // attribute lands on the file that ends up in place.
    let _readonly = ReadOnlyGuard::lift(path).map_err(E::from)?;

    // Ask for the write permission this path does not otherwise need.
    // A rename only requires a writable *directory*, so without this the
    // rewrite would happily replace a file the user had deliberately
    // made read-only — while the in-place path, which opens the file
    // itself, refuses it. "You may not write this file" has to mean the
    // same thing whichever path the container happens to take. The
    // handle is dropped immediately: holding it open would make the
    // rename fail on Windows.
    drop(open_for_write(path).map_err(E::from)?);

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("track"));
    // Beside the original, never in the system temp directory: `rename`
    // is only atomic within a filesystem, and a music library is
    // routinely on a different one than `/tmp`.
    let mut name = std::ffi::OsString::from(".");
    name.push(stem);
    // The pid separates two WaveFlow processes; the counter separates
    // two calls inside one, which the lock above already serialises —
    // belt and braces for the case where two spellings of a path did not
    // canonicalise to the same lock key.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ticket = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    name.push(format!(".{}.{ticket}.wf-tmp", std::process::id()));
    let temp = TempFile {
        path: directory.join(name),
        committed: false,
    };

    // A copy rather than an empty file: lofty reads the container it is
    // about to rewrite from the same handle it writes to.
    std::fs::copy(path, &temp.path)
        .map_err(|err| {
            // Without this, the message names a `.wf-tmp` path the user
            // has never seen and did not ask for. The operation they did
            // ask for is the one worth naming, and the directory is the
            // thing that has to be writable.
            io::Error::new(
                err.kind(),
                format!(
                    "could not create the temporary copy the rewrite needs, beside {}: {err}",
                    path.display()
                ),
            )
        })
        .map_err(E::from)?;
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temp.path)
            .map_err(E::from)?;
        body(&mut file)?;
        file.sync_all().map_err(E::from)?;
    }

    let mut temp = temp;
    rename_over(&temp.path, path).map_err(E::from)?;
    // The rename is the commit: past it the replacement has happened and
    // the scratch file is gone, so the cleanup must not run.
    temp.committed = true;
    // Durability on top of a change that has already taken effect. A
    // failure here means the rename may not survive a power cut, not
    // that it did not happen — and reporting it as a write failure would
    // leave the caller skipping the re-hash and the database sync for a
    // file that really did change.
    let _ = sync_directory(directory);
    Ok(())
}

/// Make a rename durable, not just visible.
///
/// `sync_all` on the temporary file puts its *contents* on the disk; the
/// rename is a change to the **directory**, and on Unix that is a
/// separate write which a crash can lose — leaving a name that still
/// points at the old inode, with the new one unreferenced.
///
/// Windows has no directory handle to sync. `fs::rename` there goes
/// through `MoveFileExW`, which commits the replacement as one
/// operation; what it does not do is flush the volume's metadata, so the
/// same crash window is narrower rather than closed. Opening a directory
/// fails outright on Windows, so this is Unix-only rather than
/// best-effort everywhere.
fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

/// `rename`, waiting out the same transient lock an open does.
///
/// The target is the file we are replacing, and on Windows a scanner
/// holding it open refuses the replacement exactly as it refuses an
/// open — with the difference that here the new content already exists
/// and giving up would discard it.
fn rename_over(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if attempt < LOCK_RETRIES && is_transient_lock(&err) => {
                attempt += 1;
                std::thread::sleep(LOCK_BACKOFF);
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests;
