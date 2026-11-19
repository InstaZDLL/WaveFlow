use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

fn options(path: &std::path::Path) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
}

/// A profile database's pool as far as locking goes: the handler on every
/// connection, `app.db` attached to each, and `profile_setting`, which
/// [`begin_write`] writes nothing to.
async fn pool(dir: &std::path::Path) -> SqlitePool {
    let app = SqlitePoolOptions::new()
        .connect_with(options(&dir.join("app.db")))
        .await
        .unwrap();
    sqlx::query("CREATE TABLE app_setting (key TEXT PRIMARY KEY, value TEXT)")
        .execute(&app)
        .await
        .unwrap();
    app.close().await;

    let app_path = dir.join("app.db").to_string_lossy().replace('\'', "''");
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .after_connect(move |conn, _meta| {
            let attach = format!("ATTACH DATABASE '{app_path}' AS app");
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(attach))
                    .execute(&mut *conn)
                    .await?;
                install(conn).await
            })
        })
        .connect_with(options(&dir.join("data.db")))
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE t (x INTEGER);
         CREATE TABLE profile_setting (key TEXT PRIMARY KEY, value TEXT);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// SQLite's own schedule: short sleeps first, then 100 ms at a time, and
/// the last one cut to land exactly on the deadline.
#[test]
fn the_sleeps_are_sqlites_and_end_at_the_deadline() {
    let first: Vec<_> = (0..13).map(|n| busy_delay(n, 5_000).unwrap()).collect();
    assert_eq!(first, [1, 2, 5, 10, 15, 20, 25, 25, 25, 50, 50, 100, 100]);

    let mut slept = 0;
    let mut count = 0;
    while let Some(delay) = busy_delay(count, 5_000) {
        slept += delay;
        count += 1;
    }
    assert_eq!(slept, 5_000, "the waits add up to the timeout, no more");

    assert_eq!(busy_delay(0, 0), None, "no timeout, no wait");
    assert_eq!(busy_delay(2, 4), Some(1), "cut to what is left");
}

/// A connection that finds the write lock taken says so, and wakes the
/// writer that holds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wait_for_the_write_lock_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;

    let mut holder = begin_write(&pool).await.unwrap();
    sqlx::query("INSERT INTO t VALUES (1)")
        .execute(&mut *holder)
        .await
        .unwrap();
    let held_since = Instant::now();

    let waiter = tokio::spawn({
        let pool = pool.clone();
        async move { sqlx::query("INSERT INTO t VALUES (2)").execute(&pool).await }
    });

    tokio::time::timeout(Duration::from_secs(2), someone_waits(held_since))
        .await
        .expect("the waiting connection is reported");
    assert!(waited_since(held_since));
    assert!(!waiter.is_finished(), "it is still waiting for the lock");

    holder.commit().await.unwrap();
    step_aside().await;
    waiter
        .await
        .unwrap()
        .expect("the write gets in once the lock is let go");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 2);
}

/// A writer that read before another one committed would fail its first
/// write at once, whatever the busy handler; [`begin_write`] holds the lock
/// from its start, so it waits its turn instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_transaction_waits_rather_than_failing_on_a_stale_read() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;

    // The deferred kind, to show what goes wrong: it reads, another
    // connection commits, and its write fails without waiting.
    let mut deferred = pool.begin().await.unwrap();
    let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
        .fetch_one(&mut *deferred)
        .await
        .unwrap();
    sqlx::query("INSERT INTO t VALUES (1)")
        .execute(&pool)
        .await
        .unwrap();
    let stale = sqlx::query("INSERT INTO t VALUES (2)")
        .execute(&mut *deferred)
        .await
        .expect_err("a write on a stale snapshot");
    assert_eq!(
        stale.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("517"),
        "SQLITE_BUSY_SNAPSHOT: {stale}"
    );
    deferred.rollback().await.unwrap();

    // `begin_write`, against the same interleaving.
    let mut immediate = begin_write(&pool).await.unwrap();
    let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
        .fetch_one(&mut *immediate)
        .await
        .unwrap();
    let other = tokio::spawn({
        let pool = pool.clone();
        async move { sqlx::query("INSERT INTO t VALUES (3)").execute(&pool).await }
    });
    sqlx::query("INSERT INTO t VALUES (4)")
        .execute(&mut *immediate)
        .await
        .expect("its write comes first: it held the lock since it began");
    immediate.commit().await.unwrap();
    other.await.unwrap().expect("and the other one follows");
}

/// `BEGIN IMMEDIATE` would reserve the attached `app.db` too, and the app
/// pool's writes (a setting saved) would wait on every scan batch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_transaction_leaves_the_attached_app_db_free() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    let app = SqlitePoolOptions::new()
        .connect_with(options(&dir.path().join("app.db")).busy_timeout(Duration::ZERO))
        .await
        .unwrap();

    let held = begin_write(&pool).await.unwrap();
    sqlx::query("INSERT INTO app_setting VALUES ('k', 'v')")
        .execute(&app)
        .await
        .expect("app.db is not locked by a profile write transaction");

    let profile = SqlitePoolOptions::new()
        .connect_with(options(&dir.path().join("data.db")).busy_timeout(Duration::ZERO))
        .await
        .unwrap();
    let locked = sqlx::query("INSERT INTO t VALUES (1)")
        .execute(&profile)
        .await;
    drop(held);
    assert!(locked.is_err(), "the profile database itself is locked");
}
