//! A copy of a database taken just before its migrations run.
//!
//! A migration rewrites the user's library in place. When one goes wrong
//! on a real database — a shape no test had, a disk that fills halfway —
//! the only copy of the library is the one it just damaged, and the
//! manual backups under Settings are only as recent as the user made
//! them. So before applying anything, `open` snapshots the database next
//! to itself:
//!
//! ```text
//! profiles/3/data.db
//! profiles/3/data.db.pre-20260912210000.bak   <- before that migration
//! ```
//!
//! The suffix is the first migration the copy predates. Versions are
//! fixed-width timestamps, so the names sort in the order they were taken
//! and the oldest beyond [`KEEP`] are pruned.
//!
//! Only when there is something to apply: a database already up to date
//! costs one query at launch, and a brand-new one (no `_sqlx_migrations`
//! yet) has nothing worth keeping.
//!
//! A copy only counts once it has been read back: it is opened and put
//! through `PRAGMA integrity_check` before it replaces anything, so a
//! file `VACUUM INTO` finished but the disk did not keep is a failure,
//! not a backup.
//!
//! **No copy, no migration — unless the user says so.** A snapshot that
//! fails or does not verify stops the open in [`ensure_backed_up`], which
//! asks the user to try again, go on without a copy, or stop. At launch
//! the question comes from the pre-flight, before the event loop
//! ([`crate::db::schema_guard::preflight`]), where a native dialog can
//! still be shown; its answer is remembered so the open `setup` makes a
//! moment later does not ask, or copy, a second time. Once the app is up
//! (a profile opened for the first time since the upgrade, an imported
//! archive) the dialog installed by [`install_prompt`] asks instead. In
//! between, inside `setup`, nothing can be shown, and the open is refused.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, SqlitePool};

use super::schema_guard::DbScope;
use crate::error::{AppError, AppResult};

/// How many snapshots to keep per database. One per release that ships a
/// migration, so this reaches back a few upgrades.
const KEEP: usize = 3;

/// Why there is no copy to go back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotFailure {
    /// The database that was to be copied.
    pub db: PathBuf,
    /// What went wrong, in words fit for the dialog and the log.
    pub reason: String,
}

/// What the user chose when the copy could not be taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Take the copy again — after freeing some space, say.
    Retry,
    /// Migrate without a copy.
    Continue,
    /// Leave the database as it is: quit at launch, give up the open
    /// later on.
    Quit,
}

/// Asks the user what to do about a [`SnapshotFailure`]. Blocks until
/// they answer.
pub type Prompt<'a> = dyn Fn(DbScope, &SnapshotFailure) -> Choice + Send + Sync + 'a;

/// Make sure `path` has a verified copy before `migrator` touches it, or
/// that the user agreed to go on without one.
///
/// Nothing to do when nothing is pending, or when the pre-flight already
/// settled this database at this migration. Otherwise a snapshot is
/// taken; when it fails, `ask` decides: [`Choice::Retry`] tries again,
/// [`Choice::Continue`] returns `Ok` without a copy, [`Choice::Quit`]
/// fails with [`AppError::PreMigrationBackupDeclined`] and nothing is
/// migrated. Every choice is logged.
pub async fn ensure_backed_up(
    pool: &SqlitePool,
    migrator: &Migrator,
    path: &Path,
    scope: DbScope,
    ask: &Prompt<'_>,
) -> AppResult<()> {
    let pending = first_pending_version(pool, migrator).await;
    if let Ok(Some(version)) = pending {
        if take_settled(path, version) {
            return Ok(());
        }
    }
    loop {
        let failure = match snapshot_if_pending(pool, migrator, path).await {
            Ok(_) => return Ok(()),
            Err(failure) => failure,
        };
        match ask(scope, &failure) {
            Choice::Retry => {
                tracing::info!(db = %path.display(), "pre-migration backup: the user asked to try again");
            }
            Choice::Continue => {
                tracing::warn!(
                    db = %path.display(),
                    reason = %failure.reason,
                    "pre-migration backup: the user chose to migrate without a backup"
                );
                return Ok(());
            }
            Choice::Quit => {
                tracing::warn!(
                    db = %path.display(),
                    reason = %failure.reason,
                    "pre-migration backup: the user chose not to migrate; the database is left as it was"
                );
                return Err(AppError::PreMigrationBackupDeclined { scope });
            }
        }
    }
}

