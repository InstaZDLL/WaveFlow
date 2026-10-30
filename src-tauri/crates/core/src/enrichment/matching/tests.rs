use super::*;

fn track(title: &str, ms: i64, n: i64) -> TrackSignals {
    TrackSignals {
        title: title.to_owned(),
        duration_ms: Some(ms),
        track_number: Some(n),
        disc_number: None,
    }
}

fn side(title: &str, artist: &str, tracks: Vec<TrackSignals>) -> AlbumSide {
    AlbumSide {
        title: title.to_owned(),
        artist: Some(artist.to_owned()),
        artist_names: artist.split("; ").map(str::to_owned).collect(),
        tracks,
    }
}

fn nevermind() -> Vec<TrackSignals> {
    vec![
        track("Smells Like Teen Spirit", 301_000, 1),
        track("In Bloom", 254_000, 2),
        track("Come as You Are", 219_000, 3),
    ]
}

#[test]
fn an_identical_tracklist_is_confident() {
    let local = side("Nevermind", "Nirvana", nevermind());
    let remote = side("Nevermind", "Nirvana", nevermind());
    let m = match_album(&local, &remote);
    assert_eq!(m.verdict, Verdict::Confident);
    assert_eq!(m.score.paired, 3);
}

#[test]
fn an_edition_suffix_does_not_stop_a_confident_match() {
    let local = side("Nevermind (Remastered)", "Nirvana", nevermind());
    let remote = side("Nevermind", "Nirvana", nevermind());
    assert_eq!(match_album(&local, &remote).verdict, Verdict::Confident);
}

#[test]
fn a_partial_album_is_confident_when_every_local_track_pairs() {
    let local = side("Nevermind", "Nirvana", nevermind()[..2].to_vec());
    let remote = side("Nevermind", "Nirvana", nevermind());
    assert_eq!(match_album(&local, &remote).verdict, Verdict::Confident);
}

#[test]
fn a_local_track_the_release_lacks_drops_to_review() {
    let mut tracks = nevermind();
    tracks.push(track("Bonus Demo", 180_000, 4));
    let local = side("Nevermind", "Nirvana", tracks);
    let remote = side("Nevermind", "Nirvana", nevermind());
    let m = match_album(&local, &remote);
    assert_eq!(m.verdict, Verdict::Doubtful, "{:?}", m.score);
}

#[test]
fn a_namesake_album_by_another_artist_is_never_confident() {
    let local = side("Greatest Hits", "Queen", nevermind());
    let remote = side("Greatest Hits", "ABBA", nevermind());
    assert_ne!(match_album(&local, &remote).verdict, Verdict::Confident);
}

#[test]
fn different_durations_are_not_confident() {
    let local = side("Nevermind", "Nirvana", nevermind());
    let live: Vec<_> = nevermind()
        .into_iter()
        .map(|mut t| {
            t.duration_ms = t.duration_ms.map(|d| d + 40_000);
            t
        })
        .collect();
    let remote = side("Nevermind", "Nirvana", live);
    assert_ne!(match_album(&local, &remote).verdict, Verdict::Confident);
}

#[test]
fn unrelated_albums_are_no_match() {
    let local = side("Nevermind", "Nirvana", nevermind());
    let remote = side(
        "Abbey Road",
        "The Beatles",
        vec![track("Come Together", 259_000, 1)],
    );
    assert_eq!(match_album(&local, &remote).verdict, Verdict::NoMatch);
}

#[test]
fn split_credits_agree_with_a_joined_one() {
    let local = side("Collab", "Alpha; Beta", vec![track("Song", 200_000, 1)]);
    let mut remote = side("Collab", "Alpha & Beta", vec![track("Song", 200_000, 1)]);
    remote.artist_names = vec!["Alpha".into(), "Beta".into()];
    let m = match_album(&local, &remote);
    assert!(m.score.artist >= CONFIDENT_ARTIST);
    assert_eq!(m.verdict, Verdict::Confident);
}

#[test]
fn a_near_miss_artist_spelling_is_capped_below_confident() {
    let local = side("Album", "Jon Smith", vec![track("Song", 200_000, 1)]);
    let remote = side("Album", "John Smith", vec![track("Song", 200_000, 1)]);
    let m = match_album(&local, &remote);
    assert!(m.score.artist < CONFIDENT_ARTIST);
    assert_ne!(m.verdict, Verdict::Confident);
}

#[test]
fn strip_edition_keeps_the_title_when_nothing_would_be_left() {
    assert_eq!(strip_edition("Nevermind (Deluxe Edition)"), "Nevermind");
    assert_eq!(strip_edition("Kool - Single"), "Kool");
    assert_eq!(strip_edition("Blue - EP"), "Blue");
    assert_eq!(strip_edition("(What's the Story)"), "(What's the Story)");
    assert_eq!(strip_edition("Plain"), "Plain");
}

#[test]
fn prescore_prefers_the_hit_with_the_right_track_count() {
    let local = side("Nevermind", "Nirvana", nevermind());
    let right = prescore(&local, "Nevermind", "Nirvana", Some(3));
    let wrong = prescore(&local, "Nevermind", "Nirvana", Some(1));
    let other = prescore(&local, "Bleach", "Nirvana", Some(3));
    assert!(right > wrong && right > other);
}

#[test]
fn a_guest_credit_in_the_title_does_not_stop_the_pairing() {
    let local = side(
        "Waterfalls",
        "James Hype",
        vec![track(
            "Waterfalls (feat. Sam Harper & Bobby Harvey)",
            200_000,
            1,
        )],
    );
    let remote = side(
        "Waterfalls",
        "James Hype",
        vec![track("Waterfalls", 200_500, 1)],
    );
    assert_eq!(match_album(&local, &remote).verdict, Verdict::Confident);
    // A remix is another recording, not a guest credit.
    assert_eq!(strip_featuring("Dracula (Remix)"), "Dracula (Remix)");
    assert_eq!(strip_featuring("Song [ft. X]"), "Song");
    assert_eq!(
        strip_featuring("Did It First (with Central Cee)"),
        "Did It First"
    );
    assert_eq!(strip_featuring("(feat. Nobody)"), "(feat. Nobody)");
}
