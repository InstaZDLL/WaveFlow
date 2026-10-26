//! Writing to an audio file, surviving what usually goes wrong (#598).
//!
//! Tag writing is the one place WaveFlow modifies a file the user owns
//! and did not ask us to touch beyond a field or two. Every failure mode
//! here reads to them as data loss, so each is handled explicitly rather
//! than left to the default behaviour of `File::open`.
//!
//! What this module is *not*: it does not decide what to write. lofty
//! does that, through the concrete tag types the invariant requires. The
//! job here is to hand lofty a handle it can succeed with, and to make
//! sure what it wrote has actually reached the disk.

use std::io;
use std::path::Path;
use std::time::Duration;

mod dsf;
mod flac;
mod id3v2;
mod rewrite;

pub use dsf::{read_dsf_layout, write_dsf_id3v2, DsfLayout};
pub use flac::try_flac_in_place;
pub use id3v2::{fit_id3v2_padding, read_id3v2_span, try_id3v2_in_place, Id3v2Span};
pub use rewrite::rewrite_via_temp;

/// How many times a locked file is re-opened before giving up, and how
/// long to wait between attempts.
///
/// A ceiling matters more than the number. On Windows a freshly written
/// file is routinely opened by a virus scanner for a few hundred
/// milliseconds, and a single attempt turns that into a permanent
/// failure the user reads as "cannot save". An unbounded retry turns the
/// opposite case — a file genuinely held by another program — into a
/// hang, which is worse than an error.
const LOCK_RETRIES: u32 = 5;
const LOCK_BACKOFF: Duration = Duration::from_millis(120);

/// Whether an open failed because someone else is holding the file,
/// rather than because we are not allowed to have it.
///
/// The distinction is the whole point of the retry loop: a sharing
/// violation is transient and worth waiting out, a permission error is
/// not, and waiting only postpones the same message. Windows separates
/// them by code — 32 `ERROR_SHARING_VIOLATION`, 33 `ERROR_LOCK_VIOLATION`,
/// against 5 `ERROR_ACCESS_DENIED` — and it is the only platform where
/// the transient case is common, so nothing else claims it.
fn is_transient_lock(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(err.raw_os_error(), Some(32) | Some(33))
    }
    #[cfg(not(windows))]
    {
        let _ = err;
        false
    }
}

/// The read-only flag lifted for the duration of a write, and put back
/// afterwards.
///
/// Windows refuses to open a read-only file for writing at all, so the
/// attribute is a hard stop on an operation the user is entitled to
/// perform — they can clear it in Explorer and try again, which is a
/// worse version of what we can do for them. It is restored on every way
/// out, a failed write and an unwind included, because the attribute is
/// the user's setting and not ours to consume.
///
/// Windows only: on Unix the same flag is the write bits of the mode,
/// and clearing it would hand a group-writable file back with different
/// permissions than it arrived with.
struct ReadOnlyGuard<'a> {
    #[cfg_attr(not(windows), allow(dead_code))]
    path: &'a Path,
    lifted: bool,
}

impl<'a> ReadOnlyGuard<'a> {
    #[cfg(windows)]
    fn lift(path: &'a Path) -> io::Result<Self> {
        let mut permissions = std::fs::metadata(path)?.permissions();
        if !permissions.readonly() {
            return Ok(Self {
                path,
                lifted: false,
            });
        }
        // The lint warns that this is a mode change that would leave a
        // Unix file world-writable. That is exactly why this function
        // has no Unix body: here `readonly` is the single FAT attribute
        // bit, clearing it is the documented way to make the file
        // writable, and `Drop` puts it back.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
        Ok(Self { path, lifted: true })
    }

    #[cfg(not(windows))]
    fn lift(path: &'a Path) -> io::Result<Self> {
        Ok(Self {
            path,
            lifted: false,
        })
    }
}

impl ReadOnlyGuard<'_> {
    #[cfg(windows)]
    fn restore(&self) {
        if let Ok(metadata) = std::fs::metadata(self.path) {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            let _ = std::fs::set_permissions(self.path, permissions);
        }
    }

    /// Nothing to restore: `lift` never clears anything off Windows.
    ///
    /// A separate function rather than a `#[cfg]` block inside `drop`,
    /// because emptying the tail of that function turns the guard clause
    /// above it into a `needless_return` that only Linux sees.
    #[cfg(not(windows))]
    fn restore(&self) {}
}