/// [`ensure_backed_up`] for the pre-flight, which then remembers the
/// database as settled at its first pending migration, so the open that
/// `setup` makes right after neither copies it again nor asks. Only here:
/// a settled database is one the next open migrates without a copy of
/// its own, and only the pre-flight knows that open is coming.
pub async fn settle_before_launch(
    pool: &SqlitePool,
    migrator: &Migrator,
    path: &Path,
    scope: DbScope,
    ask: &Prompt<'_>,
) -> AppResult<()> {
    ensure_backed_up(pool, migrator, path, scope, ask).await?;
    if let Ok(Some(version)) = first_pending_version(pool, migrator).await {
        let mut settled = settled().lock().unwrap_or_else(|e| e.into_inner());
        settled.retain(|(p, _)| p != path);
        settled.push((path.to_path_buf(), version));
    }
    Ok(())
}

/// Databases whose copy question has been answered this run, with the
/// first pending migration it was answered for. Written by the
/// pre-flight, read once by the open that follows it.
fn settled() -> &'static Mutex<Vec<(PathBuf, i64)>> {
    static SETTLED: OnceLock<Mutex<Vec<(PathBuf, i64)>>> = OnceLock::new();
    SETTLED.get_or_init(Default::default)
}

/// Whether `path` was settled at `version`, forgetting it either way: an
/// answer covers one open, the one it was given for.
fn take_settled(path: &Path, version: i64) -> bool {
    let mut settled = settled().lock().unwrap_or_else(|e| e.into_inner());
    let Some(index) = settled.iter().position(|(p, _)| p == path) else {
        return false;
    };
    settled.swap_remove(index).1 == version
}

/// The question the opens ask once the app is running. Unset until
/// [`install_prompt`]: inside `setup` there is no way to show a dialog,
/// so an open there that needs one is refused.
static PROMPT: OnceLock<Box<Prompt<'static>>> = OnceLock::new();

/// Install the dialog [`installed_prompt`] shows from now on. Once.
pub fn install_prompt(prompt: Box<Prompt<'static>>) {
    let _ = PROMPT.set(prompt);
}

/// The prompt `app_db::open` and `profile_db::open` pass to
/// [`ensure_backed_up`]: the installed dialog, or a refusal when there is
/// none yet.
pub fn installed_prompt(scope: DbScope, failure: &SnapshotFailure) -> Choice {
    match PROMPT.get() {
        Some(prompt) => prompt(scope, failure),
        None => {
            tracing::error!(
                db = %failure.db.display(),
                reason = %failure.reason,
                "pre-migration backup failed where no dialog can be shown; not migrating"
            );
            Choice::Quit
        }
    }
}

