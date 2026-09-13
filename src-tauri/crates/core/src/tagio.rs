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
    std::fs::copy(path, &temp.path).map_err(E::from)?;
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
    temp.committed = true;
    Ok(())
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

/// FLAC metadata block types this module needs to tell apart. The rest
/// are carried through untouched, so they never need naming.
const FLAC_BLOCK_PADDING: u8 = 1;
const FLAC_BLOCK_VORBIS_COMMENT: u8 = 4;
/// A metadata region larger than this is not something to buffer in
/// memory on the strength of a length field we have not validated.
const FLAC_MAX_METADATA: u64 = 64 * 1024 * 1024;

/// One metadata block as it sits on disk: its type, and its contents
/// byte for byte.
struct FlacBlock {
    ty: u8,
    content: Vec<u8>,
}

/// Serialise a Vorbis comment block's contents.
///
/// Little-endian lengths throughout, which is the one thing FLAC borrows
/// from Ogg rather than from its own big-endian headers — getting it
/// backwards produces a block every player rejects.
fn encode_vorbis_comments(comments: &lofty::ogg::tag::VorbisComments) -> Vec<u8> {
    let mut out = Vec::new();
    let vendor = comments.vendor().as_bytes();
    out.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    out.extend_from_slice(vendor);
    // The count has to be written before the items, and the items are
    // behind an iterator, so they are gathered first.
    let mut entries: Vec<Vec<u8>> = Vec::new();
    for (key, value) in comments.items() {
        let mut entry = Vec::with_capacity(key.len() + value.len() + 1);
        entry.extend_from_slice(key.as_bytes());
        entry.push(b'=');
        entry.extend_from_slice(value.as_bytes());
        entries.push(entry);
    }
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries {
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry);
    }
    out
}

/// Read the metadata blocks at the head of a FLAC file.
///
/// `Ok(None)` for anything this path should not touch — a file that does
/// not start with `fLaC` (an ID3v2-prefixed FLAC lands here), or a
/// metadata region whose declared size is not credible.
fn read_flac_blocks(file: &mut std::fs::File) -> io::Result<Option<Vec<FlacBlock>>> {
    use std::io::{Read, Seek, SeekFrom};

    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &magic != b"fLaC" {
        return Ok(None);
    }

    // A read that runs out of file is this function's own answer — the
    // container is not one we can splice — and not an error the edit
    // should fail on. Anything else is a real I/O failure and is
    // propagated.
    macro_rules! read_or_decline {
        ($buf:expr) => {
            match file.read_exact($buf) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(err) => return Err(err),
            }
        };
    }

    let mut blocks = Vec::new();
    let mut total: u64 = 0;
    loop {
        let mut header = [0u8; 4];
        read_or_decline!(&mut header);
        let last = header[0] & 0x80 != 0;
        let ty = header[0] & 0x7f;
        let len = u32::from(header[1]) << 16 | u32::from(header[2]) << 8 | u32::from(header[3]);
        total += u64::from(len) + 4;
        if total > FLAC_MAX_METADATA {
            return Ok(None);
        }
        let mut content = vec![0u8; len as usize];
        read_or_decline!(&mut content);
        blocks.push(FlacBlock { ty, content });
        if last {
            break;
        }
    }
    Ok(Some(blocks))
}

