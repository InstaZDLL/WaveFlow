//! Refuse a database written by a newer build — and say why.
//!
//! ## The problem
//!
//! Migrations only ever get appended, so a database carries a record of
//! the newest build that ever touched it. Point an older binary at it
//! and sqlx notices a row in `_sqlx_migrations` it has no migration
//! for, and fails:
//!
//! > `migration 20260802120000 was previously applied but is missing in
//! > the resolved migrations`
//!
//! Refusing is correct — replaying a newer schema through older code is
//! how databases get corrupted. The trouble was *where* it surfaced:
//! the error propagated out of the Tauri `setup` hook, which panics.
//! The splash window is created before `setup` runs, so the user saw it
//! paint and the whole process vanish with it — indistinguishable from
//! a crash on launch, with nothing to act on (issue #526).
//!
//! None of the ways to get here are exotic: trying a beta then going
//! back to stable, double-clicking an older AppImage still sitting in
//! `~/Downloads`, a distro package rolled back after a bad update, or
//! two versions sharing one `~/.local/share/app.waveflow`.
//!
//! ## What this module does
//!
//! Detect the case *before* the migrator runs, so the caller can turn
//! it into a sentence instead of a panic. This is deliberately distinct
//! from [`super::migration_heal`]: that one reconciles checksum drift on
//! a migration we *do* ship, where there is something to repair. Here
//! the migration genuinely isn't in this binary and there is nothing to
//! do but explain.
//!
//! Running before the heal pass also keeps us from writing canonical
//! checksums into a database this build has already decided it won't
//! touch.

use std::collections::HashSet;

use sqlx::{migrate::Migrator, SqlitePool};

use crate::error::{AppError, AppResult};

/// Which database a guard failure is about. Decides what the user can
/// actually do: another profile is a way out of a profile database
/// from the future, and no way out of `app.db`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbScope {
    /// The global `app.db` — profile list and app-wide settings.
    App,
    /// A per-profile `data.db`.
    Profile,
}

impl DbScope {
    /// Names the database. Capitalised because every use — the error's
    /// `Display`, the startup dialog — puts it first in a sentence.
    pub fn label(self) -> &'static str {
        match self {
            DbScope::App => "WaveFlow's application database",
            DbScope::Profile => "This profile's library",
        }
    }

    /// What the user can do about it. Switching profiles is a way out
    /// of a profile database from the future and no help at all against
    /// `app.db`, which every profile goes through.
    pub fn remedy(self) -> &'static str {
        match self {
            DbScope::App => {
                "Install WaveFlow again from the version you were using before."
            }
            DbScope::Profile => {
                "Install WaveFlow again from the version you were using before, \
                 or start WaveFlow with a different profile."
            }
        }
    }
}

