use super::*;

fn values(pairs: &[(Field, &str)]) -> Values {
    pairs.iter().map(|(f, v)| (*f, (*v).to_owned())).collect()
}

#[test]
fn every_field_round_trips_through_its_name() {
    for field in Field::ALL {
        assert_eq!(Field::parse(field.as_str()), Some(field));
        // The serialized name is the journal's and the frontend's: the
        // two spellings must be the same string.
        assert_eq!(
            serde_json::to_string(&field).unwrap(),
            format!("\"{}\"", field.as_str())
        );
    }
    assert_eq!(Field::parse("rating"), None);
}

#[test]
fn numbers_lose_their_zeros_and_zero_is_none() {
    assert_eq!(normalize(Field::TrackNumber, " 03 "), "3");
    assert_eq!(normalize(Field::DiscNumber, "0"), "");
    assert_eq!(normalize(Field::Year, "1991"), "1991");
    // Not a number: kept, for `validate` to name.
    assert_eq!(normalize(Field::Year, "199x"), "199x");
}

#[test]
fn artists_split_on_the_library_separator_alone() {
    assert_eq!(
        normalize(Field::Artist, " Earth, Wind & Fire "),
        "Earth, Wind & Fire"
    );
    assert_eq!(normalize(Field::Artist, "A;  B ; ; C"), "A; B; C");
    // A bare `;` is part of a name, as the scanner reads it.
    assert_eq!(normalize(Field::Artist, "A;B"), "A;B");
    assert_eq!(normalize(Field::Genre, "Rock;Jazz ; "), "Rock; Jazz");
}

#[test]
fn identifiers_are_compared_in_lower_case() {
    assert_eq!(
        normalize(
            Field::MbArtistId,
            "5B11F4CE-A62D-471E-81FC-A69A8278C7DA;5b11f4ce-a62d-471e-81fc-a69a8278c7db"
        ),
        "5b11f4ce-a62d-471e-81fc-a69a8278c7da; 5b11f4ce-a62d-471e-81fc-a69a8278c7db"
    );
}

#[test]
fn validation_names_what_cannot_be_written() {
    assert_eq!(validate(Field::Year, "abc"), Some("not_a_number"));
    assert_eq!(validate(Field::Year, "12000"), Some("out_of_range"));
    assert_eq!(validate(Field::TrackNumber, "70000"), Some("out_of_range"));
    assert_eq!(validate(Field::TrackNumber, ""), None);
    assert_eq!(
        validate(Field::MbReleaseId, "not-an-id"),
        Some("not_an_identifier")
    );
    assert_eq!(
        validate(
            Field::MbAlbumArtistId,
            "5b11f4ce-a62d-471e-81fc-a69a8278c7da; nope"
        ),
        Some("not_an_identifier")
    );
    assert_eq!(
        validate(Field::MbReleaseId, "9DCE399D-3EC7-34A4-97D0-B4BAC54E5F08"),
        None
    );
    assert_eq!(validate(Field::Title, "anything at all"), None);
}

#[test]
fn a_diff_lists_only_what_would_change() {
    let current = values(&[
        (Field::Title, "Smells Like Teen Spirit"),
        (Field::TrackNumber, "01"),
        (Field::Genre, "Grunge"),
    ]);
    let wanted = values(&[
        // Same value, other spelling of the number: not a change.
        (Field::TrackNumber, "1"),
        // Same value with spaces around it: not a change.
        (Field::Title, " Smells Like Teen Spirit "),
        // A new field.
        (Field::Album, "Nevermind"),
        // A cleared one.
        (Field::Genre, ""),
    ]);
    assert_eq!(
        diff(&current, &wanted),
        vec![
            FieldDiff {
                field: Field::Album,
                old: String::new(),
                new: "Nevermind".into(),
            },
            FieldDiff {
                field: Field::Genre,
                old: "Grunge".into(),
                new: String::new(),
            },
        ]
    );
}

#[test]
fn a_field_left_out_of_a_change_is_left_alone() {
    let current = values(&[(Field::Title, "A"), (Field::Album, "B")]);
    assert!(diff(&current, &Values::new()).is_empty());
}

#[cfg(feature = "musicbrainz")]
mod proposals {
    use super::*;
    use crate::enrichment::matching::Verdict;
    use crate::enrichment::{LocalAlbum, LocalTrack};
    use crate::metadata::album_match::TrackSignals;
    use crate::metadata::musicbrainz::Release;
    use crate::tag_workshop::proposal::propose;

    /// The enrichment engine's own fixture: a real lookup of one
    /// pressing of "Nevermind", cut to its first two tracks (CC0).
    fn nevermind() -> Release {
        serde_json::from_str(include_str!("../enrichment/extract/nevermind.json")).unwrap()
    }

