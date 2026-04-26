use std::path::Path;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    Executor, SqlitePool,
};

use crate::error::AppResult;

/// Open (and create if missing) a per-profile `data.db`, apply any pending
/// migrations, and ATTACH the global `app.db` as `app` so queries can JOIN
/// across the metadata caches that live there (`deezer_artist`,
/// `deezer_album`, `lyrics`).
///
/// `app_db_path` must point at an existing `app.db` — it's opened in the
/// `app_db::open()` step before any profile pool is created, so by the time
/// we get here the file exists on disk.
pub async fn open(path: &Path, app_db_path: &Path) -> AppResult<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);

    // ATTACH is per-connection in SQLite, so we install an after_connect hook
    // that runs once for every connection the pool spins up. SQLite does not
    // accept parameter binding for the ATTACH path, so we inline it after
    // escaping single quotes — the path is ours, not user input.
    let app_db_path_owned = app_db_path.to_path_buf();

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .after_connect(move |conn, _meta| {
            let app_db_path_owned = app_db_path_owned.clone();
            Box::pin(async move {
                let path_str = app_db_path_owned.to_string_lossy().into_owned();
                let escaped = path_str.replace('\'', "''");
                let stmt = format!("ATTACH DATABASE '{escaped}' AS app");
                conn.execute(stmt.as_str()).await?;
                Ok(())
            })
        })
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations/profile").run(&pool).await?;

    Ok(pool)
}
