use super::*;
use waveflow_core::scanner::split_genres;

/// Every field the dialog can send, set to something distinctive.
fn full_edit() -> TrackEdit {
    TrackEdit {
        title: Some("  A Title  ".into()),
        artist: Some("Artist A, Artist B".into()),
        album: Some("An Album".into()),
        year: Some(1997),
        track_number: Some(4),
        disc_number: Some(2),
        genre: Some(" Jazz ; Trip Hop".into()),
        album_artist: Some("  The Album Artist ".into()),
        composer: Some(" A Composer".into()),
    }
}

/// The same fields, all asking to be cleared.
fn clearing_edit() -> TrackEdit {
    TrackEdit {
        title: Some("   ".into()),
        artist: Some(String::new()),
        album: Some(String::new()),
        year: Some(0),
        track_number: Some(0),
        disc_number: Some(0),
        genre: Some("  ".into()),
        album_artist: Some(" ".into()),
        composer: Some(String::new()),
    }
}

/// What a tag says, in the only terms both models share.
#[derive(Debug, PartialEq, Eq)]
struct Fields {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    year: Option<i32>,
    track: Option<u32>,
    disc: Option<u32>,
    genres: Vec<String>,
    album_artist: Option<String>,
    composer: Option<String>,
}

fn through_lofty(edit: &TrackEdit) -> Fields {
    use lofty::prelude::*;
    let mut tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
    apply_patch(&mut tag, &TagPatch::Fields(edit));
    Fields {
        title: tag.title().map(|v| v.to_string()),
        artist: tag.artist().map(|v| v.to_string()),
        album: tag.album().map(|v| v.to_string()),
        year: tag.date().map(|d| i32::from(d.year)),
        track: tag.track(),
        disc: tag.disk(),
        genres: tag
            .get_strings(lofty::tag::ItemKey::Genre)
            .map(str::to_string)
            .collect(),
        album_artist: tag
            .get_string(lofty::tag::ItemKey::AlbumArtist)
            .map(str::to_string),
        composer: tag
            .get_string(lofty::tag::ItemKey::Composer)
            .map(str::to_string),
    }
}

fn through_id3(edit: &TrackEdit) -> Fields {
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    apply_patch_id3(&mut tag, &TagPatch::Fields(edit));
    Fields {
        title: tag.title().map(|v| v.to_string()),
        artist: tag.artist().map(|v| v.to_string()),
        album: tag.album().map(|v| v.to_string()),
        year: tag.date_recorded().map(|d| d.year),
        track: tag.track(),
        disc: tag.disc(),
        genres: tag
            .genres()
            .unwrap_or_default()
            .into_iter()
            .map(str::to_string)
            .collect(),
        album_artist: tag.album_artist().map(str::to_string),
        composer: tag
            .get("TCOM")
            .and_then(|f| f.content().text())
            .map(str::to_string),
    }
}

#[test]
fn both_appliers_agree_on_a_full_edit() {
    // Two tag models, one set of edit semantics. A divergence would
    // mean the same edit meaning different things depending on
    // whether the file is an MP3 or a DSF — which nobody would look
    // for, because the dialog is the same one.
    let edit = full_edit();
    let written = through_lofty(&edit);
    assert_eq!(written, through_id3(&edit));
    // Two genres, each its own value, trimmed (#729).
    assert_eq!(written.genres, ["Jazz", "Trip Hop"]);
    assert_eq!(written.album_artist.as_deref(), Some("The Album Artist"));
    assert_eq!(written.composer.as_deref(), Some("A Composer"));
}

#[test]
fn both_appliers_agree_on_clearing_every_field() {
    // The half that is easy to get wrong: "empty means clear" has to
    // be the same decision on both sides, including the year, where
    // 0 means remove rather than write a year zero.
    let edit = clearing_edit();
    let cleared = through_lofty(&edit);
    assert_eq!(cleared, through_id3(&edit));
    assert_eq!(cleared.title, None);
    assert_eq!(cleared.year, None);
    assert_eq!(cleared.track, None);
    assert!(cleared.genres.is_empty());
    assert_eq!(cleared.album_artist, None);
    assert_eq!(cleared.composer, None);
}

#[test]
fn a_title_only_save_does_not_truncate_a_full_release_date() {
    // The dialog sends the year on every save, so both appliers have
    // to leave an existing day-precision date alone when the year
    // they were handed is the one already there.
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    tag.set_date_recorded(id3::Timestamp {
        year: 1997,
        month: Some(6),
        day: Some(14),
        ..Default::default()
    });
    let edit = TrackEdit {
        title: Some("New Title".into()),
        year: Some(1997),
        ..Default::default()
    };
    apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
    let date = tag.date_recorded().expect("a date");
    assert_eq!((date.year, date.month, date.day), (1997, Some(6), Some(14)));
}

