use super::*;
use sqlx::SqlitePool;

/// The `album` columns the rule reads, with the real
/// `UNIQUE (canonical_title, artist_id)` — without it the collision
/// case would pass against a schema that cannot fail.
async fn fixture_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE album (
                 id INTEGER PRIMARY KEY,
                 canonical_title TEXT NOT NULL,
                 artist_id INTEGER,
                 album_artist TEXT,
                 is_compilation INTEGER NOT NULL DEFAULT 0,
                 UNIQUE (canonical_title, artist_id)
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 album_id INTEGER
             );",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn seed(pool: &SqlitePool, album_artist: Option<&str>, is_compilation: bool) {
    sqlx::query(
        "INSERT INTO album (id, canonical_title, artist_id, album_artist, is_compilation)
             VALUES (1, 'record', 10, ?, ?)",
    )
    .bind(album_artist)
    .bind(is_compilation as i64)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO track (id, album_id) VALUES (1, 1)")
        .execute(pool)
        .await
        .unwrap();
}

async fn album_artist_id(pool: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT artist_id FROM album WHERE id = 1")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn repoint(pool: &SqlitePool, previous: Option<i64>, new: i64) -> bool {
    let mut conn = pool.acquire().await.unwrap();
    repoint_fallback_album_artist(&mut conn, 1, previous, new)
        .await
        .unwrap()
}

#[tokio::test]
async fn follows_the_track_that_keyed_the_album() {
    let pool = fixture_pool().await;
    seed(&pool, None, false).await;
    assert!(repoint(&pool, Some(10), 20).await);
    assert_eq!(album_artist_id(&pool).await, Some(20));
}

#[tokio::test]
async fn keeps_an_explicit_album_artist() {
    let pool = fixture_pool().await;
    seed(&pool, Some("The Band"), false).await;
    assert!(!repoint(&pool, Some(10), 20).await);
    assert_eq!(album_artist_id(&pool).await, Some(10));
}

#[tokio::test]
async fn keeps_a_compilation() {
    let pool = fixture_pool().await;
    seed(&pool, None, true).await;
    assert!(!repoint(&pool, Some(10), 20).await);
    assert_eq!(album_artist_id(&pool).await, Some(10));
}

#[tokio::test]
async fn leaves_an_album_keyed_on_another_track() {
    let pool = fixture_pool().await;
    seed(&pool, None, false).await;
    assert!(!repoint(&pool, Some(30), 20).await);
    assert_eq!(album_artist_id(&pool).await, Some(10));
}

#[tokio::test]
async fn joins_the_album_that_already_holds_the_new_key() {
    let pool = fixture_pool().await;
    seed(&pool, None, false).await;
    sqlx::query("INSERT INTO album (id, canonical_title, artist_id) VALUES (2, 'record', 20)")
        .execute(&pool)
        .await
        .unwrap();
    assert!(repoint(&pool, Some(10), 20).await);
    assert_eq!(album_artist_id(&pool).await, Some(10));
    let album_id: Option<i64> = sqlx::query_scalar("SELECT album_id FROM track WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(album_id, Some(2));
}
