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
//! yet) has nothing worth keeping. A snapshot that fails is logged and
//! the migration goes ahead — refusing to start over a full disk would
//! lock the user out of a library that is, so far, intact.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sqlx::migrate::Migrator;
use sqlx::SqlitePool;

/// How many snapshots to keep per database. One per release that ships a
/// migration, so this reaches back a few upgrades.
const KEEP: usize = 3;

/// Snapshot `path` if `migrator` has migrations `pool` has not applied.
/// Returns where the copy went, or `None` when none was needed or it
/// could not be taken.
pub async fn snapshot_if_pending(
    pool: &SqlitePool,
    migrator: &Migrator,
    path: &Path,
) -> Option<PathBuf> {
    let first_pending = match first_pending_version(pool, migrator).await {
        Ok(Some(version)) => version,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(db = %path.display(), error = %e, "pre-migration backup: cannot read migration state");
            return None;
        }
    };
    let target = snapshot_path(path, first_pending)?;
    // A copy already there was taken before this same migration on an
    // earlier launch that did not get through it. It is the good one:
    // nothing has written to the database since, the app never opened.
    if target.exists() {
        return Some(target);
    }
    // `VACUUM INTO` writes a consistent, compacted copy — WAL included —
    // through SQLite itself, rather than copying files that may be
    // mid-checkpoint.
    //
    // Written under a temporary name and renamed once complete: a copy
    // cut short by a crash or a full disk must never sit at `target`,
    // where the check above would take it for a good one next launch.
    // `VACUUM INTO` also refuses an existing file, so a leftover from
    // such a crash goes first.
    let partial = target.with_extension("bak.partial");
    let _ = std::fs::remove_file(&partial);
    tracing::info!(
        db = %path.display(),
        bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        "backing up the database before migrating"
    );
    let result = sqlx::query("VACUUM main INTO ?")
        .bind(partial.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .map_err(|e| e.to_string())
        .and_then(|_| std::fs::rename(&partial, &target).map_err(|e| e.to_string()));
    if let Err(e) = result {
        tracing::warn!(db = %path.display(), error = %e, "pre-migration backup failed, migrating without one");
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    tracing::info!(backup = %target.display(), "database backed up before migrating");
    prune(path, &target);
    Some(target)
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
        assert_eq!(snapshot_if_pending(&pool, &migrator(), &path).await, None);
    }

    #[tokio::test]
    async fn an_up_to_date_database_is_not_copied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let pool = file_pool(&path).await;
        migrator().run(&pool).await.unwrap();
        assert_eq!(snapshot_if_pending(&pool, &migrator(), &path).await, None);
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
    async fn a_copy_left_by_a_failed_launch_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let (pool, newest) = one_behind(&path).await;
        let earlier = dir.path().join(format!("data.db.pre-{newest}.bak"));
        std::fs::write(&earlier, b"the copy from the launch that failed").unwrap();

        assert_eq!(
            snapshot_if_pending(&pool, &migrator(), &path).await,
            Some(earlier.clone())
        );
        assert_eq!(
            std::fs::read(&earlier).unwrap(),
            b"the copy from the launch that failed"
        );
    }

    #[tokio::test]
    async fn a_copy_cut_short_is_redone_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        let (pool, newest) = one_behind(&path).await;
        let partial = dir.path().join(format!("data.db.pre-{newest}.bak.partial"));
        std::fs::write(&partial, b"cut short").unwrap();

        let copy = snapshot_if_pending(&pool, &migrator(), &path)
            .await
            .expect("a fresh copy");
        assert!(!partial.exists());
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
}
