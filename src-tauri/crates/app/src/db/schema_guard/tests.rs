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

/// Overwrite a real migration's stored checksum, the way a build
/// that shipped different SQL under the same id would have.
async fn tamper_checksum(pool: &SqlitePool, version: i64, checksum: &[u8]) {
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
        .bind(checksum)
        .bind(version)
        .execute(pool)
        .await
        .unwrap();
}

fn first_migration(migrator: &Migrator) -> (i64, String) {
    let m = migrator.iter().next().expect("we ship migrations");
    (m.version, m.sql.as_str().to_string())
}

#[tokio::test]
async fn a_migration_applied_in_another_form_is_refused() {
    let pool = fresh_pool().await;
    let migrator = sqlx::migrate!("../../migrations/profile");
    migrator.run(&pool).await.unwrap();

    let (version, _) = first_migration(&migrator);
    tamper_checksum(&pool, version, b"not the sql we ship").await;

    let err = ensure_no_foreign_migration(&pool, &migrator, DbScope::Profile)
        .await
        .expect_err("a migration whose SQL differs must be refused");
    match err {
        AppError::SchemaWrittenElsewhere { version: v, scope } => {
            assert_eq!(v, version);
            assert_eq!(scope, DbScope::Profile);
        }
        other => panic!("expected SchemaWrittenElsewhere, got {other:?}"),
    }
}

#[tokio::test]
async fn the_guard_fires_on_what_sqlx_would_have_panicked_on() {
    // Same contract as the version guard's own test: this exists to
    // get in front of sqlx, not to invent a rule. If sqlx stops
    // refusing a checksum change, the guard has drifted into
    // deciding policy by itself.
    let pool = fresh_pool().await;
    let migrator = sqlx::migrate!("../../migrations/profile");
    migrator.run(&pool).await.unwrap();

    let (version, _) = first_migration(&migrator);
    tamper_checksum(&pool, version, b"not the sql we ship").await;

    let verdict = migrator.run(&pool).await;
    assert!(
        matches!(
            verdict,
            Err(sqlx::migrate::MigrateError::VersionMismatch(v)) if v == version
        ),
        "expected sqlx to refuse the modified migration, got {verdict:?}"
    );
}

#[tokio::test]
async fn line_ending_drift_is_not_a_foreign_migration() {
    // The whole point of the exception: a Windows checkout that
    // applied the CRLF form of the *same* SQL is healthy, and
    // `migration_heal` fixes it moments later. Refusing here would
    // turn a newline into a dead install.
    let pool = fresh_pool().await;
    let migrator = sqlx::migrate!("../../migrations/profile");
    migrator.run(&pool).await.unwrap();

    let (version, sql) = first_migration(&migrator);
    let crlf = sql.replace('\n', "\r\n");
    let crlf_hash = {
        use sha2::{Digest, Sha384};
        let mut hasher = Sha384::new();
        hasher.update(crlf.as_bytes());
        hasher.finalize()
    };
    tamper_checksum(&pool, version, &crlf_hash).await;

    ensure_no_foreign_migration(&pool, &migrator, DbScope::Profile)
        .await
        .expect("line-ending drift must not be refused");
}

#[tokio::test]
async fn an_untouched_database_passes() {
    let pool = fresh_pool().await;
    let migrator = sqlx::migrate!("../../migrations/profile");
    migrator.run(&pool).await.unwrap();
    ensure_no_foreign_migration(&pool, &migrator, DbScope::Profile)
        .await
        .expect("the database we just migrated is ours");
}

#[tokio::test]
async fn a_version_we_do_not_ship_is_left_to_the_other_guard() {
    // Both guards read the same table, and a row from a newer build
    // has no compiled-in checksum to compare against. Naming it here
    // would give the user the vaguer of the two messages.
    let pool = fresh_pool().await;
    let migrator = sqlx::migrate!("../../migrations/profile");
    migrator.run(&pool).await.unwrap();
    record_applied(&pool, 29990101000000).await;

    ensure_no_foreign_migration(&pool, &migrator, DbScope::Profile)
        .await
        .expect("an unknown version is not this guard's verdict");
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
use std::path::{Path, PathBuf};

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

    assert!(preflight(&paths, &never_asked).await.is_none());
    assert!(
        !root.exists(),
        "pre-flight must not create the app-data tree"
    );
}

#[tokio::test]
async fn preflight_accepts_a_tree_this_build_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;

    assert!(
        preflight(&paths, &never_asked).await.is_none(),
        "our own migrations must not read as newer",
    );
}

#[tokio::test]
async fn preflight_names_a_future_app_db() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;
    poison(&paths.app_db, 29990101000000).await;

    match preflight(&paths, &never_asked).await {
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

    match preflight(&paths, &never_asked).await {
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
        preflight(&paths, &never_asked).await.is_none(),
        "a profile we are not opening must not ground the launch",
    );

    poison(&paths.profile_db(2), 29990202000000).await;
    match preflight(&paths, &never_asked).await {
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

    assert!(preflight(&paths, &never_asked).await.is_none());
}

/// The pre-flight's dialog, for trees where no copy can fail.
fn never_asked(
    _: DbScope,
    _: &pre_migration_backup::SnapshotFailure,
) -> pre_migration_backup::Choice {
    panic!("the pre-flight asked about a copy it should not have needed")
}

/// Put profile 1 of a seeded tree one migration behind this build,
/// and return where its pre-migration copy will go.
async fn profile_one_behind(paths: &AppPaths) -> PathBuf {
    let pool = file_pool(&paths.profile_db(1)).await;
    let newest = sqlx::migrate!("../../migrations/profile")
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ?")
        .bind(newest)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let name = format!("data.db.pre-{newest}.bak");
    paths.profile_db(1).with_file_name(name)
}

#[tokio::test]
async fn preflight_copies_the_profile_it_is_about_to_migrate() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;
    let copy = profile_one_behind(&paths).await;

    assert!(preflight(&paths, &never_asked).await.is_none());
    assert!(copy.is_file(), "a verified copy before the migration");
}

#[tokio::test]
async fn preflight_reports_a_copy_the_user_would_not_go_without() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;
    let copy = profile_one_behind(&paths).await;
    // Something in the way of the copy, as a full disk would be.
    std::fs::create_dir(&copy).unwrap();

    let quit = |scope: DbScope, failure: &pre_migration_backup::SnapshotFailure| {
        assert_eq!(scope, DbScope::Profile);
        assert_eq!(failure.db, paths.profile_db(1));
        pre_migration_backup::Choice::Quit
    };
    match preflight(&paths, &quit).await {
        Some(AppError::PreMigrationBackupDeclined { scope }) => {
            assert_eq!(scope, DbScope::Profile);
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn continuing_at_preflight_carries_to_the_open_that_follows() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed_tree(&tmp.path().join("waveflow"), &[1]).await;
    let copy = profile_one_behind(&paths).await;
    std::fs::create_dir(&copy).unwrap();

    let go_on = |_: DbScope, _: &pre_migration_backup::SnapshotFailure| {
        pre_migration_backup::Choice::Continue
    };
    assert!(preflight(&paths, &go_on).await.is_none());

    // What `profile_db::open` asks from inside `setup`, where the
    // installed dialog is none and a question would be a refusal:
    // the answer given before the event loop stands in for it.
    let pool = file_pool(&paths.profile_db(1)).await;
    pre_migration_backup::ensure_backed_up(
        &pool,
        &sqlx::migrate!("../../migrations/profile"),
        &paths.profile_db(1),
        DbScope::Profile,
        &pre_migration_backup::installed_prompt,
    )
    .await
    .unwrap();
}
