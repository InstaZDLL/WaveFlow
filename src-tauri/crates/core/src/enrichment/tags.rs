//! The MusicBrainz identifiers a file already carries.
//!
//! A library tagged with a MusicBrainz tagger holds the release
//! identifier in every file (`MUSICBRAINZ_ALBUMID`, the ID3 `TXXX`
//! "MusicBrainz Album Id", the MP4 freeform atom). The scanner does not
//! store it — `scanner::extra_tags` leaves the `MUSICBRAINZ*` keys out of
//! the custom columns on purpose — so the engine reads it here, from one
//! file of the album, when it is about to look the album up. A release
//! named by the file needs no search.
//!
//! Read only. Nothing here opens a file for writing.

use std::path::Path;

use lofty::config::ParseOptions;
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use lofty::tag::ItemKey;

use crate::metadata::musicbrainz::is_mbid;

/// The release identifier `path`'s tags name, when they name a valid one.
pub fn release_mbid(path: &Path) -> Option<String> {
    // Tags only: the stream properties would cost a header walk the
    // engine has no use for.
    let tagged = Probe::open(path)
        .ok()?
        .options(
            ParseOptions::new()
                .read_properties(false)
                .read_cover_art(false),
        )
        .guess_file_type()
        .ok()?
        .read()
        .ok()?;
    tagged.tags().iter().find_map(|tag| {
        tag.get_string(ItemKey::MusicBrainzReleaseId)
            .map(str::trim)
            .filter(|id| is_mbid(id))
            .map(|id| id.to_ascii_lowercase())
    })
}
