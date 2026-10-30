//! Single-track ID3 / Vorbis tag editor.
//!
//! Mirrors the scanner's tag → DB pipeline in reverse: the user edits a
//! handful of fields, the values are written back to the audio file via
//! lofty, and the database (track / album / artist / track_artist /
//! genre / track_genre) is updated to match. FTS5 stays in sync via the
//! existing triggers on `track`, `album.title`, and `artist.name`.
//!
//! File-lock dance: the audio engine may have the file open if the
//! edited track is currently playing. Either way the write needs the
//! real file — in place, or as the target of a rename — so on Windows
//! the engine's read handle is enough to refuse it. We pause playback
//! before writing whenever the engine reports the same
//! `current_track_id`. Resume is left to the user: silently restarting
//! after a save would be surprising, and would re-open the file while we
//! are still writing it.
//!
//! **Two ways of writing, and which one runs matters to the file's
//! identity.** [`patch_file`] tries the in-place path first (#590): the
//! new tag is laid over the one already there, which works whenever it
//! fits the padding, and keeps the inode — with it the permissions, the
//! ACL, the extended attributes and the hard links. When it does not
//! fit, the rewrite runs through
//! [`waveflow_core::tagio::rewrite_via_temp`] (#598): a sibling
//! `.wf-tmp` beside the original, replaced by a single `rename`. That
//! one cannot be interrupted into a half-written file, which is the
//! point — but it hands back a **new inode**, so hard links to the old
//! one keep the old content and anything past the permission bits does
//! not follow. DSF is neither: its tag sits after the audio, so it is
//! always written in place ([`patch_dsf`], #592).

mod cover;
mod db_sync;
mod dsf_tag;
mod id3_tag;
mod musicbrainz_ids;
mod patch;
mod update;

pub use cover::*;
pub(crate) use db_sync::*;
pub(crate) use dsf_tag::*;
use id3_tag::*;
pub(crate) use musicbrainz_ids::*;
pub(crate) use patch::*;
pub use update::*;

#[cfg(test)]
mod patch_agreement_tests;

/// The Album Artist and the composer, written into real files of the three
/// families the scanner reads them from, and read back the way the scan
/// reads them. Every test works on a copy in a temporary directory.
#[cfg(test)]
mod album_artist_and_composer_files;

#[cfg(test)]
mod sync_db_tests;
