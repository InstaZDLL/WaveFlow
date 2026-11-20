//! A track's music video on a share (#1133).
//!
//! Found by the rule a local track's is
//! ([`waveflow_core::scanner::clips::find_clip_with`]): the video named like
//! the track, beside it or in a `Clips` folder beside it, or the track's own
//! file when it is a video. The listings and the header reads go to the
//! share; nothing is stored.
//!
//! The webview cannot read a share, so the clip is streamed to it by
//! [`crate::media_loopback`], which reads it through the source's client
//! range by range. That server serves only what [`find_clip`] handed out
//! since the sources were last loaded ([`is_allowed`]), as the asset protocol
//! serves only what a local lookup put in its scope: a locator names a source
//! by its uuid, and the next profile's source of that uuid (an imported copy
//! of the profile) can be another share.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use waveflow_core::network_locator;
use waveflow_core::scanner::clips::{
    self, ClipFolders, ClipOrigin, FolderEntry, EBML_MAGIC, ISO_BMFF_EXTENSIONS,
};

use super::reader::{RangeReader, ReadMode};
use super::Source;

/// The locators [`find_clip`] handed out, for the loopback server, and the
/// generation of the sources they were found on.
struct Allowed {
    generation: u64,
    locators: HashSet<String>,
}

static ALLOWED: LazyLock<Mutex<Allowed>> = LazyLock::new(|| {
    Mutex::new(Allowed {
        generation: 0,
        locators: HashSet::new(),
    })
});

fn allowed() -> std::sync::MutexGuard<'static, Allowed> {
    ALLOWED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether the loopback server may serve `locator`: a clip a lookup found
/// on the sources loaded now.
pub fn is_allowed(locator: &str) -> bool {
    allowed().locators.contains(locator)
}

/// The sources were loaded again or dropped: nothing found on the previous
/// ones is served any more, and a lookup that started on them allows nothing.
pub fn forget_all() {
    let mut allowed = allowed();
    allowed.generation += 1;
    allowed.locators.clear();
}

/// One share, read for [`clips::find_clip_with`]. Paths are relative to its
/// root, as everywhere in the network library.
struct ShareFolders {
    source: Arc<Source>,
    /// The size of every file the listings named, so that reading a
    /// candidate's headers costs no request to learn its length.
    sizes: RefCell<HashMap<String, u64>>,
}

impl ShareFolders {
    fn size_of(&self, path: &str) -> Option<u64> {
        if let Some(size) = self.sizes.borrow().get(path) {
            return Some(*size);
        }
        super::block_on(self.source.stat(path))
            .ok()
            .filter(|entry| !entry.is_dir)
            .map(|entry| entry.size)
    }
}

/// `name` inside the share-relative folder `dir`.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn extension_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

impl ClipFolders for ShareFolders {
    type Path = String;

    fn entries(&self, dir: &String) -> Option<Vec<FolderEntry<String>>> {
        let listed = super::block_on(self.source.list(dir)).ok()?;
        let mut sizes = self.sizes.borrow_mut();
        Some(
            listed
                .into_iter()
                // A name the share hands back passes this already (the
                // `NetworkFs` contract); one that did not would make a path
                // that climbs out of the share.
                .filter(|entry| network_locator::is_safe_path(&entry.name))
                .map(|entry| {
                    let path = join(dir, &entry.name);
                    if !entry.is_dir {
                        sizes.insert(path.clone(), entry.size);
                    }
                    FolderEntry {
                        path,
                        name: entry.name,
                        is_dir: entry.is_dir,
                    }
                })
                .collect(),
        )
    }

    fn is_video(&self, path: &String) -> bool {
        match extension_of(path) {
            Some(ext) if ISO_BMFF_EXTENSIONS.contains(&ext.as_str()) => self.has_video_track(path),
            Some(_) => super::block_on(self.source.read_at(path, 0, EBML_MAGIC.len() as u64))
                .is_ok_and(|head| head == EBML_MAGIC),
            None => false,
        }
    }

    fn has_video_track(&self, path: &String) -> bool {
        let Some(size) = self.size_of(path) else {
            return false;
        };
        let mut reader =
            RangeReader::new(Arc::clone(&self.source), path.clone(), size, ReadMode::Scan);
        clips::has_video_track(&mut reader).unwrap_or(false)
    }
}

/// The clip of the track at `locator`, as a locator, and where it was found.
/// `None` for a share that is unknown or down, or one that does not answer
/// the lookup. Blocking: run it off the async runtime.
pub fn find_clip(locator: &str) -> Option<(String, ClipOrigin)> {
    let generation = allowed().generation;
    let parsed = network_locator::parse(locator)?;
    let source = super::source(parsed.source)?;
    if source.is_down() {
        return None;
    }
    let (dir, track_name) = parsed.path.rsplit_once('/').unwrap_or(("", parsed.path));
    let dir_name = dir.rsplit('/').next().filter(|name| !name.is_empty());
    let folders = ShareFolders {
        source: Arc::clone(&source),
        sizes: RefCell::default(),
    };
    let (path, origin) = clips::find_clip_with(
        &folders,
        &dir.to_string(),
        dir_name,
        &parsed.path.to_string(),
        track_name,
    )?;
    let clip = network_locator::build(parsed.source, &path)?;
    let mut allowed = allowed();
    if allowed.generation != generation {
        return None;
    }
    allowed.locators.insert(clip.clone());
    Some((clip, origin))
}
