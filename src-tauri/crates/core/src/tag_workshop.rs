//! The tag workshop: many files' tags edited at once, matched against
//! MusicBrainz if asked, written only once their differences have been
//! seen, and undone from a journal.
//!
//! What is here is the part that needs neither a file nor the app:
//!
//! - the fields the workshop edits, and the one text form every value
//!   takes on its way between a file, the table and the journal
//!   ([`Field`], [`normalize`], [`validate`]);
//! - the difference between what a file says and what the table asks
//!   for, field by field ([`diff`]) — what the preview shows and what the
//!   journal keeps;
//! - [`proposal`] — the tags a matched MusicBrainz release gives each
//!   file (`musicbrainz` feature);
//! - [`journal`] — the batches written and the old value of every field
//!   they wrote, so a batch can be undone (`sqlite` feature).
//!
//! Reading and writing the files is the app's: it goes through the
//! editor's `patch_file`. How the whole runs: `docs/features/tag-workshop.md`.

#[cfg(feature = "sqlite")]
pub mod journal;
#[cfg(feature = "musicbrainz")]
pub mod proposal;

use std::collections::BTreeMap;

/// One tag the workshop edits.
///
/// The serialized names are the contract with the frontend's columns
/// and with the journal's `field` column, so they are as stable as a
/// migration: never renamed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    /// Every credited artist, `"; "`-joined — the library's one
    /// multi-artist separator.
    Artist,
    Album,
    AlbumArtist,
    Year,
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
    /// Every genre, `"; "`-joined.
    Genre,
    Composer,
    /// `MUSICBRAINZ_ALBUMID` / `TXXX:MusicBrainz Album Id` / the MP4
    /// freeform atom of the same name.
    MbReleaseId,
    MbReleaseGroupId,
    /// `MUSICBRAINZ_TRACKID` / the ID3v2 `UFID` frame owned by
    /// `http://musicbrainz.org` / `MusicBrainz Track Id` — all three
    /// name the recording, whatever the key says.
    MbRecordingId,
    /// `MUSICBRAINZ_RELEASETRACKID` / `MusicBrainz Release Track Id`.
    MbReleaseTrackId,
    /// Every artist of the track's credit, `"; "`-joined.
    MbArtistId,
    /// Every artist of the release's credit, `"; "`-joined.
    MbAlbumArtistId,
}

impl Field {
    pub const ALL: [Field; 17] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Year,
        Field::TrackNumber,
        Field::TrackTotal,
        Field::DiscNumber,
        Field::DiscTotal,
        Field::Genre,
        Field::Composer,
        Field::MbReleaseId,
        Field::MbReleaseGroupId,
        Field::MbRecordingId,
        Field::MbReleaseTrackId,
        Field::MbArtistId,
        Field::MbAlbumArtistId,
    ];

    /// The name the journal stores and the frontend sends.
    pub fn as_str(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Artist => "artist",
            Field::Album => "album",
            Field::AlbumArtist => "album_artist",
            Field::Year => "year",
            Field::TrackNumber => "track_number",
            Field::TrackTotal => "track_total",
            Field::DiscNumber => "disc_number",
            Field::DiscTotal => "disc_total",
            Field::Genre => "genre",
            Field::Composer => "composer",
            Field::MbReleaseId => "mb_release_id",
            Field::MbReleaseGroupId => "mb_release_group_id",
            Field::MbRecordingId => "mb_recording_id",
            Field::MbReleaseTrackId => "mb_release_track_id",
            Field::MbArtistId => "mb_artist_id",
            Field::MbAlbumArtistId => "mb_album_artist_id",
        }
    }

    pub fn parse(name: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.as_str() == name)
    }

    /// A number: kept as its decimal digits, `""` for none.
    pub fn is_number(self) -> bool {
        matches!(
            self,
            Field::Year
                | Field::TrackNumber
                | Field::TrackTotal
                | Field::DiscNumber
                | Field::DiscTotal
        )
    }

    /// A MusicBrainz identifier, or a list of them.
    pub fn is_identifier(self) -> bool {
        matches!(
            self,
            Field::MbReleaseId
                | Field::MbReleaseGroupId
                | Field::MbRecordingId
                | Field::MbReleaseTrackId
                | Field::MbArtistId
                | Field::MbAlbumArtistId
        )
    }

    /// A field that may hold several values, `"; "`-joined.
    pub fn is_list(self) -> bool {
        matches!(
            self,
            Field::Artist | Field::Genre | Field::MbArtistId | Field::MbAlbumArtistId
        )
    }
}

/// A file's tags, or the tags asked of it, as text. A field that is
/// absent from the map is absent from the file — or, in a change, left
/// alone. `""` in a change clears the field.
pub type Values = BTreeMap<Field, String>;

/// The one text form of a value, so the preview never reports a
/// difference a file would not show: surrounding spaces gone, a number
/// without its leading zeros (`0` is none), an identifier in lower case,
/// a list re-joined with `"; "` and without its empty items.
pub fn normalize(field: Field, value: &str) -> String {
    let value = value.trim();
    if field.is_number() {
        return match value.parse::<u64>() {
            Ok(0) => String::new(),
            Ok(n) => n.to_string(),
            // Not a number: kept as typed, so [`validate`] can name it.
            Err(_) => value.to_owned(),
        };
    }
    if field == Field::Genre {
        // Split and deduplicated as the tag writer does, so the preview
        // never shows a change the write cannot make ("Rock; rock" is
        // written as "Rock", "Alternative\Rock" as two genres).
        return crate::scanner::split_genres([value]).join("; ");
    }
    if field.is_list() {
        // The artist list splits on `"; "` alone — a comma belongs to
        // names like "Earth, Wind & Fire". The others are lists of
        // genres or identifiers, where a bare `;` separates too.
        let parts: Vec<String> = if field == Field::Artist {
            value
                .split("; ")
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        } else {
            value
                .split(';')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| {
                    if field.is_identifier() {
                        p.to_ascii_lowercase()
                    } else {
                        p.to_owned()
                    }
                })
                .collect()
        };
        return parts.join("; ");
    }
    if field.is_identifier() {
        return value.to_ascii_lowercase();
    }
    value.to_owned()
}

