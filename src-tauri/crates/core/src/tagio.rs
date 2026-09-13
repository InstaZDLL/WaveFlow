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

/// The ID3v2 tag already on disk, as a span of bytes at the head of the
/// file (#590).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id3v2Span {
    /// Everything the tag occupies: the ten-byte header, the frames, any
    /// padding already there, and the footer when there is one. This is
    /// the room a replacement has to fit into to avoid moving audio.
    pub total: u32,
    /// A footer forbids padding, so a tag that has one cannot be resized
    /// in place — see [`fit_id3v2_padding`].
    pub has_footer: bool,
}

/// Read the ID3v2 header at the very start of `file`, if there is one.
///
/// Deliberately strict about the offset: lofty tolerates junk before the
/// tag, and a tag that does not start at byte zero is one whose span we
/// cannot treat as a prefix to overwrite.
pub fn read_id3v2_span(file: &mut std::fs::File) -> io::Result<Option<Id3v2Span>> {
    use std::io::{Read, Seek, SeekFrom};

    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 10];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        // A file shorter than a header has no tag, which is an answer,
        // not a failure.
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &header[0..3] != b"ID3" {
        return Ok(None);
    }
    // The size is synchsafe: four bytes of seven bits each, so the high
    // bit never forms a false MPEG sync. A byte with its high bit set is
    // not a valid synchsafe digit and means this is not a header we
    // understand well enough to overwrite.
    let mut size: u32 = 0;
    for byte in &header[6..10] {
        if byte & 0x80 != 0 {
            return Ok(None);
        }
        size = (size << 7) | u32::from(*byte);
    }
    let has_footer = header[5] & 0x10 != 0;
    // The declared size covers neither the header nor the footer.
    let total = size
        .checked_add(if has_footer { 20 } else { 10 })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "ID3v2 size overflows"))?;
    Ok(Some(Id3v2Span { total, has_footer }))
}

/// How much padding to ask for so a re-encoded tag lands on exactly the
/// span the old one occupied — `None` when it cannot (#590).
///
/// This is the whole of the fast path's arithmetic. ID3v2 declares its
/// own length and tolerates trailing zeros inside it, so a tag that is
/// no larger than the one already there can be padded back out to the
/// same length and written straight over it. Not one audio byte moves,
/// and the file is never truncated.
///
/// `None` in three cases, each a real one:
/// - there is no tag on disk yet, so there is no span to reuse;
/// - the new tag is bigger than the old span, which is the case the
///   rewrite exists for;
/// - the old tag has a footer, and the spec forbids padding a tag that
///   has one, so the padded re-encode would not be the size we asked
///   for.
pub fn fit_id3v2_padding(existing: Option<Id3v2Span>, encoded_len: u32) -> Option<u32> {
    let span = existing?;
    if span.has_footer {
        return None;
    }
    span.total.checked_sub(encoded_len)
}

/// Write `tag` over the ID3v2 tag already at the head of `file` without
/// moving a single audio byte (#590). `Ok(false)` when it does not fit
/// and the caller must fall back to the rewrite.
///
/// Worth the trouble twice over. It is the difference between touching a
/// few kilobytes and moving the whole file — on a large file over a
/// network share, seconds against milliseconds — and it is also the safe
/// path: lofty's rewrite reads the entire file into memory, calls
/// `truncate(0)` on the original, and writes it back, so an interruption
/// anywhere in there leaves a truncated audio file. This writes a prefix
/// of exactly the length it replaces and never shortens anything.
///
/// The length check before the write is not a belt-and-braces assertion.
/// A padded encode that came back a different length than the arithmetic
/// promised would, written here, overlay the first frames of audio — so
/// a surprise is a reason to take the slow path, never to continue.
pub fn try_id3v2_in_place(
    file: &mut std::fs::File,
    tag: &lofty::id3::v2::Id3v2Tag,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::WriteOptions;
    use lofty::prelude::TagExt;
    use std::io::{Seek, SeekFrom, Write};

    let existing = read_id3v2_span(file)?;

    let mut bare = Vec::new();
    tag.dump_to(&mut bare, WriteOptions::new().preferred_padding(0))?;
    // lofty encodes a frameless tag as nothing at all, which is its way
    // of saying "strip this tag". Writing zero bytes over the old span
    // would leave the old tag in place and claim success, so removal
    // goes the long way round.
    if bare.is_empty() {
        return Ok(false);
    }
    let bare_len = u32::try_from(bare.len())?;

    let Some(padding) = fit_id3v2_padding(existing, bare_len) else {
        return Ok(false);
    };
    let target = existing.expect("a padding fit implies a span").total;

    let bytes = if padding == 0 {
        bare
    } else {
        let mut padded = Vec::new();
        tag.dump_to(&mut padded, WriteOptions::new().preferred_padding(padding))?;
        padded
    };
    if bytes.len() as u64 != u64::from(target) {
        return Ok(false);
    }

    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes)?;
    Ok(true)
}

