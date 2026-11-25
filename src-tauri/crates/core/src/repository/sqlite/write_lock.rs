//! A transaction that holds the database's write lock from its start.
//!
//! A deferred transaction (sqlx's `begin`) takes no lock until its first
//! write, and once it has read it cannot wait for one: if another
//! connection holds the lock at that write, the write fails at once with
//! `SQLITE_BUSY`, and if another connection committed since the read, with
//! `SQLITE_BUSY_SNAPSHOT` (517). No busy handler waits either out. A
//! transaction that checks something before it writes, which is most of
//! them, opens with [`begin_write_on`] instead: its first statement is a
//! write, which waits for the lock in the busy handler like any other, and
//! from there on nothing can commit under it.
//!
//! Not `BEGIN IMMEDIATE`: that reserves every database of the connection,
//! and a profile connection has `app.db` attached, whose own pool would
//! then wait on every such transaction. A write that changes nothing in one
//! table of `main` takes `main`'s lock alone.

use sqlx::{Sqlite, SqliteConnection, SqlitePool, Transaction};

/// Begin a transaction on `pool` that holds `main`'s write lock, taken by a
/// write to `table` (a rowid table of `main`) that changes no row.
pub async fn begin_write_on(
    pool: &SqlitePool,
    table: &'static str,
) -> sqlx::Result<Transaction<'static, Sqlite>> {
    let mut tx = pool.begin().await?;
    take_lock(&mut tx, table).await?;
    Ok(tx)
}

/// [`begin_write_on`] on a connection the caller already holds: for a
/// pass that reads on it first and must know, once the lock is held,
/// whether anyone committed since (`PRAGMA data_version` is only
/// comparable between two reads on the same connection).
pub async fn begin_write_on_connection<'c>(
    conn: &'c mut SqliteConnection,
    table: &'static str,
) -> sqlx::Result<Transaction<'c, Sqlite>> {
    let mut tx = sqlx::Connection::begin(conn).await?;
    take_lock(&mut tx, table).await?;
    Ok(tx)
}

async fn take_lock(conn: &mut SqliteConnection, table: &'static str) -> sqlx::Result<()> {
    let take_lock = format!("UPDATE main.{table} SET rowid = rowid WHERE 0");
    sqlx::query(sqlx::AssertSqlSafe(take_lock))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

    async fn pool(dir: &std::path::Path) -> SqlitePool {
        let options = SqliteConnectOptions::new()
            .filename(dir.join("data.db"))
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_millis(100));
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    /// The lock is held from the start: another writer cannot get in
    /// before the transaction ends, and gets in once it has.
    #[tokio::test]
    async fn the_lock_is_held_from_the_first_statement() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;

        let tx = begin_write_on(&pool, "t").await.unwrap();
        let locked = sqlx::query("INSERT INTO t VALUES (1)").execute(&pool).await;
        assert!(locked.is_err(), "another writer waits out its busy timeout");
        tx.commit().await.unwrap();
        sqlx::query("INSERT INTO t VALUES (1)")
            .execute(&pool)
            .await
            .expect("free once the transaction ended");
    }

    /// A read before the first real write no longer leaves the transaction
    /// on a snapshot another writer can make stale.
    #[tokio::test]
    async fn a_read_before_the_write_cannot_go_stale() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;

        let mut tx = begin_write_on(&pool, "t").await.unwrap();
        let _: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let other = sqlx::query("INSERT INTO t VALUES (1)").execute(&pool).await;
        assert!(other.is_err(), "nobody commits under it");
        sqlx::query("INSERT INTO t VALUES (2)")
            .execute(&mut *tx)
            .await
            .expect("its own write goes through");
        tx.commit().await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }
}
