//! What a scan needs of a share: its listing, and a reader per file.
//!
//! The scanner itself is the one in `commands::scan`, down to its transaction
//! batches and its single writer. Only the listing differs, and it is the
//! better half of the deal: size and modification time come with it, so a
//! rescan that finds nothing changed has read no audio at all.
//!
//! ## A listing that did not finish says nothing
//!
//! The scan treats the list as the folder's contents and marks what is missing
//! from it as gone, which hides those tracks everywhere. So [`walk`] returns a
//! whole list or an error, never part of one. Only a directory that vanished
//! between its parent's listing and its own (`NotFound`) is passed over.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use sqlx::SqlitePool;
use waveflow_core::error::{CoreError, CoreResult};
use waveflow_core::network_locator::{self, is_safe_path, LOCAL_ONLY_EXTENSIONS};
use waveflow_core::scanner::{
    apply_network_covers,
    extract::{pick_folder_cover, store_folder_cover},
    is_reserved_dir_name, network_cover_candidates, ExtractedCover, SidecarStamp, AUDIO_EXTENSIONS,
    HASH_CHUNK_BYTES,
};

use super::fs::{Entry, NetError};
use super::reader::{RangeReader, ReadMode};
use super::Source;

/// Directories listed at once: the wait on a share is the round trip, and a
/// NAS on spinning disks does not thank anyone for more.
const CONCURRENT_LISTINGS: usize = 4;

/// Files read at once by a scan. Lower than the local scan's, which is sized
/// for a CPU hashing, for the same reason.
pub const CONCURRENT_EXTRACTIONS: usize = 4;

/// A guard against a link loop on the server.
const MAX_DEPTH: usize = 64;

/// The largest sidecar a scan reads.
const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;

/// A server's own bookkeeping, and what a Mac leaves on a share. Never music.
const SKIPPED_DIRS: &[&str] = &["@eaDir", "#recycle", ".AppleDouble", ".Trashes"];

/// What the listing says of one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetFile {
    pub size: u64,
    pub modified_ms: i64,
}

/// A share's folder as one walk found it.
pub struct Listing {
    pub source: Arc<Source>,
    /// Locators, as `PathBuf` like the local walk's list.
    pub files: Vec<PathBuf>,
    meta: HashMap<String, NetFile>,
    /// Share-relative directory → the image files in it.
    images: HashMap<String, Vec<(String, NetFile)>>,
    /// A sidecar is one file for a whole album: fetched once per directory.
    covers: Mutex<HashMap<String, Option<ExtractedCover>>>,
    /// Locators of the directories a parent named and this walk could not read:
    /// refused to the account, or not found. Nothing is known of what they hold,
    /// so nothing in them may be swept as gone.
    refused: Vec<String>,
}

fn is_scannable_name(name: &str) -> bool {
    if name.starts_with("._") {
        return false;
    }
    let Some(ext) = Path::new(name).extension().and_then(OsStr::to_str) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    AUDIO_EXTENSIONS.contains(&ext.as_str()) && !LOCAL_ONLY_EXTENSIONS.contains(&ext.as_str())
}

fn is_image_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| {
            waveflow_core::scanner::extract::FOLDER_COVER_EXTENSIONS
                .contains(&ext.to_ascii_lowercase().as_str())
        })
}

