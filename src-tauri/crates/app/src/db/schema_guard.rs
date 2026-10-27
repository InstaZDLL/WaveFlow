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
use std::path::Path;

use sqlx::{
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};

use super::pre_migration_backup;
use crate::error::{AppError, AppResult};
use crate::paths::AppPaths;

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
            DbScope::App => "Install WaveFlow again from the version you were using before.",
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

/// Fail with [`AppError::SchemaWrittenElsewhere`] if a migration this
/// binary *does* ship was applied in a different form.
///
/// The sibling of [`ensure_not_from_the_future`], for the other half of
/// the same accident. Going back to an older build usually shows up as
/// an unknown version; it shows up as a checksum mismatch instead
/// whenever the two builds share a migration id whose SQL changed —
/// a migration amended before release, a beta whose file was edited
/// after the stable cut. sqlx refuses both, and refusing is right, but
/// only the first one had an explanation attached: the second still
/// reached the user as `MigrateError::VersionMismatch` panicking out of
/// the Tauri `setup` hook, which is the crash-on-launch shape issue
/// #526 set out to remove.
///
/// Line-ending drift is deliberately *not* a mismatch here:
/// [`super::migration_heal`] repairs that a moment later, and refusing
/// on it would turn a Windows checkout quirk into a dead install.
pub async fn ensure_no_foreign_migration(
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

    let stored: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;

    for (version, checksum) in stored {
        // An unknown version is the other guard's business; saying
        // nothing here keeps the two verdicts from racing to name the
        // same database.
        let Some(migration) = migrator.iter().find(|m| m.version == version) else {
            continue;
        };
        if checksum.as_slice() == migration.checksum.as_ref() {
            continue;
        }
        if super::migration_heal::is_line_ending_drift(migration.sql.as_str(), &checksum) {
            continue;
        }

        tracing::error!(
            version,
            scope = ?scope,
            "migration {version} was applied in a form this build does not ship — refusing to open it"
        );
        return Err(AppError::SchemaWrittenElsewhere { scope, version });
    }

    Ok(())
}

/// Vet the databases this launch is about to open, **before**
/// `tauri::Builder::run`.
///
/// [`ensure_not_from_the_future`] already runs inside `app_db::open` /
/// `profile_db::open`, which is where the authority belongs — but those
/// run from the Tauri `setup` hook, and `setup` runs from *inside* the
/// event loop (`RuntimeRunEvent::Ready`). A fatal path there never
/// returns to the loop, and on Linux that deadlocks the process instead
/// of explaining anything: `rfd`'s GTK backend hands every dialog to a
/// global thread that iterates the default GLib main context, and `tao`
/// already owns that context on the main thread we are blocking (issue
/// #529). Asking the question early — no windows, no event loop, no
/// main context held — is what lets the answer reach the user on all
/// three platforms.
///
/// **Best effort by construction.** Anything that isn't a clear verdict
/// (no database yet, a file we can't open, a query that fails) returns
/// `None` and lets startup proceed: the real guards downstream are
/// still there, and a pre-flight that refused on its own uncertainty
/// would be a new way to brick a healthy install.
///
/// Vets exactly the two databases startup will open — `app.db`, then
/// the profile [`resolve_target_profile`](crate::state::resolve_target_profile)
/// picks. Not every profile on disk: one profile left behind by a newer
/// build is no reason to refuse a launch that won't touch it.
///
/// Creates no database. Missing files are the fresh-install case and
/// are left for the real open to create.
///
/// A database it lets through that has migrations pending is copied
/// here too ([`pre_migration_backup::ensure_backed_up`]), because a copy
/// that fails needs the user's answer and this is where a dialog can
/// ask: `ask` is that dialog. A refusal comes back as
/// [`AppError::PreMigrationBackupDeclined`].
pub async fn preflight(
    paths: &AppPaths,
    ask: &pre_migration_backup::Prompt<'_>,
) -> Option<AppError> {
    let app_pool = open_existing(&paths.app_db).await?;

    let app_migrator = sqlx::migrate!("../../migrations/app");
    let mut verdict = match vet(&app_pool, &app_migrator, DbScope::App).await {
        Ok(()) => back_up(&app_pool, &app_migrator, &paths.app_db, DbScope::App, ask).await,
        Err(refusal) => refusal,
    };

    if verdict.is_none() {
        verdict = match crate::state::resolve_target_profile(&app_pool).await {
            Ok(Some(profile_id)) => vet_profile(paths, profile_id, ask).await,
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(%err, "startup pre-flight: could not resolve the target profile");
                None
            }
        };
    }

    app_pool.close().await;
    verdict
}