/// Write `comments` into the metadata region a FLAC file already has,
/// absorbing the size difference into its PADDING block (#590).
/// `Ok(false)` when it does not fit and the caller must fall back.
///
/// This is lofty's own open TODO (`lofty-rs#445`): its writer builds the
/// new block set and then calls a `replace_range` that shifts **every
/// audio byte** whenever the new metadata differs in size from the old,
/// in 64 KB chunks over the whole file. Padding exists precisely so that
/// never has to happen, and every tagger leaves some.
///
/// Only the comment block is regenerated. Pictures, seek table, cue
/// sheet and application blocks are copied through byte for byte, which
/// is both faster and the only way to honour the rule that an edit to a
/// title never costs a cover or a MusicBrainz identifier. A cover edit
/// therefore does not come here — regenerating picture blocks is the
/// rewrite's job.
pub fn try_flac_in_place(
    file: &mut std::fs::File,
    comments: &lofty::ogg::tag::VorbisComments,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use lofty::ogg::OggPictureStorage;
    use std::io::{Seek, SeekFrom, Write};

    let Some(blocks) = read_flac_blocks(file)? else {
        return Ok(false);
    };
    // Everything the region holds today, headers included. This is the
    // budget the replacement has to land inside.
    let region: u64 = blocks
        .iter()
        .map(|block| block.content.len() as u64 + 4)
        .sum();

    // FLAC normally keeps its pictures in their own metadata blocks,
    // which is why they are copied through untouched above. But a
    // `VorbisComments` can carry pictures too — some taggers write the
    // base64 `METADATA_BLOCK_PICTURE` comment even in a FLAC, and lofty's
    // split/merge round trip can hand them back on the tag — and the
    // encoder here writes only the vendor string and the items. Encoding
    // a set that has pictures would drop them, which is the one thing
    // this path must never do. The rewrite knows how to write them.
    if !comments.pictures().is_empty() {
        return Ok(false);
    }

    let encoded = encode_vorbis_comments(comments);
    // A block's length field is 24 bits; a comment set larger than that
    // cannot be written as one block at all.
    if encoded.len() > 0x00ff_ffff {
        return Ok(false);
    }

    // The comment block is replaced and the padding is recomputed;
    // everything else stays exactly as it was.
    let kept: Vec<&FlacBlock> = blocks
        .iter()
        .filter(|block| block.ty != FLAC_BLOCK_VORBIS_COMMENT && block.ty != FLAC_BLOCK_PADDING)
        .collect();
    let fixed: u64 = kept
        .iter()
        .map(|block| block.content.len() as u64 + 4)
        .sum::<u64>()
        + encoded.len() as u64
        + 4;

    let padding = match region.checked_sub(fixed) {
        // An exact fit needs no padding block at all.
        Some(0) => None,
        // A padding block cannot be smaller than its own header.
        Some(slack) if slack >= 4 => Some(slack - 4),
        _ => return Ok(false),
    };
    if padding.is_some_and(|len| len > 0x00ff_ffff) {
        return Ok(false);
    }

    let mut out: Vec<u8> = Vec::with_capacity(region as usize);
    let write_block = |out: &mut Vec<u8>, ty: u8, content: &[u8], last: bool| {
        let len = content.len() as u32;
        out.push(if last { ty | 0x80 } else { ty });
        out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8]);
        out.extend_from_slice(content);
    };
    let has_padding = padding.is_some();
    for block in &kept {
        write_block(&mut out, block.ty, &block.content, false);
    }
    write_block(&mut out, FLAC_BLOCK_VORBIS_COMMENT, &encoded, !has_padding);
    if let Some(len) = padding {
        write_block(&mut out, FLAC_BLOCK_PADDING, &vec![0u8; len as usize], true);
    }

    // Same reasoning as the ID3v2 path: a region of the wrong length
    // written here would run into the audio frames behind it, so a
    // surprise sends us to the slow path rather than to the disk.
    if out.len() as u64 != region {
        return Ok(false);
    }

    file.seek(SeekFrom::Start(4))?;
    file.write_all(&out)?;
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
    fn a_cover_survives_the_in_place_write() {
        // Why the ID3v2 fast path takes covers and the FLAC one does
        // not: here the picture is an APIC frame *inside* the tag, so
        // `dump_to` writes it with everything else. The FLAC guard
        // exists because that encoder rebuilds only the comment block
        // and pictures live in blocks of their own — a different gap,
        // not a symmetry to copy.
        use lofty::id3::v2::{AttachedPictureFrame, Frame};
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::TextEncoding;

        let audio: Vec<u8> = (0..2048u32).map(|i| (i % 199) as u8 + 1).collect();
        let (_dir, path) = tagged_file("Before", 8192, &audio);
        let before_len = std::fs::metadata(&path).expect("meta").len();

        let mut tag = Id3v2Tag::default();
        tag.set_title("After".to_string());
        let cover: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8 + 2).collect();
        tag.insert(Frame::Picture(AttachedPictureFrame::new(
            TextEncoding::UTF8,
            Picture::unchecked(cover.clone())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        )));

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let span = read_id3v2_span(&mut file).expect("span").expect("a tag");
        assert!(try_id3v2_in_place(&mut file, &tag).expect("in place"));
        file.sync_all().expect("sync");
        drop(file);

        let after = std::fs::read(&path).expect("read");
        assert_eq!(after.len() as u64, before_len);
        assert_eq!(
            &after[span.total as usize..],
            &audio[..],
            "the audio is untouched"
        );
        assert!(
            after[..span.total as usize]
                .windows(cover.len())
                .any(|w| w == &cover[..]),
            "the cover went into the file with the rest of the tag"
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

/// Where a DSF file keeps its tag (#592).
///
/// DSF is the one container here whose metadata sits **after** the
/// audio, at an offset the header declares. That makes writing it the
/// easy case rather than the hard one: the tag can grow or shrink freely
/// and not one audio byte moves, because there is nothing behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsfLayout {
    /// The size the header claims the file is. Updated with the tag,
    /// because a player that trusts it and finds something else has
    /// every reason to call the file damaged.
    pub total_size: u64,
    /// Where the ID3v2 tag starts, or `0` for a file that has none.
    pub metadata_offset: u64,
}

