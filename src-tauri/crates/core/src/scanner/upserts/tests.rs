use super::*;
use crate::scanner::extract::ExtractedCover;
use sqlx::SqlitePool;

/// Only what `upsert_artwork` and `set_track_cover` read or write, with the
/// real constraint on `artwork.source`.
async fn fixture_pool() -> SqlitePool {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE artwork (
                 id INTEGER PRIMARY KEY,
                 hash TEXT NOT NULL UNIQUE,
                 format TEXT NOT NULL,
                 source TEXT NOT NULL
                     CHECK (source IN ('embedded','folder','deezer','manual')),
                 created_at INTEGER NOT NULL
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 artwork_id INTEGER REFERENCES artwork(id) ON DELETE SET NULL
             );
             INSERT INTO track (id) VALUES (1), (2);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

fn cover(hash: &str, format: &str) -> ExtractedCover {
    ExtractedCover {
        hash: hash.to_string(),
        format: format.to_string(),
        source: ArtworkSource::Embedded,
    }
}

async fn format_of(pool: &SqlitePool, hash: &str) -> String {
    sqlx::query_scalar("SELECT format FROM artwork WHERE hash = ?")
        .bind(hash)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A WebP cover scanned before #1100 is a row in `bin`; the scan that reads
/// the file again writes the real name and the row follows it. A format that
/// is not `bin` is a file that exists under that name, and stays.
#[tokio::test]
async fn a_bin_row_takes_the_real_extension_and_nothing_else_does() {
    let pool = fixture_pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let old = upsert_artwork(&mut conn, "aa", "bin", ArtworkSource::Embedded)
        .await
        .unwrap();
    let again = upsert_artwork(&mut conn, "aa", "webp", ArtworkSource::Embedded)
        .await
        .unwrap();
    assert_eq!(old, again);
    assert_eq!(format_of(&pool, "aa").await, "webp");

    upsert_artwork(&mut conn, "bb", "jpg", ArtworkSource::Embedded)
        .await
        .unwrap();
    upsert_artwork(&mut conn, "bb", "webp", ArtworkSource::Embedded)
        .await
        .unwrap();
    assert_eq!(format_of(&pool, "bb").await, "jpg");

    upsert_artwork(&mut conn, "aa", "bin", ArtworkSource::Embedded)
        .await
        .unwrap();
    assert_eq!(format_of(&pool, "aa").await, "webp");
}

#[tokio::test]
async fn a_track_cover_is_set_kept_and_cleared() {
    let pool = fixture_pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let own = |id: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<i64>>("SELECT artwork_id FROM track WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    set_track_cover(&mut conn, 1, Some(&cover("aa", "webp")))
        .await
        .unwrap();
    let first = own(1).await;
    assert!(first.is_some());
    assert_eq!(own(2).await, None);

    // Twice is the same row.
    set_track_cover(&mut conn, 1, Some(&cover("aa", "webp")))
        .await
        .unwrap();
    assert_eq!(own(1).await, first);

    // Another picture replaces it, and a file with none clears it.
    set_track_cover(&mut conn, 1, Some(&cover("bb", "jpg")))
        .await
        .unwrap();
    assert_ne!(own(1).await, first);
    set_track_cover(&mut conn, 1, None).await.unwrap();
    assert_eq!(own(1).await, None);
}