#[cfg(test)]
mod in_place_tests {
    use super::*;
    use lofty::config::WriteOptions;
    use lofty::id3::v2::Id3v2Tag;
    use lofty::prelude::{Accessor, TagExt};

    /// A file shaped like a tagged MP3: an ID3v2 tag with room to spare,
    /// then bytes standing in for audio frames.
    fn tagged_file(
        title: &str,
        padding: u32,
        audio: &[u8],
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.mp3");
        let mut tag = Id3v2Tag::default();
        tag.set_title(title.to_string());
        let mut bytes = Vec::new();
        tag.dump_to(&mut bytes, WriteOptions::new().preferred_padding(padding))
            .expect("dump");
        bytes.extend_from_slice(audio);
        std::fs::write(&path, &bytes).expect("seed");
        (dir, path)
    }

    #[test]
    fn an_edit_that_fits_leaves_every_audio_byte_where_it_was() {
        // The claim the whole fast path rests on. The check is not "the
        // file still parses" but "these exact bytes did not move": the
        // failure being guarded against is a prefix of the wrong length
        // overlaying the first audio frames.
        let audio: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let (_dir, path) = tagged_file("Before", 2048, &audio);
        let before_len = std::fs::metadata(&path).expect("meta").len();

        let mut replacement = Id3v2Tag::default();
        replacement.set_title("After".to_string());

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let span_before = read_id3v2_span(&mut file).expect("span").expect("a tag");
        let wrote_in_place = try_id3v2_in_place(&mut file, &replacement).expect("in place");
        file.sync_all().expect("sync");
        drop(file);

        assert!(wrote_in_place, "a shorter tag fits in the old span");

        let after = std::fs::read(&path).expect("read");
        assert_eq!(
            after.len() as u64,
            before_len,
            "the file did not change length"
        );
        assert_eq!(
            &after[span_before.total as usize..],
            &audio[..],
            "the audio behind the tag is byte for byte what it was"
        );

        let mut file = std::fs::File::open(&path).expect("reopen");
        let span_after = read_id3v2_span(&mut file).expect("span").expect("a tag");
        assert_eq!(
            span_after, span_before,
            "the span it declares is the one it filled"
        );
        assert!(
            after[..span_before.total as usize]
                .windows(5)
                .any(|w| w == b"After"),
            "the new title is in the tag"
        );
    }

    #[test]
    fn an_edit_that_outgrows_the_padding_asks_for_the_rewrite() {
        // No padding to grow into, and a title far longer than the one
        // it replaces: this is the case the slow path exists for, and
        // saying so is the only safe answer.
        let (_dir, path) = tagged_file("A", 0, b"audio");
        let mut replacement = Id3v2Tag::default();
        replacement.set_title("A".repeat(512));

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let before = std::fs::read(&path).expect("read");
        let wrote_in_place = try_id3v2_in_place(&mut file, &replacement).expect("in place");
        drop(file);

        assert!(!wrote_in_place);
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "refusing the fast path must not have written anything"
        );
    }

    #[test]
    fn a_file_that_never_had_a_tag_asks_for_the_rewrite() {
        // There is no span to overwrite, and writing a tag at offset
        // zero would bury the first audio frames under it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bare.mp3");
        std::fs::write(&path, b"no tag here, just audio").expect("seed");

        let mut tag = Id3v2Tag::default();
        tag.set_title("New".to_string());
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        assert!(!try_id3v2_in_place(&mut file, &tag).expect("in place"));
        drop(file);
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"no tag here, just audio"
        );
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    fn span(total: u32) -> Option<Id3v2Span> {
        Some(Id3v2Span {
            total,
            has_footer: false,
        })
    }

    #[test]
    fn a_smaller_tag_is_padded_back_to_the_old_span() {
        // The point of the whole exercise: the replacement ends up the
        // same length as what it replaces, so the audio behind it never
        // moves.
        assert_eq!(fit_id3v2_padding(span(4096), 1200), Some(2896));
    }

    #[test]
    fn an_exact_fit_asks_for_no_padding() {
        assert_eq!(fit_id3v2_padding(span(4096), 4096), Some(0));
    }

    #[test]
    fn a_bigger_tag_does_not_fit() {
        // One byte over is still over — `checked_sub` is what keeps this
        // from wrapping into an enormous padding request.
        assert_eq!(fit_id3v2_padding(span(4096), 4097), None);
    }

    #[test]
    fn a_file_with_no_tag_has_no_span_to_reuse() {
        assert_eq!(fit_id3v2_padding(None, 10), None);
    }

    #[test]
    fn a_footer_rules_out_padding() {
        // ID3v2.4: "[A tag] MUST NOT have any padding when a tag footer
        // is added". Asking for padding anyway would produce a tag of a
        // different length than the arithmetic here promised.
        let footered = Some(Id3v2Span {
            total: 4096,
            has_footer: true,
        });
        assert_eq!(fit_id3v2_padding(footered, 10), None);
    }
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
