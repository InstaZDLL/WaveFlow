use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;

/// A profile pool the way the application opens one: both databases on
/// disk under their real migrations, foreign keys on, and `app.db`
/// attached as `app` on every connection (`db::profile_db`). A query that
/// names a table the migrations renamed fails here as it fails there.
async fn profile_pool(dir: &Path) -> SqlitePool {
    let opts_for = |path: &Path| {
        SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .foreign_keys(true)
    };
    let app_path = dir.join("app.db");
    let app_pool = SqlitePool::connect_with(opts_for(&app_path)).await.unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&app_pool)
        .await
        .unwrap();
    app_pool.close().await;

    let attach = format!(
        "ATTACH DATABASE '{}' AS app",
        app_path.display().to_string().replace('\'', "''")
    );
    let pool = SqlitePoolOptions::new()
        .after_connect(move |conn, _meta| {
            let attach = attach.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(attach))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(opts_for(&dir.join("profile.db")))
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();

    sqlx::raw_sql(
        "INSERT INTO library (id, name, created_at, updated_at) VALUES (1, 'L', 0, 0);
         INSERT INTO artist (id, name, canonical_name) VALUES (1, 'Nirvana', 'nirvana');
         INSERT INTO album (id, title, canonical_title, artist_id, deezer_id)
              VALUES (1, 'Nevermind', 'nevermind', 1, 1234),
                     (2, 'Bleach', 'bleach', 1, NULL),
                     (3, 'In Utero', 'in utero', 1, 5678);
         INSERT INTO track
             (id, library_id, file_path, file_hash, file_size, file_modified,
              title, album_id, primary_artist, duration_ms, added_at)
         VALUES (1, 1, '/m/1.flac', 'h', 0, 0, 'In Bloom', 1, 1, 1, 0),
                (2, 1, '/m/2.flac', 'h', 0, 0, 'About a Girl', 2, 1, 1, 0),
                (3, 1, '/m/3.flac', 'h', 0, 0, 'Serve the Servants', 3, 1, 1, 0),
                (4, 1, '/m/4.flac', 'h', 0, 0, 'Loose', NULL, 1, 1, 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn cache_album(pool: &SqlitePool, deezer_id: i64, cover_url: Option<&str>) {
    sqlx::query(
        "INSERT INTO app.metadata_album
             (deezer_id, title, cover_url, fetched_at, expires_at)
         VALUES (?, 't', ?, 0, 0)",
    )
    .bind(deezer_id)
    .bind(cover_url)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn the_cached_cover_is_read_from_the_migrated_cache() {
    let dir = tempfile::tempdir().unwrap();
    let pool = profile_pool(dir.path()).await;
    // Expired on purpose: an address once cached is still served.
    cache_album(&pool, 1234, Some("https://cdn.example/nevermind.jpg")).await;

    assert_eq!(
        cached_cover_url(&pool, 1).await.unwrap().as_deref(),
        Some("https://cdn.example/nevermind.jpg")
    );
}

#[tokio::test]
async fn a_track_without_a_cached_address_is_a_miss_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let pool = profile_pool(dir.path()).await;
    cache_album(&pool, 5678, Some("")).await;

    // Album never linked to Deezer.
    assert_eq!(cached_cover_url(&pool, 2).await.unwrap(), None);
    // Linked, cached, but without an address.
    assert_eq!(cached_cover_url(&pool, 3).await.unwrap(), None);
    // No album at all.
    assert_eq!(cached_cover_url(&pool, 4).await.unwrap(), None);
    // No such track.
    assert_eq!(cached_cover_url(&pool, 99).await.unwrap(), None);
}

/// The table this lookup used to read was renamed by
/// `app/20260427000000_rename_deezer_to_metadata.sql`; the failure was
/// swallowed and every track went on to a Deezer search instead.
#[tokio::test]
async fn the_old_cache_table_is_gone_from_the_migrated_schema() {
    let dir = tempfile::tempdir().unwrap();
    let pool = profile_pool(dir.path()).await;

    let old = sqlx::query_scalar::<_, String>("SELECT cover_url FROM app.deezer_album LIMIT 1")
        .fetch_optional(&pool)
        .await;
    assert!(old.is_err(), "app.deezer_album should not exist any more");
}
