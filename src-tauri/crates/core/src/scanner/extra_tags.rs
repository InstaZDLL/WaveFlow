//! The tags a file carries that the generic `Tag` cannot model (#588).
//!
//! Track lists shipped one fixed set of columns, so somebody whose
//! library is organised around a field we don't show — a composer, a
//! catalogue number, a rip source kept in a custom frame — had no way
//! to see it. Showing one means storing it, and storing it means
//! reading it.
//!
//! # Why this needs a second read of the file
//!
//! The issue assumed the reading already existed, because `edit.rs`
//! goes through the concrete tag type per format precisely so
//! non-standard frames survive a save. That is true of the **writer**.
//! The scanner reads through `lofty::read_from_path`, which hands back
//! generic `Tag`s produced by *splitting* each concrete tag: what has
//! an `ItemKey` mapping goes into the `Tag`, and the rest goes into a
//! remainder. `ItemKey` has no `Unknown` variant, and the remainder is
//! stashed in a `pub(crate)` companion slot — unreachable from outside
//! lofty, and for Vorbis comments not kept at all.
//!
//! So the custom frames are simply not present in what the scanner
//! already parsed, and the only way to see them is to open the concrete
//! container. The cost is one extra tag parse per file — the header
//! region, which the first parse has just pulled into the page cache —
//! and it is timed separately by the scanner so it shows up in the
//! per-scan log rather than hiding inside the total.
//!
//! # What counts as a custom tag
//!
//! Whatever is left after lofty has taken everything it can model. That
//! is the honest definition: it is exactly the set of values no other
//! part of WaveFlow can already show, which is what makes a column for
//! one of them worth having.

use std::path::Path;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType};
use lofty::probe::Probe;
use lofty::tag::SplitTag;

/// Longest key and value we keep.
///
/// A tag can hold a whole lyric sheet or an embedded cue sheet, and a
/// column showing one of those is a column showing a wall of text. The
/// cap keeps the side table proportional to the library rather than to
/// whatever the largest frame in it happens to be.
const MAX_KEY: usize = 64;
const MAX_VALUE: usize = 512;

/// `ItemKey`s WaveFlow already stores in `track`, `album` or `artist`.
///
/// These have their own columns and their own UI, so a `tag:` column
/// for one of them would be a second, stale copy of something the
/// library models properly. Everything else lofty *can* map is fair
/// game — and that is where the issue's own headline example lives:
/// `Composer` has an `ItemKey`, so it never reaches the remainder, and
/// a version of this that only read the remainder missed the one field
/// the issue opens with.
fn is_modelled(key: lofty::tag::ItemKey) -> bool {
    use lofty::tag::ItemKey as K;
    matches!(
        key,
        K::TrackTitle
            | K::TrackArtist
            | K::TrackArtists
            | K::AlbumTitle
            | K::AlbumArtist
            | K::TrackNumber
            | K::TrackTotal
            | K::DiscNumber
            | K::DiscTotal
            | K::Genre
            | K::Year
            | K::RecordingDate
            | K::ReleaseDate
            | K::Popularimeter
            | K::InitialKey
            | K::FlagCompilation
            | K::Lyrics
            | K::ReplayGainTrackGain
            | K::ReplayGainTrackPeak
            | K::ReplayGainAlbumGain
            | K::ReplayGainAlbumPeak
    )
}

/// Keys we never store, because WaveFlow already owns the value and a
/// column for it would show a stale copy of something the library
/// models properly.
///
/// Matched case-insensitively. `SYNCEDLYRICS` is ours (written by
/// `commands::lyrics`), the ReplayGain family is read into `track`
/// columns by the scanner itself, and the MusicBrainz identifiers are
/// long opaque UUIDs nobody wants in a table cell.
fn is_boring(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.starts_with("REPLAYGAIN_")
        || upper.starts_with("MUSICBRAINZ")
        || matches!(
            upper.as_str(),
            "SYNCEDLYRICS" | "LYRICS" | "UNSYNCEDLYRICS" | "COVERART" | "CUESHEET"
        )
}