/// A DSF keeps the ID3 version it had, so both versions must carry
/// several genres to the scanner, which reads TCON whole and splits
/// it with `split_genres` (#729).
#[test]
fn several_genres_survive_an_id3_round_trip_in_both_versions() {
    use id3::TagLike;

    for version in [id3::Version::Id3v23, id3::Version::Id3v24] {
        let mut tag = id3::Tag::new();
        let edit = TrackEdit {
            genre: Some("Jazz; Trip Hop".into()),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        join_genres_for_legacy(&mut tag, version);
        let mut bytes = Vec::new();
        tag.write_to(&mut bytes, version).expect("write");
        let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");
        assert_eq!(
            split_genres(back.genre()),
            ["Jazz", "Trip Hop"],
            "{version:?}"
        );
    }
}

#[test]
fn an_id3v23_tag_keeps_its_year_where_a_v23_reader_looks() {
    // TDRC is a 2.4 frame. The `id3` crate writes the frames the tag
    // holds whatever version it is told to target, so a 2.3 tag built
    // from a TDRC comes back out carrying TDRC — and `year()`, which
    // reads TYER, answers None. Measured, not assumed: that is what
    // the round trip below asserted before the mirror existed.
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    tag.set_title("Before");
    let edit = TrackEdit {
        year: Some(1997),
        ..Default::default()
    };
    apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
    mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v23);

    let mut bytes = Vec::new();
    tag.write_to(&mut bytes, id3::Version::Id3v23)
        .expect("write");
    let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");

    assert_eq!(back.year(), Some(1997), "a 2.3 reader finds the year");
    assert_eq!(
        back.date_recorded().map(|d| d.year),
        Some(1997),
        "and the 2.4 frame still agrees with it"
    );
}

#[test]
fn clearing_the_year_clears_both_frames_in_a_legacy_tag() {
    // A stale TYER outliving the date it duplicated would show the
    // old year to exactly the readers the mirror exists for.
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    tag.set_year(1997);
    tag.set_date_recorded(id3::Timestamp {
        year: 1997,
        ..Default::default()
    });
    let edit = TrackEdit {
        year: Some(0),
        ..Default::default()
    };
    apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
    mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v23);

    assert_eq!(tag.year(), None);
    assert_eq!(tag.date_recorded(), None);
}

#[test]
fn a_v24_tag_is_left_with_the_frame_its_version_defines() {
    // The mirror is a 2.3 accommodation and must not add a legacy
    // frame to a tag that has no use for one.
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    let edit = TrackEdit {
        year: Some(2011),
        ..Default::default()
    };
    apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
    mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v24);

    assert_eq!(tag.date_recorded().map(|d| d.year), Some(2011));
    assert_eq!(tag.year(), None, "no TYER in a 2.4 tag");
}

#[test]
fn an_id3v22_tag_can_actually_be_written() {
    // Not a conformance nicety: the writer refuses a tag carrying a
    // frame it cannot downgrade, so a TDRC we added ourselves failed
    // the whole edit on a 2.2 file.
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    tag.set_title("Before");
    let edit = TrackEdit {
        year: Some(1997),
        ..Default::default()
    };
    apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
    mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v22);

    let mut bytes = Vec::new();
    tag.write_to(&mut bytes, id3::Version::Id3v22)
        .expect("a 2.2 tag we built must be writable as 2.2");
    let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");
    assert_eq!(back.year(), Some(1997));
    assert_eq!(back.title(), Some("Before"));
}

#[test]
fn a_cover_edit_does_not_erase_a_v23_tag_s_year() {
    // The shape that matters: a genuine 2.3 tag carries TYER and no
    // TDRC. A mirror that ran on every save would read "no date"
    // from it and remove the year — so setting a cover, or fixing a
    // title, silently erased the year of the file it was helping.
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    tag.set_year(1997);
    let mime = lofty::picture::MimeType::Jpeg;
    let patch = TagPatch::Cover {
        bytes: &[1, 2, 3],
        mime: &mime,
    };
    apply_patch_id3(&mut tag, &patch);
    mirror_year_for_legacy(&mut tag, &patch, id3::Version::Id3v23);

    assert_eq!(tag.year(), Some(1997), "the year the edit never mentioned");
}

#[test]
fn a_title_only_edit_does_not_erase_a_v23_tag_s_year() {
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    tag.set_year(1997);
    let edit = TrackEdit {
        title: Some("New Title".into()),
        ..Default::default()
    };
    apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
    mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v23);

    assert_eq!(tag.year(), Some(1997));
    assert_eq!(tag.title(), Some("New Title"));
}