/// The fixed part of a DSF header: magic, chunk size, file size, and the
/// metadata pointer.
const DSF_HEADER_LEN: u64 = 28;

/// Read the DSD chunk at the head of a DSF file.
///
/// `Ok(None)` when this is not a DSF at all, or when the header's own
/// numbers do not agree with the file — a metadata pointer past the end,
/// or one pointing into the header itself. Those are not files to write
/// a tag into on the strength of the pointer.
pub fn read_dsf_layout(file: &mut std::fs::File) -> io::Result<Option<DsfLayout>> {
    use std::io::{Read, Seek, SeekFrom};

    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; DSF_HEADER_LEN as usize];
    match file.read_exact(&mut header) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &header[0..4] != b"DSD " {
        return Ok(None);
    }
    let read_u64 = |at: usize| {
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&header[at..at + 8]);
        u64::from_le_bytes(buf)
    };
    if read_u64(4) != DSF_HEADER_LEN {
        return Ok(None);
    }
    let metadata_offset = read_u64(20);
    if metadata_offset != 0 && (metadata_offset < DSF_HEADER_LEN || metadata_offset > len) {
        return Ok(None);
    }
    Ok(Some(DsfLayout {
        total_size: read_u64(12),
        metadata_offset,
    }))
}

/// Replace a DSF file's ID3v2 tag, and tell the header about it (#592).
///
/// An empty `tag` removes it: the file is cut back to the end of the
/// audio and the pointer set to zero, which is how a DSF says it has no
/// metadata.
///
/// The two header fields are written **after** the tag, so a file
/// interrupted mid-write still declares the tag it had rather than
/// pointing at half of a new one.
pub fn write_dsf_id3v2(file: &mut std::fs::File, tag: &[u8]) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    let Some(layout) = read_dsf_layout(file)? else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a DSF file, or its header does not describe itself",
        ));
    };
    // With no tag on record, the tag goes where the file currently ends
    // — which is the end of the audio.
    let offset = if layout.metadata_offset != 0 {
        layout.metadata_offset
    } else {
        file.metadata()?.len()
    };

    file.seek(SeekFrom::Start(offset))?;
    file.write_all(tag)?;
    let new_len = offset + tag.len() as u64;
    // Only ever cuts the tag region: `offset` is at or after the end of
    // the audio by construction.
    file.set_len(new_len)?;

    file.seek(SeekFrom::Start(12))?;
    file.write_all(&new_len.to_le_bytes())?;
    file.seek(SeekFrom::Start(20))?;
    let pointer = if tag.is_empty() { 0 } else { offset };
    file.write_all(&pointer.to_le_bytes())?;
    Ok(())
}

#[cfg(test)]
mod dsf_tests {
    use super::*;

