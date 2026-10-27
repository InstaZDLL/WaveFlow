use super::*;

/// End-to-end check that the `VACUUM INTO ?` bound-parameter form
/// actually runs, creates the snapshot file, and yields a consistent
/// copy of the committed data — the crux of the pre-reset safety
/// backup's snapshot path.
#[tokio::test]
async fn vacuum_into_file_snapshots_committed_rows() {
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::{ConnectOptions, Connection};

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("data.db");
    let snap_path = dir.path().join("snap.dbtmp");

    {
        let mut conn = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .connect()
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
            .execute(&mut conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (v) VALUES ('hello')")
            .execute(&mut conn)
            .await
            .unwrap();
        conn.close().await.unwrap();
    }

    vacuum_into_file(&db_path, &snap_path).await.unwrap();
    assert!(snap_path.exists(), "snapshot file should be created");

    let mut snap = SqliteConnectOptions::new()
        .filename(&snap_path)
        .create_if_missing(false)
        .connect()
        .await
        .unwrap();
    let v: String = sqlx::query_scalar("SELECT v FROM t WHERE id = 1")
        .fetch_one(&mut snap)
        .await
        .unwrap();
    assert_eq!(v, "hello");
    snap.close().await.unwrap();

    // VACUUM INTO refuses to overwrite; the helper removes a stale
    // snapshot first, so a second run must also succeed.
    vacuum_into_file(&db_path, &snap_path).await.unwrap();
}

/// A missing profile database is a skip (Err), not a silent success —
/// the caller drops the profile rather than archiving an empty db.
#[tokio::test]
async fn vacuum_into_file_missing_db_errors() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.db");
    let snap = dir.path().join("snap.dbtmp");
    assert!(vacuum_into_file(&missing, &snap).await.is_err());
}
