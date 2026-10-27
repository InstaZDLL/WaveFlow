use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The real migrator, `foreign_keys` on: the generators write
/// `playlist` and `playlist_track` rows the schema has to accept.
async fn pool() -> SqlitePool {
    let options = waveflow_core::repository::sqlite::collation::register(
        SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true),
    );
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

fn labels() -> MixLabels {
    MixLabels {
        genre_name: "{name} Mix".into(),
        genre_description: "The best of {name}".into(),
        decade_name: "{decade}s".into(),
        decade_description: "Your {decade}s".into(),
        artist_name: "{name} Mix".into(),
        artist_description: "{name} and artists like them".into(),
    }
}

/// Rock: 30 tracks by 5 artists, all from the 1990s, played. Jazz: 10
/// tracks by 2 artists, too small for a mix, all from the 1970s.
async fn seed(pool: &SqlitePool) {
    let mut statements = vec![
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)"
            .to_string(),
        "INSERT INTO genre (id, name, canonical_name) VALUES (1, 'Rock', 'rock'),
                                                                 (2, 'Jazz', 'jazz')"
            .to_string(),
    ];
    for a in 1..=7 {
        statements.push(format!(
            "INSERT INTO artist (id, name, canonical_name) VALUES ({a}, 'A{a}', 'a{a}')"
        ));
    }
    let mut id = 0;
    let mut track = |genre: i64, artist: i64, year: i64, statements: &mut Vec<String>| {
        id += 1;
        statements.push(format!(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                    title, year, duration_ms, added_at, is_available,
                                    hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical,
                                    primary_artist)
                 VALUES ({id}, 1, '/m/{id}.flac', 'h{id}', 1, 0, 'T{id}', {year}, 1000, 0, 1,
                         0, 0, 0, 0, {artist})"
        ));
        statements.push(format!(
            "INSERT INTO track_genre (track_id, genre_id) VALUES ({id}, {genre})"
        ));
    };
    for n in 0..30 {
        track(1, 1 + n % 5, 1990 + n % 10, &mut statements);
    }
    for n in 0..10 {
        track(2, 6 + n % 2, 1975, &mut statements);
    }
    let now = chrono::Utc::now().timestamp_millis();
    statements.push(format!(
        "INSERT INTO play_event (track_id, played_at, listened_ms, completed, skipped)
             VALUES (1, {now}, 60000, 1, 0)"
    ));
    for statement in statements {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(pool)
            .await
            .unwrap();
    }
}

fn ctx(dir: &std::path::Path) -> PathsContext {
    PathsContext {
        metadata_artwork_dir: dir.join("metadata"),
        app_db_path: dir.join("app.db"),
        profile_artwork_dir: dir.join("artwork"),
    }
}

async fn names(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT name FROM playlist WHERE is_smart = 1 ORDER BY position")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_genre_and_a_decade_big_enough_get_a_mix_named_in_the_labels() {
    let pool = pool().await;
    seed(&pool).await;
    let dir = tempfile::tempdir().unwrap();
    let genres = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
        .await
        .unwrap();
    let decades = themed::regenerate_decade_mixes(&pool, &ctx(dir.path()), 1, &labels())
        .await
        .unwrap();
    // Jazz and the 1970s hold ten tracks by two artists: no mix.
    assert_eq!((genres.len(), decades.len()), (1, 1));
    assert_eq!(names(&pool).await, ["Rock Mix", "1990s"]);

    // At most three tracks per artist, every one of them Rock.
    let per_artist: Vec<i64> = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playlist_track pt JOIN track t ON t.id = pt.track_id
              WHERE pt.playlist_id = ? GROUP BY t.primary_artist",
    )
    .bind(genres[0])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(per_artist.len(), 5);
    assert!(per_artist.iter().all(|n| *n <= 3));
}

#[tokio::test]
async fn a_seed_that_no_longer_qualifies_takes_its_mix_with_it() {
    let pool = pool().await;
    seed(&pool).await;
    let dir = tempfile::tempdir().unwrap();
    let first = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    // A regen that rewrites the same mix keeps its row.
    let again = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
        .await
        .unwrap();
    assert_eq!(again, first);

    sqlx::query("UPDATE track SET is_available = 0 WHERE id > 20")
        .execute(&pool)
        .await
        .unwrap();
    let after = themed::regenerate_genre_mixes(&pool, &ctx(dir.path()), 1, &labels())
        .await
        .unwrap();
    assert!(after.is_empty());
    assert!(names(&pool).await.is_empty());
}

/// Artist 1 (20 tracks) is played, and artists 2, 3 and 4 (10 tracks
/// each) within its sessions; artist 5 a day later. No similar-artists
/// cache: the neighbours come from the sessions alone.
#[tokio::test]
async fn an_artist_mix_draws_on_the_artists_played_beside_it() {
    let pool = pool().await;
    let now = chrono::Utc::now().timestamp_millis();
    let mut statements = vec![
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)"
            .to_string(),
    ];
    let mut id = 0;
    for (artist, count) in [(1, 20), (2, 10), (3, 10), (4, 10), (5, 10)] {
        statements.push(format!(
            "INSERT INTO artist (id, name, canonical_name) VALUES ({artist}, 'A{artist}', 'a{artist}')"
        ));
        for _ in 0..count {
            id += 1;
            statements.push(format!(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                        title, duration_ms, added_at, is_available,
                                        hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical,
                                        primary_artist)
                     VALUES ({id}, 1, '/m/{id}.flac', 'h{id}', 1, 0, 'T{id}', 1000, 0, 1,
                             0, 0, 0, 0, {artist})"
            ));
        }
    }
    // Artist 1's first track, then three plays each of 2, 3 and 4
    // minutes later (a session neighbour needs `MIN_CO_LISTENS`);
    // artist 5's a day before.
    let mut plays = vec![(1, now - 600_000, 900_000), (51, now - 86_400_000, 60_000)];
    for (k, track) in [21, 31, 41].into_iter().enumerate() {
        for r in 0..3 {
            plays.push((track, now - 540_000 + (k * 3 + r) as i64 * 10_000, 60_000));
        }
    }
    for (track, at, ms) in plays {
        statements.push(format!(
            "INSERT INTO play_event (track_id, played_at, listened_ms, completed, skipped)
                 VALUES ({track}, {at}, {ms}, 1, 0)"
        ));
    }
    for statement in statements {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(&pool)
            .await
            .unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    themed::regenerate_artist_mixes(&pool, &ctx(dir.path()), 1, &labels())
        .await
        .unwrap();
    let (mix_id, name): (i64, String) =
        sqlx::query_as("SELECT id, name FROM playlist WHERE smart_rules LIKE '%\"artist_id\":1}%'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(name, "A1 Mix");

    let per_artist: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT t.primary_artist, COUNT(*) FROM playlist_track pt
               JOIN track t ON t.id = pt.track_id
              WHERE pt.playlist_id = ? GROUP BY t.primary_artist ORDER BY 1",
    )
    .bind(mix_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        per_artist,
        [(1, 15), (2, 4), (3, 4), (4, 4)],
        "the artist up to its cap, its session neighbours up to theirs, and not artist 5"
    );
}
