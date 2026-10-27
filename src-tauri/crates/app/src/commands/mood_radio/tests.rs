use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// The repo's own profile migrations, with `foreign_keys` on.
///
/// A hand-written fixture would let these queries pass against a
/// schema the app never has — the same reason the inventory tests
/// run the real migrations.
async fn pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// Every query in this module runs, against the real schema, for
/// every mood.
///
/// The tempo gate is pasted into three statements by a macro and
/// none of them is a compile-time-checked query: a missing
/// parenthesis or a column that does not exist would compile
/// perfectly and fail the first time a user pressed a mood tile.
/// An empty library is enough to prove the SQL parses and that the
/// placeholders line up with the binds.
#[tokio::test]
async fn every_mood_query_parses_and_binds() {
    let pool = pool().await;
    for mood in [
        Mood::Focus,
        Mood::Chill,
        Mood::Workout,
        Mood::Party,
        Mood::Sleep,
    ] {
        assert!(candidate_pool(&pool, mood, 10).await.unwrap().is_empty());
        assert!(mood_radio_by_album(&pool, mood).await.unwrap().is_empty());
        assert_eq!(count_for_mood(&pool, mood).await.unwrap(), 0);
        for kind in [OverrideKind::Include, OverrideKind::Exclude] {
            assert!(overridden_tracks(&pool, mood, kind)
                .await
                .unwrap()
                .is_empty());
        }
    }
    assert_eq!(analysis_coverage(&pool).await.unwrap(), (0, 0));
}

