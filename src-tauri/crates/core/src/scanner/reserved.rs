//! The directory WaveFlow keeps its own media in, inside a user's
//! library folder — and the one rule every walker has to know about it.
//!
//! Issue #695 asks for Canvas clips and animated covers to live next to
//! the music rather than in the app's data folder, so they travel with
//! the files and survive a reinstall. The obstacle is not where to write
//! them; it is that **the scanner would index them as songs**:
//!
//! - `mp4` is in [`AUDIO_EXTENSIONS`](super::extract::AUDIO_EXTENSIONS),
//!   legitimately — an `.mp4` usually holds AAC;
//! - [`is_scannable_audio`](super::extract::is_scannable_audio) accepts it
//!   without opening the file (only `ogg` / `oga` pay for a header read),
//!   so a clip with no audio stream at all passes on sight;
//! - and the walk had no directory filter whatsoever, so a leading dot
//!   would have bought nothing: that is a display convention, not
//!   something a file walker obeys.
//!
//! Hence one reserved name, checked in one place, by everything that
//! walks a library: the scanner's walk and the filesystem watcher, which
//! must not turn our own writes into a rescan.

use std::path::Path;

/// The directory name WaveFlow reserves inside a library folder.
///
/// The leading dot hides it from file managers, which is courtesy; what
/// actually keeps it out of the library is this constant being honoured
/// by every walker.
pub const RESERVED_DIR_NAME: &str = ".waveflow";

/// Whether a directory entry's own name is the reserved one.
///
/// For a walker that can prune a subtree as it meets it — cheaper than
/// testing full paths, and it skips the whole branch rather than each
/// file in it.
pub fn is_reserved_dir_name(name: &std::ffi::OsStr) -> bool {
    name == RESERVED_DIR_NAME
}

/// Whether `path` lies inside the reserved directory of `root`.
///
/// **Relative to the root on purpose.** Testing every component of an
/// absolute path would exclude a whole library that happens to sit under
/// a directory of that name — someone whose music lives in
/// `~/.waveflow/Music` would have an empty library and no way to tell
/// why.
///
/// A path that is not under `root` answers `false`: the caller then
/// treats it as ordinary, which for the watcher means one scan it did not
/// strictly need — the safe direction.
pub fn is_in_reserved_dir(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .map(|relative| {
            relative
                .components()
                .any(|component| is_reserved_dir_name(component.as_os_str()))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