impl Drop for ReadOnlyGuard<'_> {
    fn drop(&mut self) {
        if self.lifted {
            self.restore();
        }
    }
}

/// Open `path` for reading and writing, waiting out a transient lock.
fn open_for_write(path: &Path) -> io::Result<std::fs::File> {
    let mut attempt = 0;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(err) if attempt < LOCK_RETRIES && is_transient_lock(&err) => {
                attempt += 1;
                std::thread::sleep(LOCK_BACKOFF);
            }
            Err(err) => return Err(err),
        }
    }
}

/// The lock protecting one file's rewrite, shared by everyone editing
/// that file.
///
/// Three commands reach a tag write — a single edit, a batch, and a
/// cover — and each hands it to the blocking pool, so two of them can be
/// inside this module on the same file at the same time. Without this
/// they interleave a copy with a write and race on the rename, and the
/// loser's edit disappears into a file the winner replaced. Keyed by
/// path rather than global, so editing two different files still
/// proceeds in parallel.
///
/// `Weak` values, swept on each insert: a session editing thousands of
/// files must not end up holding one mutex per file forever.
fn rewrite_locks() -> &'static std::sync::Mutex<
    std::collections::HashMap<std::path::PathBuf, std::sync::Weak<std::sync::Mutex<()>>>,
> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<std::path::PathBuf, std::sync::Weak<std::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default)
}

fn lock_for(path: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    // Canonicalised so two spellings of one file — a different case on
    // Windows, a `..` segment — take the same lock rather than two.
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut map = rewrite_locks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = map.get(&key).and_then(std::sync::Weak::upgrade) {
        return existing;
    }
    let fresh = std::sync::Arc::new(std::sync::Mutex::new(()));
    map.insert(key, std::sync::Arc::downgrade(&fresh));
    map.retain(|_, weak| weak.strong_count() > 0);
    fresh
}

/// Run `body` against `path` opened for writing, and do not return
/// success until the bytes it wrote are on the disk.
///
/// Blocking, and sometimes for seconds — a library on a network share is
/// exactly the case this exists for — so it belongs on a blocking
/// thread, never on an async worker.
///
/// The file is written **in place**, which is deliberate: the inode is
/// the one the user's permissions, ACLs, extended attributes and hard
/// links are attached to, and rewriting through a temporary file would
/// hand all of that back changed unless every piece of it were copied
/// across by hand. What in-place costs is atomicity against a crash, so
/// this is for writes that cannot destroy the file if they stop half way
/// — the padding paths below. Everything else goes through
/// [`rewrite_via_temp`].
///
/// `sync_all` before returning, not after: the caller re-hashes the file
/// and writes that hash to the database, and a hash of bytes that never
/// reached the platter is a row describing a file that does not exist.
pub fn with_writable_file<T, E>(
    path: &Path,
    body: impl FnOnce(&mut std::fs::File) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<io::Error>,
{
    let serialised = lock_for(path);
    let _serialised = serialised
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _readonly = ReadOnlyGuard::lift(path).map_err(E::from)?;
    let mut file = open_for_write(path).map_err(E::from)?;
    let value = body(&mut file)?;
    file.sync_all().map_err(E::from)?;
    Ok(value)
}

/// A cheap fingerprint of `path` as it is on disk: its size and its
/// modification time, to the precision the filesystem keeps.
///
/// What the tag editor reads when it opens and checks again before it
/// writes. Its form shows the values the library holds, and a save
/// writes them all back, so a file another tagger rewrote in between
/// would quietly lose that program's change. The pair is the one the
/// scanner's fast path trusts to say a file has not changed, and it
/// costs a `stat` rather than a read of the file.
///
/// Opaque on purpose: it is only ever compared for equality, and a
/// string carries nanoseconds intact through JSON, where a number would
/// be rounded to what a double holds.
///
/// What it does not catch: a rewrite that keeps both the size and the
/// modification time, which a tagger has to do on purpose (restoring the
/// time after a same-size edit). Comparing content would catch it, at the
/// cost of reading the whole file twice per save; a dialog that guards
/// against an ordinary second tagger does not need that.
pub fn file_stamp(path: &Path) -> io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified()?;
    let nanos = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => after.as_nanos() as i128,
        // A file dated before 1970 is unusual, not invalid.
        Err(before) => -(before.duration().as_nanos() as i128),
    };
    Ok(format!("{}:{nanos}", meta.len()))
}

#[cfg(test)]
mod tests;
