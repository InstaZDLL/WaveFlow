//! Where the six MusicBrainz identifiers go in each format, and how the
//! two appliers write them.
//!
//! The convention is the one MusicBrainz's own tagger writes, which is
//! also the one lofty maps to its `MusicBrainz*` keys:
//!
//! | identifier         | Vorbis / FLAC                 | ID3v2                                   | MP4 (`----:com.apple.iTunes:…`) |
//! |--------------------|-------------------------------|-----------------------------------------|---------------------------------|
//! | release            | `MUSICBRAINZ_ALBUMID`         | `TXXX:MusicBrainz Album Id`             | `MusicBrainz Album Id`          |
//! | release group      | `MUSICBRAINZ_RELEASEGROUPID`  | `TXXX:MusicBrainz Release Group Id`     | `MusicBrainz Release Group Id`  |
//! | recording          | `MUSICBRAINZ_TRACKID`         | `UFID` owned by `http://musicbrainz.org` | `MusicBrainz Track Id`         |
//! | release track      | `MUSICBRAINZ_RELEASETRACKID`  | `TXXX:MusicBrainz Release Track Id`     | `MusicBrainz Release Track Id`  |
//! | track artists      | `MUSICBRAINZ_ARTISTID`        | `TXXX:MusicBrainz Artist Id`            | `MusicBrainz Artist Id`         |
//! | album artists      | `MUSICBRAINZ_ALBUMARTISTID`   | `TXXX:MusicBrainz Album Artist Id`      | `MusicBrainz Album Artist Id`   |
//!
//! "Track Id" naming the recording is the tagger's historical choice and
//! every reader follows it. The release identifier is the one the
//! enrichment engine reads back (`waveflow_core::enrichment::tags`).

use lofty::tag::ItemKey;

use super::MusicBrainzEdit;

/// The owner of the `UFID` frame that holds the recording.
pub(crate) const MUSICBRAINZ_UFID_OWNER: &str = "http://musicbrainz.org";

/// Where one identifier goes in an ID3v2 tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Id3Slot {
    /// A `TXXX` frame with this description.
    UserText(&'static str),
    /// The `UFID` frame owned by [`MUSICBRAINZ_UFID_OWNER`].
    Ufid,
}

/// One identifier: its lofty key, its ID3v2 frame, and whether it may
/// hold several values.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IdSlot {
    pub key: ItemKey,
    pub id3: Id3Slot,
    pub list: bool,
}

pub(crate) const RELEASE: IdSlot = IdSlot {
    key: ItemKey::MusicBrainzReleaseId,
    id3: Id3Slot::UserText("MusicBrainz Album Id"),
    list: false,
};
pub(crate) const RELEASE_GROUP: IdSlot = IdSlot {
    key: ItemKey::MusicBrainzReleaseGroupId,
    id3: Id3Slot::UserText("MusicBrainz Release Group Id"),
    list: false,
};
pub(crate) const RECORDING: IdSlot = IdSlot {
    key: ItemKey::MusicBrainzRecordingId,
    id3: Id3Slot::Ufid,
    list: false,
};
pub(crate) const RELEASE_TRACK: IdSlot = IdSlot {
    key: ItemKey::MusicBrainzTrackId,
    id3: Id3Slot::UserText("MusicBrainz Release Track Id"),
    list: false,
};
pub(crate) const ARTIST: IdSlot = IdSlot {
    key: ItemKey::MusicBrainzArtistId,
    id3: Id3Slot::UserText("MusicBrainz Artist Id"),
    list: true,
};
pub(crate) const ALBUM_ARTIST: IdSlot = IdSlot {
    key: ItemKey::MusicBrainzReleaseArtistId,
    id3: Id3Slot::UserText("MusicBrainz Album Artist Id"),
    list: true,
};

