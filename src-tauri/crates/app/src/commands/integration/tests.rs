use super::store_lastfm_api_key;

async fn app_pool(dir: &std::path::Path) -> sqlx::SqlitePool {
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    let options =
        SqliteConnectOptions::from_str(&format!("sqlite://{}", dir.join("app.db").display()))
            .unwrap()
            .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&pool)
        .await
        .unwrap();
    pool
}

async fn seed(pool: &sqlx::SqlitePool, deezer_id: i64, source: Option<&str>) {
    sqlx::query(
        "INSERT INTO metadata_artist (deezer_id, name, bio_source, fetched_at, expires_at)
             VALUES (?, 'artist', ?, 1, 9999999999999)",
    )
    .bind(deezer_id)
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

async fn expires_at(pool: &sqlx::SqlitePool, deezer_id: i64) -> i64 {
    sqlx::query_scalar("SELECT expires_at FROM metadata_artist WHERE deezer_id = ?")
        .bind(deezer_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A biography cached while no key was set is empty and stamped
/// `lastfm`; placing a key must make it ask again rather than serve
/// that emptiness for the rest of the month. Biographies from another
/// source owe the key nothing and keep their TTL. A row with no source
/// predates the column and is read as Last.fm, so it expires too.
#[tokio::test]
async fn placing_or_clearing_the_key_expires_only_lastfm_biographies() {
    for (before, after) in [
        (None, Some("a-key-from-the-settings")),
        (Some("a-key-from-the-settings"), Some("another-key")),
        (Some("a-key-from-the-settings"), None),
    ] {
        assert_eq!(
            save_over(before, after).await,
            [0, KEPT, 0],
            "{before:?} -> {after:?}"
        );
    }
}

/// The Settings card sends the key on every save, changed or not; a
/// save that changes nothing must not cost the biographies their cache.
#[tokio::test]
async fn saving_the_stored_key_again_expires_nothing() {
    for key in [Some("a-key-from-the-settings"), None] {
        assert_eq!(save_over(key, key).await, [KEPT; 3], "{key:?}");
    }
}

const KEPT: i64 = 9_999_999_999_999;

/// Store `before`, seed a `lastfm`, a `theaudiodb` and a source-less
/// row, save `after`, and read the three expiries back.
async fn save_over(before: Option<&str>, after: Option<&str>) -> [i64; 3] {
    let dir = tempfile::tempdir().unwrap();
    let pool = app_pool(dir.path()).await;
    store_lastfm_api_key(&pool, before, 1).await.unwrap();
    seed(&pool, 1, Some("lastfm")).await;
    seed(&pool, 2, Some("theaudiodb")).await;
    seed(&pool, 3, None).await;

    store_lastfm_api_key(&pool, after, 42).await.unwrap();

    [
        expires_at(&pool, 1).await,
        expires_at(&pool, 2).await,
        expires_at(&pool, 3).await,
    ]
}
