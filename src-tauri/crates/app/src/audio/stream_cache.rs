//! On-disk cache for remote audio streams (lot 3 of the unified library).
//!
//! Until now a remote track was re-downloaded in full on every play: the
//! projection caches metadata, and [`artwork`](super::artwork) caches covers,
//! but the audio itself was fetched from the server each time. On a laptop
//! that plays the same album twice in an evening that is the whole album,
//! twice.
//!
//! ## Filled from what playback already reads
//!
//! The cache is not a downloader. Playback proceeds from the network exactly
//! as before, and every block the decoder reads is written **at its absolute
//! offset** into a sparse working file. Nothing is fetched that would not have
//! been fetched anyway, and nothing is delayed: the first play sounds
//! identical, the second reads from disk.
//!
//! Writing by offset rather than by append is what makes this survive
//! symphonia, which seeks while probing and again on a scrub. An append-only
//! tee would have to give up at the first seek — which, for most formats,
//! arrives within the first few kilobytes and would mean caching almost
//! nothing.
//!
//! ## An entry counts as complete only when every byte is covered
//!
//! Partial files are worse than absent ones: a truncated audio file decodes
//! for a while and then stops, which reads as a broken track rather than a
//! cold cache. So the writer tracks the byte ranges it has covered, merges
//! them, and only publishes the entry — one atomic rename out of `.part` —
//! when a single range spans the whole body. Anything else is discarded on
//! drop. A body of unknown length is never cached at all, since completeness
//! could not be decided.
//!
//! ## Keyed by what was asked for, not by the URL
//!
//! The stream URL carries a single-use, time-limited ticket, so it is
//! different on every play of the same track. The key is
//! `(track id, format, bitrate)` — exactly the triple that determines the
//! bytes, and the same one the server keys its own transcode cache by.
//!
//! ## Why it lives under `audio` and not under `remote`
//!
//! The whole `remote` module is gated on the `sync_v2` feature, and the audio
//! layer is compiled unconditionally — so a cache target named inside
//! `AudioCmd` cannot come from there. The split is the right one anyway: this
//! module owns the mechanics of a file on disk, and `remote` owns what the key
//! means, which is why the format arrives here as a plain string rather than
//! as a remote enum.

use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// How much disk the cache may hold before the least-recently-used entries
/// are evicted. Audio files are two orders of magnitude larger than covers,
/// so this is a real budget rather than the incidental one the cover cache
/// gets away with.
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Where a stream should be cached, named before the response is opened.
///
/// The writer itself cannot be built until `Content-Length` arrives, and that
/// happens inside the HTTP source on the decoder thread — so the caller names
/// the destination and the source decides whether it can be honoured.
#[derive(Debug, Clone)]
pub struct CacheTarget {
    /// The profile's stream-cache directory, from
    /// [`Paths::profile_remote_stream_dir`](crate::paths::Paths::profile_remote_stream_dir).
    pub dir: PathBuf,
    pub name: String,
}

/// The name a given request's bytes are stored under.
///
/// Hashed rather than composed from the parts: a server track id is opaque
/// and may contain anything, including a path separator. The extension is
/// kept legible so the decoder can hint the probe from it, exactly as it does
/// for a library file.
pub fn file_name(track_id: &str, format: &str, bitrate: u32, ext: &str) -> String {
    let key = blake3::hash(format!("{track_id}:{format}:{bitrate}").as_bytes())
        .to_hex()
        .to_string();
    format!("{key}.{}", sanitize_ext(ext))
}

/// Keep the extension to a short alphanumeric run. It comes from the server's
/// `suffix` column, which is metadata read out of a file — not something to
/// paste into a path unchecked.
fn sanitize_ext(ext: &str) -> String {
    let cleaned: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        "bin".to_string()
    } else {
        cleaned
    }
}

/// The cached file for a request, if a complete one exists.
///
/// Touches the entry's mtime on a hit so eviction can order by real use
/// rather than by when the bytes were first written — an album played every
/// week should outlive one played once.
pub fn lookup(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let _ = fs::File::open(&path).and_then(|file| {
        file.set_times(fs::FileTimes::new().set_accessed(std::time::SystemTime::now()))
    });
    Some(path)
}

/// Total bytes held, and how many entries. For the settings card.
pub fn info(dir: &Path) -> (u64, usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut bytes = 0;
    let mut count = 0;
    for entry in entries.flatten() {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                bytes += meta.len();
                count += 1;
            }
        }
    }
    (bytes, count)
}

