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
//! ([`allowed_source`]), as the asset protocol serves only what a local
//! lookup put in its scope, and reads it through **the source instance the
//! lookup found it on**, for as long as that instance is the registered one
//! ([`is_current`]). A locator names its source by uuid, and after a profile
//! switch the next profile's source of that uuid (an imported copy of the
//! profile) can be another share: neither an old address nor a response
//! already streaming reaches it. Each address carries a grant of its own, so
//! a later lookup of the same clip on another source allows a new address,
//! never an old one again.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use waveflow_core::network_locator;
use waveflow_core::scanner::clips::{
    self, ClipFolders, ClipOrigin, FolderEntry, EBML_MAGIC, ISO_BMFF_EXTENSIONS,
};

use super::reader::{RangeReader, ReadMode};
use super::Source;

/// What one address [`find_clip`] handed out allows: its locator, read
/// through the source instance it was found on.
struct Grant {
    id: String,
    locator: String,
    source: Weak<Source>,
}

/// How many grants are kept, oldest dropped first: one is made per lookup of
/// a track on a share, and only the last few can still be in a player.
const MAX_GRANTS: usize = 256;

static ALLOWED: LazyLock<Mutex<VecDeque<Grant>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

fn allowed() -> std::sync::MutexGuard<'static, VecDeque<Grant>> {
    ALLOWED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `source` is still the registered source of its uuid: the sources
/// were not loaded again since it was looked up.
pub fn is_current(source: &Arc<Source>) -> bool {
    super::source(&source.uuid).is_some_and(|now| Arc::ptr_eq(&now, source))
}

/// The source the loopback server reads `locator` through under the grant
/// `id`: the one the lookup that made the grant found it on, while that one
/// is still registered. `None` for anything a lookup did not hand out.
pub fn allowed_source(id: &str, locator: &str) -> Option<Arc<Source>> {
    let source = allowed()
        .iter()
        .find(|grant| grant.id == id && grant.locator == locator)?
        .source
        .upgrade()?;
    is_current(&source).then_some(source)
}

/// The sources were loaded again or dropped: what was found on the previous
/// ones goes. [`allowed_source`] already refuses it, this frees it. A grant
/// on a source still registered stays: a lookup that ended just after the
/// new sources came in made it on one of them.
pub fn forget_stale() {
    allowed().retain(|grant| {
        grant
            .source
            .upgrade()
            .is_some_and(|source| is_current(&source))
    });
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

/// A clip [`find_clip`] found on a share, and the grant that lets the
/// loopback server serve it.
pub struct FoundClip {
    pub locator: String,
    pub origin: ClipOrigin,
    pub grant: String,
}

/// The clip of the track at `locator`, on `source`, the source its profile
/// names (resolved by the caller while that profile was held). `None` for a
/// share that is down or does not answer the lookup, and for sources loaded
/// again meanwhile. Blocking: run it off the async runtime.
pub fn find_clip(source: Arc<Source>, locator: &str) -> Option<FoundClip> {
    let parsed = network_locator::parse(locator)?;
    if parsed.source != source.uuid || source.is_down() {
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
    // Sources loaded again during the lookup: what it found is on a share
    // that is no longer one of them.
    if !is_current(&source) {
        return None;
    }
    let grant = uuid::Uuid::new_v4().simple().to_string();
    if allowed.len() >= MAX_GRANTS {
        allowed.pop_front();
    }
    allowed.push_back(Grant {
        id: grant.clone(),
        locator: clip.clone(),
        source: Arc::downgrade(&source),
    });
    Some(FoundClip {
        locator: clip,
        origin,
        grant,
    })
}