/// Snapshot `path` if `migrator` has migrations `pool` has not applied.
/// Returns where the verified copy went, or `None` when none was needed.
pub async fn snapshot_if_pending(
    pool: &SqlitePool,
    migrator: &Migrator,
    path: &Path,
) -> Result<Option<PathBuf>, SnapshotFailure> {
    let fail = |reason: String| SnapshotFailure {
        db: path.to_path_buf(),
        reason,
    };
    let first_pending = match first_pending_version(pool, migrator).await {
        Ok(Some(version)) => version,
        Ok(None) => return Ok(None),
        Err(e) => {
            tracing::warn!(db = %path.display(), error = %e, "pre-migration backup: cannot read migration state");
            return Err(fail(format!("cannot read which updates are pending: {e}")));
        }
    };
    let target = snapshot_path(path, first_pending)
        .ok_or_else(|| fail("the database path has no file name".to_string()))?;
    // A copy already at `target` is from an earlier launch that stopped
    // at the same migration. It is retaken rather than trusted: the user
    // may have gone back to the previous build and kept listening since,
    // and the schema being the same says nothing about the data.
    //
    // `VACUUM INTO` writes a consistent, compacted copy — WAL included —
    // through SQLite itself, rather than copying files that may be
    // mid-checkpoint. It goes to a temporary name, is read back, and
    // replaces `target` by rename only once it verifies, so a copy cut
    // short by a crash or a full disk never stands in for a good one,
    // and a copy or rename that fails leaves the previous copy where it
    // was. `std::fs::rename` replaces an existing file on Windows as on
    // Unix.
    //
    // The temporary name carries the process id. This runs before the
    // single-instance lock, so a second launch while the first is still
    // copying is possible, and with one shared name the two would delete
    // and verify each other's half-written file. `VACUUM INTO` refuses an
    // existing file, so this process's own leftover goes first, with the
    // ones a crash left a day ago or more.
    let partial = partial_path(&target);
    sweep_partials(&target, &partial);
    tracing::info!(
        db = %path.display(),
        bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        "backing up the database before migrating"
    );
    let copied = sqlx::query("VACUUM main INTO ?")
        .bind(partial.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| format!("the copy could not be written: {e}"));
    let result = match copied {
        Ok(()) => verify_copy(&partial).await,
        Err(e) => Err(e),
    }
    .and_then(|()| {
        std::fs::rename(&partial, &target)
            .map_err(|e| format!("the copy could not be put in place: {e}"))
    });
    if let Err(reason) = result {
        tracing::warn!(db = %path.display(), error = %reason, "pre-migration backup failed");
        let _ = std::fs::remove_file(&partial);
        return Err(fail(reason));
    }
    tracing::info!(backup = %target.display(), "database backed up and verified before migrating");
    prune(path, &target);
    Ok(Some(target))
}

/// `<target>.<pid>.partial`: where this process writes the copy before
/// it verifies.
fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.partial", std::process::id()));
    target.with_file_name(name)
}

/// Remove `own`, and the partials of `target` (with SQLite's
/// `-journal` beside them) that are a day old: no copy takes that long,
/// so nothing is still writing them. A copy of another launch running
/// right now is younger and stays.
fn sweep_partials(target: &Path, own: &Path) {
    let _ = std::fs::remove_file(own);
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return;
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let partial = file_name.ends_with(".partial") || file_name.ends_with(".partial-journal");
        if !(file_name.starts_with(&prefix) && partial) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= waveflow_core::atomic_write::STALE_PARTIAL_AGE);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Read `copy` back and run `PRAGMA integrity_check` over it.
///
/// Opened `immutable`: nothing else knows the file, so there is nothing
/// to lock against, and SQLite creates no `-wal` / `-shm` beside it that
/// the rename would leave behind.
async fn verify_copy(copy: &Path) -> Result<(), String> {
    let mut conn = SqliteConnectOptions::new()
        .filename(copy)
        .create_if_missing(false)
        .immutable(true)
        .disable_statement_logging()
        .connect()
        .await
        .map_err(|e| format!("the copy could not be opened: {e}"))?;
    let report: Result<Vec<String>, _> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut conn)
        .await;
    let _ = conn.close().await;
    match report {
        Ok(lines) if lines == ["ok"] => Ok(()),
        Ok(lines) => Err(format!(
            "the copy failed its integrity check: {}",
            lines.first().map(String::as_str).unwrap_or("no report")
        )),
        Err(e) => Err(format!("the copy could not be checked: {e}")),
    }
}

