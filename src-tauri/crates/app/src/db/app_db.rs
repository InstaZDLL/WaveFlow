use std::path::Path;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
    SqlitePool,
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

    // Reconcile any line-ending drift in `_sqlx_migrations` BEFORE
    // running the migrator — without this, a Windows working tree that
    // briefly held CRLF when a new migration was first applied will
    // panic at every subsequent boot once the file is restored to LF.
    // See [`crate::db::migration_heal`] for the full backstory.
    let migrator = sqlx::migrate!("../../migrations/app");
    super::migration_heal::heal_line_ending_drift(&pool, &migrator).await?;
    migrator.run(&pool).await?;

    // Phase 1.g.3 — UUIDs every pre-existing `profile.canonical_id`
    // row that arrived as NULL from the `20260605000000` migration.
    // Idempotent: post-first-boot this is a single zero-row SELECT.
    super::profile_meta::backfill_canonical_ids(&pool).await?;

    Ok(pool)
}
