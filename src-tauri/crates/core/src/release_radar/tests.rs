use super::*;

fn answer(groups: &str) -> Vec<u8> {
    format!(r#"{{"count":1,"offset":0,"release-groups":[{groups}]}}"#).into_bytes()
}

fn group(id: &str, title: &str, date: &str, kind: &str, credits: &str) -> String {
    format!(
        r#"{{"id":"{id}","title":"{title}","first-release-date":"{date}",
            "primary-type":"{kind}","secondary-types":[],"artist-credit":[{credits}]}}"#
    )
}

fn credit(name: &str, mbid: &str, join: &str) -> String {
    format!(
        r#"{{"name":"{name}","joinphrase":"{join}","artist":{{"id":"{mbid}","name":"{name}"}}}}"#
    )
}

#[test]
fn a_partial_date_compares_as_its_first_day() {
    assert_eq!(date_key("2026"), "2026-01-01");
    assert_eq!(date_key("2026-05"), "2026-05-01");
    assert_eq!(date_key("2026-05-17"), "2026-05-17");
    assert!(date_key("2026").as_str() >= "2025-12-31");
    assert!(date_key("2025-11").as_str() < "2025-11-02");
}

#[test]
fn the_cutoff_is_counted_back_from_now() {
    let now = DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(cutoff_date(now, 180), "2026-05-03");
}

#[test]
fn a_query_goes_by_id_when_there_is_one_and_quotes_a_name() {
    assert_eq!(
        lucene_query(&ArtistQuery::Mbid("abc-123"), "2026-01-01"),
        "arid:abc-123 AND firstreleasedate:[2026-01-01 TO *]"
    );
    assert_eq!(
        lucene_query(&ArtistQuery::Name("The \"Band\""), "2026-01-01"),
        r#"artist:"The \"Band\"" AND firstreleasedate:[2026-01-01 TO *]"#
    );
}

#[test]
fn a_release_by_a_homonym_or_out_of_the_window_is_dropped() {
    let body = answer(
        &[
            group(
                "1",
                "New",
                "2026-09-01",
                "Album",
                &credit("Nova", "mb-nova", ""),
            ),
            // Out of the window.
            group(
                "2",
                "Old",
                "2024-01-01",
                "Album",
                &credit("Nova", "mb-nova", ""),
            ),
            // Only mentions the name.
            group(
                "3",
                "Tribute",
                "2026-09-02",
                "Album",
                &credit("Nova Tribute Band", "mb-x", ""),
            ),
            // No date: cannot be called recent.
            group("4", "Undated", "", "Single", &credit("Nova", "mb-nova", "")),
        ]
        .join(","),
    );
    let page = parse_release_groups(&body, &ArtistQuery::Name("Nova"), "2026-05-01").unwrap();
    let ids: Vec<_> = page.releases.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["1"]);
    assert_eq!(page.releases[0].credit_mbid.as_deref(), Some("mb-nova"));
}

fn confirmed(ids: &[&str]) -> HashMap<String, LocalMatch> {
    ids.iter()
        .map(|id| {
            (
                id.to_string(),
                LocalMatch {
                    track_ids: vec![1],
                    matched_by: MatchedBy::Album,
                },
            )
        })
        .collect()
}

#[test]
fn a_name_two_artists_share_teaches_no_id() {
    let body = answer(
        &[
            group(
                "1",
                "A",
                "2026-09-01",
                "Album",
                &credit("Nova", "mb-one", ""),
            ),
            group(
                "2",
                "B",
                "2026-09-02",
                "Single",
                &credit("Nova", "mb-two", ""),
            ),
        ]
        .join(","),
    );
    let page = parse_release_groups(&body, &ArtistQuery::Name("Nova"), "2026-05-01").unwrap();
    assert_eq!(page.releases.len(), 2);
    // Both matched locally, under two ids: nothing is learnt.
    assert_eq!(
        confirmed_mbid(&page.releases, &confirmed(&["1", "2"])),
        None
    );
    // Only the first is in the library: its id is the artist's.
    assert_eq!(
        confirmed_mbid(&page.releases, &confirmed(&["1"])).as_deref(),
        Some("mb-one")
    );
}

#[test]
fn a_homonym_alone_in_the_window_teaches_nothing_without_the_library() {
    // The artist put nothing out lately; a homonym did.
    let body = answer(&group(
        "1",
        "Their record",
        "2026-09-01",
        "Album",
        &credit("Nova", "mb-homonym", ""),
    ));
    let page = parse_release_groups(&body, &ArtistQuery::Name("Nova"), "2026-05-01").unwrap();
    assert_eq!(page.releases.len(), 1, "still shown, as discovered");
    assert_eq!(confirmed_mbid(&page.releases, &HashMap::new()), None);
}

#[test]
fn a_query_by_id_keeps_the_releases_that_credit_that_id() {
    let body = answer(
        &[
            group(
                "1",
                "Mine",
                "2026-09-01",
                "Album",
                &credit("Nova", "mb-one", ""),
            ),
            group(
                "2",
                "Theirs",
                "2026-09-02",
                "Album",
                &credit("Nova", "mb-two", ""),
            ),
        ]
        .join(","),
    );
    let page = parse_release_groups(&body, &ArtistQuery::Mbid("mb-one"), "2026-05-01").unwrap();
    let ids: Vec<_> = page.releases.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["1"]);
    // A query by id has nothing left to learn.
    assert_eq!(confirmed_mbid(&page.releases, &confirmed(&["1"])), None);
}