/// The oldest migration `migrator` ships that `pool` has not applied, or
/// `None` when it is up to date or has never been migrated at all.
async fn first_pending_version(
    pool: &SqlitePool,
    migrator: &Migrator,
) -> Result<Option<i64>, sqlx::Error> {
    let has_table: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if has_table.is_none() {
        return Ok(None);
    }
    let applied: HashSet<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    Ok(migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .filter(|v| !applied.contains(v))
        .min())
}

/// `<file name>.pre-<version>.bak`, next to the database.
fn snapshot_path(path: &Path, version: i64) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!("{name}.pre-{version}.bak")))
}

/// Delete all but the newest [`KEEP`] snapshots of `path`. `fresh`, the
/// copy just taken, always stays, whatever its name sorts as.
fn prune(path: &Path, fresh: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!("{}.pre-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut snapshots: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy())
                .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".bak"))
                && p != fresh
        })
        .collect();
    // Same prefix, fixed-width version: name order is age order.
    snapshots.sort();
    let excess = snapshots.len().saturating_sub(KEEP - 1);
    for old in &snapshots[..excess] {
        if let Err(e) = std::fs::remove_file(old) {
            tracing::warn!(backup = %old.display(), error = %e, "could not prune an old pre-migration backup");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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

    fn migrator() -> Migrator {
        sqlx::migrate!("../../migrations/profile")
    }

    /// A profile database one migration behind this build: fully
    /// migrated, then the newest migration's record removed so the
    /// migrator sees it as pending.
    async fn one_behind(path: &Path) -> (SqlitePool, i64) {
        let pool = file_pool(path).await;
        let migrator = migrator();
        migrator.run(&pool).await.unwrap();
        let newest = migrator.iter().map(|m| m.version).max().unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = ?")
            .bind(newest)
            .execute(&pool)
            .await
            .unwrap();
        (pool, newest)
    }

    #[tokio::test]
    async fn a_brand_new_database_is_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let pool = file_pool(&path).await;
        assert_eq!(
            snapshot_if_pending(&pool, &migrator(), &path).await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn an_up_to_date_database_is_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let pool = file_pool(&path).await;
        migrator().run(&pool).await.unwrap();
        assert_eq!(
            snapshot_if_pending(&pool, &migrator(), &path).await,
            Ok(None)
        );
    }

    #[tokio::test]
    async fn a_pending_migration_is_preceded_by_a_readable_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let (pool, newest) = one_behind(&path).await;
        sqlx::raw_sql("CREATE TABLE kept (v TEXT); INSERT INTO kept VALUES ('library')")
            .execute(&pool)
            .await
            .unwrap();

        let copy = snapshot_if_pending(&pool, &migrator(), &path)
            .await
            .unwrap()
            .expect("a copy before the pending migration");
        assert_eq!(
            copy.file_name().unwrap().to_string_lossy(),
            format!("data.db.pre-{newest}.bak")
        );
        let restored = file_pool(&copy).await;
        let kept: String = sqlx::query_scalar("SELECT v FROM kept")
            .fetch_one(&restored)
            .await
            .unwrap();
        assert_eq!(kept, "library");
    }

    #[tokio::test]
    async fn a_copy_left_by_an_earlier_launch_is_retaken() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let (pool, newest) = one_behind(&path).await;
        let earlier = dir.path().join(format!("data.db.pre-{newest}.bak"));
        std::fs::write(&earlier, b"a copy from before weeks of listening").unwrap();
        sqlx::raw_sql("CREATE TABLE kept (v TEXT); INSERT INTO kept VALUES ('since')")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            snapshot_if_pending(&pool, &migrator(), &path).await,
            Ok(Some(earlier.clone()))
        );
        let restored = file_pool(&earlier).await;
        let kept: String = sqlx::query_scalar("SELECT v FROM kept")
            .fetch_one(&restored)
            .await
            .unwrap();
        assert_eq!(kept, "since");
    }

    #[tokio::test]
    async fn a_replacement_that_fails_leaves_the_earlier_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let (pool, newest) = one_behind(&path).await;
        // A directory at the target: the copy is written, the rename over
        // it fails on every platform, and what was there must survive.
        let target = dir.path().join(format!("data.db.pre-{newest}.bak"));
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inside"), b"untouched").unwrap();

        assert!(snapshot_if_pending(&pool, &migrator(), &path)
            .await
            .is_err());
        assert_eq!(std::fs::read(target.join("inside")).unwrap(), b"untouched");
        assert!(!partial_path(&target).exists());
    }

    #[tokio::test]
    async fn a_copy_cut_short_is_redone_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let (pool, newest) = one_behind(&path).await;
        let target = dir.path().join(format!("data.db.pre-{newest}.bak"));
        // This process's own leftover, one a crash left long ago (the
        // name older builds used), and one another launch is writing.
        let own = partial_path(&target);
        std::fs::write(&own, b"cut short").unwrap();
        let old = dir.path().join(format!("data.db.pre-{newest}.bak.partial"));
        std::fs::File::create(&old)
            .unwrap()
            .set_modified(
                std::time::SystemTime::now()
                    - waveflow_core::atomic_write::STALE_PARTIAL_AGE
                    - std::time::Duration::from_secs(60),
            )
            .unwrap();
        let running = dir
            .path()
            .join(format!("data.db.pre-{newest}.bak.1.partial"));
        std::fs::write(&running, b"in progress").unwrap();

        let copy = snapshot_if_pending(&pool, &migrator(), &path)
            .await
            .unwrap()
            .expect("a fresh copy");
        assert!(!own.exists());
        assert!(!old.exists());
        assert!(running.exists());
        let restored = file_pool(&copy).await;
        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&restored)
            .await
            .unwrap();
        assert!(applied > 0);
    }

    #[tokio::test]
    async fn only_the_newest_copies_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        for version in [20200101000000_i64, 20210101000000, 20220101000000] {
            std::fs::write(dir.path().join(format!("data.db.pre-{version}.bak")), b"").unwrap();
        }
        // Another database's copies share the folder and are not ours.
        std::fs::write(dir.path().join("app.db.pre-20200101000000.bak"), b"").unwrap();
        let (pool, newest) = one_behind(&path).await;
        snapshot_if_pending(&pool, &migrator(), &path)
            .await
            .unwrap()
            .unwrap();

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".bak"))
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "app.db.pre-20200101000000.bak".to_string(),
                "data.db.pre-20210101000000.bak".to_string(),
                "data.db.pre-20220101000000.bak".to_string(),
                format!("data.db.pre-{newest}.bak"),
            ]
        );
    }

    /// A file that opens but whose pages do not hold together.
    async fn corrupted_database(path: &Path) {
        let pool = file_pool(path).await;
        sqlx::raw_sql(
            "CREATE TABLE t (v TEXT); \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000) \
             INSERT INTO t SELECT printf('row %08d', i) FROM n; \
             CREATE INDEX t_v ON t (v);",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        verify_copy(path).await.expect("intact before the damage");
        // Page 1 (the header and the schema) is left alone so the file
        // still opens; a page in the middle of the table is overwritten.
        let mut bytes = std::fs::read(path).unwrap();
        let page = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
        assert!(bytes.len() > page * 4);
        bytes[page * 3..page * 3 + 512].fill(0xA5);
        std::fs::write(path, bytes).unwrap();
    }

    #[tokio::test]
    async fn a_damaged_copy_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("data.db.pre-1.bak.partial");
        corrupted_database(&copy).await;
        let err = verify_copy(&copy).await.unwrap_err();
        assert!(
            err.contains("integrity") || err.contains("malformed"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_copy_that_is_not_a_database_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("data.db.pre-1.bak.partial");
        std::fs::write(&copy, vec![0x5A; 8192]).unwrap();
        assert!(verify_copy(&copy).await.is_err());
    }

    #[tokio::test]
    async fn a_wal_database_is_copied_and_verified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        let (pool, _) = one_behind(&path).await;
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE kept (v TEXT); INSERT INTO kept VALUES ('in the wal')")
            .execute(&pool)
            .await
            .unwrap();

        let copy = snapshot_if_pending(&pool, &migrator(), &path)
            .await
            .unwrap()
            .expect("a verified copy");
        // The read-back left nothing beside the copy.
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".bak"))
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![copy.file_name().unwrap().to_string_lossy().into_owned()]
        );
        let restored = file_pool(&copy).await;
        let kept: String = sqlx::query_scalar("SELECT v FROM kept")
            .fetch_one(&restored)
            .await
            .unwrap();
        assert_eq!(kept, "in the wal");
    }

    /// A database one migration behind whose copy cannot be put in
    /// place: a directory sits where it would go. Returns that path, so
    /// a test can clear the way.
    async fn blocked(dir: &Path) -> (SqlitePool, PathBuf, PathBuf) {
        let path = dir.join("data.db");
        let (pool, newest) = one_behind(&path).await;
        let target = dir.join(format!("data.db.pre-{newest}.bak"));
        std::fs::create_dir(&target).unwrap();
        (pool, path, target)
    }

    #[tokio::test]
    async fn nothing_pending_asks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let pool = file_pool(&path).await;
        migrator().run(&pool).await.unwrap();
        let ask = |_: DbScope, _: &SnapshotFailure| -> Choice { panic!("nothing to ask") };
        ensure_backed_up(&pool, &migrator(), &path, DbScope::Profile, &ask)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn retry_takes_the_copy_again() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, path, target) = blocked(dir.path()).await;
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let ask = |scope: DbScope, failure: &SnapshotFailure| {
            assert_eq!(scope, DbScope::Profile);
            assert_eq!(failure.db, path);
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // The user frees the way, then asks for another try.
            std::fs::remove_dir(&target).unwrap();
            Choice::Retry
        };
        ensure_backed_up(&pool, &migrator(), &path, DbScope::Profile, &ask)
            .await
            .unwrap();
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(target.is_file());
    }

    #[tokio::test]
    async fn continue_goes_on_without_a_copy_and_covers_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, path, target) = blocked(dir.path()).await;
        let asked = std::sync::atomic::AtomicUsize::new(0);
        let ask = |_: DbScope, _: &SnapshotFailure| {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Choice::Continue
        };
        settle_before_launch(&pool, &migrator(), &path, DbScope::App, &ask)
            .await
            .unwrap();
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(target.is_dir(), "no copy was put in place");

        // The open that follows the pre-flight is not asked again …
        let never = |_: DbScope, _: &SnapshotFailure| -> Choice { panic!("already answered") };
        ensure_backed_up(&pool, &migrator(), &path, DbScope::App, &never)
            .await
            .unwrap();
        // … but the answer covers that one open only, and an open that
        // is not the pre-flight's settles nothing.
        ensure_backed_up(&pool, &migrator(), &path, DbScope::App, &ask)
            .await
            .unwrap();
        ensure_backed_up(&pool, &migrator(), &path, DbScope::App, &ask)
            .await
            .unwrap();
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn quit_refuses_the_migration() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, path, _) = blocked(dir.path()).await;
        let ask = |_: DbScope, _: &SnapshotFailure| Choice::Quit;
        let err = ensure_backed_up(&pool, &migrator(), &path, DbScope::Profile, &ask)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::PreMigrationBackupDeclined {
                scope: DbScope::Profile
            }
        ));
        // Still one behind: nothing ran.
        let first = first_pending_version(&pool, &migrator()).await.unwrap();
        assert!(first.is_some());
    }

    #[test]
    fn with_no_dialog_installed_the_open_is_refused() {
        // `install_prompt` is never called in tests.
        let failure = SnapshotFailure {
            db: PathBuf::from("data.db"),
            reason: "disk full".into(),
        };
        assert_eq!(installed_prompt(DbScope::App, &failure), Choice::Quit);
    }
}
