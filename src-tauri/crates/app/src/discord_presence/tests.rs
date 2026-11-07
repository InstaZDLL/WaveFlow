use super::*;
use sqlx::sqlite::SqlitePoolOptions;

/// One connection, so the attached in-memory `app` stays the one the
/// rows were written to. A track whose album has a cached cover address:
/// any lookup that runs finds it.
async fn pool_with_a_cached_cover() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(
        "ATTACH DATABASE ':memory:' AS app;
         CREATE TABLE album (id INTEGER PRIMARY KEY, deezer_id INTEGER);
         CREATE TABLE track (id INTEGER PRIMARY KEY, album_id INTEGER);
         CREATE TABLE app.metadata_album (deezer_id INTEGER PRIMARY KEY, cover_url TEXT);
         INSERT INTO album (id, deezer_id) VALUES (1, 42);
         INSERT INTO track (id, album_id) VALUES (7, 1);
         INSERT INTO app.metadata_album (deezer_id, cover_url)
              VALUES (42, 'https://cdn.example/cover.jpg');",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

#[tokio::test]
async fn nothing_is_looked_up_while_the_presence_is_off() {
    let pool = pool_with_a_cached_cover().await;
    let dir = tempfile::tempdir().unwrap();
    // The fixture does resolve, so a `None` below is the gate's doing.
    assert_eq!(
        resolve_cover_url(&pool, dir.path(), 1, 7).await.as_deref(),
        Some("https://cdn.example/cover.jpg")
    );

    let presence = init(false).expect("presence thread");
    assert!(!presence.is_live().await);
    assert_eq!(presence.cover_url_for(&pool, dir.path(), 1, 7).await, None);
}

#[test]
fn an_album_that_missed_is_left_alone_for_a_while() {
    // Keys no real profile uses, so tests running beside it cannot meet them.
    let missed = (-1, -10);
    let other = (-1, -11);
    assert!(!missed_recently(missed));
    record_miss(missed);
    assert!(missed_recently(missed));
    assert!(!missed_recently(other));
}