#[test]
fn the_credit_is_written_whole_and_a_collaboration_still_matches() {
    let credits = format!(
        "{},{}",
        credit("Nova", "mb-nova", " feat. "),
        credit("Guest", "mb-guest", "")
    );
    let body = answer(&group("1", "Duet", "2026-09-01", "Single", &credits));
    let page = parse_release_groups(&body, &ArtistQuery::Name("nova"), "2026-05-01").unwrap();
    assert_eq!(page.releases[0].artist_credit, "Nova feat. Guest");
    assert_eq!(page.releases[0].primary_type.as_deref(), Some("Single"));
}

#[test]
fn an_answer_that_is_not_the_expected_json_is_an_error() {
    assert!(parse_release_groups(b"<html>", &ArtistQuery::Name("Nova"), "2026-05-01").is_err());
}

#[test]
fn an_edition_suffix_is_the_same_record_and_a_remix_is_not() {
    assert_eq!(title_key("Album (Deluxe Edition)"), title_key("album"));
    assert_eq!(title_key("Album [2024 Remaster]"), "album");
    assert_eq!(title_key("Song - Single"), "song");
    assert_eq!(title_key("Songs - EP"), "songs");
    assert_eq!(title_key("Album (Deluxe) - EP"), "album");
    assert_ne!(title_key("Song (Remix)"), title_key("Song"));
    // A title that is only a bracket keeps it.
    assert_eq!(title_key("(Deluxe)"), "deluxe");
    // A multibyte character before the suffix does not split.
    assert_eq!(title_key("Été - Single"), "ete");
}

fn local(
    track_id: i64,
    artist_id: i64,
    title: &str,
    album: Option<(i64, &str)>,
    number: i64,
) -> LocalTrack {
    LocalTrack {
        track_id,
        artist_id,
        title: title.to_string(),
        album_id: album.map(|a| a.0),
        album_title: album.map(|a| a.1.to_string()),
        disc_number: Some(1),
        track_number: Some(number),
    }
}

fn release(id: &str, artist_id: i64, title: &str, kind: &str) -> ReleaseRef {
    ReleaseRef {
        id: id.to_string(),
        artist_id,
        title: title.to_string(),
        primary_type: Some(kind.to_string()),
    }
}

#[test]
fn an_album_matches_its_local_album_in_track_order() {
    let tracks = vec![
        local(12, 1, "Second", Some((7, "Record (Deluxe Edition)")), 2),
        local(11, 1, "First", Some((7, "Record (Deluxe Edition)")), 1),
        local(13, 1, "Elsewhere", Some((8, "Other")), 1),
    ];
    let found = match_releases(&[release("r", 1, "Record", "Album")], &tracks);
    assert_eq!(
        found.get("r"),
        Some(&LocalMatch {
            track_ids: vec![11, 12],
            matched_by: MatchedBy::Album
        })
    );
}

#[test]
fn a_release_never_matches_another_artists_tracks() {
    let tracks = vec![local(11, 2, "Home", Some((7, "Home")), 1)];
    let found = match_releases(
        &[
            release("a", 1, "Home", "Album"),
            release("s", 1, "Home", "Single"),
        ],
        &tracks,
    );
    assert!(found.is_empty());
}

#[test]
fn a_single_falls_back_to_a_track_and_an_album_does_not() {
    // The single came out first; the user has the album it is on.
    let tracks = vec![local(21, 1, "Hit", Some((9, "Longplayer")), 3)];
    let found = match_releases(
        &[
            release("s", 1, "Hit - Single", "Single"),
            release("a", 1, "Hit", "Album"),
        ],
        &tracks,
    );
    assert_eq!(found.get("s").map(|m| m.matched_by), Some(MatchedBy::Track));
    assert_eq!(found.get("s").map(|m| m.track_ids.clone()), Some(vec![21]));
    assert!(!found.contains_key("a"));
}

#[test]
fn a_track_credited_twice_is_listed_once() {
    // One row per (track, artist) pairing: the track artist and the album
    // artist are the same here.
    let tracks = vec![
        local(31, 1, "One", Some((5, "Record")), 1),
        local(31, 1, "One", Some((5, "Record")), 1),
    ];
    let found = match_releases(&[release("r", 1, "Record", "EP")], &tracks);
    assert_eq!(found["r"].track_ids, vec![31]);
}

#[test]
fn the_links_point_at_the_release_group() {
    assert_eq!(
        cover_url("abc"),
        "https://coverartarchive.org/release-group/abc/front-250"
    );
    assert_eq!(
        musicbrainz_url("abc"),
        "https://musicbrainz.org/release-group/abc"
    );
}

#[test]
fn a_band_credit_matches_the_release_but_teaches_no_id() {
    let body = answer(&group(
        "1",
        "Live",
        "2026-09-01",
        "Album",
        &credit("Nova & The Band", "mb-band", ""),
    ));
    let page = parse_release_groups(&body, &ArtistQuery::Name("Nova"), "2026-05-01").unwrap();
    assert_eq!(page.releases.len(), 1);
    assert_eq!(page.releases[0].credit_mbid, None);
    assert_eq!(confirmed_mbid(&page.releases, &confirmed(&["1"])), None);
}
