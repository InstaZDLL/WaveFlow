//! Bringing an imported database's migration history in line with this
//! build, and checkpointing its WAL.

use std::path::Path;

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, SqlitePool};

use crate::error::{AppError, AppResult};

// ── helpers ────────────────────────────────────────────────────────

/// Rewrite `_sqlx_migrations.checksum` for every previously-applied
/// migration so it matches the SHA-384 of the *local* migration file
/// bundled into the running binary. Called on a freshly extracted
/// `data.db` before the sqlx migrator runs.
///
/// Three failure modes the caller surfaces verbatim:
///   - Local migrator missing a version present in the archive
///     → the archive is from a *newer* build than the one importing it,
///     and we genuinely can't roll the schema forward.
///   - A version both sides have whose stored checksum is not that of
///     our SQL, in either line-ending form → the archive's build applied
///     different SQL under the same migration (one amended before a
///     release, a fork), and its schema is not the one ours expects.
///   - Anything else → propagated as a generic `Other` error.
///
/// Only the line-ending case is rewritten, the one
/// [`crate::db::migration_heal`] repairs on every open: the same SQL,
/// checked out with CRLF by an older Windows build. Every row is checked
/// before the first is rewritten, so a refused archive is left as it
/// came.
pub(super) async fn normalise_migration_checksums(db_path: &Path) -> AppResult<()> {
    let migrator = sqlx::migrate!("../../migrations/profile");

    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        // Skip the noisy "executing statement" log line on every checksum
        // UPDATE — these are pure plumbing rewrites, not user-visible
        // DB activity.
        .disable_statement_logging();
    let mut conn = opts.connect().await?;

    // The archive may predate the introduction of `_sqlx_migrations`
    // (very unlikely, but we don't want to crash on the bootstrap case).
    let table_exists: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_optional(&mut conn)
    .await?;
    if table_exists.is_none() {
        conn.close().await?;
        return Ok(());
    }

    let stored: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(&mut conn)
            .await?;

    let verdict = judge_migrations(&migrator, &stored);
    // Closed before the caller removes the extracted profile: on Windows
    // an open `data.db` would keep its folder from going.
    let drifted = match verdict {
        MigrationVerdict::Compatible { drifted } => drifted,
        MigrationVerdict::NewerBuild { version } => {
            conn.close().await?;
            return Err(AppError::Other(format!(
                "archive contains migration {version} not present in this build — \
                 export was produced by a newer WaveFlow version"
            )));
        }
        MigrationVerdict::DifferentSql {
            version,
            description,
        } => {
            conn.close().await?;
            return Err(AppError::Other(format!(
                "archive was not imported: its migration {version} ({description}) was applied \
                 with different SQL than the one this build ships — export was produced by a \
                 different WaveFlow build"
            )));
        }
    };
    for (version, checksum) in drifted {
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
            .bind(checksum)
            .bind(version)
            .execute(&mut conn)
            .await?;
    }

    conn.close().await?;
    Ok(())
}

/// What this build makes of the migrations an archive's `data.db` records.
///
/// One judgement for the import and its preview, so the preview never
/// promises an import the import then refuses.
pub(super) enum MigrationVerdict {
    /// Every recorded migration is one this build ships, with the same SQL.
    /// `drifted` lists the ones recorded under a CRLF checkout of that SQL,
    /// with the checksum the import rewrites them to.
    Compatible { drifted: Vec<(i64, Vec<u8>)> },
    /// The archive's build applied a migration this one does not have.
    NewerBuild { version: i64 },
    /// Both have the migration, but the archive's build applied other SQL
    /// under it (one amended before a release, a fork).
    DifferentSql { version: i64, description: String },
}

pub(super) fn judge_migrations(
    migrator: &sqlx::migrate::Migrator,
    stored: &[(i64, Vec<u8>)],
) -> MigrationVerdict {
    let mut drifted = Vec::new();
    for (version, stored_checksum) in stored {
        let Some(local) = migrator.iter().find(|m| m.version == *version) else {
            return MigrationVerdict::NewerBuild { version: *version };
        };
        if local.checksum.as_ref() == stored_checksum.as_slice() {
            continue;
        }
        if !crate::db::migration_heal::is_line_ending_drift(local.sql.as_str(), stored_checksum) {
            return MigrationVerdict::DifferentSql {
                version: *version,
                description: local.description.to_string(),
            };
        }
        drifted.push((*version, local.checksum.to_vec()));
    }
    MigrationVerdict::Compatible { drifted }
}

/// Force a full WAL checkpoint so the archive captures every committed
/// page. `TRUNCATE` resets the WAL file to zero length on success,
/// which also keeps `.waveflow` archives from carrying a stale
/// sidecar's worth of bytes.
pub(crate) async fn checkpoint_wal(pool: &SqlitePool) -> AppResult<()> {
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(pool)
        .await?;
    Ok(())
}
