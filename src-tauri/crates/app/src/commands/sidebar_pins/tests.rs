use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// The real migrator, `foreign_keys` on — the cascade and the CHECK are
/// the point of the table, and a hand-written fixture would have
/// neither. The listing joins the attached `app` database, so the
/// fixture attaches one with the single table it reads.
async fn pool() -> SqlitePool {
    let options = waveflow_core::repository::sqlite::collation::register(
        SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true),
    );
    let pool = SqlitePoolOptions::new()
        // `ATTACH` is per connection.
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("ATTACH DATABASE ':memory:' AS app")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE app.metadata_artist (
                deezer_id    INTEGER PRIMARY KEY,
                picture_url  TEXT,
                picture_hash TEXT
             )",
    )
    .execute(&pool)
    .await
    .unwrap();
    for statement in [
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (1, 'One', 'one', 1, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (2, 'Two', 'two', 1, 0)",
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, 'T1', 1, 1000, 0, 1, 0, 0, 0, 0)",
        "INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 1, 0)",
        "INSERT INTO playlist (id, name, created_at, updated_at) VALUES (1, 'P', 0, 0)",
    ] {
        sqlx::raw_sql(statement).execute(&pool).await.unwrap();
    }
    pool
}

async fn listed(pool: &SqlitePool) -> Vec<(String, i64, String)> {
    list_rows(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.kind, r.target_id, r.name))
        .collect()
}

#[tokio::test]
async fn pins_list_in_the_order_they_were_pinned() {
    let pool = pool().await;
    sidebar_pin::pin(&pool, PinKind::Playlist, 1, 10)
        .await
        .unwrap();
    sidebar_pin::pin(&pool, PinKind::Album, 1, 20)
        .await
        .unwrap();
    // Inserted last, pinned first: the order is `pinned_at`, not the
    // rowid.
    sidebar_pin::pin(&pool, PinKind::Artist, 1, 5)
        .await
        .unwrap();
    // A second pin of the same album keeps its first place.
    sidebar_pin::pin(&pool, PinKind::Album, 1, 40)
        .await
        .unwrap();
    assert_eq!(
        listed(&pool).await,
        vec![
            ("artist".into(), 1, "A".into()),
            ("playlist".into(), 1, "P".into()),
            ("album".into(), 1, "One".into()),
        ]
    );
    let row = list_rows(&pool).await.unwrap().remove(2);
    assert_eq!(row.subtitle.as_deref(), Some("A"));
}

#[tokio::test]
async fn unpin_removes_only_that_pin() {
    let pool = pool().await;
    sidebar_pin::pin(&pool, PinKind::Album, 1, 1).await.unwrap();
    sidebar_pin::pin(&pool, PinKind::Playlist, 1, 2)
        .await
        .unwrap();
    assert!(sidebar_pin::unpin(&pool, PinKind::Album, 1).await.unwrap());
    assert!(!sidebar_pin::unpin(&pool, PinKind::Album, 1).await.unwrap());
    assert_eq!(
        listed(&pool).await,
        vec![("playlist".into(), 1, "P".into())]
    );
}

#[tokio::test]
async fn a_deleted_playlist_takes_its_pin_with_it() {
    let pool = pool().await;
    sidebar_pin::pin(&pool, PinKind::Playlist, 1, 1)
        .await
        .unwrap();
    sqlx::query("DELETE FROM playlist WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sidebar_pin")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn an_album_without_available_tracks_is_hidden_not_unpinned() {
    let pool = pool().await;
    // Album 2 has no track at all.
    sidebar_pin::pin(&pool, PinKind::Album, 2, 1).await.unwrap();
    assert!(listed(&pool).await.is_empty());
    sqlx::query("UPDATE track SET album_id = 2 WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(listed(&pool).await, vec![("album".into(), 2, "Two".into())]);
    sqlx::query("UPDATE track SET is_available = 0")
        .execute(&pool)
        .await
        .unwrap();
    assert!(listed(&pool).await.is_empty());
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sidebar_pin")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, 1);
}

#[tokio::test]
async fn a_pin_points_at_exactly_one_target() {
    let pool = pool().await;
    let both =
        sqlx::query("INSERT INTO sidebar_pin (album_id, artist_id, pinned_at) VALUES (1, 1, 0)")
            .execute(&pool)
            .await;
    assert!(both.is_err(), "the CHECK must refuse two targets");
    let missing = sidebar_pin::pin(&pool, PinKind::Album, 99, 0).await;
    assert!(
        missing.is_err(),
        "the foreign key must refuse a ghost album"
    );
}

#[tokio::test]
async fn merging_an_implicit_compilation_carries_its_pin() {
    let pool = pool().await;
    // One title under three different artists, no album artist, no
    // compilation flag: the post-scan pass merges them into the
    // lowest id.
    for statement in [
        "INSERT INTO artist (id, name, canonical_name) VALUES (2, 'B', 'b')",
        "INSERT INTO artist (id, name, canonical_name) VALUES (3, 'C', 'c')",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (10, 'Hits', 'hits', 1, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (11, 'Hits', 'hits', 2, 0)",
        "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (12, 'Hits', 'hits', 3, 0)",
        "UPDATE track SET album_id = 11 WHERE id = 1",
    ] {
        sqlx::raw_sql(statement).execute(&pool).await.unwrap();
    }
    sidebar_pin::pin(&pool, PinKind::Album, 11, 1)
        .await
        .unwrap();
    waveflow_core::scanner::merge_implicit_compilations(&pool)
        .await
        .unwrap();
    assert_eq!(
        listed(&pool).await,
        vec![("album".into(), 10, "Hits".into())]
    );
}