/// An octave-out tempo reaches the pool, which is what the gate in
/// SQL exists for — the scorer cannot rescue a row the query never
/// returned.
#[tokio::test]
async fn the_pool_accepts_a_doubled_tempo() {
    let pool = pool().await;
    sqlx::query(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Fast', 200000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'Slow', 200000, 0),
                         (3, 1, '/l/c.flac', 'h3', 1, 0, 'Plain', 200000, 0);
             -- 30 BPM, not 40: doubled, 40 lands on 80, which is
             -- inside Focus. Every reading of 30 (30, 60, 15) is out.
             INSERT INTO track_analysis (track_id, bpm, analyzed_at)
                  VALUES (1, 170.0, 0), (2, 30.0, 0), (3, 90.0, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let ids: Vec<i64> = candidate_pool(&pool, Mood::Focus, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.track_id)
        .collect();
    assert!(
        ids.contains(&1),
        "170 BPM must reach a mood built on 88, as a halved reading"
    );
    assert!(!ids.contains(&2), "30 BPM is out under every reading");
    assert_eq!(count_for_mood(&pool, Mood::Focus).await.unwrap(), 2);

    // A measured match comes before a corrected one, whatever the
    // shuffle does inside each group: a guess must not take a place
    // in the pool from a track that really is this tempo.
    assert_eq!(
        ids.first(),
        Some(&3),
        "90 BPM is measured inside the window; 170 is a rescue"
    );
}

/// Sleep keeps a quiet slow track and nothing else: not a fast one
/// read at half speed, not a loud one the estimator read as slow.
/// Before, the halved reading alone let in every track up to 136.
#[tokio::test]
async fn sleep_keeps_only_quiet_slow_tracks() {
    let pool = pool().await;
    sqlx::query(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Lullaby', 200000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'Dance', 200000, 0),
                         (3, 1, '/l/c.flac', 'h3', 1, 0, 'Loud', 200000, 0),
                         (4, 1, '/l/d.flac', 'h4', 1, 0, 'Unmeasured', 200000, 0);
             INSERT INTO track_analysis (track_id, bpm, loudness_lufs, analyzed_at)
                  VALUES (1, 55.0, -20.0, 0), (2, 120.0, -16.0, 0),
                         (3, 62.0, -7.0, 0), (4, 60.0, NULL, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut ids: Vec<i64> = candidate_pool(&pool, Mood::Sleep, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.track_id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 4]);
    assert_eq!(count_for_mood(&pool, Mood::Sleep).await.unwrap(), 2);

    // Taken out by hand: out of the pool and the count. Put in by
    // hand: counted, though its tempo is not Sleep's at all.
    sqlx::query(
        "INSERT INTO mood_override (mood, track_id, kind, added_at)
                  VALUES ('sleep', 4, 'exclude', 0), ('sleep', 2, 'include', 0),
                         ('focus', 1, 'exclude', 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let ids: Vec<i64> = candidate_pool(&pool, Mood::Sleep, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.track_id)
        .collect();
    assert_eq!(ids, vec![1], "another mood's removal does not apply");
    assert_eq!(count_for_mood(&pool, Mood::Sleep).await.unwrap(), 2);
    assert_eq!(
        overridden_tracks(&pool, Mood::Sleep, OverrideKind::Include)
            .await
            .unwrap(),
        vec![2]
    );
}

/// In album mode a record plays whole, so a single track measured
/// over Sleep's loudness gate rules the record out — the tempo fit
/// only asks that most of it be right, and would queue that track.
#[tokio::test]
async fn sleep_refuses_a_record_with_one_loud_track() {
    let pool = pool().await;
    sqlx::query(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO album (id, title, canonical_title)
                  VALUES (1, 'Quiet', 'quiet'), (2, 'Mostly quiet', 'mostly quiet');
             INSERT INTO track (id, library_id, album_id, track_number, file_path,
                                file_hash, file_size, file_modified, title,
                                duration_ms, added_at)
                  VALUES (1, 1, 1, 1, '/q/1.flac', 'q1', 1, 0, 'a', 200000, 0),
                         (2, 1, 1, 2, '/q/2.flac', 'q2', 1, 0, 'b', 200000, 0),
                         (3, 1, 1, 3, '/q/3.flac', 'q3', 1, 0, 'c', 200000, 0),
                         (4, 1, 2, 1, '/m/1.flac', 'm1', 1, 0, 'd', 200000, 0),
                         (5, 1, 2, 2, '/m/2.flac', 'm2', 1, 0, 'e', 200000, 0),
                         (6, 1, 2, 3, '/m/3.flac', 'm3', 1, 0, 'f', 200000, 0),
                         (7, 1, 2, 4, '/m/4.flac', 'm4', 1, 0, 'g', 200000, 0);
             -- Four on the second record, so that taking the loud one out
             -- still leaves the three analysed tracks a record needs.
             INSERT INTO track_analysis (track_id, bpm, loudness_lufs, analyzed_at)
                  VALUES (1, 55.0, -20.0, 0), (2, 55.0, -20.0, 0), (3, 55.0, NULL, 0),
                         (4, 55.0, -20.0, 0), (5, 55.0, -20.0, 0), (6, 55.0, -7.0, 0),
                         (7, 55.0, -20.0, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let ids = mood_radio_by_album(&pool, Mood::Sleep).await.unwrap();
    assert_eq!(ids, vec![1, 2, 3], "an unmeasured track does not count");

    // Taking the loud track out of Sleep by hand lets its record in:
    // what is left of it is all quiet.
    sqlx::query(
        "INSERT INTO mood_override (mood, track_id, kind, added_at)
                  VALUES ('sleep', 6, 'exclude', 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut ids = mood_radio_by_album(&pool, Mood::Sleep).await.unwrap();
    ids.sort_unstable();
    assert_eq!(
        ids,
        vec![1, 2, 3, 4, 5, 6, 7],
        "the queue reads the whole record"
    );
}

/// The SQL gate and the scorer must agree on what "trusted" means,
/// or a track would be ranked as rescued by a query that never
/// returned it — or returned by one the scorer then refuses.
#[test]
fn the_sql_threshold_is_the_core_one() {
    let sql: f64 = trusted_tempo!().parse().unwrap();
    assert_eq!(sql, waveflow_core::mood::TRUSTED_TEMPO);
}

/// A tempo the estimator is sure of is not doubled or halved into a
/// mood; a doubtful one, or one from analysis version 1, still is.
#[tokio::test]
async fn only_a_doubtful_tempo_is_read_at_another_octave() {
    let pool = pool().await;
    sqlx::query(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Sure', 200000, 0),
                         (2, 1, '/l/b.flac', 'h2', 1, 0, 'Doubtful', 200000, 0),
                         (3, 1, '/l/c.flac', 'h3', 1, 0, 'Version 1', 200000, 0);
             INSERT INTO track_analysis (track_id, bpm, bpm_confidence, analyzed_at)
                  VALUES (1, 170.0, 0.9, 0), (2, 170.0, 0.1, 0), (3, 170.0, NULL, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut ids: Vec<i64> = candidate_pool(&pool, Mood::Focus, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.track_id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![2, 3]);
    assert_eq!(count_for_mood(&pool, Mood::Focus).await.unwrap(), 2);
}

/// Sleep keeps out a track measured as intense, at any loudness —
/// the case loudness could not catch once a library is normalised —
/// and lets an unmeasured one through. A record with one intense
/// track is out whole, since it would play that track too.
#[tokio::test]
async fn sleep_keeps_out_what_is_measured_as_intense() {
    let pool = pool().await;
    sqlx::query(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'l', 0, 0);
             INSERT INTO album (id, title, canonical_title)
                  VALUES (1, 'Calm', 'calm'), (2, 'Mixed', 'mixed');
             INSERT INTO track (id, library_id, album_id, track_number, file_path,
                                file_hash, file_size, file_modified, title,
                                duration_ms, added_at)
                  VALUES (1, 1, 1, 1, '/c/1.flac', 'c1', 1, 0, 'a', 200000, 0),
                         (2, 1, 1, 2, '/c/2.flac', 'c2', 1, 0, 'b', 200000, 0),
                         (3, 1, 1, 3, '/c/3.flac', 'c3', 1, 0, 'c', 200000, 0),
                         (4, 1, 2, 1, '/m/1.flac', 'm1', 1, 0, 'd', 200000, 0),
                         (5, 1, 2, 2, '/m/2.flac', 'm2', 1, 0, 'e', 200000, 0),
                         (6, 1, 2, 3, '/m/3.flac', 'm3', 1, 0, 'f', 200000, 0);
             -- All at -14 LUFS, a normalised library: loudness tells
             -- nothing apart here, only energy does.
             INSERT INTO track_analysis
                    (track_id, bpm, bpm_confidence, loudness_lufs, energy, analyzed_at)
                  VALUES (1, 55.0, 0.8, -14.0, 0.1, 0),
                         (2, 55.0, 0.8, -14.0, NULL, 0),
                         (3, 55.0, 0.8, -14.0, 0.2, 0),
                         (4, 55.0, 0.8, -14.0, 0.1, 0),
                         (5, 55.0, 0.8, -14.0, 0.9, 0),
                         (6, 55.0, 0.8, -14.0, 0.2, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut ids: Vec<i64> = candidate_pool(&pool, Mood::Sleep, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.track_id)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2, 3, 4, 6], "the intense track is out");
    assert_eq!(count_for_mood(&pool, Mood::Sleep).await.unwrap(), 5);

    let albums = mood_radio_by_album(&pool, Mood::Sleep).await.unwrap();
    assert_eq!(albums, vec![1, 2, 3], "the mixed record is out whole");
}