    /// A file shaped like a DSF: the DSD chunk, bytes standing in for
    /// the fmt/data chunks, then a tag at the declared offset.
    fn dsf_file(audio: &[u8], tag: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.dsf");
        let offset = if tag.is_empty() {
            0
        } else {
            DSF_HEADER_LEN + audio.len() as u64
        };
        let total = DSF_HEADER_LEN + audio.len() as u64 + tag.len() as u64;
        let mut bytes = b"DSD ".to_vec();
        bytes.extend_from_slice(&DSF_HEADER_LEN.to_le_bytes());
        bytes.extend_from_slice(&total.to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.extend_from_slice(audio);
        bytes.extend_from_slice(tag);
        std::fs::write(&path, &bytes).expect("seed");
        (dir, path)
    }

    fn open(path: &std::path::Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open")
    }

    #[test]
    fn a_bigger_tag_does_not_disturb_the_audio() {
        // The property that makes DSF the easy container: the tag is
        // last, so it can grow without anything moving.
        let audio: Vec<u8> = (0..4096u32).map(|i| (i % 253) as u8 + 1).collect();
        let (_dir, path) = dsf_file(&audio, b"small tag");

        let mut file = open(&path);
        let replacement = vec![0x42u8; 8192];
        write_dsf_id3v2(&mut file, &replacement).expect("write");
        drop(file);

        let after = std::fs::read(&path).expect("read");
        assert_eq!(
            &after[DSF_HEADER_LEN as usize..DSF_HEADER_LEN as usize + audio.len()],
            &audio[..],
            "the audio is byte for byte what it was"
        );
        assert_eq!(
            &after[DSF_HEADER_LEN as usize + audio.len()..],
            &replacement[..]
        );

        let mut file = open(&path);
        let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
        assert_eq!(layout.metadata_offset, DSF_HEADER_LEN + audio.len() as u64);
        assert_eq!(
            layout.total_size,
            after.len() as u64,
            "the header declares the size the file actually is"
        );
    }

    #[test]
    fn a_smaller_tag_cuts_only_the_tag() {
        let audio: Vec<u8> = (0..1024u32).map(|i| (i % 97) as u8 + 1).collect();
        let (_dir, path) = dsf_file(&audio, &vec![0xEEu8; 4096]);

        let mut file = open(&path);
        write_dsf_id3v2(&mut file, b"tiny").expect("write");
        drop(file);

        let after = std::fs::read(&path).expect("read");
        assert_eq!(after.len() as u64, DSF_HEADER_LEN + audio.len() as u64 + 4);
        assert_eq!(
            &after[DSF_HEADER_LEN as usize..DSF_HEADER_LEN as usize + audio.len()],
            &audio[..]
        );
    }

    #[test]
    fn a_file_that_had_no_tag_gets_one_at_the_end() {
        let audio: Vec<u8> = (0..512u32).map(|i| (i % 71) as u8 + 1).collect();
        let (_dir, path) = dsf_file(&audio, b"");

        let mut file = open(&path);
        assert_eq!(
            read_dsf_layout(&mut file)
                .expect("layout")
                .expect("a DSF")
                .metadata_offset,
            0
        );
        write_dsf_id3v2(&mut file, b"a brand new tag").expect("write");
        drop(file);

        let mut file = open(&path);
        let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
        assert_eq!(layout.metadata_offset, DSF_HEADER_LEN + audio.len() as u64);
        let after = std::fs::read(&path).expect("read");
        assert_eq!(
            &after[layout.metadata_offset as usize..],
            b"a brand new tag"
        );
    }

    #[test]
    fn an_empty_tag_removes_it_and_says_so() {
        let audio: Vec<u8> = vec![7; 256];
        let (_dir, path) = dsf_file(&audio, b"an existing tag");

        let mut file = open(&path);
        write_dsf_id3v2(&mut file, b"").expect("write");
        drop(file);

        let mut file = open(&path);
        let layout = read_dsf_layout(&mut file).expect("layout").expect("a DSF");
        assert_eq!(
            layout.metadata_offset, 0,
            "zero is how a DSF says it has none"
        );
        assert_eq!(layout.total_size, DSF_HEADER_LEN + audio.len() as u64);
        assert_eq!(
            std::fs::read(&path).expect("read").len() as u64,
            DSF_HEADER_LEN + audio.len() as u64
        );
    }

    #[test]
    fn a_header_that_does_not_describe_itself_is_refused() {
        // A pointer past the end of the file, or into the header: the
        // write would either land in the audio or past it, and neither
        // is a tag edit.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.dsf");
        let mut bytes = b"DSD ".to_vec();
        bytes.extend_from_slice(&DSF_HEADER_LEN.to_le_bytes());
        bytes.extend_from_slice(&100u64.to_le_bytes());
        bytes.extend_from_slice(&9_000u64.to_le_bytes()); // past the end
        bytes.extend_from_slice(&[0u8; 64]);
        std::fs::write(&path, &bytes).expect("seed");

        let mut file = open(&path);
        assert!(read_dsf_layout(&mut file).expect("layout").is_none());
        assert!(write_dsf_id3v2(&mut file, b"tag").is_err());
    }
}

#[cfg(test)]
mod rewrite_tests {
    use super::*;

