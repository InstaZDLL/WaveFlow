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