/// Vet one profile's `data.db`, then copy it if it is about to migrate.
async fn vet_profile(
    paths: &AppPaths,
    profile_id: i64,
    ask: &pre_migration_backup::Prompt<'_>,
) -> Option<AppError> {
    let path = paths.profile_db(profile_id);
    let pool = open_existing(&path).await?;
    let migrator = sqlx::migrate!("../../migrations/profile");
    let verdict = match vet(&pool, &migrator, DbScope::Profile).await {
        Ok(()) => back_up(&pool, &migrator, &path, DbScope::Profile, ask).await,
        Err(refusal) => refusal,
    };
    pool.close().await;
    verdict
}

/// [`pre_migration_backup::settle_before_launch`]: the user's refusal is
/// the one answer to report, and the open downstream will not ask again
/// for a database settled here.
async fn back_up(
    pool: &SqlitePool,
    migrator: &Migrator,
    path: &Path,
    scope: DbScope,
    ask: &pre_migration_backup::Prompt<'_>,
) -> Option<AppError> {
    pre_migration_backup::settle_before_launch(pool, migrator, path, scope, ask)
        .await
        .err()
}

/// [`ensure_not_from_the_future`] reduced to the one answer the
/// pre-flight can act on. A database error here means we couldn't tell,
/// which is not the same as "it's fine" — but it is the same *decision*,
/// because the downstream guard will be asked again with the pool that
/// actually matters.
///
/// `Ok` when the database is clear to open, `Err(Some(_))` for a refusal
/// and `Err(None)` when it could not tell. Only a clear database is
/// copied here: one this pass could not read is left, copy and all, to
/// the open downstream.
async fn vet(
    pool: &SqlitePool,
    migrator: &Migrator,
    scope: DbScope,
) -> Result<(), Option<AppError>> {
    // Ordered the way the accident reads: "newer build" names how far
    // ahead the database is and is the more actionable of the two, so
    // it gets to answer first when a database is both ahead *and*
    // divergent.
    let verdict = match ensure_not_from_the_future(pool, migrator, scope).await {
        Ok(()) => ensure_no_foreign_migration(pool, migrator, scope).await,
        ahead => ahead,
    };
    match verdict {
        Ok(()) => Ok(()),
        Err(
            err @ (AppError::SchemaFromTheFuture { .. } | AppError::SchemaWrittenElsewhere { .. }),
        ) => Err(Some(err)),
        Err(err) => {
            tracing::warn!(%err, ?scope, "startup pre-flight: inconclusive, deferring to startup");
            Err(None)
        }
    }
}

/// Open an existing SQLite file, or give up quietly.
///
/// `create_if_missing(false)` is the point: a pre-flight that
/// materialized `app.db` would turn "no install yet" into "install
/// with an empty database", and the fresh-install path downstream
/// would never run.
async fn open_existing(path: &Path) -> Option<SqlitePool> {
    if !path.exists() {
        return None;
    }

    // Same options as the real open minus `create_if_missing`, so the
    // pre-flight sees what startup will see — and minus `journal_mode`,
    // which is not the no-op it looks like. sqlx issues no `PRAGMA
    // journal_mode` of its own, and its source says why: the mode is
    // permanent for a created database, and moving into or out of it
    // takes an exclusive lock `sqlite3_busy_timeout()` cannot wait on.
    // Asking for WAL here was a real write, and a non-negotiable lock,
    // from a pass whose job is to look before anything changes the
    // database — the pre-migration copy it may take reads it, and
    // writes only the file beside it.
    // Startup's own open still sets it.
    //
    // Not `read_only` either, for the opposite reason: a database left
    // with a hot WAL by a crash needs recovery on open, and a read-only
    // connection fails on exactly the installs most likely to be in
    // trouble — abstaining where a verdict was there to be had.
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true);

    match SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
    {
        Ok(pool) => Some(pool),
        Err(err) => {
            tracing::warn!(
                %err,
                path = %path.display(),
                "startup pre-flight: could not open the database, deferring to startup",
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
