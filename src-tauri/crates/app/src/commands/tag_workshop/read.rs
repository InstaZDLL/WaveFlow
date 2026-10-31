//! A file's tags as the workshop shows them, and the edit that writes a
//! set of them.
//!
//! Read from the same tag the scanner reads (`primary_tag`, else the
//! first), through the same lofty probe, so what the table shows is what
//! a rescan would store. DSF has no lofty reader: its ID3v2 block is read
//! by the `id3` crate, at the offset its header declares — the block
//! `edit::patch_dsf` writes.

use std::path::Path;

use waveflow_core::scanner::split_genres;
use waveflow_core::tag_workshop::{normalize, validate, Field, Values};

use crate::commands::edit::{
    read_musicbrainz_id3, IdSlot, MusicBrainzEdit, TrackEdit, ALBUM_ARTIST, ARTIST, RECORDING,
    RELEASE, RELEASE_GROUP, RELEASE_TRACK,
};

/// The identifier fields and their slots, in one place for the reader.
const ID_FIELDS: [(Field, IdSlot); 6] = [
    (Field::MbReleaseId, RELEASE),
    (Field::MbReleaseGroupId, RELEASE_GROUP),
    (Field::MbRecordingId, RECORDING),
    (Field::MbReleaseTrackId, RELEASE_TRACK),
    (Field::MbArtistId, ARTIST),
    (Field::MbAlbumArtistId, ALBUM_ARTIST),
];

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Every field the workshop edits, as `path` carries it. A field the file
/// does not carry is absent. `Err` is a message for the row: the file
/// cannot be read, or its container cannot be written.
pub(crate) fn read_values(path: &Path) -> Result<Values, String> {
    match extension(path).as_str() {
        // The same refusal `patch_file` gives: indexed, never written.
        "dff" => Err("DFF files have no writable tag block in this build".into()),
        "dsf" => read_dsf(path),
        _ => read_lofty(path),
    }
}

fn put(values: &mut Values, field: Field, value: Option<String>) {
    if let Some(value) = value {
        let value = normalize(field, &value);
        if !value.is_empty() {
            values.insert(field, value);
        }
    }
}

fn read_lofty(path: &Path) -> Result<Values, String> {
    use lofty::file::TaggedFileExt;
    use lofty::prelude::*;
    use lofty::tag::ItemKey;

    let tagged = waveflow_core::scanner::extract::read_tagged_file(path)?;
    let mut values = Values::new();
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(values);
    };
    let number = |n: Option<u32>| n.map(|n| n.to_string());
    put(
        &mut values,
        Field::Title,
        tag.title().map(|s| s.into_owned()),
    );
    put(
        &mut values,
        Field::Artist,
        tag.artist().map(|s| s.into_owned()),
    );
    put(
        &mut values,
        Field::Album,
        tag.album().map(|s| s.into_owned()),
    );
    put(
        &mut values,
        Field::AlbumArtist,
        tag.get_string(ItemKey::AlbumArtist).map(str::to_owned),
    );
    put(
        &mut values,
        Field::Year,
        tag.date().map(|d| d.year.to_string()),
    );
    put(&mut values, Field::TrackNumber, number(tag.track()));
    put(&mut values, Field::TrackTotal, number(tag.track_total()));
    put(&mut values, Field::DiscNumber, number(tag.disk()));
    put(&mut values, Field::DiscTotal, number(tag.disk_total()));
    put(
        &mut values,
        Field::Genre,
        Some(split_genres(tag.get_strings(ItemKey::Genre)).join("; ")),
    );
    put(
        &mut values,
        Field::Composer,
        tag.get_string(ItemKey::Composer).map(str::to_owned),
    );
    for (field, slot) in ID_FIELDS {
        // ID3v2 holds a list as one `\0`-separated frame, which lofty
        // splits on reading; split again in case another tool did not.
        let ids: Vec<&str> = tag
            .get_strings(slot.key)
            .flat_map(|v| v.split('\0'))
            .collect();
        put(&mut values, field, Some(ids.join("; ")));
    }
    Ok(values)
}