/// Why a value cannot be written, or `None`. The reason is a key the
/// frontend translates.
pub fn validate(field: Field, value: &str) -> Option<&'static str> {
    let value = normalize(field, value);
    if value.is_empty() {
        return None;
    }
    if field.is_number() {
        let max = if field == Field::Year {
            9999
        } else {
            u64::from(u16::MAX)
        };
        return match value.parse::<u64>() {
            Ok(n) if n <= max => None,
            Ok(_) => Some("out_of_range"),
            Err(_) => Some("not_a_number"),
        };
    }
    if field.is_identifier() && !value.split("; ").all(is_identifier) {
        return Some("not_an_identifier");
    }
    None
}

/// A MusicBrainz identifier: a UUID in its 36-character form.
pub fn is_identifier(value: &str) -> bool {
    value.len() == 36 && uuid::Uuid::parse_str(value).is_ok()
}

/// One field a change would rewrite.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FieldDiff {
    pub field: Field,
    /// What the file says now, `""` for nothing.
    pub old: String,
    /// What it will say, `""` for nothing.
    pub new: String,
}

/// The fields `wanted` would change in a file whose tags are `current`,
/// in [`Field`] order, each value [`normalize`]d. A field `wanted` does
/// not name is left alone; one it names with the value the file already
/// has is not a change.
pub fn diff(current: &Values, wanted: &Values) -> Vec<FieldDiff> {
    wanted
        .iter()
        .filter_map(|(&field, new)| {
            let old = current
                .get(&field)
                .map(|v| normalize(field, v))
                .unwrap_or_default();
            let new = normalize(field, new);
            (old != new).then_some(FieldDiff { field, old, new })
        })
        .collect()
}

/// `wanted`, with the MusicBrainz artist ids its new names imply (#1076).
///
/// A new artist credit or Album Artist written over a file whose ids
/// belong to the old names would leave those ids behind, and the next
/// scan, finding the artist by them, would put the track back. So when
/// `wanted` renames without naming ids of its own, the ids follow the
/// rule the tag editor writes by:
///
/// - the artist ids as
///   [`reconcile_artist_mbids`](crate::scanner::reconcile_artist_mbids)
///   gives them: kept for a spelling fix, reordered with their names, all
///   removed once one name is another artist;
/// - the Album Artist ids removed when the new name is another artist, or
///   when the file holds several Album Artist values (`album_artist_values`,
///   all of them `"; "`-joined; `current` holds the first): the write
///   leaves one name, and an id that was nobody's beside two would become
///   that one's.
///
/// They become ordinary changes of the batch: the preview shows them, the
/// journal records their old values, and an undo puts them back.
pub fn with_implied_ids(current: &Values, wanted: &Values, album_artist_values: &str) -> Values {
    use crate::scanner::{artist_mbids, canonical_name, reconcile_artist_mbids};

    let mut out = wanted.clone();
    let now = |field: Field| current.get(&field).map(String::as_str).unwrap_or("");
    if let (Some(credit), false) = (
        wanted.get(&Field::Artist),
        wanted.contains_key(&Field::MbArtistId),
    ) {
        let ids = artist_mbids([now(Field::MbArtistId)]);
        if let Some(kept) = reconcile_artist_mbids(now(Field::Artist), &ids, credit) {
            out.insert(Field::MbArtistId, kept.join("; "));
        }
    }
    if let (Some(name), false) = (
        wanted.get(&Field::AlbumArtist),
        wanted.contains_key(&Field::MbAlbumArtistId),
    ) {
        let held = !artist_mbids([now(Field::MbAlbumArtistId)]).is_empty();
        let several = album_artist_values
            .split("; ")
            .filter(|v| !v.trim().is_empty())
            .count()
            > 1;
        let renamed = canonical_name(name) != canonical_name(now(Field::AlbumArtist));
        if held && (several || renamed) {
            out.insert(Field::MbAlbumArtistId, String::new());
        }
    }
    out
}

/// What a write of `asked` does to a file whose tags are `current`, with
/// the ids its new names imply ([`with_implied_ids`]), or `None` when the
/// file changed since the preview the write follows.
///
/// `expected` is the old value of every field the preview listed. Every
/// difference the write would make is held to it, the implied ones too:
/// an id another tagger wrote after the preview would otherwise be
/// removed by a change the user never saw.
pub fn planned_write(
    current: &Values,
    asked: &Values,
    expected: &Values,
    album_artist_values: &str,
) -> Option<Vec<FieldDiff>> {
    let diffs = diff(
        current,
        &with_implied_ids(current, asked, album_artist_values),
    );
    let seen: Values = diffs.iter().map(|d| (d.field, d.old.clone())).collect();
    let expected: Values = expected
        .iter()
        .map(|(f, v)| (*f, normalize(*f, v)))
        .collect();
    (seen == expected).then_some(diffs)
}

#[cfg(test)]
mod tests;
