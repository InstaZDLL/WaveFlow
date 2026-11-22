//! A command's transaction on its leased pool holds the write lock from its
//! start (`ProfilePool::begin`). Nearly every command reads before it
//! writes (a like reads the like); a deferred transaction that has read
//! fails at its first write while another connection holds the lock, and a
//! like or an addition to a playlist failed that way during every scan.

use super::*;
use crate::db::lock_waiters;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::time::Duration;

/// A profile database's pool as far as locking goes: the busy handler on
/// every connection, `profile_setting`, and a table to like tracks in.
async fn pool(dir: &std::path::Path) -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _meta| Box::pin(lock_waiters::install(conn)))
        .connect_with(
            SqliteConnectOptions::new()
                .filename(dir.join("data.db"))
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE profile_setting (key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE liked_track (track_id INTEGER PRIMARY KEY);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

/// The pool's own deferred kind, to show what went wrong: it reads, a
/// writer holds the lock, and its write fails without waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deferred_transaction_that_has_read_fails_under_a_held_lock() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;

    let mut deferred = pool.begin().await.unwrap();
    let _: Option<i64> = sqlx::query_scalar("SELECT 1 FROM liked_track WHERE track_id = 1")
        .fetch_optional(&mut *deferred)
        .await
        .unwrap();
    let holder = lock_waiters::begin_write(&pool).await.unwrap();
    let refused = sqlx::query("INSERT INTO liked_track (track_id) VALUES (1)")
        .execute(&mut *deferred)
        .await
        .expect_err("a read transaction cannot wait for the lock");
    assert_eq!(
        refused
            .as_database_error()
            .and_then(|e| e.code())
            .as_deref(),
        Some("5"),
        "SQLITE_BUSY: {refused}"
    );
    deferred.rollback().await.unwrap();
    holder.rollback().await.unwrap();
}

/// The same like through the leased pool: it waits for the writer holding
/// the lock, then lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_transaction_waits_for_the_lock_then_writes() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    let active = ActiveProfile::new(1, pool.clone());
    let leased = active.lease();

    let holder = lock_waiters::begin_write(&pool).await.unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        holder.commit().await.unwrap();
    });

    let mut tx = leased.begin().await.expect("waits for the holder");
    let was_liked: Option<i64> = sqlx::query_scalar("SELECT 1 FROM liked_track WHERE track_id = 1")
        .fetch_optional(&mut *tx)
        .await
        .unwrap();
    assert_eq!(was_liked, None);
    sqlx::query("INSERT INTO liked_track (track_id) VALUES (1)")
        .execute(&mut *tx)
        .await
        .expect("no writer can get in between its read and its write");
    tx.commit().await.unwrap();
    release.await.unwrap();

    let liked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM liked_track")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(liked, 1);
}