/// Every identifier an edit names, with the value asked for it.
pub(crate) fn musicbrainz_slots(edit: &MusicBrainzEdit) -> Vec<(IdSlot, &str)> {
    [
        (RELEASE, edit.release_id.as_deref()),
        (RELEASE_GROUP, edit.release_group_id.as_deref()),
        (RECORDING, edit.recording_id.as_deref()),
        (RELEASE_TRACK, edit.release_track_id.as_deref()),
        (ARTIST, edit.artist_id.as_deref()),
        (ALBUM_ARTIST, edit.album_artist_id.as_deref()),
    ]
    .into_iter()
    .filter_map(|(slot, value)| value.map(|v| (slot, v)))
    .collect()
}

/// The values of one identifier field: one for a single identifier, the
/// `;`-separated items of a list. An identifier never holds a `;`.
pub(crate) fn identifier_values(slot: IdSlot, value: &str) -> Vec<String> {
    let value = value.trim();
    if value.is_empty() {
        return Vec::new();
    }
    if !slot.list {
        return vec![value.to_ascii_lowercase()];
    }
    value
        .split(';')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Write the identifiers into a generic lofty tag. Each named key is
/// cleared first, so a value another tagger left under it cannot
/// outlive the one written.
///
/// A list goes in as one item per identifier — repeated Vorbis comments,
/// repeated MP4 freeform values — except in ID3v2, where one `TXXX`
/// frame holds them `\0`-separated: the ID3v2.4 multi-value form, which
/// lofty splits back into one item each on reading.
pub(super) fn apply_musicbrainz(tag: &mut lofty::tag::Tag, edit: &MusicBrainzEdit) {
    use lofty::tag::{ItemValue, TagItem, TagType};

    for (slot, value) in musicbrainz_slots(edit) {
        tag.remove_key(slot.key);
        let values = identifier_values(slot, value);
        if values.is_empty() {
            continue;
        }
        if tag.tag_type() == TagType::Id3v2 {
            tag.push(TagItem::new(slot.key, ItemValue::Text(values.join("\0"))));
        } else {
            for v in values {
                tag.push(TagItem::new(slot.key, ItemValue::Text(v)));
            }
        }
    }
}

/// The same, into an `id3` tag (DSF). A list is `\0`-joined in one
/// frame, as the lofty side writes it.
pub(super) fn apply_musicbrainz_id3(tag: &mut id3::Tag, edit: &MusicBrainzEdit) {
    use id3::TagLike;

    for (slot, value) in musicbrainz_slots(edit) {
        let values = identifier_values(slot, value);
        match slot.id3 {
            Id3Slot::UserText(description) => {
                tag.remove_extended_text(Some(description), None);
                if !values.is_empty() {
                    tag.add_frame(id3::frame::ExtendedText {
                        description: description.to_owned(),
                        value: values.join("\0"),
                    });
                }
            }
            Id3Slot::Ufid => {
                tag.remove_unique_file_identifier_by_owner_identifier(MUSICBRAINZ_UFID_OWNER);
                if let Some(id) = values.into_iter().next() {
                    tag.add_frame(id3::frame::UniqueFileIdentifier {
                        owner_identifier: MUSICBRAINZ_UFID_OWNER.to_owned(),
                        identifier: id.into_bytes(),
                    });
                }
            }
        }
    }
}

/// The identifiers an `id3` tag carries, for the workshop's reader: the
/// same slots read back, a list joined with `"; "`.
pub(crate) fn read_musicbrainz_id3(tag: &id3::Tag, slot: IdSlot) -> Option<String> {
    let raw: Vec<String> = match slot.id3 {
        Id3Slot::UserText(description) => tag
            .extended_texts()
            .filter(|t| t.description.eq_ignore_ascii_case(description))
            .flat_map(|t| {
                // `\0` is the 2.4 separator; an older tag written by
                // another tool may join with `/`, which no identifier
                // contains.
                t.value
                    .split(['\0', '/'])
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect(),
        Id3Slot::Ufid => tag
            .unique_file_identifiers()
            .filter(|u| u.owner_identifier == MUSICBRAINZ_UFID_OWNER)
            .filter_map(|u| String::from_utf8(u.identifier.clone()).ok())
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .take(1)
            .collect(),
    };
    (!raw.is_empty()).then(|| raw.join("; "))
}