fn skips_dir(name: &str) -> bool {
    is_reserved_dir_name(OsStr::new(name)) || SKIPPED_DIRS.contains(&name)
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// Sort one directory's entries into the listing and the next directories.
fn absorb(
    uuid: &str,
    dir: &str,
    entries: Vec<Entry>,
    next: &mut Vec<String>,
    listing: &mut Listing,
) {
    for entry in entries {
        // The server's word for a name is data; a backend already filters
        // these, and a stored locator is worth checking twice.
        if !is_safe_path(&entry.name) || entry.name.contains('/') {
            continue;
        }
        let path = join(dir, &entry.name);
        if entry.is_dir {
            if !skips_dir(&entry.name) {
                next.push(path);
            }
        } else if is_image_name(&entry.name) {
            listing.images.entry(dir.to_string()).or_default().push((
                entry.name,
                NetFile {
                    size: entry.size,
                    modified_ms: entry.modified_ms,
                },
            ));
        } else if is_scannable_name(&entry.name) {
            if let Some(locator) = network_locator::build(uuid, &path) {
                listing.meta.insert(
                    locator.clone(),
                    NetFile {
                        size: entry.size,
                        modified_ms: entry.modified_ms,
                    },
                );
                listing.files.push(PathBuf::from(locator));
            }
        }
    }
}

/// List a share's folder, reporting as it goes and stoppable. `Ok(None)` when
/// stopped, which like an error yields no list.
pub fn walk(
    folder: &str,
    mut on_progress: impl FnMut(usize, String),
    should_stop: impl Fn() -> bool,
) -> Result<Option<Listing>, NetError> {
    let parsed = network_locator::parse(folder)
        .ok_or_else(|| NetError::Protocol("not a network folder".into()))?;
    let source = super::source(parsed.source)
        .ok_or_else(|| NetError::Protocol("this network source is not available".into()))?;
    let uuid = parsed.source.to_string();
    let mut listing = Listing {
        source: Arc::clone(&source),
        files: Vec::new(),
        meta: HashMap::new(),
        images: HashMap::new(),
        covers: Mutex::new(HashMap::new()),
        refused: Vec::new(),
    };
    let mut level = vec![parsed.path.to_string()];
    let mut depth = 0;
    let mut is_root = true;
    while !level.is_empty() && depth < MAX_DEPTH {
        let mut next = Vec::new();
        let dirs = std::mem::take(&mut level);
        let source_for_level = Arc::clone(&source);
        let stopped = super::block_on(async {
            let mut results = futures::stream::iter(dirs)
                .map(|dir| {
                    let source = Arc::clone(&source_for_level);
                    async move {
                        let entries = source.list(&dir).await;
                        (dir, entries)
                    }
                })
                .buffer_unordered(CONCURRENT_LISTINGS);
            while let Some((dir, entries)) = results.next().await {
                if should_stop() {
                    return Ok(true);
                }
                match entries {
                    Ok(entries) => absorb(&uuid, &dir, entries, &mut next, &mut listing),
                    // One folder this account may not read (a system folder on
                    // a Windows share) does not stop the rest being listed. Nor
                    // does one gone since its parent was listed, whose tracks
                    // go when the parent stops naming it. The folder itself
                    // missing says more.
                    Err(NetError::Auth(_) | NetError::NotFound) if !is_root => {
                        if let Some(locator) = network_locator::build(&uuid, &dir) {
                            listing.refused.push(locator);
                        }
                    }
                    Err(err) => return Err(err),
                }
                on_progress(listing.files.len(), join(&source_for_level.name, &dir));
            }
            Ok(false)
        })?;
        if stopped {
            return Ok(None);
        }
        level = next;
        depth += 1;
        is_root = false;
    }
    if !level.is_empty() {
        return Err(NetError::Protocol(
            "the folder is nested too deep, or loops".into(),
        ));
    }
    Ok(Some(listing))
}

impl Listing {
    /// Whether a locator lies under a directory this walk could not read, where
    /// its absence from the list says nothing.
    pub fn is_under_refused(&self, locator: &str) -> bool {
        self.refused.iter().any(|dir| {
            locator
                .strip_prefix(dir.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    pub fn meta(&self, locator: &str) -> Option<NetFile> {
        self.meta.get(locator).copied()
    }

    /// A reader on one file, its two hash windows already fetched: `hash_file`
    /// wants exactly those, and the tags of most files lie inside them
    /// (measured: under 25 KiB outside an embedded picture). One request each,
    /// and then lofty and the hash read from memory.
    pub fn open_for_scan(&self, locator: &str, meta: NetFile) -> std::io::Result<RangeReader> {
        let parsed = network_locator::parse(locator)
            .ok_or_else(|| std::io::Error::other("not a locator"))?;
        let mut reader = RangeReader::new(
            Arc::clone(&self.source),
            parsed.path.to_string(),
            meta.size,
            ReadMode::Scan,
        );
        if meta.size <= 2 * HASH_CHUNK_BYTES {
            reader.preload(0, meta.size)?;
        } else {
            reader.preload(0, HASH_CHUNK_BYTES)?;
            reader.preload(meta.size - HASH_CHUNK_BYTES, HASH_CHUNK_BYTES)?;
        }
        reader.seek(SeekFrom::Start(0))?;
        Ok(reader)
    }

    /// The sidecar cover of a track's directory, fetched once for all of them.
    pub fn folder_cover(&self, locator: &str, artwork_dir: &Path) -> Option<ExtractedCover> {
        let parsed = network_locator::parse(locator)?;
        let dir = network_locator::parent_dir(parsed.path);
        if let Some(known) = self.covers.lock().ok()?.get(dir).cloned() {
            return known;
        }
        let cover = self.fetch_folder_cover(dir, artwork_dir);
        self.covers
            .lock()
            .ok()?
            .insert(dir.to_string(), cover.clone());
        cover
    }

    /// What this walk shows of the sidecar a track's directory would use, with
    /// nothing asked of the share. `None` for a directory it did not read, or
    /// holding nothing worth reading.
    pub fn listed_cover(&self, locator: &str) -> Option<SidecarStamp> {
        let parsed = network_locator::parse(locator)?;
        if parsed.source != self.source.uuid {
            return None;
        }
        let (name, file) = self.sidecar_of(network_locator::parent_dir(parsed.path))?;
        Some(SidecarStamp {
            name: name.to_string(),
            size: file.size as i64,
            modified_ms: file.modified_ms,
        })
    }

    fn sidecar_of(&self, dir: &str) -> Option<(&str, NetFile)> {
        let listed = self.images.get(dir)?;
        let name = pick_folder_cover(listed.iter().map(|(name, _)| name.as_str()))?;
        let (_, file) = listed.iter().find(|(listed, _)| listed == name)?;
        // A cover is not worth more than a track's worth of the budget.
        (file.size > 0 && file.size <= MAX_COVER_BYTES).then_some((name, *file))
    }

    fn fetch_folder_cover(&self, dir: &str, artwork_dir: &Path) -> Option<ExtractedCover> {
        let (name, file) = self.sidecar_of(dir)?;
        let path = join(dir, name);
        // Changed since the listing: the next one says what it is now, and
        // reading to the old length would keep part of an image.
        let entry = super::block_on(self.source.stat(&path)).ok()?;
        if entry.size != file.size {
            return None;
        }
        let mut reader =
            RangeReader::new(Arc::clone(&self.source), path, entry.size, ReadMode::Scan);
        reader.preload(0, entry.size).ok()?;
        let mut bytes = Vec::with_capacity(entry.size as usize);
        reader.read_to_end(&mut bytes).ok()?;
        store_folder_cover(
            &bytes,
            Path::new(name).extension().and_then(OsStr::to_str),
            artwork_dir,
        )
    }
}

/// The share's half of the folder-cover reconciliation, after a scan: the fast
/// path skips unchanged tracks, so a sidecar added or replaced next to them
/// reaches the album here.
///
/// `refresh_folder_covers` compares bytes, which on a share means downloading
/// every cover on every scan. This compares what the listing shows of the
/// sidecar with what it showed when the album last took stock of it, and reads
/// the image only when they differ. A deep scan, which is the way to notice a
/// change that kept the file's date, reads it regardless.
///
/// A sidecar the listing no longer shows changes nothing, nor does a read that
/// fails, which the next scan tries again. Takes a whole listing: [`walk`]
/// gives no other.
pub async fn refresh_covers(
    pool: &SqlitePool,
    listing: &Arc<Listing>,
    artwork_dir: &Path,
    folder_id: i64,
    deep: bool,
    should_stop: impl Fn() -> bool,
) -> CoreResult<u32> {
    let mut reads = Vec::new();
    for candidate in network_cover_candidates(pool, folder_id).await? {
        let Some(stamp) = listing.listed_cover(&candidate.locator) else {
            continue;
        };
        if !deep && candidate.seen.as_ref() == Some(&stamp) {
            continue;
        }
        if should_stop() {
            break;
        }
        let cover = tokio::task::spawn_blocking({
            let listing = Arc::clone(listing);
            let locator = candidate.locator.clone();
            let artwork_dir = artwork_dir.to_path_buf();
            move || listing.folder_cover(&locator, &artwork_dir)
        })
        .await
        .map_err(|e| CoreError::Other(format!("network cover read join: {e}")))?;
        if let Some(cover) = cover {
            reads.push((candidate, stamp, cover));
        }
    }
    apply_network_covers(pool, reads).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_offers_what_can_be_read_from_it() {
        for name in ["a.flac", "B.MP3", "c d.m4a", "e.opus"] {
            assert!(is_scannable_name(name), "{name}");
        }
        for name in ["a.dsf", "a.DFF", "._a.flac", "a.txt", "flac", "cover.jpg"] {
            assert!(!is_scannable_name(name), "{name}");
        }
    }

    #[test]
    fn a_servers_bookkeeping_is_not_walked() {
        for name in [".waveflow", "@eaDir", "#recycle", ".AppleDouble"] {
            assert!(skips_dir(name), "{name}");
        }
        assert!(!skips_dir("Rock"));
    }
}
