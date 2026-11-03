//! The local copies of tracks on shares.
//!
//! Over the mechanics of [`stream_cache`](crate::audio::stream_cache): a
//! working file filled by offset, published by one rename once whole, the
//! least recently used evicted past a budget. It lives in the profile's cache
//! directory, so nothing is ever written next to the music.
//!
//! No database row: the name is derived from the locator, and the file's own
//! length and modification time hold what it was copied from. The modification
//! time is set to the one the share reported, so a file changed on the share
//! no longer matches and is fetched again, while the eviction, which goes by
//! access time, is not disturbed.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use waveflow_core::network_locator;

use crate::audio::stream_cache::{self, CacheWriter};

/// The default budget, in GiB. Mirrored by `settingsRegistry.ts`.
pub const DEFAULT_MAX_GB: u64 = 5;
pub const MIN_MAX_GB: u64 = 1;
pub const MAX_MAX_GB: u64 = 500;

/// A file system can round a modification time down to two seconds.
const MTIME_SLACK_MS: i64 = 2_000;

/// `<first 8 of the source uuid>-<hash of the locator>.<ext>`. The prefix is
/// what lets a removed source's copies go without reading each one.
pub fn entry_name(locator: &str) -> Option<String> {
    let parsed = network_locator::parse(locator)?;
    let hash = blake3::hash(locator.as_bytes()).to_hex();
    let ext = std::path::Path::new(parsed.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    Some(format!(
        "{}-{}.{}",
        source_prefix(parsed.source),
        &hash.as_str()[..32],
        stream_cache::sanitize_ext(ext)
    ))
}

fn source_prefix(source: &str) -> String {
    source.chars().take(8).collect()
}

/// A complete copy in the cache.
#[derive(Debug, Clone)]
pub struct CacheSlot {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

impl CacheSlot {
    /// Whether this copy is of the file the share now describes.
    pub fn matches(&self, size: u64, modified_ms: i64) -> bool {
        if self.len != size {
            return false;
        }
        // A server that reports no time can only be judged on the length.
        if modified_ms == 0 {
            return true;
        }
        let Some(modified) = self.modified else {
            return false;
        };
        let held = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        (held - modified_ms).abs() <= MTIME_SLACK_MS
    }

    /// The path, marked as just used so the eviction keeps it longer.
    pub fn use_path(&self) -> Option<PathBuf> {
        let dir = self.path.parent()?;
        let name = self.path.file_name()?.to_str()?;
        stream_cache::lookup(dir, name)
    }

    pub fn discard(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn slot_in(dir: &Path, locator: &str) -> Option<CacheSlot> {
    let path = dir.join(entry_name(locator)?);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    Some(CacheSlot {
        path,
        len: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// The copy held for a locator, whatever it was copied from.
pub fn slot(locator: &str) -> Option<CacheSlot> {
    let (dir, _) = super::cache_config()?;
    slot_in(&dir, locator)
}

/// A writer for a locator's bytes. `None` means play without keeping them.
pub fn writer(locator: &str, size: u64, modified_ms: i64) -> Option<CacheWriter> {
    let (dir, max_bytes) = super::cache_config()?;
    let name = entry_name(locator)?;
    let modified = (modified_ms > 0)
        .then(|| SystemTime::UNIX_EPOCH + Duration::from_millis(modified_ms as u64));
    CacheWriter::create_limited(&dir, &name, size, max_bytes, modified)
}

/// Drop what is cached for one source. Best effort: a copy the next eviction
/// reaches anyway is not worth failing a removal over.
pub fn forget_source(dir: &Path, source: &str) {
    let prefix = format!("{}-", source_prefix(source));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_matches_only_the_file_it_was_made_from() {
        let dir = tempfile::tempdir().unwrap();
        let locator = "wfnet://abcdefgh-1234/Rock/a.flac";
        let name = entry_name(locator).unwrap();
        assert!(name.starts_with("abcdefgh-"), "{name}");
        assert!(name.ends_with(".flac"), "{name}");

        let mtime = 1_700_000_000_000_i64;
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(mtime as u64);
        let mut writer =
            CacheWriter::create_limited(dir.path(), &name, 4, 1024 * 1024, Some(at)).unwrap();
        assert!(slot_in(dir.path(), locator).is_none(), "not published yet");
        writer.write_at(0, b"abcd");

        let slot = slot_in(dir.path(), locator).unwrap();
        assert!(slot.matches(4, mtime));
        assert!(slot.matches(4, mtime + 1_000), "a coarser file system");
        assert!(slot.matches(4, 0), "a server that reports no time");
        assert!(!slot.matches(5, mtime), "another length");
        assert!(!slot.matches(4, mtime + 60_000), "edited on the share");
    }

    #[test]
    fn a_removed_source_takes_only_its_own_copies() {
        let dir = tempfile::tempdir().unwrap();
        let mine = entry_name("wfnet://aaaaaaaa-1/a.flac").unwrap();
        let other = entry_name("wfnet://bbbbbbbb-1/a.flac").unwrap();
        std::fs::write(dir.path().join(&mine), b"x").unwrap();
        std::fs::write(dir.path().join(&other), b"x").unwrap();
        forget_source(dir.path(), "aaaaaaaa-1");
        assert!(!dir.path().join(&mine).exists());
        assert!(dir.path().join(&other).exists());
    }
}
