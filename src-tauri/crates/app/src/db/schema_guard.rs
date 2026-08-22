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
/// Creates nothing. Missing files are the fresh-install case and are
/// left for the real open to create.
pub async fn preflight(paths: &AppPaths) -> Option<AppError> {
    let app_pool = open_existing(&paths.app_db).await?;

    let app_migrator = sqlx::migrate!("../../migrations/app");
    let mut verdict = vet(&app_pool, &app_migrator, DbScope::App).await;

    if verdict.is_none() {
        verdict = match crate::state::resolve_target_profile(&app_pool).await {
            Ok(Some(profile_id)) => vet_profile(paths, profile_id).await,
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

/// Vet one profile's `data.db`.
async fn vet_profile(paths: &AppPaths, profile_id: i64) -> Option<AppError> {
    let pool = open_existing(&paths.profile_db(profile_id)).await?;
    let migrator = sqlx::migrate!("../../migrations/profile");
    let verdict = vet(&pool, &migrator, DbScope::Profile).await;
    pool.close().await;
    verdict
}

/// [`ensure_not_from_the_future`] reduced to the one answer the
/// pre-flight can act on. A database error here means we couldn't tell,
/// which is not the same as "it's fine" — but it is the same *decision*,
/// because the downstream guard will be asked again with the pool that
/// actually matters.
async fn vet(pool: &SqlitePool, migrator: &Migrator, scope: DbScope) -> Option<AppError> {
    match ensure_not_from_the_future(pool, migrator, scope).await {
        Ok(()) => None,
        Err(err @ AppError::SchemaFromTheFuture { .. }) => Some(err),
        Err(err) => {
            tracing::warn!(%err, ?scope, "startup pre-flight: inconclusive, deferring to startup");
            None
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
    // from the one pass whose whole job is to look without touching.
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

    // ---- pre-flight ------------------------------------------------
    //
    // Same forged row as above, but reached through the real path
    // layout and the real profile lookup, on files rather than
    // `:memory:` — the pre-flight's whole job is deciding *which*
    // database to ask about, and that is not something an in-memory
    // pool can get wrong.

    use crate::paths::AppPaths;
    use std::path::Path;

    /// Open (creating) a database file, the way startup would.
    async fn file_pool(path: &Path) -> SqlitePool {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap()
    }

    /// A minimal but real app-data tree: `app.db` migrated by the app
    /// migrator, one profile row per id, each with its own migrated
    /// `data.db` under `profiles/<id>/`.
    async fn seed_tree(root: &Path, profile_ids: &[i64]) -> AppPaths {
        let paths = AppPaths::from_root(root.to_path_buf(), None);
        paths.ensure_dirs().unwrap();

        let app_db = file_pool(&paths.app_db).await;
        sqlx::migrate!("../../migrations/app")
            .run(&app_db)
            .await
            .unwrap();

        for id in profile_ids {
            sqlx::query(
                "INSERT INTO profile (id, name, color_id, avatar_hash, data_dir, created_at, last_used_at)
                 VALUES (?, ?, 'emerald', NULL, ?, 0, ?)",
            )
            .bind(id)
            .bind(format!("Profile {id}"))
            .bind(AppPaths::profile_rel_dir(*id))
            .bind(id)
            .execute(&app_db)
            .await
            .unwrap();

            paths.ensure_profile_dirs(*id).unwrap();
            let profile_db = file_pool(&paths.profile_db(*id)).await;
            sqlx::migrate!("../../migrations/profile")
                .run(&profile_db)
                .await
                .unwrap();
            profile_db.close().await;
        }

        app_db.close().await;
        paths
    }

    /// Point `app.last_profile_id` at a profile, the way a profile
    /// switch does.
    async fn pin_last_profile(paths: &AppPaths, profile_id: i64) {
        let app_db = file_pool(&paths.app_db).await;
        sqlx::query(
            "INSERT INTO app_setting (key, value, value_type, updated_at)
             VALUES ('app.last_profile_id', ?, 'int', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(profile_id.to_string())
        .execute(&app_db)
        .await
        .unwrap();
        app_db.close().await;
    }

    /// Forge a migration from the future into a database file.
    async fn poison(path: &Path, version: i64) {
        let pool = file_pool(path).await;
        record_applied(&pool, version).await;
        pool.close().await;
    }

    #[tokio::test]
    async fn preflight_creates_nothing_on_a_fresh_install() {
        // The dangerous failure mode: a pre-flight that materializes
        // `app.db` turns "no install yet" into "install with an empty
        // database", and the first-run path never runs again.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("waveflow");
        let paths = AppPaths::from_root(root.clone(), None);

        assert!(preflight(&paths).await.is_none());
        assert!(!root.exists(), "pre-flight must not create the app-data tree");
    }

    #[tokio::test]
    async fn preflight_accepts_a_tree_this_build_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;

        assert!(
            preflight(&paths).await.is_none(),
            "our own migrations must not read as newer",
        );
    }

    #[tokio::test]
    async fn preflight_names_a_future_app_db() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;
        poison(&paths.app_db, 29990101000000).await;

        match preflight(&paths).await {
            Some(AppError::SchemaFromTheFuture { scope, version, .. }) => {
                assert_eq!(scope, DbScope::App);
                assert_eq!(version, 29990101000000);
            }
            other => panic!("expected an App-scoped refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_names_a_future_profile_db() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;
        poison(&paths.profile_db(1), 29990101000000).await;

        match preflight(&paths).await {
            Some(AppError::SchemaFromTheFuture { scope, version, .. }) => {
                assert_eq!(scope, DbScope::Profile);
                assert_eq!(version, 29990101000000);
            }
            other => panic!("expected a Profile-scoped refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_vets_the_profile_startup_will_actually_open() {
        // Two profiles, and the one from the future is not the one
        // being opened. Refusing here would ground a launch over a
        // database it never touches — and the symmetric case has to
        // still fire, or the lookup is just decoration.
        let tmp = tempfile::tempdir().unwrap();
        let paths = seed_tree(&tmp.path().join("waveflow"), &[1, 2]).await;
        pin_last_profile(&paths, 2).await;
        poison(&paths.profile_db(1), 29990101000000).await;

        assert!(
            preflight(&paths).await.is_none(),
            "a profile we are not opening must not ground the launch",
        );

        poison(&paths.profile_db(2), 29990202000000).await;
        match preflight(&paths).await {
            Some(AppError::SchemaFromTheFuture { version, .. }) => {
                assert_eq!(version, 29990202000000);
            }
            other => panic!("expected the pinned profile to be refused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_defers_when_it_cannot_read_the_database() {
        // Inconclusive is not a verdict. A file that isn't SQLite at
        // all stands in for the whole class (corruption, permissions,
        // a lock we lost the race for): startup proceeds and the real
        // guard gets the last word.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("waveflow");
        let paths = AppPaths::from_root(root, None);
        paths.ensure_dirs().unwrap();
        std::fs::write(&paths.app_db, b"not a database").unwrap();

        assert!(preflight(&paths).await.is_none());
    }
}
