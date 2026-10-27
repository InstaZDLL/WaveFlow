use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

/// The real migrator, `foreign_keys` on, as the browse tests do.
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

/// Two albums in 1997, the second with more tracks and a cover; one
/// track in 2003; one with no year, one tagged `0`, one unavailable.
async fn seed(pool: &SqlitePool) {
    let mut statements = vec![
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)"
            .to_string(),
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')".to_string(),
        "INSERT INTO artwork (id, hash, format, width, height, source, created_at)
             VALUES (1, 'c0ffee', 'jpg', 1, 1, 'embedded', 0)"
            .to_string(),
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (1, 'One', 'one', 1, 0), (2, 'Two', 'two', 1, 0)"
            .to_string(),
        "UPDATE album SET artwork_id = 1 WHERE id = 2".to_string(),
    ];
    let tracks: [(i64, Option<i64>, Option<i64>, i64); 7] = [
        (1, Some(1997), Some(1), 1),
        (2, Some(1997), Some(2), 1),
        (3, Some(1997), Some(2), 1),
        (4, Some(2003), Some(1), 1),
        (5, None, None, 1),
        (6, Some(0), None, 1),
        (7, Some(1997), Some(1), 0),
    ];
    for (id, year, album, available) in tracks {
        let year = year.map_or("NULL".into(), |y| y.to_string());
        let album = album.map_or("NULL".into(), |a| a.to_string());
        statements.push(format!(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                    title, album_id, year, duration_ms, added_at, is_available,
                                    hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical,
                                    primary_artist)
                 VALUES ({id}, 1, '/m/{id}.flac', 'h{id}', 1, 0, 'T{id}', {album}, {year},
                         1000, 0, {available}, 0, 0, 0, 0, 1)"
        ));
    }
    for statement in statements {
        sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn years_newest_first_unknown_last() {
    let pool = pool().await;
    seed(&pool).await;
    let rows = sqlx::query_as::<_, YearRawRow>(list_years_sql())
        .bind(None::<i64>)
        .bind(None::<i64>)
        .fetch_all(&pool)
        .await
        .unwrap();
    let summary: Vec<_> = rows
        .iter()
        .map(|r| (r.year, r.track_count, r.album_count, r.artwork_hash.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            (Some(2003), 1, 1, None),
            // The unavailable track is not counted; album 2 holds most
            // of the year and lends its cover.
            (Some(1997), 3, 2, Some("c0ffee".to_string())),
            // No year and a year of 0 are the same bucket.
            (None, 2, 0, None),
        ]
    );
}

#[tokio::test]
async fn one_year_and_the_unknown_bucket() {
    let pool = pool().await;
    seed(&pool).await;
    let sql = || detail_tracks_sql("", &format!("({TRACK_YEAR}) IS ?"));
    let ids = |rows: Vec<DetailTrackRaw>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();

    let mut in_1997 = ids(sqlx::query_as::<_, DetailTrackRaw>(sql())
        .bind(Some(1997_i64))
        .fetch_all(&pool)
        .await
        .unwrap());
    in_1997.sort();
    assert_eq!(in_1997, [1, 2, 3]);

    let mut unknown = ids(sqlx::query_as::<_, DetailTrackRaw>(sql())
        .bind(None::<i64>)
        .fetch_all(&pool)
        .await
        .unwrap());
    unknown.sort();
    assert_eq!(unknown, [5, 6]);
}