fn keep(out: &mut Vec<(String, String)>, key: &str, value: &str) {
    // Upper-cased here, once, for every source. The column's *identity*
    // is this string: it is what the picker lists, what the stored
    // layout names, and what the cell lookup binds. `COLLATE NOCASE` on
    // the column makes those match whatever the case, but it does not
    // decide which spelling comes back from a `GROUP BY` -- so a
    // library holding both `Composer` and `COMPOSER` would offer one
    // column under a name that changes between reads. Three of the four
    // sources already answered in upper case; this is the fourth, a
    // `TXXX` description, brought into line rather than left to whoever
    // wrote the file.
    let key = key.trim().to_ascii_uppercase();
    let key = key.as_str();
    let value = value.trim();
    if key.is_empty() || value.is_empty() || key.len() > MAX_KEY || is_boring(key) {
        return;
    }
    // Compared without regard to case, which is not the same question
    // as how the key is *stored*. The halves feeding one file name the
    // same field differently: the generic half comes through
    // `map_key`, which answers in upper case, the APEv2 and Vorbis
    // halves are upper-cased at their call sites, and a `TXXX`
    // description keeps whatever case the tagger wrote. So `COMPOSER`
    // from a `TCOM` frame and `Composer` from a `TXXX` frame are one
    // field arriving twice, and a case-sensitive test would offer the
    // user two columns for it — the exact duplication the upper-casing
    // exists to prevent.
    //
    // Two `TXXX` frames differing only in case are, by the letter of
    // ID3v2, two distinct frames. They are still not two columns: a
    // column is one cell, so the rule below covers them as it covers
    // any repeated key.
    if out
        .iter()
        .any(|(existing, _)| existing.eq_ignore_ascii_case(key))
    {
        // A repeated key is a multi-value field. The first wins rather
        // than the values being joined: a column is one cell, and
        // joining would make a cell that is right for nobody.
        return;
    }
    let value = if value.len() > MAX_VALUE {
        // Cut on a character boundary, not a byte one.
        let end = value
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|i| *i <= MAX_VALUE)
            .last()
            .unwrap_or(0);
        &value[..end]
    } else {
        value
    };
    out.push((key.to_string(), value.to_string()));
}

/// Read the custom tags of one file.
///
/// `None` means **could not read** — an unsupported container, a parse
/// failure, a file that vanished. `Some(vec![])` means read, and there
/// were none.
///
/// The distinction is not pedantry: the scanner writes these by
/// delete-then-insert, so folding a failure into "no tags" would erase
/// a track's existing tags every time a file happened to be locked by
/// another process for the length of one parse.
pub fn read_extra_tags(path: &Path) -> Option<Vec<(String, String)>> {
    read_inner(path)
}

