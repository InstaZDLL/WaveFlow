use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

#[test]
fn file_name_handles_both_separators() {
    assert_eq!(file_name_of(r"E:\Music\a\b.flac"), "b.flac");
    assert_eq!(file_name_of("/home/u/Music/a/b.flac"), "b.flac");
    assert_eq!(file_name_of("bare.flac"), "bare.flac");
}

/// The repo's own profile migrations, with `foreign_keys` on.
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

/// Both reads run against the real schema, and the credit comes
/// back in `track_artist.position` order.
///
/// Neither query is compile-time checked, and the credit is built
/// by a correlated `GROUP_CONCAT` over an ordered subquery — the
/// shape that is easy to write in a way that compiles, runs, and
/// quietly returns the names in whatever order the scan reached
/// them.
#[tokio::test]
async fn the_reads_run_and_the_credit_keeps_its_order() {
    let pool = pool().await;
    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at)
                  VALUES (1, 'l', 0, 0);
             INSERT INTO artist (id, name, canonical_name)
                  VALUES (1, 'Second', 'second'), (2, 'First', 'first');
             INSERT INTO album (id, title, canonical_title, artist_id)
                  VALUES (1, 'Record', 'record', 2);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at,
                                album_id, track_number, year)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Song', 200000, 0, 1, 1, 1999);
             -- Inserted out of order on purpose: position decides, not
             -- the insertion order and not the artist id.
             INSERT INTO track_artist (track_id, artist_id, position)
                  VALUES (1, 1, 1), (1, 2, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (title, artist) = album_identity(&pool, 1).await.unwrap();
    assert_eq!(title, "Record");
    assert_eq!(artist.as_deref(), Some("First"));

    let tracks = local_tracks(&pool, 1).await.unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].artist.as_deref(), Some("First; Second"));
    assert_eq!(tracks[0].album.as_deref(), Some("Record"));
    assert_eq!(tracks[0].year, Some(1999));
}

/// An album nobody has is an error, not an empty answer: the
/// review screen would otherwise open on nothing and say nothing.
#[tokio::test]
async fn a_missing_album_is_an_error() {
    let pool = pool().await;
    assert!(album_identity(&pool, 404).await.is_err());
}
