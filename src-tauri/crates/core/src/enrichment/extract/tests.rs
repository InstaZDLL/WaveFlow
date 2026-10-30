use super::*;
use crate::metadata::musicbrainz::Release;

/// A real lookup of one pressing of "Nevermind", cut to its first two
/// tracks. MusicBrainz data is CC0.
fn nevermind() -> Release {
    serde_json::from_str(include_str!("nevermind.json")).unwrap()
}

#[test]
fn a_real_release_lookup_parses() {
    let release = nevermind();
    assert_eq!(release.title, "Nevermind");
    assert_eq!(release.track_count(), 2);
    let side = remote_side(&release);
    assert_eq!(side.artist.as_deref(), Some("Nirvana"));
    assert_eq!(side.tracks[0].title, "Smells Like Teen Spirit");
    assert_eq!(side.tracks[0].track_number, Some(1));
    assert_eq!(side.tracks[1].duration_ms, Some(254_893));
    // One disc: no disc number to disagree with a local file's.
    assert_eq!(side.tracks[0].disc_number, None);
}

#[test]
fn facts_come_from_the_release_group() {
    let facts = album_facts(&nevermind());
    assert_eq!(facts.original_date.as_deref(), Some("1991-09-24"));
    assert_eq!(facts.original_year, Some(1991));
    // grunge 49, alternative rock 21, rock 5, punk rock 3, metal 1: a
    // quarter of the top vote keeps the first two.
    assert_eq!(facts.genres, vec!["Grunge", "Alternative Rock"]);
    assert!(facts.has_front_cover);
    assert_eq!(
        facts.release_group_mbid.as_deref(),
        Some("1b022e01-4da6-387b-8658-8678046e4cef")
    );
}

#[test]
fn credits_list_the_writers_first_and_keep_the_credited_name() {
    let release = nevermind();
    let credits = track_credits(&release.media[0].tracks[0]);
    assert_eq!(credits[0].role, "composer");
    assert_eq!(credits[0].name, "Kurt Cobain");
    assert!(credits
        .iter()
        .any(|c| c.role == "lyricist" && c.name == "Kurt Cobain"));
    let drums = credits
        .iter()
        .find(|c| c.role == "instrument" && c.detail.contains("drums"))
        .unwrap();
    // The release prints "David Grohl"; the artist is "Dave Grohl".
    assert_eq!(drums.name, "David Grohl");
    assert!(credits
        .iter()
        .any(|c| c.role == "mix" && c.name == "Andy Wallace"));
    // No duplicates of one (role, name, detail).
    let mut keys: Vec<_> = credits
        .iter()
        .map(|c| (&c.role, &c.name, &c.detail))
        .collect();
    let before = keys.len();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), before);
}

#[test]
fn genres_are_capitalised_per_word_and_per_hyphen() {
    assert_eq!(display_genre("alternative rock"), "Alternative Rock");
    assert_eq!(display_genre("lo-fi"), "Lo-Fi");
    assert_eq!(display_genre("EDM"), "EDM");
    assert_eq!(display_genre("  hip  hop "), "Hip Hop");
}

#[test]
fn a_date_out_of_range_has_no_year() {
    assert_eq!(year_of("1991-09-24"), Some(1991));
    assert_eq!(year_of("1991"), Some(1991));
    assert_eq!(year_of("0000"), None);
    assert_eq!(year_of("19"), None);
}

#[test]
fn a_lone_genre_with_no_votes_is_kept() {
    let genres = vec![Genre {
        name: "jazz".into(),
        count: 0,
    }];
    assert_eq!(top_genres(&genres), vec!["Jazz"]);
}