fn read_inner(path: &Path) -> Option<Vec<(String, String)>> {
    // One open, not two. `Probe::open` already has the file, and
    // `guess_inner` restores the stream position it read from -- so the
    // reader it hands back is at the start and ready to parse. Opening
    // the path a second time costs a syscall per track in the scan's
    // hot path, and races anything that moves the file between the two.
    // It is a `BufReader` besides, which the bare `File` was not.
    let probe = Probe::open(path).ok()?.guess_file_type().ok()?;
    let file_type = probe.file_type()?;
    let mut handle = probe.into_inner();
    // Tags only. This is a *second* parse of every file in the scan's
    // hot path, and the first one already read the properties and
    // lifted the cover art — doing either again would double the one
    // cost this feature adds for nothing.
    let options = ParseOptions::new()
        .read_properties(false)
        .read_cover_art(false);
    let mut out = Vec::new();

    match file_type {
        // ID3v2 containers: the custom values are `TXXX` frames, whose
        // description is the key.
        FileType::Mpeg => {
            let file = lofty::mpeg::MpegFile::read_from(&mut handle, options).ok()?;
            id3v2_into(file.id3v2(), &mut out);
            // An MP3 can carry an APEv2 tag beside its ID3v2 one, and
            // taggers that write custom fields there do exist. Read
            // second, so a key present in both keeps the ID3v2 value —
            // `keep` lets the first writer win, and ID3v2 is what every
            // other reader in WaveFlow looks at.
            ape_into(file.ape(), &mut out);
        }
        FileType::Aac => {
            let file = lofty::aac::AacFile::read_from(&mut handle, options).ok()?;
            id3v2_into(file.id3v2(), &mut out);
        }
        FileType::Wav => {
            let file = lofty::iff::wav::WavFile::read_from(&mut handle, options).ok()?;
            id3v2_into(file.id3v2(), &mut out);
            // A WAV's native tag is its RIFF INFO chunk; the ID3v2 one
            // is the bolt-on. Read second so an ID3v2 value wins a
            // collision, matching the MP3 branch.
            //
            // Iterated directly rather than through `split_tag`: RIFF
            // INFO's remainder is a unit struct, so the split throws
            // away every key lofty cannot map — which is exactly the
            // set this function exists to find.
            if let Some(riff) = file.riff_info() {
                for (key, value) in riff {
                    // `IENG` is the same field a FLAC spells
                    // `ENGINEER`, and offering them as two columns is
                    // the split `generic_into` goes out of its way to
                    // avoid. So a recognised chunk id is renamed the
                    // way that half names things, and only a code lofty
                    // has no mapping for keeps its raw spelling.
                    match lofty::tag::ItemKey::from_key(lofty::tag::TagType::RiffInfo, key) {
                        Some(item_key) if is_modelled(item_key) => {}
                        Some(item_key) => {
                            let name = item_key
                                .map_key(lofty::tag::TagType::VorbisComments)
                                .map(str::to_string)
                                .unwrap_or_else(|| key.to_ascii_uppercase());
                            keep(&mut out, &name, value);
                        }
                        None => keep(&mut out, key, value),
                    }
                }
            }
        }
        // The one container the tag editor could write and this could
        // not read. `edit.rs` has an `AiffFile` arm, so a custom tag
        // added through the editor landed in the file -- and then the
        // next scan fell through to `_ => {}`, answered `Some(vec![])`,
        // and `write_extra_tags` cleared the row for it. The value
        // stayed in the file and vanished from the library, on every
        // rescan, for good.
        FileType::Aiff => {
            let file = lofty::iff::aiff::AiffFile::read_from(&mut handle, options).ok()?;
            // ID3v2 only, unlike the WAV arm beside it. AIFF's native
            // metadata is its text chunks, and those are not a
            // key-value map: `AiffTextChunks` is a struct of five fixed
            // fields (name, author, copyright, annotations, comments),
            // every one of them already modelled. There is no room in
            // it for a custom key, so there is nothing there to find.
            id3v2_into(file.id3v2(), &mut out);
        }
        // Vorbis comments: the key is the comment name, so anything
        // non-standard is already in the shape we want.
        FileType::Flac => {
            let file = lofty::flac::FlacFile::read_from(&mut handle, options).ok()?;
            vorbis_into(file.vorbis_comments(), &mut out);
        }
        FileType::Vorbis => {
            let file = lofty::ogg::VorbisFile::read_from(&mut handle, options).ok()?;
            vorbis_into(Some(file.vorbis_comments()), &mut out);
        }
        FileType::Opus => {
            let file = lofty::ogg::OpusFile::read_from(&mut handle, options).ok()?;
            vorbis_into(Some(file.vorbis_comments()), &mut out);
        }
        FileType::Speex => {
            let file = lofty::ogg::SpeexFile::read_from(&mut handle, options).ok()?;
            vorbis_into(Some(file.vorbis_comments()), &mut out);
        }
        // MP4 freeform atoms (`----:com.apple.iTunes:KEY`).
        FileType::Mp4 => {
            let file = lofty::mp4::Mp4File::read_from(&mut handle, options).ok()?;
            ilst_into(file.ilst(), &mut out);
        }
        FileType::Ape => {
            let file = lofty::ape::ApeFile::read_from(&mut handle, options).ok()?;
            ape_into(file.ape(), &mut out);
        }
        // A container we do not read custom frames from — DSF and DFF,
        // which have their own pipeline, and anything lofty recognises
        // that we have not wired up. Falls through to `Some(vec![])`,
        // not `None`: we opened the file and there is nothing here to
        // offer, which is what lets the scanner clear rows a file kept
        // from before it was re-encoded into such a container. `None`
        // stays reserved for "could not read", which leaves the stored
        // rows alone.
        _ => {}
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    Some(out)
}

/// The half of a split that lofty *could* map, minus what WaveFlow
/// already stores.
///
/// Composer, Comment, Publisher, ISRC, Lyricist, Conductor, Mood,
/// Barcode, CatalogNumber — all of these have an `ItemKey`, so none of
/// them lands in the remainder, and none of them has a column of its
/// own in WaveFlow. They are exactly the fields somebody organises a
/// library around, which is what this feature is for.
///
/// Named through `map_key(TagType::VorbisComments)` so the same field
/// reads as the same key whatever container it came from — `COMPOSER`
/// for a FLAC and for an MP3 alike, rather than `COMPOSER` and `TCOM`
/// being offered as two different columns.
fn generic_into(tag: &lofty::tag::Tag, out: &mut Vec<(String, String)>) {
    use lofty::tag::{ItemValue, TagType};
    for item in tag.items() {
        if is_modelled(item.key()) {
            continue;
        }
        let ItemValue::Text(value) = item.value() else {
            continue;
        };
        let Some(name) = item.key().map_key(TagType::VorbisComments) else {
            continue;
        };
        keep(out, name, value);
    }
}

fn id3v2_into(tag: Option<&lofty::id3::v2::Id3v2Tag>, out: &mut Vec<(String, String)>) {
    let Some(tag) = tag else { return };
    // Both halves of the split: the remainder holds what lofty could
    // not map at all, the generic half holds what it mapped onto a key
    // WaveFlow has no column for.
    let (remainder, generic) = tag.clone().split_tag();
    generic_into(&generic, out);
    // `SplitTagRemainder` derefs to the concrete tag, which is what
    // carries the iterator.
    for frame in &*remainder {
        if let lofty::id3::v2::Frame::UserText(text) = frame {
            keep(out, &text.description, &text.content);
        }
    }
}

fn vorbis_into(tag: Option<&lofty::ogg::tag::VorbisComments>, out: &mut Vec<(String, String)>) {
    let Some(tag) = tag else { return };
    let (remainder, generic) = tag.clone().split_tag();
    generic_into(&generic, out);
    for (key, value) in remainder.items() {
        // Vorbis comment names are case-insensitive by spec, so
        // `SOURCE` and `source` are one field — and a file written by
        // two taggers routinely carries both spellings. `keep`
        // normalises the case for every source, so nothing is needed
        // here beyond handing it the name as written.
        keep(out, key, value);
    }
}

fn ilst_into(tag: Option<&lofty::mp4::Ilst>, out: &mut Vec<(String, String)>) {
    let Some(tag) = tag else { return };
    let (remainder, generic) = tag.clone().split_tag();
    generic_into(&generic, out);
    for atom in &*remainder {
        // Only the freeform atoms carry a name a user would recognise;
        // a four-character code like `©too` is an encoder string, not a
        // field anybody organises a library around.
        let lofty::mp4::AtomIdent::Freeform { name, .. } = atom.ident() else {
            continue;
        };
        for data in atom.data() {
            if let lofty::mp4::AtomData::UTF8(value) | lofty::mp4::AtomData::UTF16(value) = data {
                keep(out, name.as_ref(), value);
                break;
            }
        }
    }
}

fn ape_into(tag: Option<&lofty::ape::ApeTag>, out: &mut Vec<(String, String)>) {
    let Some(tag) = tag else { return };
    let (remainder, generic) = tag.clone().split_tag();
    generic_into(&generic, out);
    for item in &*remainder {
        if let lofty::tag::ItemValue::Text(value) = item.value() {
            // Same as the Vorbis half: lofty compares APEv2 keys
            // without regard to case, so `SOURCE` and `source` are one
            // field and must not become two columns. `keep` is where
            // that is settled.
            keep(out, item.key(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but valid AIFF, so lofty will open it and write to it.
    fn tiny_aiff() -> Vec<u8> {
        let frames: u32 = 8;
        let channels: u16 = 2;
        let sample_size: u16 = 16;
        let data_len = frames as usize * channels as usize * 2;

        let mut comm = Vec::new();
        comm.extend_from_slice(&channels.to_be_bytes());
        comm.extend_from_slice(&frames.to_be_bytes());
        comm.extend_from_slice(&sample_size.to_be_bytes());
        comm.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);

        let mut ssnd = Vec::new();
        ssnd.extend_from_slice(&0u32.to_be_bytes());
        ssnd.extend_from_slice(&0u32.to_be_bytes());
        ssnd.resize(8 + data_len, 0);

        let mut body = Vec::new();
        body.extend_from_slice(b"AIFF");
        body.extend_from_slice(b"COMM");
        body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
        body.extend_from_slice(&comm);
        body.extend_from_slice(b"SSND");
        body.extend_from_slice(&((8 + data_len) as u32).to_be_bytes());
        body.extend_from_slice(&ssnd);

        let mut out = Vec::new();
        out.extend_from_slice(b"FORM");
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// AIFF was the one container the tag editor could write and this
    /// module could not read. It fell through to the catch-all arm and
    /// answered `Some(vec![])` -- "opened it, nothing here" -- which is
    /// the answer that tells the scanner to clear the track's stored
    /// rows. So a custom tag added through the editor went into the
    /// file and left the library on the next scan, every scan.
    ///
    /// Written with lofty and read back, rather than asserting on a
    /// match arm: the arm existing proves nothing about whether it
    /// finds what the writer put there.
    #[test]
    fn a_custom_tag_written_into_an_aiff_is_read_back() {
        use lofty::config::WriteOptions;
        use lofty::id3::v2::{ExtendedTextFrame, Frame, Id3v2Tag};
        use lofty::tag::{Accessor, TagExt};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.aiff");
        std::fs::write(&path, tiny_aiff()).expect("write aiff");

        // The concrete tag, not the generic one: `Tag` has no way to
        // carry a `TXXX` frame -- which is the whole reason this module
        // exists, and why writing through it would test nothing.
        let mut tag = Id3v2Tag::default();
        tag.set_title("Ghost".to_string());
        tag.insert(Frame::UserText(ExtendedTextFrame::new(
            lofty::TextEncoding::UTF8,
            "MY_CUSTOM".to_string(),
            "kept".to_string(),
        )));
        tag.save_to_path(&path, WriteOptions::default())
            .expect("save id3v2 into the aiff");

        // The file really is an AIFF lofty recognises, not something
        // that happens to parse: a wrong answer here would make the
        // assertion below pass for the wrong reason.
        let probed = Probe::open(&path)
            .expect("probe")
            .guess_file_type()
            .expect("guess");
        assert_eq!(probed.file_type(), Some(FileType::Aiff));

        let tags = read_extra_tags(&path).expect("aiff opened");
        assert!(
            tags.iter().any(|(k, v)| k == "MY_CUSTOM" && v == "kept"),
            "custom tag lost: {tags:?}"
        );
        // The title is modelled, so it is not offered as a custom
        // column -- the same rule every other container follows here.
        assert!(
            !tags.iter().any(|(k, _)| k.eq_ignore_ascii_case("TITLE")),
            "a modelled field leaked into the custom set: {tags:?}"
        );
    }

    #[test]
    fn a_value_is_cut_on_a_character_boundary() {
        let mut out = Vec::new();
        // Multi-byte characters straddling the cap: slicing on the byte
        // index alone would panic here.
        let long = "é".repeat(MAX_VALUE);
        keep(&mut out, "NOTE", &long);
        assert_eq!(out.len(), 1);
        assert!(out[0].1.len() <= MAX_VALUE + 1);
        assert!(out[0].1.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_first_value_of_a_repeated_key_wins() {
        let mut out = Vec::new();
        keep(&mut out, "SOURCE", "CD");
        keep(&mut out, "SOURCE", "Vinyl");
        assert_eq!(out, vec![("SOURCE".to_string(), "CD".to_string())]);
    }

    /// The same field reaching `keep` under two spellings is one
    /// column, not two. An MP3 carrying `TCOM` and a `TXXX:Composer`
    /// is the case that motivated this: the generic half names the
    /// first `COMPOSER`, the remainder hands over the description
    /// verbatim, and a case-sensitive test would offer both.
    #[test]
    fn a_key_repeated_in_another_case_is_the_same_key() {
        let mut out = Vec::new();
        keep(&mut out, "COMPOSER", "Ravel");
        keep(&mut out, "Composer", "Maurice Ravel");
        keep(&mut out, "composer", "M. Ravel");
        assert_eq!(out, vec![("COMPOSER".to_string(), "Ravel".to_string())]);
    }

    /// The stored spelling is the canonical one, not the one the
    /// tagger happened to write. It is the column's identity -- the
    /// picker lists it, the layout names it, the cell lookup binds it --
    /// so it cannot depend on which file was read first.
    #[test]
    fn a_key_is_stored_in_its_canonical_case() {
        let mut out = Vec::new();
        keep(&mut out, "Ripper", "EAC");
        assert_eq!(out, vec![("RIPPER".to_string(), "EAC".to_string())]);
    }

    #[test]
    fn empty_and_oversized_keys_are_dropped() {
        let mut out = Vec::new();
        keep(&mut out, "  ", "value");
        keep(&mut out, "KEY", "   ");
        keep(&mut out, &"K".repeat(MAX_KEY + 1), "value");
        assert!(out.is_empty());
    }

    /// The values WaveFlow already models must not come back as a
    /// second, stale copy in a column of their own.
    #[test]
    fn keys_the_library_already_owns_are_skipped() {
        let mut out = Vec::new();
        for key in [
            "REPLAYGAIN_TRACK_GAIN",
            "replaygain_album_peak",
            "SYNCEDLYRICS",
            "MusicBrainz Track Id",
            "Lyrics",
        ] {
            keep(&mut out, key, "x");
        }
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn an_ordinary_custom_key_is_kept() {
        let mut out = Vec::new();
        keep(&mut out, "CATALOGNUMBER", "SRCL-1234");
        keep(&mut out, "RIPPER", "EAC 1.6");
        assert_eq!(out.len(), 2);
    }

    /// An unreadable or unsupported file is a track with no custom
    /// tags, never an error: this feeds an optional column set.
    #[test]
    fn an_unreadable_file_is_none_not_empty() {
        // `None` and `Some(vec![])` drive different writes: one leaves
        // the stored tags alone, the other replaces them with nothing.
        assert!(read_extra_tags(Path::new("no-such-file.flac")).is_none());
    }
}