#[test]
fn a_dsf_cover_replaces_the_front_and_keeps_the_rest() {
    // Same rule as the lofty side: a booklet or a back cover is not
    // the front cover, and nothing brings it back.
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    tag.add_frame(id3::frame::Picture {
        mime_type: "image/jpeg".into(),
        picture_type: id3::frame::PictureType::CoverBack,
        description: String::new(),
        data: vec![1, 2, 3],
    });
    tag.add_frame(id3::frame::Picture {
        mime_type: "image/jpeg".into(),
        picture_type: id3::frame::PictureType::CoverFront,
        description: String::new(),
        data: vec![9, 9, 9],
    });

    let mime = lofty::picture::MimeType::Jpeg;
    apply_patch_id3(
        &mut tag,
        &TagPatch::Cover {
            bytes: &[4, 5, 6],
            mime: &mime,
        },
    );

    let pictures: Vec<_> = tag.pictures().collect();
    assert_eq!(pictures.len(), 2);
    assert!(
        pictures.iter().any(
            |p| p.picture_type == id3::frame::PictureType::CoverBack && p.data == vec![1, 2, 3]
        ),
        "the back cover survived"
    );
    assert!(
        pictures
            .iter()
            .any(|p| p.picture_type == id3::frame::PictureType::CoverFront
                && p.data == vec![4, 5, 6]),
        "the front cover is the new one"
    );
}

/// The byte the database holds is the byte another player reads, on
/// the container where the rating used to stop at the database (#644).
#[test]
fn a_rating_reaches_an_id3_tag_as_the_byte_it_was_given() {
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    tag.set_title("Keep me");

    apply_patch_id3(&mut tag, &TagPatch::Rating(Some(196)));

    // Through an encode/decode, because a frame the writer refuses is
    // a frame the file never gets.
    let mut bytes = Vec::new();
    tag.write_to(&mut bytes, id3::Version::Id3v24)
        .expect("write");
    let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");

    let popm = back
        .frames()
        .find_map(|frame| match frame.content() {
            id3::Content::Popularimeter(p) => Some(p),
            _ => None,
        })
        .expect("a POPM frame");
    assert_eq!(popm.rating, 196);
    assert_eq!(
        back.title(),
        Some("Keep me"),
        "a rating is one frame, not a new tag"
    );
}

/// POPM is keyed by user, so a tag can hold several and the reader
/// takes the first. Clearing one star out of five has to take every
/// one of them, or the old rating simply comes back.
#[test]
fn clearing_a_rating_takes_every_popm_frame_with_it() {
    use id3::TagLike;
    let mut tag = id3::Tag::new();
    for user in ["", "someone@else"] {
        tag.add_frame(id3::Frame::with_content(
            "POPM",
            id3::Content::Popularimeter(id3::frame::Popularimeter {
                user: user.to_string(),
                rating: 255,
                counter: 0,
            }),
        ));
    }

    apply_patch_id3(&mut tag, &TagPatch::Rating(None));

    assert_eq!(tag.frames().filter(|f| f.id() == "POPM").count(), 0);
}

/// The generic side of the same edit — the containers that keep a
/// rating as plain text on the 0-100 scale, which is the shape the
/// scanner reads back from them. ID3v2 does not come through here:
/// see [`apply_rating_id3v2`].
#[test]
fn the_generic_applier_writes_the_text_scale() {
    use lofty::tag::{ItemKey, Tag, TagType};

    let mut vorbis = Tag::new(TagType::VorbisComments);
    apply_patch(&mut vorbis, &TagPatch::Rating(Some(196)));
    assert_eq!(
        vorbis.get_string(ItemKey::Popularimeter),
        Some("76"),
        "196 of 255 on the 0-100 scale"
    );

    apply_patch(&mut vorbis, &TagPatch::Rating(None));
    assert_eq!(vorbis.get_string(ItemKey::Popularimeter), None);
}

/// The byte the user set is the byte the frame carries.
///
/// Measured on lofty 0.25: a rating put into the **generic** tag
/// does not survive the merge back — `Binary` and a plain number
/// both leave zero frames, and only `<provider>|<stars>|<counter>`
/// with a whole star between the pipes produces one, re-encoded on
/// that provider's scale. None of those is a 0-255 value in
/// half-star steps, which is what the database holds, so the frame
/// is built directly.
#[test]
fn a_rating_is_written_as_the_frame_it_is() {
    use lofty::id3::v2::{Frame, Id3v2Tag};

    let mut tag = Id3v2Tag::default();
    apply_rating_id3v2(&mut tag, Some(196));

    let ratings: Vec<u8> = tag
        .iter()
        .filter_map(|frame| match frame {
            Frame::Popularimeter(popm) => Some(popm.rating),
            _ => None,
        })
        .collect();
    assert_eq!(ratings, vec![196]);
}