/// Drop every cached stream. Errors are reported rather than swallowed: a
/// "clear" that silently left files behind would be a lie told to someone
/// trying to reclaim disk.
pub fn clear(dir: &Path) -> std::io::Result<usize> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut removed = 0;
    let mut last_error = None;
    for entry in entries.flatten() {
        if entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
            match fs::remove_file(entry.path()) {
                Ok(()) => removed += 1,
                Err(err) => last_error = Some(err),
            }
        }
    }
    match last_error {
        Some(err) => Err(err),
        None => Ok(removed),
    }
}

/// Evict least-recently-used entries until the directory fits the budget.
///
/// Runs after a successful publish, which is the only moment the total can
/// grow. Best-effort throughout: failing to evict is not a reason to fail the
/// entry that was just cached.
fn evict(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            // Accessed time when the platform keeps one, created/modified as
            // the fallback — any monotone-ish ordering beats deleting at
            // random.
            let stamp = meta
                .accessed()
                .or_else(|_| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            Some((stamp, meta.len(), entry.path()))
        })
        .collect();

    let mut total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= MAX_BYTES {
        return;
    }
    files.sort_by_key(|(stamp, _, _)| *stamp);
    for (_, len, path) in files {
        if total <= MAX_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(len);
        }
    }
}

/// Writes a stream to disk as playback reads it.
///
/// Every method is infallible from the caller's point of view: a cache that
/// cannot be written is a missing optimisation, never a failed playback. The
/// first I/O error poisons the writer, which then does nothing and cleans up
/// after itself.
pub struct CacheWriter {
    file: Option<fs::File>,
    part: PathBuf,
    final_path: PathBuf,
    dir: PathBuf,
    /// Total body length, from `Content-Length`. Completeness is decided
    /// against it, so a writer is never built without one.
    len: u64,
    /// Byte ranges written so far, sorted and merged.
    covered: Vec<(u64, u64)>,
    published: bool,
}

impl CacheWriter {
    /// Open a working file for a request whose length the server declared.
    ///
    /// `None` when the cache is unusable — no length, no writable directory —
    /// which the caller treats as "stream without caching".
    pub fn create(dir: &Path, name: &str, len: u64) -> Option<Self> {
        if len == 0 {
            return None;
        }
        fs::create_dir_all(dir).ok()?;
        let final_path = dir.join(name);
        if final_path.exists() {
            // Already cached by an earlier play; nothing to write.
            return None;
        }
        // A per-writer suffix, not just the pid: two windows of the same
        // process can play the same track at once, and they would otherwise
        // share one working file and corrupt each other's bytes.
        let part = dir.join(format!(
            "{name}.{}.part",
            blake3::hash(
                format!(
                    "{:?}:{:?}",
                    std::time::SystemTime::now(),
                    std::thread::current().id()
                )
                .as_bytes()
            )
            .to_hex()
            .to_string()
            .split_at(16)
            .0
        ));
        let file = fs::File::create(&part).ok()?;
        Some(Self {
            file: Some(file),
            part,
            final_path,
            dir: dir.to_path_buf(),
            len,
            covered: Vec::new(),
            published: false,
        })
    }