    #[test]
    fn a_failed_rewrite_leaves_the_original_exactly_as_it_was() {
        // The whole point. lofty's own rewrite truncates the file it is
        // rewriting before it writes anything back, so a body that fails
        // half way used to be the difference between an edit and a
        // ruined track.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, b"the original bytes").expect("seed");

        let outcome: Result<(), io::Error> = rewrite_via_temp(&path, |file| {
            use std::io::Write;
            // Do real damage first, then fail: a body that fails before
            // touching anything would prove nothing.
            file.set_len(0)?;
            file.write_all(b"half")?;
            Err(io::Error::other("the write gave up here"))
        });

        assert!(outcome.is_err());
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"the original bytes",
            "the original is untouched until the rename"
        );
    }

    #[test]
    fn nothing_is_left_beside_the_users_music() {
        // A scratch file dropped in a library folder is one the scanner
        // eventually finds, so it has to go on every way out.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, b"original").expect("seed");

        let _ = rewrite_via_temp(&path, |_| -> Result<(), io::Error> {
            Err(io::Error::other("no"))
        });
        rewrite_via_temp(&path, |file| -> Result<(), io::Error> {
            use std::io::Write;
            file.set_len(0)?;
            file.write_all(b"rewritten")
        })
        .expect("rewrite");

        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(left.len(), 1, "only the track remains: {left:?}");
        assert_eq!(std::fs::read(&path).expect("read"), b"rewritten");
    }

