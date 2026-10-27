//! The tag patch and its application to a file: the generic lofty tag,
//! the rating, and the in-place or rewritten write of `patch_file`.

use waveflow_core::scanner::split_genres;

use super::*;

/// Containers the library indexes but nothing here can tag.
///
/// lofty 0.25's `FileType` carries no DSD variant, so asking it about a
/// `.dsf` or a `.dff` fails with a generic "unknown format" — accurate,
/// but it reads as a corrupt file rather than as a format we never
/// supported writing. `.dsf` no longer needs lofty: its tag is a plain
/// ID3v2 block after the audio and [`patch_dsf`] writes it (#592).
///
/// `.dff` stays here. It carries no ID3 by convention, its metadata
/// lives in its own chunk structure, and some taggers append an ID3
/// chunk anyway — so which shape to write is a real question with no
/// obvious answer, and half an answer would be worse than a clear no.
const UNTAGGABLE_EXTENSIONS: &[&str] = &["dff"];

/// `Err` when `path` names a container this build can index but not write
/// tags into. The message is user-facing — it reaches the properties
/// dialog through `AppError::Other`.
fn reject_untaggable(
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return Ok(());
    };
    if UNTAGGABLE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "{} files have no writable tag block in this build",
            ext.to_uppercase()
        )
        .into());
    }
    Ok(())
}