/// Fail with [`AppError::SchemaFromTheFuture`] if `_sqlx_migrations`
/// holds a version this binary doesn't ship.
///
/// Reports the **highest** unknown version, which is the one that names
/// how far ahead the database is. Short-circuits on a fresh database,
/// where `_sqlx_migrations` doesn't exist yet — the migrator creates it.
pub async fn ensure_not_from_the_future(
    pool: &SqlitePool,
    migrator: &Migrator,
    scope: DbScope,
) -> AppResult<()> {
    let table_present: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table_present.is_none() {
        return Ok(());
    }

    // `installed_on` is declared TIMESTAMP and filled by SQLite's
    // CURRENT_TIMESTAMP, so it lands as `YYYY-MM-DD HH:MM:SS` text. The
    // CAST keeps that true whatever a future sqlx decides to store.
    let applied: Vec<(i64, Option<String>)> = sqlx::query_as(
        "SELECT version, CAST(installed_on AS TEXT) FROM _sqlx_migrations ORDER BY version DESC",
    )
    .fetch_all(pool)
    .await?;

    let known: HashSet<i64> = migrator.iter().map(|m| m.version).collect();
    let Some((version, installed_on)) = applied.into_iter().find(|(v, _)| !known.contains(v))
    else {
        return Ok(());
    };

    tracing::error!(
        version,
        ?installed_on,
        scope = ?scope,
        "database carries a migration this build does not ship — refusing to open it"
    );

    Err(AppError::SchemaFromTheFuture {
        scope,
        version,
        installed_on,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn fresh_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap()
    }

    /// Forge a row for a migration no build ships. Mirrors what sqlx
    /// itself writes, so the guard is exercised against the real table
    /// shape rather than a convenient stand-in.
    async fn record_applied(pool: &SqlitePool, version: i64) {
        sqlx::query(
            "INSERT INTO _sqlx_migrations
                 (version, description, installed_on, success, checksum, execution_time)
             VALUES (?, 'from the future', '2099-01-01 00:00:00', 1, X'00', 0)",
        )
        .bind(version)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn fresh_database_has_nothing_to_refuse() {
        let pool = fresh_pool().await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        // No `_sqlx_migrations` yet — the migrator hasn't run.
        ensure_not_from_the_future(&pool, &migrator, DbScope::Profile)
            .await
            .expect("a fresh database is not from the future");
    }

    #[tokio::test]
    async fn a_database_this_build_wrote_is_accepted() {
        let pool = fresh_pool().await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        migrator.run(&pool).await.unwrap();

        ensure_not_from_the_future(&pool, &migrator, DbScope::Profile)
            .await
            .expect("our own migrations must not read as newer");
    }

    #[tokio::test]
    async fn an_unknown_version_is_refused_and_named() {
        let pool = fresh_pool().await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        migrator.run(&pool).await.unwrap();
        record_applied(&pool, 29990101000000).await;

        let err = ensure_not_from_the_future(&pool, &migrator, DbScope::Profile)
            .await
            .expect_err("a migration we don't ship must be refused");

        match err {
            AppError::SchemaFromTheFuture {
                version,
                installed_on,
                ..
            } => {
                assert_eq!(version, 29990101000000);
                assert_eq!(installed_on.as_deref(), Some("2099-01-01 00:00:00"));
            }
            other => panic!("expected SchemaFromTheFuture, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_guard_fires_on_exactly_what_sqlx_refuses() {
        // The guard's job is to get in front of sqlx, not to invent a
        // rule of its own. If a future sqlx stops refusing this — or
        // refuses it differently — the guard has drifted into deciding
        // policy by itself, and this is where that shows up.
        let pool = fresh_pool().await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        migrator.run(&pool).await.unwrap();
        record_applied(&pool, 29990101000000).await;

        let verdict = migrator.run(&pool).await;
        assert!(
            matches!(
                verdict,
                Err(sqlx::migrate::MigrateError::VersionMissing(v)) if v == 29990101000000
            ),
            "expected sqlx to refuse the unknown version, got {verdict:?}"
        );
    }

    #[tokio::test]
    async fn the_highest_unknown_version_is_the_one_reported() {
        // Two builds ahead of us. Naming the newest is what tells the
        // user how far forward they have to go to open this again.
        let pool = fresh_pool().await;
        let migrator = sqlx::migrate!("../../migrations/profile");
        migrator.run(&pool).await.unwrap();
        record_applied(&pool, 29990101000000).await;
        record_applied(&pool, 29990202000000).await;

        let err = ensure_not_from_the_future(&pool, &migrator, DbScope::Profile)
            .await
            .expect_err("a migration we don't ship must be refused");

        match err {
            AppError::SchemaFromTheFuture { version, .. } => {
                assert_eq!(version, 29990202000000)
            }
            other => panic!("expected SchemaFromTheFuture, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_app_migrator_guards_the_app_database() {
        // The two databases have independent migration timelines, so
        // the guard has to be run with the matching migrator — pairing
        // app.db with the profile migrator would refuse every install.
        let pool = fresh_pool().await;
        let migrator = sqlx::migrate!("../../migrations/app");
        migrator.run(&pool).await.unwrap();

        ensure_not_from_the_future(&pool, &migrator, DbScope::App)
            .await
            .expect("app.db written by this build must be accepted");
    }
}