/// POPM is keyed by email, so a file can hold several and a reader
/// takes the first. A rating the user replaced has to stop being
/// the one another player shows.
#[test]
fn writing_a_rating_takes_every_popm_the_file_held() {
    use lofty::id3::v2::{Frame, Id3v2Tag, PopularimeterFrame};

    let mut tag = Id3v2Tag::default();
    tag.insert(Frame::Popularimeter(PopularimeterFrame::new(
        "someone@else",
        255,
        12,
    )));
    tag.insert(Frame::Popularimeter(PopularimeterFrame::new("", 51, 0)));

    apply_rating_id3v2(&mut tag, Some(153));
    assert_eq!(
        tag.iter()
            .filter(|f| matches!(f, Frame::Popularimeter(_)))
            .count(),
        1
    );

    apply_rating_id3v2(&mut tag, None);
    assert_eq!(
        tag.iter()
            .filter(|f| matches!(f, Frame::Popularimeter(_)))
            .count(),
        0,
        "clearing the stars clears the frame"
    );
}

/// The same round trip on the container that loses what it is not
/// asked about: `From<VorbisComments> for Tag` throws the remainder
/// away, which is the whole reason `patch_file` merges it back.
#[test]
fn lyrics_survive_it_on_a_vorbis_tag_and_take_nothing_else_with_them() {
    use lofty::ogg::tag::VorbisComments;
    use lofty::prelude::{ItemKey, SplitTag};
    use lofty::tag::MergeTag;

    let mut comments = VorbisComments::default();
    // A comment lofty does not model, of exactly the kind a save
    // used to drop.
    comments.push("REPLAYGAIN_TRACK_GAIN".to_string(), "-6.5 dB".to_string());

    let (remainder, mut generic) = comments.split_tag();
    apply_patch(
        &mut generic,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised("the words")),
    );
    let concrete: VorbisComments = remainder.merge_tag(generic);

    assert_eq!(
        concrete.get("REPLAYGAIN_TRACK_GAIN"),
        Some("-6.5 dB"),
        "the remainder came back"
    );
    let (_, as_the_reader_sees_it) = concrete.split_tag();
    assert_eq!(
        as_the_reader_sees_it.get_string(ItemKey::UnsyncLyrics),
        Some("the words")
    );
}

/// Saving lyrics twice leaves one set, not two. USLT is keyed by
/// language and description, and a second frame under the same pair
/// is what a reader shows instead of the edit.
#[test]
fn lyrics_replace_what_the_tag_already_held() {
    let mut tag = id3::Tag::new();

    apply_patch_id3(
        &mut tag,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised("first take")),
    );
    apply_patch_id3(
        &mut tag,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised("second take")),
    );

    let all: Vec<_> = tag.lyrics().collect();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].text, "second take");
}

#[test]
fn clearing_lyrics_empties_the_frame_rather_than_blanking_it() {
    let mut tag = id3::Tag::new();
    apply_patch_id3(
        &mut tag,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised("words")),
    );

    apply_patch_id3(&mut tag, &TagPatch::Lyrics(LyricsSlot::Cleared));

    assert_eq!(tag.lyrics().count(), 0);
}

/// The generic applier answers the same way its `id3` sibling does,
/// on the container where the two could have disagreed: a write
/// that ID3v2 silently drops must not be preceded by a clear that
/// it does not.
#[test]
fn an_arbitrary_slot_leaves_a_generic_id3v2_tag_alone_too() {
    use lofty::tag::{ItemKey, Tag, TagType};

    let mut id3 = Tag::new(TagType::Id3v2);
    apply_patch(
        &mut id3,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised("words already there")),
    );

    apply_patch(&mut id3, &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt/>")));
    assert_eq!(
        id3.get_string(ItemKey::UnsyncLyrics),
        Some("words already there")
    );

    // A container that does have the slot takes the write.
    let mut vorbis = Tag::new(TagType::VorbisComments);
    apply_patch(
        &mut vorbis,
        &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt/>")),
    );
    assert_eq!(vorbis.get_string(ItemKey::Lyrics), Some("<tt/>"));
}

/// The arm that cannot be reached today, asserted anyway: ID3v2 has
/// no arbitrary-string lyrics slot, and the day the guard upstream
/// moves, the wrong answer here is to clear a file's lyrics to make
/// room for a write that never happens.
#[test]
fn an_arbitrary_slot_leaves_an_id3_tag_exactly_as_it_was() {
    let mut tag = id3::Tag::new();
    apply_patch_id3(
        &mut tag,
        &TagPatch::Lyrics(LyricsSlot::Unsynchronised("words already there")),
    );

    apply_patch_id3(
        &mut tag,
        &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt>…</tt>")),
    );

    let all: Vec<_> = tag.lyrics().collect();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].text, "words already there");
}
