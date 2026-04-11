use std::path::Path;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::error::AppResult;

/// Open (and create if missing) a per-profile `data.db` and apply any pending
/// migrations.
///
/// Uses a slightly larger pool than [`crate::db::app_db::open`] because this
/// database handles all scans, playlists, queue and analytics reads/writes.
pub async fn open(path: &Path) -> AppResult<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations/profile").run(&pool).await?;

    Ok(pool)
}
