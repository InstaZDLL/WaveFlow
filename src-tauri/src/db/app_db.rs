use std::path::Path;

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::error::AppResult;

/// Open (and create if missing) the global `app.db` and run all pending
/// migrations against it.
///
/// The pool is configured with:
/// - WAL journal mode for better concurrent reads during writes
/// - Foreign keys enabled (off by default in SQLite)
/// - A small connection pool — `app.db` sees very little traffic
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
        .max_connections(4)
        .connect_with(opts)
        .await?;

    sqlx::migrate!("./migrations/app").run(&pool).await?;

    Ok(pool)
}
