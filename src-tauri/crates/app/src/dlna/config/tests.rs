use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

/// `app.db` as the app opens it: the real migrator, foreign keys on.
/// One connection, since every `:memory:` connection is its own database.
async fn app_db() -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// The first save on a fresh database inserts every key; it used to
/// fail on `app_setting.value_type`, so turning the server on from
/// Settings never persisted.
#[tokio::test]
async fn a_first_save_persists_every_key() {
    let pool = app_db().await;
    assert_eq!(load(&pool).await.unwrap(), DlnaConfig::default());

    let cfg = DlnaConfig {
        enabled: true,
        server_name: "Living room".into(),
        port: 18200,
    };
    save(&pool, &cfg).await.unwrap();
    assert_eq!(load(&pool).await.unwrap(), cfg);

    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT key, value_type, updated_at FROM app_setting
             WHERE key LIKE 'dlna.%' ORDER BY key",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    for (key, value_type, updated_at) in rows {
        assert_eq!(value_type, "string", "{key}");
        assert!(updated_at > 0, "{key}");
    }
}

/// Saving over keys that already exist takes the upsert's update path,
/// which the same NOT NULL check also guarded.
#[tokio::test]
async fn a_second_save_overwrites_the_first() {
    let pool = app_db().await;
    let on = DlnaConfig {
        enabled: true,
        server_name: "Den".into(),
        port: 0,
    };
    save(&pool, &on).await.unwrap();
    let off = DlnaConfig {
        enabled: false,
        ..on.clone()
    };
    save(&pool, &off).await.unwrap();
    assert_eq!(load(&pool).await.unwrap(), off);
}
