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

impl Drop for ReadOnlyGuard<'_> {
    fn drop(&mut self) {
        if !self.lifted {
            return;
        }
        #[cfg(windows)]
        if let Ok(metadata) = std::fs::metadata(self.path) {
            let mut permissions = metadata.permissions();
            permissions.set_readonly(true);
            let _ = std::fs::set_permissions(self.path, permissions);
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
/// across by hand. What in-place costs is atomicity against a crash.
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
    let _readonly = ReadOnlyGuard::lift(path).map_err(E::from)?;
    let mut file = open_for_write(path).map_err(E::from)?;
    let value = body(&mut file)?;
    file.sync_all().map_err(E::from)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_error_is_not_worth_waiting_out() {
        // The retry loop's whole job is telling these apart: waiting out
        // a scanner is right, waiting out "you may not have this file"
        // is a hang that ends in the same message anyway.
        let denied = io::Error::from_raw_os_error(5);
        assert!(!is_transient_lock(&denied));
    }

    #[cfg(windows)]
    #[test]
    fn a_sharing_violation_is() {
        assert!(is_transient_lock(&io::Error::from_raw_os_error(32)));
        assert!(is_transient_lock(&io::Error::from_raw_os_error(33)));
    }

    #[test]
    fn the_body_sees_a_handle_it_can_write_through() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.bin");
        std::fs::write(&path, b"before").expect("seed");

        with_writable_file(&path, |file| -> Result<(), io::Error> {
            use std::io::{Seek, SeekFrom, Write};
            file.seek(SeekFrom::Start(0))?;
            file.write_all(b"after!")
        })
        .expect("write");

        assert_eq!(std::fs::read(&path).expect("read"), b"after!");
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_file_is_written_and_left_read_only() {
        // The reported stop: Windows refuses `write(true).open()` on a
        // read-only file, so the edit failed with a message describing
        // the symptom rather than the cause. Lifting it is ours to do;
        // keeping it lifted is not — the attribute is the user's.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("locked.bin");
        std::fs::write(&path, b"before").expect("seed");
        let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).expect("set read-only");

        with_writable_file(&path, |file| -> Result<(), io::Error> {
            use std::io::{Seek, SeekFrom, Write};
            file.seek(SeekFrom::Start(0))?;
            file.write_all(b"after!")
        })
        .expect("write");

        assert_eq!(std::fs::read(&path).expect("read"), b"after!");
        assert!(
            std::fs::metadata(&path)
                .expect("meta")
                .permissions()
                .readonly(),
            "the read-only attribute belongs to the user, not to the write"
        );
    }
}