/// Where a set of lyrics goes in the tag.
///
/// The key is decided by the caller, which is the only place that knows
/// the format: plain text and LRC go where every player looks for
/// lyrics, TTML needs a slot that accepts arbitrary text. Expressed as
/// one choice rather than two optional strings so "both at once" cannot
/// be written down.
pub(crate) enum LyricsSlot<'a> {
    /// Both keys go and nothing replaces them.
    Cleared,
    /// `USLT` for ID3v2, `UNSYNCEDLYRICS` for Vorbis, `©lyr` for MP4 —
    /// what every other player reads.
    Unsynchronised(&'a str),
    /// `ItemKey::Lyrics`, the arbitrary-string slot TTML needs. Vorbis
    /// and MP4 only; ID3v2 has nothing equivalent.
    Arbitrary(&'a str),
}

/// One edit to a file's tags, applied through [`patch_file`].
pub(crate) enum TagPatch<'a> {
    /// The metadata fields the properties dialog exposes.
    Fields(&'a TrackEdit),
    /// A new front cover. Every other embedded image survives.
    Cover {
        bytes: &'a [u8],
        mime: &'a lofty::picture::MimeType,
    },
    /// The star rating as the raw 0-255 POPM byte, or `None` to clear
    /// it. Comes through here rather than through lofty directly so a
    /// `.dsf` gets its rating like any other container (#644).
    Rating(Option<u8>),
    /// The lyrics, under the key [`LyricsSlot`] names. Same reason: the
    /// route through `patch_file` is what gives DSD a lyrics write at
    /// all, and what keeps the non-standard comments of every other
    /// container from being dropped on the way (#644).
    Lyrics(LyricsSlot<'a>),
}

/// Apply a patch to a generic [`lofty::tag::Tag`].
///
/// The edit semantics live here, in one place, whatever the container:
/// [`patch_file`] converts each concrete tag into this shape and back.
pub(super) fn apply_patch(tag: &mut lofty::tag::Tag, patch: &TagPatch<'_>) {
    use lofty::prelude::*;
    use lofty::tag::ItemKey;

    let edit = match patch {
        TagPatch::Fields(edit) => edit,
        TagPatch::Rating(rating) => {
            // Cleared first whatever comes next, so a rating written
            // earlier — possibly by another tagger, under a different
            // email — cannot shadow the new one.
            tag.remove_key(ItemKey::Popularimeter);
            if let Some(r) = rating {
                // Vorbis / MP4 / APE / WavPack: text on the 0-100
                // scale, the shape the scanner reads back.
                //
                // ID3v2 does not come through here at all. lofty's
                // generic tag carries a rating as a **whole star**
                // (`<provider>|<stars>|<counter>`), so a value on this
                // 0-255 scale either fails to convert — which is
                // silent, the frame simply never reaches the file — or
                // is rounded to a star and re-encoded on the provider's
                // own scale. Neither is the byte the user set, so that
                // container is written as the POPM frame it is, by
                // [`apply_rating_id3v2`].
                let as_100 = ((*r as u16) * 100 / 255) as u8;
                tag.insert_text(ItemKey::Popularimeter, as_100.to_string());
            }
            return;
        }
        TagPatch::Lyrics(slot) => {
            // Both keys go before either is written. Switching format
            // (LRC → TTML, say) moves the lyrics from one key to the
            // other, and the reader checks `UnsyncLyrics` first — left
            // in place, the old content would shadow the new.
            // ID3v2 has no arbitrary-string lyrics slot — lofty says so
            // outright — so an `Arbitrary` write there would be dropped
            // on the way to the file, and the clear above would have
            // spent the file's lyrics to make room for it.
            // `write_lyrics_to_file` refuses TTML on those containers
            // before this is called; this is the same answer the `id3`
            // applier gives, so the two cannot disagree about it.
            if matches!(
                (slot, tag.tag_type()),
                (LyricsSlot::Arbitrary(_), lofty::tag::TagType::Id3v2)
            ) {
                return;
            }
            tag.remove_key(ItemKey::UnsyncLyrics);
            tag.remove_key(ItemKey::Lyrics);
            match slot {
                LyricsSlot::Cleared => {}
                LyricsSlot::Unsynchronised(text) => {
                    tag.insert_text(ItemKey::UnsyncLyrics, (*text).to_string());
                }
                LyricsSlot::Arbitrary(text) => {
                    tag.insert_text(ItemKey::Lyrics, (*text).to_string());
                }
            }
            return;
        }
        TagPatch::Cover { bytes, mime } => {
            use lofty::picture::{Picture, PictureType};
            // Replace the cover, not the artwork. A release with a
            // booklet, a back cover or an artist shot carries several
            // pictures, and clearing the list to make room for one of
            // them threw the rest away — nothing brings them back.
            //
            // `Other` goes with `CoverFront` because taggers that don't
            // set a type write the cover there; keeping it would leave
            // the previous cover in the file next to the new one.
            let mut i = 0;
            while i < tag.pictures().len() {
                match tag.pictures()[i].pic_type() {
                    PictureType::CoverFront | PictureType::Other => {
                        tag.remove_picture(i);
                    }
                    _ => i += 1,
                }
            }
            tag.push_picture(
                Picture::unchecked(bytes.to_vec())
                    .pic_type(PictureType::CoverFront)
                    .mime_type((*mime).clone())
                    .build(),
            );
            return;
        }
    };

    if let Some(t) = edit.title.as_ref() {
        if t.trim().is_empty() {
            tag.remove_title();
        } else {
            tag.set_title(t.trim().to_string());
        }
    }
    if let Some(a) = edit.artist.as_ref() {
        if a.trim().is_empty() {
            tag.remove_artist();
        } else {
            // Multi-artist files store the comma-joined string in the
            // tag — the DB-side split is what materialises the many-
            // to-many. Keeping the raw string in the file is also what
            // the scanner reads back, so this round-trips cleanly.
            tag.set_artist(a.trim().to_string());
        }
    }
    if let Some(al) = edit.album.as_ref() {
        if al.trim().is_empty() {
            tag.remove_album();
        } else {
            tag.set_album(al.trim().to_string());
        }
    }
    if let Some(y) = edit.year {
        // The year rides on the *date* item (`TDRC` / `DATE` / `©day` /
        // `ICRD`), which is what the scanner reads back through
        // `Accessor::date`. `ItemKey::Year` has no ID3v2 mapping at all,
        // so writing it there dropped the value on the way to the file
        // and left the database claiming a year no player could see.
        if y <= 0 {
            tag.remove_date();
            // Legacy files may still carry a bare `YEAR` next to the
            // date; clearing one without the other leaves the value
            // visible to every other player.
            tag.remove_key(ItemKey::Year);
        } else if let Ok(year) = u16::try_from(y) {
            match tag.date() {
                // The dialog sends the year on every save, so a save
                // that changed only the title must not truncate a full
                // release date to its year.
                Some(existing) if existing.year == year => {}
                _ => tag.set_date(lofty::tag::items::Timestamp {
                    year,
                    ..Default::default()
                }),
            }
        }
        // A year `u16` cannot hold never reaches here — `validate_edit`
        // refuses the whole edit first, because clamping it would write
        // one value to the file and another to the row.
    }
    if let Some(n) = edit.track_number {
        if n > 0 {
            tag.set_track(n as u32);
        } else {
            tag.remove_track();
        }
    }
    if let Some(n) = edit.disc_number {
        if n > 0 {
            tag.set_disk(n as u32);
        } else {
            tag.remove_disk();
        }
    }
    if let Some(g) = edit.genre.as_ref() {
        // One item per genre, never `set_genre`, which keeps one. Merged
        // into the concrete tag they become a multi-value TCON, repeated
        // Vorbis GENRE comments or several MP4 `©gen`: the forms the
        // scanner reads back as separate genres.
        tag.remove_genre();
        for name in split_genres([g.as_str()]) {
            tag.push(lofty::tag::TagItem::new(
                ItemKey::Genre,
                lofty::tag::ItemValue::Text(name),
            ));
        }
    }
    // Removed first in both cases, so a second value another tagger left
    // under the same key cannot outlive the one the user replaced.
    if let Some(aa) = edit.album_artist.as_ref() {
        tag.remove_key(ItemKey::AlbumArtist);
        if !aa.trim().is_empty() {
            tag.insert_text(ItemKey::AlbumArtist, aa.trim().to_string());
        }
    }
    if let Some(c) = edit.composer.as_ref() {
        tag.remove_key(ItemKey::Composer);
        if !c.trim().is_empty() {
            tag.insert_text(ItemKey::Composer, c.trim().to_string());
        }
    }
}

/// Write the rating into an ID3v2 tag as the frame it is.
///
/// The star rating is a **0-255 POPM byte**, in half-star steps, and
/// lofty's generic tag cannot carry one: since 0.25 it converts a POPM
/// into `<provider>|<stars>|<counter>` and back through a whole-star
/// `StarRating`, so a raw byte put into the generic tag is dropped on
/// the way back to the file — measured: the frame count is zero, and
/// nothing says so. That is why this is applied to the concrete tag,
/// beside the reasons the rest of `patch_file` already exists for.
///
/// Removing by frame id takes every POPM the file holds. They are keyed
/// by email, so a file can carry several — one per tagger — and a
/// reader takes the first. Leaving the others would let a rating the
/// user replaced go on being the one another player shows.
pub(super) fn apply_rating_id3v2(tag: &mut lofty::id3::v2::Id3v2Tag, rating: Option<u8>) {
    use lofty::id3::v2::{Frame, FrameId, PopularimeterFrame};

    let popm = FrameId::Valid(std::borrow::Cow::Borrowed("POPM"));
    // Consumed, not dropped: `remove` hands back a lazy iterator over
    // what it takes, so ignoring it removes nothing at all.
    tag.remove(&popm).for_each(drop);
    if let Some(rating) = rating {
        // An empty email is the anonymous user the spec allows, and
        // what every reader checked (foobar2000, Mp3tag, MusicBee)
        // accepts. The counter stays 0 — we do not track plays here.
        tag.insert(Frame::Popularimeter(PopularimeterFrame::new("", rating, 0)));
    }
}

/// Apply `patch` to the file at `path`, keeping every field the generic
/// [`lofty::tag::Tag`] shape cannot model.
///
/// `lofty::read_from_path` hands back a `TaggedFile`, whose tags are
/// generic `Tag`s produced by *splitting* each concrete tag: what has an
/// `ItemKey` mapping goes into the `Tag`, the rest stays behind in a
/// remainder. What happens to that remainder is not uniform, and that is
/// the whole reason this function exists:
///
/// * `Id3v2Tag` stashes it in the `Tag`'s companion slot, so a `TXXX`
///   lofty doesn't model survives a save.
/// * `VorbisComments` has no such slot — `From<VorbisComments> for Tag`
///   is `split_tag().1` and throws the remainder away. Every
///   non-standard comment on a FLAC / Ogg / Opus / Speex file therefore
///   disappeared the first time the user pressed Save: our own
///   `SYNCEDLYRICS` (written by `commands::lyrics`), `REPLAYGAIN_*`
///   values another tagger left, anything a different player stores.
///
/// Reading the concrete file, splitting it here and merging the
/// remainder back covers both cases the same way, so the guarantee no
/// longer depends on which container the user happens to own.
///
/// Only the containers the scanner indexes are handled; anything else is
/// refused rather than written through a path we haven't checked.
pub(crate) fn patch_file(
    path: &std::path::Path,
    patch: &TagPatch<'_>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::{AudioFile, FileType};
    use lofty::prelude::*;
    use lofty::probe::Probe;

    reject_untaggable(path)?;

    // DSF before the probe: lofty has no FileType for it, so asking it
    // to guess would fail with "unrecognised container" on a file we
    // can in fact write (#592).
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dsf"))
    {
        return patch_dsf(path, patch);
    }

    let file_type = Probe::open(path)?
        .guess_file_type()?
        .file_type()
        .ok_or("unrecognised audio container")?;

    // Round-trip one tag slot: take it out of the file, split it into a
    // generic tag plus the frames lofty can't model, apply the patch to
    // the generic half, then merge the two back together. An absent slot
    // starts from an empty tag, which is what gives a never-tagged file
    // its first tag.
    macro_rules! patch_slot {
        ($file:expr, $take:ident, $set:ident) => {{
            let (remainder, mut generic) = $file.$take().unwrap_or_default().split_tag();
            apply_patch(&mut generic, patch);
            $file.$set(remainder.merge_tag(generic));
        }};
    }

    // The ID3v2 slot, which takes one payload off the generic path: a
    // rating is written as the POPM frame it is, because lofty's
    // generic tag cannot carry the byte. Everything else is the round
    // trip above.
    macro_rules! patch_id3v2_slot {
        ($file:expr) => {{
            let mut tag = $file.remove_id3v2().unwrap_or_default();
            match patch {
                TagPatch::Rating(rating) => apply_rating_id3v2(&mut tag, *rating),
                _ => {
                    let (remainder, mut generic) = tag.split_tag();
                    apply_patch(&mut generic, patch);
                    tag = remainder.merge_tag(generic);
                }
            }
            $file.set_id3v2(tag);
        }};
    }

    // Same, for a slot the container always has (the Ogg families carry
    // their Vorbis comments by spec, so lofty models them unwrapped).
    macro_rules! patch_required_slot {
        ($file:expr, $take:ident, $set:ident) => {{
            let (remainder, mut generic) = $file.$take().split_tag();
            apply_patch(&mut generic, patch);
            $file.$set(remainder.merge_tag(generic));
        }};
    }

    // The rewrite: read the concrete file, run the body over it, save it
    // back — through a copy that replaces the original in one `rename`
    // (#598). lofty's own rewrite truncates the file it is rewriting, so
    // the original has to stay out of its reach until the new bytes are
    // on the disk. lofty rewinds the handle itself at the top of its
    // writer, so reading and writing through the same one is expected.
    macro_rules! with_file {
        ($ty:ty, |$f:ident, $h:ident| $body:block) => {{
            waveflow_core::tagio::rewrite_via_temp(
                path,
                |$h| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    let mut $f = <$ty>::read_from($h, ParseOptions::new())?;
                    $body
                    $f.save_to($h, WriteOptions::default())?;
                    Ok(())
                },
            )?;
        }};
    }

    // First pass: lay the new tag over the one already in the file,
    // which works whenever the edit did not outgrow the padding there
    // (#590). That is the common case — a corrected title is a few bytes
    // either way and taggers leave kilobytes of slack — and it is the
    // only path that touches neither the audio nor the file's length.
    // `false` means "could not", and the rewrite below takes over.
    //
    // An **error** here fails the edit rather than falling through, and
    // that is deliberate. The rewrite opens the same container with the
    // same parser on a copy of the same bytes, so a parse or encode
    // failure would simply happen again one file later. The one error
    // that would genuinely behave differently is the open: `rewrite_via
    // _temp` never opens the original for writing, only for reading, so
    // it would succeed on a file the user has deliberately made
    // read-only on Unix — replacing it through a rename the directory
    // permits. Turning "you may not write this file" into a silent
    // replacement is not a fallback, it is a way around the answer.
    // Windows is the exception the guard already handles: there
    // read-only is a single attribute we lift and put back.
    macro_rules! try_in_place_id3v2 {
        ($ty:ty) => {{
            waveflow_core::tagio::with_writable_file(
                path,
                |handle| -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                    let mut f = <$ty>::read_from(handle, ParseOptions::new())?;
                    patch_id3v2_slot!(f);
                    match f.id3v2() {
                        Some(tag) => waveflow_core::tagio::try_id3v2_in_place(handle, tag),
                        None => Ok(false),
                    }
                },
            )?
        }};
    }

    let written_in_place = match file_type {
        FileType::Mpeg => try_in_place_id3v2!(lofty::mpeg::MpegFile),
        FileType::Aac => try_in_place_id3v2!(lofty::aac::AacFile),
        // Anything but a cover. A cover edit regenerates picture
        // blocks, which the FLAC fast path deliberately copies through
        // rather than rebuilds, so it has nothing to offer there — a
        // rating and a set of lyrics are comment values like any other
        // and take the same cheap route the fields do.
        FileType::Flac if !matches!(patch, TagPatch::Cover { .. }) => {
            waveflow_core::tagio::with_writable_file(
                path,
                |handle| -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                    let mut f = lofty::flac::FlacFile::read_from(handle, ParseOptions::new())?;
                    patch_slot!(f, remove_vorbis_comments, set_vorbis_comments);
                    match f.vorbis_comments() {
                        Some(comments) => waveflow_core::tagio::try_flac_in_place(handle, comments),
                        None => Ok(false),
                    }
                },
            )?
        }
        _ => false,
    };
    if written_in_place {
        return Ok(());
    }

    match file_type {
        FileType::Mpeg => with_file!(lofty::mpeg::MpegFile, |f, handle| {
            patch_id3v2_slot!(f);
        }),
        FileType::Aac => with_file!(lofty::aac::AacFile, |f, handle| {
            patch_id3v2_slot!(f);
        }),
        FileType::Mp4 => with_file!(lofty::mp4::Mp4File, |f, handle| {
            patch_slot!(f, remove_ilst, set_ilst);
        }),
        // WAV and AIFF can carry both an ID3v2 chunk and their native
        // list, and players disagree on which one they read. Patching
        // only the primary would leave the other one contradicting it,
        // so both get the edit whenever both exist; a file with neither
        // gets the ID3v2 chunk lofty treats as primary.
        FileType::Wav => with_file!(lofty::iff::wav::WavFile, |f, handle| {
            if f.riff_info().is_some() {
                patch_slot!(f, remove_riff_info, set_riff_info);
            }
            patch_id3v2_slot!(f);
        }),
        FileType::Aiff => with_file!(lofty::iff::aiff::AiffFile, |f, handle| {
            if f.text_chunks().is_some() {
                patch_slot!(f, remove_text_chunks, set_text_chunks);
            }
            patch_id3v2_slot!(f);
        }),
        FileType::Vorbis => with_file!(lofty::ogg::VorbisFile, |f, handle| {
            patch_required_slot!(f, remove_vorbis_comments, set_vorbis_comments);
        }),
        FileType::Opus => with_file!(lofty::ogg::OpusFile, |f, handle| {
            patch_required_slot!(f, remove_vorbis_comments, set_vorbis_comments);
        }),
        FileType::Speex => with_file!(lofty::ogg::SpeexFile, |f, handle| {
            patch_required_slot!(f, remove_vorbis_comments, set_vorbis_comments);
        }),
        // FLAC keeps its pictures in their own metadata blocks, which
        // lofty exposes on the file and not on the tag — a cover pushed
        // into the Vorbis comments here would be written *alongside* the
        // blocks already there rather than replacing the front cover.
        FileType::Flac => with_file!(lofty::flac::FlacFile, |f, handle| {
            match patch {
                TagPatch::Cover { bytes, mime } => {
                    use lofty::ogg::OggPictureStorage;
                    use lofty::picture::{Picture, PictureInformation, PictureType};
                    f.remove_picture_type(PictureType::CoverFront);
                    f.remove_picture_type(PictureType::Other);
                    let picture = Picture::unchecked(bytes.to_vec())
                        .pic_type(PictureType::CoverFront)
                        .mime_type((*mime).clone())
                        .build();
                    // `from_picture` decodes the header for width /
                    // height / colour depth. A format it can't read (our
                    // WebP path) is no reason to refuse the cover — the
                    // fields are informational and readers fall back to
                    // the image itself.
                    let info = PictureInformation::from_picture(&picture).unwrap_or_default();
                    f.insert_picture(picture, Some(info))?;
                }
                // Fields, rating, lyrics: all of them are comment
                // values, and the pictures stay where they are.
                _ => {
                    patch_slot!(f, remove_vorbis_comments, set_vorbis_comments);
                }
            }
        }),
        other => {
            return Err(format!("{other:?} files are not editable in this build").into());
        }
    }

    Ok(())
}

/// Apply the edit to the file's tags. Creates a tag when the file has
/// none, so a previously-untagged file gets properly tagged on first
/// edit. Returns the boxed lofty error untouched so the caller can
/// surface a useful message.
pub(super) fn write_tags_to_file(
    path: &std::path::Path,
    edit: &TrackEdit,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    patch_file(path, &TagPatch::Fields(edit))
}
