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

/// A merge that is due while another connection holds the write lock waits
/// for it, then merges. The pass used to read everything first and fail its
/// first write at once with `SQLITE_BUSY`: a deferred transaction that has
/// read cannot wait for the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_due_merge_waits_for_the_lock_instead_of_failing() {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use sqlx::{ConnectOptions, Connection};

    let dir = tempfile::tempdir().unwrap();
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join("data.db"))
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(3));
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .connect_with(options.clone())
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0)")
        .execute(&pool)
        .await
        .unwrap();
    // The Various Artists row already exists, as it does after the first
    // compilation a library meets: the pass then only reads until it merges.
    sqlx::query("INSERT INTO artist (id, name, canonical_name) VALUES (9, ?, ?)")
        .bind(VARIOUS_ARTISTS_LABEL)
        .bind(canonical_name(VARIOUS_ARTISTS_LABEL))
        .execute(&pool)
        .await
        .unwrap();
    // One record in one directory, a different artist on each track.
    for n in 1..=3_i64 {
        sqlx::query("INSERT INTO artist (id, name, canonical_name) VALUES (?, ?, ?)")
            .bind(n)
            .bind(format!("Artist {n}"))
            .bind(format!("artist {n}"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO album (id, title, canonical_title, artist_id) VALUES (?, 'Record', 'record', ?)",
        )
        .bind(n)
        .bind(n)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track
                (id, library_id, file_path, file_hash, file_size, file_modified,
                 title, album_id, primary_artist, track_number, duration_ms, added_at)
             VALUES (?, 1, ?, 'h', 0, 0, 'Song', ?, ?, ?, 1000, 0)",
        )
        .bind(n)
        .bind(format!("/music/Record/{n:02}.flac"))
        .bind(n)
        .bind(n)
        .bind(n)
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut holder = options.connect().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut holder)
        .await
        .unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        sqlx::query("COMMIT").execute(&mut holder).await.unwrap();
        holder.close().await.unwrap();
    });

    merge_implicit_compilations(&pool)
        .await
        .expect("the merge waits for the lock");
    release.await.unwrap();

    let albums: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT DISTINCT t.album_id, al.artist_id FROM track t JOIN album al ON al.id = t.album_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(albums, vec![(1, 9)], "one Various Artists record");
}
