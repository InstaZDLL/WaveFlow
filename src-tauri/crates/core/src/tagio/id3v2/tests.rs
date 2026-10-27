mod in_place_tests {
    use super::super::*;
    use lofty::config::WriteOptions;
    use lofty::id3::v2::Id3v2Tag;
    use lofty::prelude::{Accessor, TagExt};

    /// A file shaped like a tagged MP3: an ID3v2 tag with room to spare,
    /// then bytes standing in for audio frames.
    fn tagged_file(
        title: &str,
        padding: u32,
        audio: &[u8],
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("track.mp3");
        let mut tag = Id3v2Tag::default();
        tag.set_title(title.to_string());
        let mut bytes = Vec::new();
        tag.dump_to(&mut bytes, WriteOptions::new().preferred_padding(padding))
            .expect("dump");
        bytes.extend_from_slice(audio);
        std::fs::write(&path, &bytes).expect("seed");
        (dir, path)
    }

    #[test]
    fn an_edit_that_fits_leaves_every_audio_byte_where_it_was() {
        // The claim the whole fast path rests on. The check is not "the
        // file still parses" but "these exact bytes did not move": the
        // failure being guarded against is a prefix of the wrong length
        // overlaying the first audio frames.
        let audio: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let (_dir, path) = tagged_file("Before", 2048, &audio);
        let before_len = std::fs::metadata(&path).expect("meta").len();

        let mut replacement = Id3v2Tag::default();
        replacement.set_title("After".to_string());

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let span_before = read_id3v2_span(&mut file).expect("span").expect("a tag");
        let wrote_in_place = try_id3v2_in_place(&mut file, &replacement).expect("in place");
        file.sync_all().expect("sync");
        drop(file);

        assert!(wrote_in_place, "a shorter tag fits in the old span");

        let after = std::fs::read(&path).expect("read");
        assert_eq!(
            after.len() as u64,
            before_len,
            "the file did not change length"
        );
        assert_eq!(
            &after[span_before.total as usize..],
            &audio[..],
            "the audio behind the tag is byte for byte what it was"
        );

        let mut file = std::fs::File::open(&path).expect("reopen");
        let span_after = read_id3v2_span(&mut file).expect("span").expect("a tag");
        assert_eq!(
            span_after, span_before,
            "the span it declares is the one it filled"
        );
        assert!(
            after[..span_before.total as usize]
                .windows(5)
                .any(|w| w == b"After"),
            "the new title is in the tag"
        );
    }

    #[test]
    fn a_cover_survives_the_in_place_write() {
        // Why the ID3v2 fast path takes covers and the FLAC one does
        // not: here the picture is an APIC frame *inside* the tag, so
        // `dump_to` writes it with everything else. The FLAC guard
        // exists because that encoder rebuilds only the comment block
        // and pictures live in blocks of their own — a different gap,
        // not a symmetry to copy.
        use lofty::id3::v2::{AttachedPictureFrame, Frame};
        use lofty::picture::{MimeType, Picture, PictureType};
        use lofty::TextEncoding;

        let audio: Vec<u8> = (0..2048u32).map(|i| (i % 199) as u8 + 1).collect();
        let (_dir, path) = tagged_file("Before", 8192, &audio);
        let before_len = std::fs::metadata(&path).expect("meta").len();

        let mut tag = Id3v2Tag::default();
        tag.set_title("After".to_string());
        let cover: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8 + 2).collect();
        tag.insert(Frame::Picture(AttachedPictureFrame::new(
            TextEncoding::UTF8,
            Picture::unchecked(cover.clone())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Jpeg)
                .build(),
        )));

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let span = read_id3v2_span(&mut file).expect("span").expect("a tag");
        assert!(try_id3v2_in_place(&mut file, &tag).expect("in place"));
        file.sync_all().expect("sync");
        drop(file);

        let after = std::fs::read(&path).expect("read");
        assert_eq!(after.len() as u64, before_len);
        assert_eq!(
            &after[span.total as usize..],
            &audio[..],
            "the audio is untouched"
        );
        assert!(
            after[..span.total as usize]
                .windows(cover.len())
                .any(|w| w == &cover[..]),
            "the cover went into the file with the rest of the tag"
        );
    }

    #[test]
    fn an_edit_that_outgrows_the_padding_asks_for_the_rewrite() {
        // No padding to grow into, and a title far longer than the one
        // it replaces: this is the case the slow path exists for, and
        // saying so is the only safe answer.
        let (_dir, path) = tagged_file("A", 0, b"audio");
        let mut replacement = Id3v2Tag::default();
        replacement.set_title("A".repeat(512));

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        let before = std::fs::read(&path).expect("read");
        let wrote_in_place = try_id3v2_in_place(&mut file, &replacement).expect("in place");
        drop(file);

        assert!(!wrote_in_place);
        assert_eq!(
            std::fs::read(&path).expect("read"),
            before,
            "refusing the fast path must not have written anything"
        );
    }

    #[test]
    fn a_file_that_never_had_a_tag_asks_for_the_rewrite() {
        // There is no span to overwrite, and writing a tag at offset
        // zero would bury the first audio frames under it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bare.mp3");
        std::fs::write(&path, b"no tag here, just audio").expect("seed");

        let mut tag = Id3v2Tag::default();
        tag.set_title("New".to_string());
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        assert!(!try_id3v2_in_place(&mut file, &tag).expect("in place"));
        drop(file);
        assert_eq!(
            std::fs::read(&path).expect("read"),
            b"no tag here, just audio"
        );
    }
}

mod fit_tests {
    use super::super::*;

    fn span(total: u32) -> Option<Id3v2Span> {
        Some(Id3v2Span {
            total,
            has_footer: false,
        })
    }

    #[test]
    fn a_smaller_tag_is_padded_back_to_the_old_span() {
        // The point of the whole exercise: the replacement ends up the
        // same length as what it replaces, so the audio behind it never
        // moves.
        assert_eq!(fit_id3v2_padding(span(4096), 1200), Some(2896));
    }

    #[test]
    fn an_exact_fit_asks_for_no_padding() {
        assert_eq!(fit_id3v2_padding(span(4096), 4096), Some(0));
    }

    #[test]
    fn a_bigger_tag_does_not_fit() {
        // One byte over is still over — `checked_sub` is what keeps this
        // from wrapping into an enormous padding request.
        assert_eq!(fit_id3v2_padding(span(4096), 4097), None);
    }

    #[test]
    fn a_file_with_no_tag_has_no_span_to_reuse() {
        assert_eq!(fit_id3v2_padding(None, 10), None);
    }

    #[test]
    fn a_footer_rules_out_padding() {
        // ID3v2.4: "[A tag] MUST NOT have any padding when a tag footer
        // is added". Asking for padding anyway would produce a tag of a
        // different length than the arithmetic here promised.
        let footered = Some(Id3v2Span {
            total: 4096,
            has_footer: true,
        });
        assert_eq!(fit_id3v2_padding(footered, 10), None);
    }
}
