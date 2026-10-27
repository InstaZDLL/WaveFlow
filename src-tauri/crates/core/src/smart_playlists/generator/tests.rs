use super::*;

#[test]
fn rules_json_contains_slot_for_lookup() {
    let json = SmartPlaylistRules::DailyMix {
        slot: 2,
        album_mode: true,
    }
    .to_json()
    .expect("serialize");
    // The upsert query LIKEs on `"slot":N` — guard the format here so
    // a serde rename doesn't silently break refresh-in-place behaviour.
    assert!(
        json.contains("\"slot\":2"),
        "slot serialized incorrectly: {json}"
    );
    // The play path reads this field back to decide the gain, and
    // `serde(default)` means a renamed one would not fail to parse
    // — it would quietly read as "a mix of tracks" on every record
    // session there will ever be.
    assert!(
        json.contains("\"album_mode\":true"),
        "album_mode serialized incorrectly: {json}"
    );
}

/// A mix written before `album_mode` existed still has to parse, and
/// to read as what it was: a session of individual tracks.
#[test]
fn a_rule_without_the_album_flag_reads_as_track_mode() {
    let parsed: SmartPlaylistRules =
        serde_json::from_str(r#"{"kind":"daily_mix","slot":3}"#).expect("parse");
    assert!(matches!(
        parsed,
        SmartPlaylistRules::DailyMix {
            slot: 3,
            album_mode: false
        }
    ));
}

/// `bounds` is the SQL spelling of `matches`, so album mode and
/// track mode must place the same record in the same mix. Two
/// descriptions of one partition drift silently otherwise — the
/// bug would be a Daily Mix whose albums and tracks disagree about
/// what "calm" means.
#[test]
fn the_sql_bounds_agree_with_the_rust_predicate() {
    for bucket in Bucket::ALL {
        let (low, high) = bucket.bounds();
        for bpm in [
            0.0, 40.0, 94.9, 95.0, 95.1, 129.9, 130.0, 130.1, 200.0, 400.0,
        ] {
            // Spelled without `Option` combinators: the MSRV in
            // clippy.toml is 1.80, and `is_none_or` landed in 1.82.
            let by_bounds =
                low.unwrap_or(f64::NEG_INFINITY) <= bpm && bpm < high.unwrap_or(f64::INFINITY);
            assert_eq!(
                by_bounds,
                bucket.matches(bpm),
                "{bucket:?} disagrees with itself at {bpm} BPM"
            );
        }
    }
}

#[test]
fn one_artist_cannot_fill_a_daily_mix() {
    // Twelve artists, the first with a hundred tracks: an even share
    // is five each, so it takes five and the others fill the rest.
    let mut tracks: Vec<TrackPickRow> = (0..100)
        .map(|i| TrackPickRow {
            track_id: i,
            artist_id: 0,
            play_count: 0,
        })
        .collect();
    for a in 1..12 {
        for i in 0..10 {
            tracks.push(TrackPickRow {
                track_id: a * 1000 + i,
                artist_id: a,
                play_count: 0,
            });
        }
    }
    let ids = cap_per_artist(&tracks, 12, TRACKS_PER_MIX);
    assert_eq!(ids.len(), TRACKS_PER_MIX);
    assert_eq!(ids.iter().filter(|id| **id < 1000).count(), 5);
}

#[test]
fn a_small_bucket_keeps_a_floor_of_three() {
    assert_eq!(artist_cap(1, 50), 50);
    assert_eq!(artist_cap(12, 50), 5);
    assert_eq!(artist_cap(40, 50), 3);
    assert_eq!(artist_cap(0, 50), 50);
}

/// The cap keeps whichever tracks the shuffle put first, so the mix
/// is only stable across regens if the shuffle is: the same seed
/// must give the same order, and another bucket's seed another one.
#[test]
fn the_seeded_shuffle_is_reproducible() {
    let mut a: Vec<i64> = (0..100).collect();
    let mut b = a.clone();
    shuffle_with_seed(&mut a, SHUFFLE_SEED ^ 1);
    shuffle_with_seed(&mut b, SHUFFLE_SEED ^ 1);
    assert_eq!(a, b);

    let mut c: Vec<i64> = (0..100).collect();
    shuffle_with_seed(&mut c, SHUFFLE_SEED ^ 2);
    assert_ne!(a, c);
}

/// The energy bounds are the SQL spelling of `matches_energy`, as
/// `bounds` is of `matches`.
#[test]
fn the_sql_energy_bounds_agree_with_the_rust_predicate() {
    for bucket in Bucket::ALL {
        let (low, high) = bucket.energy_bounds();
        for energy in [0.0, 0.2, 0.399, 0.4, 0.401, 0.649, 0.65, 0.651, 1.0] {
            let by_bounds = low.unwrap_or(f64::NEG_INFINITY) <= energy
                && energy < high.unwrap_or(f64::INFINITY);
            assert_eq!(
                by_bounds,
                bucket.matches_energy(energy),
                "{bucket:?} disagrees with itself at energy {energy}"
            );
        }
    }
}

#[test]
fn every_energy_lands_in_exactly_one_bucket() {
    for step in 0..=100 {
        let energy = f64::from(step) / 100.0;
        let n = Bucket::ALL
            .iter()
            .filter(|b| b.matches_energy(energy))
            .count();
        assert_eq!(n, 1, "energy {energy} matched {n} buckets");
    }
}

/// The album query, run against SQLite: an album is placed by its
/// tracks' energy, and one analysed before energy existed by their
/// tempo — the same rule as the artists'.
#[tokio::test]
async fn albums_are_placed_by_energy_and_older_ones_by_tempo() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE track (id INTEGER PRIMARY KEY, album_id INTEGER,
                                 is_available INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE track_artist (track_id INTEGER, artist_id INTEGER,
                                        position INTEGER);
             CREATE TABLE play_event (id INTEGER PRIMARY KEY, track_id INTEGER);
             CREATE TABLE track_analysis (track_id INTEGER PRIMARY KEY,
                                          bpm REAL, energy REAL);
             -- Album 1: slow but intense, measured for energy.
             INSERT INTO track VALUES (1, 1, 1), (2, 1, 1), (3, 1, 1);
             INSERT INTO track_analysis VALUES (1, 70, 0.8), (2, 72, 0.75), (3, 68, 0.9);
             -- Album 2: fast, analysed before energy existed.
             INSERT INTO track VALUES (4, 2, 1), (5, 2, 1), (6, 2, 1);
             INSERT INTO track_analysis VALUES (4, 150, NULL), (5, 145, NULL), (6, 160, NULL);
             INSERT INTO track_artist VALUES (1, 9, 0), (2, 9, 0), (3, 9, 0),
                                             (4, 9, 0), (5, 9, 0), (6, 9, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let albums = |bucket| {
        let pool = pool.clone();
        async move {
            let mut ids: Vec<i64> = pick_albums_for_artists(&pool, &[9], bucket)
                .await
                .unwrap()
                .into_iter()
                .map(|a| a.album_id)
                .collect();
            ids.sort();
            ids
        }
    };
    assert_eq!(albums(Bucket::Energy).await, vec![1, 2]);
    assert!(albums(Bucket::Calm).await.is_empty());
    assert!(albums(Bucket::Groove).await.is_empty());
}

/// An artist's mean energy is over its tracks, not its plays: a calm
/// track replayed ten times does not outweigh an intense one played
/// once. Listening time still adds up every play.
#[tokio::test]
async fn an_artists_energy_is_the_mean_of_its_tracks_not_its_plays() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE artist (id INTEGER PRIMARY KEY, deezer_id INTEGER);
             CREATE TABLE track (id INTEGER PRIMARY KEY,
                                 is_available INTEGER NOT NULL DEFAULT 1);
             CREATE TABLE track_artist (track_id INTEGER, artist_id INTEGER,
                                        position INTEGER);
             CREATE TABLE play_event (id INTEGER PRIMARY KEY, track_id INTEGER,
                                      played_at INTEGER, listened_ms INTEGER);
             CREATE TABLE track_analysis (track_id INTEGER PRIMARY KEY,
                                          bpm REAL, energy REAL);
             INSERT INTO artist VALUES (1, NULL);
             INSERT INTO track VALUES (1, 1), (2, 1);
             INSERT INTO track_artist VALUES (1, 1, 0), (2, 1, 0);
             INSERT INTO track_analysis VALUES (1, 80, 0.2), (2, 140, 0.8);",
    )
    .execute(&pool)
    .await
    .unwrap();
    for i in 0..10 {
        sqlx::query(
            "INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (1, ?, 1000)",
        )
        .bind(100 + i)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO play_event (track_id, played_at, listened_ms) VALUES (2, 200, 1000)")
        .execute(&pool)
        .await
        .unwrap();

    let rows = artist_listening(&pool, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.listened_ms, 11_000);
    assert!((row.mean_energy.unwrap() - 0.5).abs() < 1e-9, "{row:?}");
    assert!((row.median_bpm.unwrap() - 110.0).abs() < 1e-9, "{row:?}");
    assert_eq!(
        Bucket::for_measures(row.mean_energy, row.median_bpm),
        Bucket::Groove
    );
}

/// Energy decides when it was measured, whatever the tempo says; the
/// tempo stands in only for a row analysed before energy existed.
#[test]
fn energy_sorts_an_artist_and_the_tempo_only_stands_in() {
    // A slow but intense artist: the tempo alone would say Calm.
    assert_eq!(Bucket::for_measures(Some(0.8), Some(70.0)), Bucket::Energy);
    // A fast but airy one: the tempo alone would say Energy.
    assert_eq!(Bucket::for_measures(Some(0.2), Some(150.0)), Bucket::Calm);
    assert_eq!(Bucket::for_measures(None, Some(150.0)), Bucket::Energy);
    assert_eq!(Bucket::for_measures(None, None), Bucket::Groove);
    assert_eq!(
        Bucket::for_measures(Some(f64::NAN), Some(70.0)),
        Bucket::Calm
    );
}

#[test]
fn bucket_thresholds_partition_full_bpm_range() {
    // Every plausible BPM should match exactly one bucket.
    for bpm_int in 30..=220 {
        let bpm = bpm_int as f64;
        let matches: Vec<_> = Bucket::ALL.iter().filter(|b| b.matches(bpm)).collect();
        assert_eq!(
            matches.len(),
            1,
            "bpm {bpm} matched {} buckets",
            matches.len()
        );
    }
}
