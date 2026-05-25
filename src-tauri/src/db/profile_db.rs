use std::path::Path;

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

use crate::error::AppResult;

/// Open (and create if missing) a per-profile `data.db`, apply any pending
/// migrations, and ATTACH the global `app.db` as `app` so queries can JOIN
/// across the metadata caches that live there (`metadata_artist`,
/// `metadata_album`, `lyrics`).
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
        .journal_mode(SqliteJournalMode::Wal)
        // NORMAL is the recommended pairing with WAL: fsync happens on
        // checkpoint instead of every commit. Cuts library-scan time by
        // ~5× on cold disks because we no longer pay 800 fsyncs to
        // import 800 tracks. Crash recovery is still safe — WAL guarantees
        // committed transactions survive a process kill, just not a power
        // loss within a few hundred ms of commit.
        .synchronous(SqliteSynchronous::Normal);

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
                sqlx::query(sqlx::AssertSqlSafe(stmt))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect_with(opts)
        .await?;

    // Same self-healing dance as `app_db::open` — line-ending drift
    // in the working tree gets reconciled before sqlx's strict
    // checksum check fires. Details in [`crate::db::migration_heal`].
    let migrator = sqlx::migrate!("./migrations/profile");
    super::migration_heal::heal_line_ending_drift(&pool, &migrator).await?;
    migrator.run(&pool).await?;

    Ok(pool)
}