    /// Record `bytes` as living at `offset` in the stream.
    pub fn write_at(&mut self, offset: u64, bytes: &[u8]) {
        if bytes.is_empty() || self.published {
            return;
        }
        let Some(file) = self.file.as_mut() else {
            return;
        };
        let end = offset.saturating_add(bytes.len() as u64);
        if end > self.len {
            // The body is longer than it declared: the length we would decide
            // completeness against is wrong, so nothing here can be trusted.
            self.poison();
            return;
        }
        if file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.write_all(bytes))
            .is_err()
        {
            self.poison();
            return;
        }
        self.cover(offset, end);
        if self.is_complete() {
            self.publish();
        }
    }

    fn cover(&mut self, start: u64, end: u64) {
        self.covered.push((start, end));
        self.covered.sort_unstable();
        let mut merged: Vec<(u64, u64)> = Vec::with_capacity(self.covered.len());
        for (start, end) in self.covered.drain(..) {
            match merged.last_mut() {
                // Touching counts as overlapping: two adjacent reads leave no
                // hole between them, and treating them as separate would make
                // a fully-read stream look incomplete forever.
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        self.covered = merged;
    }

    fn is_complete(&self) -> bool {
        matches!(self.covered.as_slice(), [(0, end)] if *end >= self.len)
    }

    /// Move the working file into place. Only ever called once every byte is
    /// covered, so a file that appears under its final name is whole by
    /// construction — which is what lets `lookup` trust it without reading it.
    fn publish(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        // Flush before the rename, not after: a rename publishes the name,
        // and a reader that finds the name must find the bytes too.
        if file.sync_all().is_err() {
            drop(file);
            let _ = fs::remove_file(&self.part);
            return;
        }
        drop(file);
        if fs::rename(&self.part, &self.final_path).is_ok() {
            self.published = true;
            evict(&self.dir);
        } else {
            let _ = fs::remove_file(&self.part);
        }
    }

    fn poison(&mut self) {
        self.file = None;
        let _ = fs::remove_file(&self.part);
    }
}

impl Drop for CacheWriter {
    fn drop(&mut self) {
        // Anything not published is partial. Leaving it would accumulate
        // working files that no lookup can ever use and no clear expects.
        if !self.published {
            self.file = None;
            let _ = fs::remove_file(&self.part);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_separates_what_produces_different_bytes() {
        let raw = file_name("t1", "raw", 0, "flac");
        let mp3 = file_name("t1", "mp3", 192, "mp3");
        let quieter = file_name("t1", "mp3", 128, "mp3");
        let other = file_name("t2", "raw", 0, "flac");
        assert_ne!(raw, mp3, "format changes the bytes");
        assert_ne!(mp3, quieter, "bitrate changes the bytes");
        assert_ne!(raw, other, "track changes the bytes");
        assert!(raw.ends_with(".flac"), "extension stays legible: {raw}");
    }

    #[test]
    fn an_extension_from_the_server_cannot_escape_the_directory() {
        let name = file_name("t1", "raw", 0, "../../etc");
        assert!(!name.contains('/'), "{name}");
        assert!(
            !name.contains('.') || name.matches('.').count() == 1,
            "{name}"
        );
        let empty = file_name("t1", "raw", 0, "");
        assert!(empty.ends_with(".bin"), "{empty}");
    }

    #[test]
    fn only_a_fully_covered_body_is_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = file_name("t1", "raw", 0, "flac");
        let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
        writer.write_at(0, &[0u8; 4]);
        assert!(
            lookup(dir.path(), &name).is_none(),
            "a partial body must not be visible"
        );
        writer.write_at(4, &[0u8; 6]);
        assert!(
            lookup(dir.path(), &name).is_some(),
            "a complete body is published"
        );
    }

    #[test]
    fn a_hole_left_by_a_seek_keeps_the_entry_unpublished() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = file_name("t2", "raw", 0, "flac");
        let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
        // Probe reads the head, the decoder then seeks past the middle: the
        // skipped bytes were never fetched, so the file must not be offered.
        writer.write_at(0, &[0u8; 2]);
        writer.write_at(6, &[0u8; 4]);
        assert!(lookup(dir.path(), &name).is_none());
        // Filling the hole completes it, in whatever order it arrives.
        writer.write_at(2, &[0u8; 4]);
        assert!(lookup(dir.path(), &name).is_some());
    }

    #[test]
    fn overlapping_reads_do_not_count_twice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = file_name("t3", "raw", 0, "flac");
        let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
        // A re-read of already-covered bytes must not make a short body look
        // whole — the naive "sum the lengths" tally would publish here.
        writer.write_at(0, &[0u8; 6]);
        writer.write_at(0, &[0u8; 6]);
        assert!(lookup(dir.path(), &name).is_none());
    }

    #[test]
    fn a_body_longer_than_declared_is_refused_rather_than_truncated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = file_name("t4", "raw", 0, "flac");
        let mut writer = CacheWriter::create(dir.path(), &name, 4).expect("writer");
        writer.write_at(0, &[0u8; 8]);
        assert!(lookup(dir.path(), &name).is_none());
    }

    #[test]
    fn dropping_an_incomplete_writer_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = file_name("t5", "raw", 0, "flac");
        {
            let mut writer = CacheWriter::create(dir.path(), &name, 10).expect("writer");
            writer.write_at(0, &[0u8; 3]);
        }
        let (bytes, count) = info(dir.path());
        assert_eq!((bytes, count), (0, 0), "no .part file may survive");
    }
}
