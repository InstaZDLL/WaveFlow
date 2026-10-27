use super::*;

fn candidates(artists: usize, per_artist: usize) -> Vec<Candidate> {
    let mut out = Vec::new();
    for a in 0..artists {
        for t in 0..per_artist {
            out.push(Candidate {
                track_id: (a * 100 + t) as i64,
                primary_artist: Some(a as i64),
                plays: 0,
                title: format!("Song {a}-{t}"),
                bit_depth: None,
                sample_rate: None,
            });
        }
    }
    out
}

#[test]
fn no_artist_takes_more_than_the_cap() {
    let picked = pick(candidates(30, 10), 7);
    assert_eq!(picked.len(), TRACKS_PER_MIX);
    let mut per_artist: HashMap<i64, usize> = HashMap::new();
    for id in &picked {
        *per_artist.entry(id / 100).or_default() += 1;
    }
    assert!(per_artist.values().all(|n| *n <= PER_ARTIST_CAP));
}

#[test]
fn the_same_seed_gives_the_same_mix_and_another_seed_another() {
    assert_eq!(pick(candidates(30, 10), 7), pick(candidates(30, 10), 7));
    assert_ne!(pick(candidates(30, 10), 7), pick(candidates(30, 10), 8));
}

#[test]
fn played_tracks_lead_without_filling_the_mix() {
    // One heavily played track per artist among unplayed ones: every
    // one of them makes the mix, and the rest is still drawn.
    let mut pool = candidates(30, 10);
    for c in pool.iter_mut().filter(|c| c.track_id % 100 == 0) {
        c.plays = 40;
    }
    let picked = pick(pool, 3);
    let favourites = picked.iter().filter(|id| *id % 100 == 0).count();
    assert_eq!(favourites, 30);
    assert_eq!(picked.len(), TRACKS_PER_MIX);
}

#[test]
fn one_song_on_two_files_is_one_candidate_the_best_copy() {
    let copy = |id, title: &str, bit_depth, plays| Candidate {
        track_id: id,
        primary_artist: Some(1),
        plays,
        title: title.to_string(),
        bit_depth,
        sample_rate: Some(44_100),
    };
    let kept = dedupe(vec![
        copy(1, "Heaven", None, 3),
        copy(2, "heaven!", Some(24), 2),
        copy(3, "Meltdown", None, 0),
    ]);
    assert_eq!(kept.len(), 2);
    // The Hi-Res copy stands for the song, with both copies' plays.
    assert_eq!((kept[0].track_id, kept[0].plays), (2, 5));
    // The same title by another artist is another song.
    let other = Candidate {
        primary_artist: Some(2),
        ..copy(4, "Heaven", None, 0)
    };
    assert_eq!(dedupe(vec![copy(1, "Heaven", None, 0), other]).len(), 2);
    // Two tracks with no artist are two tracks, whatever their title.
    let anonymous = |id| Candidate {
        primary_artist: None,
        ..copy(id, "Track 1", None, 0)
    };
    assert_eq!(dedupe(vec![anonymous(5), anonymous(6)]).len(), 2);
    // Nor do two by one artist whose titles fold to nothing.
    assert_eq!(
        dedupe(vec![copy(7, "???", None, 0), copy(8, "!!!", None, 0)]).len(),
        2
    );
}

#[test]
fn the_artist_keeps_its_own_larger_cap() {
    // Artist 0 is the seed: up to 15 of its tracks, 4 of anyone else.
    let picked = pick_with(candidates(12, 20), 5, |artist| {
        if artist == Some(0) {
            SEED_ARTIST_CAP
        } else {
            NEIGHBOUR_CAP
        }
    });
    let mut per_artist: HashMap<i64, usize> = HashMap::new();
    for id in &picked {
        *per_artist.entry(id / 100).or_default() += 1;
    }
    assert!(per_artist.get(&0).copied().unwrap_or(0) <= SEED_ARTIST_CAP);
    assert!(per_artist
        .iter()
        .filter(|(a, _)| **a != 0)
        .all(|(_, n)| *n <= NEIGHBOUR_CAP));
}

#[test]
fn needles_are_unique_per_seed_and_per_family() {
    let genre = |id| {
        SmartPlaylistRules::GenreMix { genre_id: id }
            .to_json()
            .unwrap()
    };
    let one = genre(1);
    let twelve = genre(12);
    assert_eq!(one, r#"{"kind":"genre_mix","genre_id":1}"#);
    assert!(
        !twelve.contains(needle(&one)),
        "genre 1 must not match genre 12"
    );
    let decade = SmartPlaylistRules::DecadeMix { decade: 1990 }
        .to_json()
        .unwrap();
    assert_eq!(decade, r#"{"kind":"decade_mix","decade":1990}"#);
    // Neither family matches the other ones' needles, nor theirs.
    let daily = SmartPlaylistRules::DailyMix {
        slot: 1,
        album_mode: false,
    }
    .to_json()
    .unwrap();
    for other in [&daily, &decade] {
        assert!(!other.contains(needle(&one)));
    }
    assert!(!one.contains("\"slot\":"));
    assert!(!decade.contains("\"slot\":"));
}