    #[test]
    fn two_rewrites_of_one_file_take_turns() {
        // Three commands reach a tag write and each hands it to the
        // blocking pool, so two can be here on the same file at once.
        // Interleaved, they race on the rename and one edit vanishes;
        // worse, each temp file is deleted by the other's cleanup.
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("contested.mp3");
        std::fs::write(&path, b"original").expect("seed");

        let inside = Arc::new(AtomicUsize::new(0));
        let overlapped = Arc::new(AtomicUsize::new(0));

        let threads: Vec<_> = (0..4)
            .map(|i| {
                let path = path.clone();
                let inside = Arc::clone(&inside);
                let overlapped = Arc::clone(&overlapped);
                std::thread::spawn(move || {
                    rewrite_via_temp(&path, |file| -> Result<(), io::Error> {
                        use std::io::Write;
                        if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                            overlapped.fetch_add(1, Ordering::SeqCst);
                        }
                        // Long enough that an unserialised pair would
                        // certainly be caught overlapping.
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        file.set_len(0)?;
                        let written = format!("written by {i}");
                        file.write_all(written.as_bytes())?;
                        inside.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("join").expect("rewrite");
        }

        assert_eq!(
            overlapped.load(Ordering::SeqCst),
            0,
            "two rewrites of the same file were inside at once"
        );
        // And the file is one writer's whole output, not a blend.
        let after = std::fs::read(&path).expect("read");
        let text = String::from_utf8(after).expect("utf8");
        assert!(text.starts_with("written by "), "got {text:?}");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(left.len(), 1, "no scratch file survived: {left:?}");
    }

    #[cfg(windows)]
    #[test]
    fn the_read_only_attribute_lands_on_the_file_that_ends_up_in_place() {
        // The rename replaces the inode, so restoring the attribute has
        // to happen after it — on the new file, not the one that is
        // already gone.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.mp3");
        std::fs::write(&path, b"original").expect("seed");
        let mut permissions = std::fs::metadata(&path).expect("meta").permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).expect("set read-only");

        rewrite_via_temp(&path, |file| -> Result<(), io::Error> {
            use std::io::Write;
            file.set_len(0)?;
            file.write_all(b"rewritten")
        })
        .expect("rewrite");

        assert_eq!(std::fs::read(&path).expect("read"), b"rewritten");
        assert!(
            std::fs::metadata(&path)
                .expect("meta")
                .permissions()
                .readonly(),
            "the attribute the user set is back on the file that replaced it"
        );
    }
}

#[cfg(test)]
mod flac_tests {
    use super::*;
    use lofty::ogg::tag::VorbisComments;

    fn block(ty: u8, content: &[u8], last: bool) -> Vec<u8> {
        let len = content.len() as u32;
        let mut out = vec![
            if last { ty | 0x80 } else { ty },
            (len >> 16) as u8,
            (len >> 8) as u8,
            len as u8,
        ];
        out.extend_from_slice(content);
        out
    }

    /// A file shaped like a tagged FLAC: the magic, a stand-in
    /// STREAMINFO, a comment block, a padding block, then bytes standing
    /// in for audio frames.
    fn flac_file(padding: usize, audio: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.flac");
        let mut comments = VorbisComments::default();
        comments.push("TITLE".to_string(), "Before".to_string());

        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&block(0, &[0u8; 34], false));
        bytes.extend_from_slice(&block(
            FLAC_BLOCK_VORBIS_COMMENT,
            &encode_vorbis_comments(&comments),
            false,
        ));
        bytes.extend_from_slice(&block(FLAC_BLOCK_PADDING, &vec![0u8; padding], true));
        bytes.extend_from_slice(audio);
        std::fs::write(&path, &bytes).expect("seed");
        (dir, path)
    }

    fn open(path: &std::path::Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .expect("open")
    }

    #[test]
    fn an_edit_that_fits_the_padding_leaves_the_audio_alone() {
        // The claim the benchmark in the issue rests on: the write
        // touches the metadata region and nothing else, so the cost is
        // the region rather than the file.
        let audio: Vec<u8> = (0..8192u32).map(|i| (i % 241) as u8 + 1).collect();
        let (_dir, path) = flac_file(1024, &audio);
        let before_len = std::fs::metadata(&path).expect("meta").len();

        let mut comments = VorbisComments::default();
        comments.push("TITLE".to_string(), "After the edit".to_string());
        comments.push("ARTIST".to_string(), "Someone".to_string());

        let mut file = open(&path);
        assert!(try_flac_in_place(&mut file, &comments).expect("in place"));
        file.sync_all().expect("sync");
        drop(file);

        let after = std::fs::read(&path).expect("read");
        assert_eq!(
            after.len() as u64,
            before_len,
            "the file did not change length"
        );
        assert_eq!(
            &after[after.len() - audio.len()..],
            &audio[..],
            "every audio byte is where it was"
        );

        // And the region still parses as blocks, ending on a last-block
        // flag — a region that did not close would take the first audio
        // bytes with it on the next read.
        let mut file = open(&path);
        let blocks = read_flac_blocks(&mut file).expect("read").expect("blocks");
        let region: usize = blocks.iter().map(|b| b.content.len() + 4).sum();
        assert_eq!(region + 4 + audio.len(), after.len());
        let comment = blocks
            .iter()
            .find(|b| b.ty == FLAC_BLOCK_VORBIS_COMMENT)
            .expect("a comment block");
        assert!(
            comment.content.windows(14).any(|w| w == b"After the edit"),
            "the new title is in the block"
        );
    }

    #[test]
    fn an_edit_too_big_for_the_padding_asks_for_the_rewrite() {
        // No slack at all, and far more to write than there was: this is
        // the case that has to move the file, and saying so is the only
        // safe answer.
        let (_dir, path) = flac_file(0, b"audio bytes");
        let before = std::fs::read(&path).expect("read");

        let mut comments = VorbisComments::default();
        comments.push("TITLE".to_string(), "T".repeat(4096));

        let mut file = open(&path);
        assert!(!try_flac_in_place(&mut file, &comments).expect("in place"));
        drop(file);
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "refusing must not have written anything"
        );
    }

    #[test]
    fn a_file_that_is_not_flac_is_left_alone() {
        // An ID3v2-prefixed FLAC lands here too: the magic is not at
        // byte zero, so the region offsets would all be wrong.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not.flac");
        std::fs::write(&path, b"ID3\x04\x00\x00\x00\x00\x00\x00rest").expect("seed");
        let mut file = open(&path);
        assert!(!try_flac_in_place(&mut file, &VorbisComments::default()).expect("in place"));
    }

    #[test]
    fn a_comment_set_carrying_a_picture_asks_for_the_rewrite() {
        // FLAC normally keeps pictures in their own blocks, which is why
        // this path copies those through untouched. But a VorbisComments
        // can carry them too, and the encoder here writes only the
        // vendor and the items — so encoding a set that has pictures
        // would drop them. Losing a cover to a title edit is the one
        // thing this path must never do.
        use lofty::ogg::OggPictureStorage;
        use lofty::picture::{Picture, PictureInformation, PictureType};

        let audio: Vec<u8> = vec![3; 512];
        let (_dir, path) = flac_file(4096, &audio);
        let before = std::fs::read(&path).expect("read");

        let mut comments = VorbisComments::default();
        comments.push("TITLE".to_string(), "After".to_string());
        let picture = Picture::unchecked(vec![0xFF; 64])
            .pic_type(PictureType::CoverFront)
            .mime_type(lofty::picture::MimeType::Jpeg)
            .build();
        comments
            .insert_picture(picture, Some(PictureInformation::default()))
            .expect("insert");

        let mut file = open(&path);
        assert!(!try_flac_in_place(&mut file, &comments).expect("in place"));
        drop(file);
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "declining must not have written anything"
        );
    }