fn read_dsf(path: &Path) -> Result<Values, String> {
    use id3::TagLike;
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let layout = waveflow_core::tagio::read_dsf_layout(&mut file)
        .map_err(|e| e.to_string())?
        .ok_or("this DSF file's header does not describe itself")?;
    let mut values = Values::new();
    if layout.metadata_offset == 0 {
        return Ok(values);
    }
    file.seek(SeekFrom::Start(layout.metadata_offset))
        .map_err(|e| e.to_string())?;
    let tag = match id3::Tag::read_from2(&mut file) {
        Ok(tag) => tag,
        Err(err) if matches!(err.kind, id3::ErrorKind::NoTag) => return Ok(values),
        Err(err) => return Err(err.to_string()),
    };
    let number = |n: Option<u32>| n.map(|n| n.to_string());
    put(&mut values, Field::Title, tag.title().map(str::to_owned));
    put(&mut values, Field::Artist, tag.artist().map(str::to_owned));
    put(&mut values, Field::Album, tag.album().map(str::to_owned));
    put(
        &mut values,
        Field::AlbumArtist,
        tag.album_artist().map(str::to_owned),
    );
    put(
        &mut values,
        Field::Year,
        tag.date_recorded()
            .map(|d| d.year)
            .or_else(|| tag.year())
            .map(|y| y.to_string()),
    );
    put(&mut values, Field::TrackNumber, number(tag.track()));
    put(&mut values, Field::TrackTotal, number(tag.total_tracks()));
    put(&mut values, Field::DiscNumber, number(tag.disc()));
    put(&mut values, Field::DiscTotal, number(tag.total_discs()));
    put(
        &mut values,
        Field::Genre,
        tag.genres().map(|g| split_genres(g).join("; ")),
    );
    put(
        &mut values,
        Field::Composer,
        tag.get("TCOM")
            .and_then(|f| f.content().text())
            .map(str::to_owned),
    );
    for (field, slot) in ID_FIELDS {
        put(&mut values, field, read_musicbrainz_id3(&tag, slot));
    }
    Ok(values)
}

/// The first value of `values` that cannot be written, as
/// `(field, reason)`.
pub(crate) fn first_invalid(values: &Values) -> Option<(Field, &'static str)> {
    values
        .iter()
        .find_map(|(field, value)| validate(*field, value).map(|reason| (*field, reason)))
}

/// The edit that writes `values` into a file: each named field set, an
/// empty one cleared, every other field left alone. Call
/// [`first_invalid`] first — a value that does not parse is cleared here.
pub(crate) fn to_edit(values: &Values) -> TrackEdit {
    let mut edit = TrackEdit::default();
    let mut ids = MusicBrainzEdit::default();
    for (&field, value) in values {
        let value = normalize(field, value);
        // A number is `0` to clear, which is what `apply_patch` reads
        // as "remove".
        let number = || value.parse::<i64>().unwrap_or(0);
        match field {
            Field::Title => edit.title = Some(value),
            Field::Artist => edit.artist = Some(value),
            Field::Album => edit.album = Some(value),
            Field::AlbumArtist => edit.album_artist = Some(value),
            Field::Year => edit.year = Some(number()),
            Field::TrackNumber => edit.track_number = Some(number()),
            Field::TrackTotal => edit.track_total = Some(number()),
            Field::DiscNumber => edit.disc_number = Some(number()),
            Field::DiscTotal => edit.disc_total = Some(number()),
            Field::Genre => edit.genre = Some(value),
            Field::Composer => edit.composer = Some(value),
            Field::MbReleaseId => ids.release_id = Some(value),
            Field::MbReleaseGroupId => ids.release_group_id = Some(value),
            Field::MbRecordingId => ids.recording_id = Some(value),
            Field::MbReleaseTrackId => ids.release_track_id = Some(value),
            Field::MbArtistId => ids.artist_id = Some(value),
            Field::MbAlbumArtistId => ids.album_artist_id = Some(value),
        }
    }
    edit.musicbrainz = ids;
    edit
}

/// The same edit, as the database stores it: a title cleared from the
/// file is the file's name in the library, which is what the scanner
/// would store for an untitled file.
pub(crate) fn to_db_edit(values: &Values, path: &Path) -> TrackEdit {
    let mut edit = to_edit(values);
    if edit.title.as_deref() == Some("") {
        edit.title = Some(
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_owned(),
        );
    }
    edit
}