    fn local(titles: &[(&str, i64, Option<i64>)]) -> LocalAlbum {
        LocalAlbum {
            album_id: 7,
            title: "nevermind".into(),
            artist: Some("Nirvana".into()),
            tracks: titles
                .iter()
                .enumerate()
                .map(|(i, (title, duration, number))| LocalTrack {
                    track_id: 100 + i as i64,
                    signals: TrackSignals {
                        title: (*title).to_owned(),
                        duration_ms: Some(*duration),
                        track_number: *number,
                        disc_number: None,
                    },
                    file_path: format!("/m/{i}.flac"),
                })
                .collect(),
        }
    }

    #[test]
    fn every_paired_file_gets_the_release_tags_and_its_six_identifiers() {
        // Listed out of order on purpose: the pairing, not the list
        // order, decides which track a file is.
        let album = local(&[
            ("In Bloom", 254_900, Some(2)),
            ("smells like teen spirit", 301_000, Some(1)),
        ]);
        let proposal = propose(&album, &nevermind(), false);
        assert_eq!(proposal.verdict, Verdict::Confident);
        assert!(proposal.unmatched.is_empty());
        assert_eq!(
            proposal.release.mbid,
            "9dce399d-3ec7-34a4-97d0-b4bac54e5f08"
        );

        let teen = proposal
            .tracks
            .iter()
            .find(|t| t.track_id == 101)
            .expect("the second file pairs with the first track");
        let get = |f: Field| teen.values.get(&f).map(String::as_str);
        assert_eq!(get(Field::Title), Some("Smells Like Teen Spirit"));
        assert_eq!(get(Field::Artist), Some("Nirvana"));
        assert_eq!(get(Field::Album), Some("Nevermind"));
        assert_eq!(get(Field::AlbumArtist), Some("Nirvana"));
        // The album's year (the release group's first), not this
        // pressing's 1996.
        assert_eq!(get(Field::Year), Some("1991"));
        assert_eq!(get(Field::TrackNumber), Some("1"));
        assert_eq!(get(Field::TrackTotal), Some("2"));
        assert_eq!(get(Field::DiscNumber), Some("1"));
        assert_eq!(get(Field::DiscTotal), Some("1"));
        assert_eq!(get(Field::Genre), Some("Grunge; Alternative Rock"));
        assert_eq!(
            get(Field::MbReleaseId),
            Some("9dce399d-3ec7-34a4-97d0-b4bac54e5f08")
        );
        assert_eq!(
            get(Field::MbReleaseGroupId),
            Some("1b022e01-4da6-387b-8658-8678046e4cef")
        );
        assert_eq!(
            get(Field::MbRecordingId),
            Some("5fb524f1-8cc8-4c04-a921-e34c0a911ea7")
        );
        assert_eq!(
            get(Field::MbReleaseTrackId),
            Some("e18075e4-fb74-3896-8967-d4b1e683702f")
        );
        assert_eq!(
            get(Field::MbArtistId),
            Some("5b11f4ce-a62d-471e-81fc-a69a8278c7da")
        );
        assert_eq!(
            get(Field::MbAlbumArtistId),
            Some("5b11f4ce-a62d-471e-81fc-a69a8278c7da")
        );
        // Composer is not proposed: the release does not carry it as a
        // tag, and the file's own stays.
        assert_eq!(get(Field::Composer), None);
        // Every proposed value is one the writer accepts.
        for (field, value) in &teen.values {
            assert_eq!(validate(*field, value), None, "{field:?} = {value}");
        }
    }

    #[test]
    fn a_file_no_track_pairs_with_is_left_unmatched() {
        let album = local(&[
            ("Smells Like Teen Spirit", 301_000, Some(1)),
            ("In Bloom", 254_900, Some(2)),
            ("Endless, Nameless", 400_000, Some(13)),
        ]);
        let proposal = propose(&album, &nevermind(), false);
        assert_eq!(proposal.unmatched, vec![102]);
        assert_eq!(proposal.tracks.len(), 2);
        // A track the release lacks is not confident: review it.
        assert_ne!(proposal.verdict, Verdict::Confident);
    }

    #[test]
    fn a_release_named_by_the_files_is_trusted_when_every_file_pairs() {
        let mut album = local(&[
            ("Smells Like Teen Spirit", 301_000, Some(1)),
            ("In Bloom", 254_900, Some(2)),
        ]);
        // A title the catalogue would not call confident on its own.
        album.title = "NVM (my rip)".into();
        let proposal = propose(&album, &nevermind(), true);
        assert!(proposal.from_tags);
        assert_eq!(proposal.verdict, Verdict::Confident);
    }
}