    #[test]
    fn a_truncated_file_asks_for_the_rewrite_instead_of_failing() {
        // A block header that runs off the end is this function saying
        // "not a container I can splice", not an I/O failure the edit
        // should die on.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cut.flac");
        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&block(0, &[0u8; 34], false));
        // A header promising far more content than the file holds.
        bytes.extend_from_slice(&[FLAC_BLOCK_VORBIS_COMMENT, 0x00, 0xFF, 0x00]);
        bytes.extend_from_slice(b"only a few bytes");
        std::fs::write(&path, &bytes).expect("seed");

        let mut file = open(&path);
        assert!(read_flac_blocks(&mut file).expect("read").is_none());
        let mut file = open(&path);
        assert!(
            !try_flac_in_place(&mut file, &VorbisComments::default()).expect("in place"),
            "a truncated file declines rather than erroring"
        );
    }

    #[test]
    fn the_blocks_we_did_not_edit_are_copied_through_byte_for_byte() {
        // The rule the whole tag path follows: correcting a title never
        // costs a cover or a MusicBrainz identifier.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rich.flac");
        let picture: Vec<u8> = (0..300u32).map(|i| (i % 255) as u8).collect();
        let seektable: Vec<u8> = vec![0xAB; 180];

        let mut comments = VorbisComments::default();
        comments.push("TITLE".to_string(), "Before".to_string());
        let mut bytes = b"fLaC".to_vec();
        bytes.extend_from_slice(&block(0, &[0u8; 34], false));
        bytes.extend_from_slice(&block(3, &seektable, false));
        bytes.extend_from_slice(&block(6, &picture, false));
        bytes.extend_from_slice(&block(
            FLAC_BLOCK_VORBIS_COMMENT,
            &encode_vorbis_comments(&comments),
            false,
        ));
        bytes.extend_from_slice(&block(FLAC_BLOCK_PADDING, &vec![0u8; 512], true));
        std::fs::write(&path, &bytes).expect("seed");

        let mut edited = VorbisComments::default();
        edited.push("TITLE".to_string(), "After".to_string());
        let mut file = open(&path);
        assert!(try_flac_in_place(&mut file, &edited).expect("in place"));
        drop(file);

        let mut file = open(&path);
        let blocks = read_flac_blocks(&mut file).expect("read").expect("blocks");
        assert_eq!(
            blocks.iter().find(|b| b.ty == 6).expect("picture").content,
            picture,
            "the cover survived a title edit"
        );
        assert_eq!(
            blocks
                .iter()
                .find(|b| b.ty == 3)
                .expect("seektable")
                .content,
            seektable,
            "the seek table survived a title edit"
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
