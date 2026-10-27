use super::*;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

/// Real schema, not a hand-rolled fixture: the CHECK constraints are
/// half of what this module relies on, and a fixture without them
/// would prove nothing.
async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(":memory:")
        .await
        .unwrap();
    // The real migrator rather than a hand-listed subset: a
    // migration that touches a table this list happened to omit
    // breaks the fixture and not the code, which is a failure that
    // teaches nothing. Same fixture the browse tests use.
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

fn waveflow_binding(cursor: i64) -> RemoteBinding {
    RemoteBinding {
        server_url: "https://music.example".into(),
        identity: RemoteIdentity::Waveflow {
            account_id: "acc-1".into(),
            device_id: "dev-1".into(),
            cursor,
        },
        active_library_id: None,
        bootstrapped_at: None,
    }
}

#[tokio::test]
async fn unbound_profile_reads_as_none() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(read(&mut conn).await.unwrap(), None);
}

#[tokio::test]
async fn switching_accounts_leaves_no_inherited_metadata() {
    // Binding a second account after clearing must not let any of the
    // first account's projection or queued writes survive — a cursor
    // and queued mutations name *that* account's resources, and cached
    // titles/artwork keyed by remote_id would resolve to the wrong
    // library.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    // Account 1, with some of everything projected.
    write(&mut conn, &waveflow_binding(42)).await.unwrap();
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES ('p1', 'Acc1 mix')")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO remote_playlist_track VALUES ('p1', 0, 't1')")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO remote_favorite (entity_type, entity_id, starred_at) \
             VALUES ('track', 't1', 0)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    // `cached_at` is NOT NULL in the real schema this fixture runs, like
    // the `starred_at` / `created_at` the neighbouring inserts already pass.
    sqlx::query(
        "INSERT INTO remote_track (remote_id, title, cached_at) \
             VALUES ('t1', 'Acc1 song', 0)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO remote_mutation (operation_id, kind, payload, created_at) \
             VALUES ('op-1', 'set_favorite', '{}', 0)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    // Switch: clear, then bind account 2.
    clear_account_state(&mut conn).await.unwrap();
    let mut account_two = waveflow_binding(0);
    if let RemoteIdentity::Waveflow {
        account_id,
        device_id,
        ..
    } = &mut account_two.identity
    {
        *account_id = "acc-2".into();
        *device_id = "dev-2".into();
    }
    write(&mut conn, &account_two).await.unwrap();

    // Nothing from account 1 survives, in any table. (Literal SQL per
    // table — sqlx 0.9 rejects a `format!`d query string.)
    async fn count(conn: &mut SqliteConnection, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(&mut *conn).await.unwrap()
    }
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_playlist").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_playlist_track").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_favorite").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_track").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_mutation").await,
        0
    );

    // And the binding is now account 2.
    let bound = read(&mut conn).await.unwrap().unwrap();
    assert_eq!(bound.identity, account_two.identity);
}

#[tokio::test]
async fn waveflow_binding_round_trips() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let binding = waveflow_binding(7);
    write(&mut conn, &binding).await.unwrap();
    assert_eq!(read(&mut conn).await.unwrap(), Some(binding));
}

#[tokio::test]
async fn subsonic_binding_round_trips_without_a_cursor() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let binding = RemoteBinding {
        server_url: "https://other.example".into(),
        identity: RemoteIdentity::Subsonic {
            username: "bob".into(),
        },
        active_library_id: Some("lib-3".into()),
        bootstrapped_at: None,
    };
    write(&mut conn, &binding).await.unwrap();
    let read_back = read(&mut conn).await.unwrap().unwrap();
    assert_eq!(read_back, binding);
    assert_eq!(read_back.identity.cursor(), None);
    assert_eq!(read_back.identity.device_id(), None);
}

#[tokio::test]
async fn trailing_slash_is_stripped_so_paths_never_double_up() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let mut binding = waveflow_binding(0);
    binding.server_url = "https://music.example/".into();
    write(&mut conn, &binding).await.unwrap();
    assert_eq!(
        read(&mut conn).await.unwrap().unwrap().server_url,
        "https://music.example"
    );
}

#[tokio::test]
async fn a_late_lower_cursor_does_not_drag_the_position_backwards() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    write(&mut conn, &waveflow_binding(0)).await.unwrap();

    advance_cursor(&mut conn, 42).await.unwrap();
    // A catch-up pass that started earlier finishing later.
    advance_cursor(&mut conn, 11).await.unwrap();

    assert_eq!(
        read(&mut conn).await.unwrap().unwrap().identity.cursor(),
        Some(42)
    );
}

#[tokio::test]
async fn a_snapshot_pins_its_cursor_even_when_lower() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    write(&mut conn, &waveflow_binding(0)).await.unwrap();
    advance_cursor(&mut conn, 42).await.unwrap();

    // The recovery path: the projection was discarded, so the
    // position it had reached is meaningless.
    mark_bootstrapped(&mut conn, 11).await.unwrap();

    let binding = read(&mut conn).await.unwrap().unwrap();
    assert_eq!(binding.identity.cursor(), Some(11));
    assert!(binding.bootstrapped_at.is_some());
}

#[tokio::test]
async fn rebinding_replaces_rather_than_merges() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    write(&mut conn, &waveflow_binding(0)).await.unwrap();
    advance_cursor(&mut conn, 99).await.unwrap();

    // Signing into a different account must not inherit a cursor
    // into a journal it never belonged to.
    write(&mut conn, &waveflow_binding(0)).await.unwrap();
    assert_eq!(
        read(&mut conn).await.unwrap().unwrap().identity.cursor(),
        Some(0)
    );
}

#[tokio::test]
async fn clearing_account_state_empties_the_projection_and_the_queue() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('p1', 'P');
             INSERT INTO remote_playlist_track VALUES ('p1', 0, 't1');
             INSERT INTO remote_favorite VALUES ('track', 't1', 1);
             INSERT INTO remote_rating VALUES ('track', 't1', 4, 1);
             INSERT INTO remote_history VALUES ('t1', 1000, 1);
             INSERT INTO remote_queue (id, position_ms, updated_at) VALUES (1, 0, 1);
             INSERT INTO remote_queue_track VALUES (0, 't1');
             INSERT INTO remote_share (remote_id) VALUES ('s1');
             INSERT INTO remote_share_track VALUES ('s1', 0, 't1');
             INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
                VALUES ('op1', 'k', '{}', 0);",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    clear_account_state(&mut conn).await.unwrap();

    for table in [
        "remote_playlist",
        "remote_playlist_track",
        "remote_favorite",
        "remote_rating",
        "remote_history",
        "remote_queue",
        "remote_queue_track",
        "remote_share",
        "remote_share_track",
        "remote_mutation",
    ] {
        // `AssertSqlSafe` because the table name comes from the
        // literal list above, never from input.
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(count, 0, "{table} still holds rows");
    }
}

#[tokio::test]
async fn clearing_account_state_does_not_depend_on_foreign_keys_being_on() {
    // `foreign_keys` is a per-connection pragma, so a cascade would
    // make this correctness depend on how the caller connected.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('p1', 'P');
             INSERT INTO remote_playlist_track VALUES ('p1', 0, 't1');",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    clear_account_state(&mut conn).await.unwrap();

    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_playlist_track")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(orphans, 0);
}

#[tokio::test]
async fn advancing_a_subsonic_binding_is_a_no_op() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    write(
        &mut conn,
        &RemoteBinding {
            server_url: "https://other.example".into(),
            identity: RemoteIdentity::Subsonic {
                username: "bob".into(),
            },
            active_library_id: None,
            bootstrapped_at: None,
        },
    )
    .await
    .unwrap();

    advance_cursor(&mut conn, 42).await.unwrap();
    mark_bootstrapped(&mut conn, 42).await.unwrap();

    let binding = read(&mut conn).await.unwrap().unwrap();
    assert_eq!(binding.identity.cursor(), None);
    assert_eq!(binding.bootstrapped_at, None);
}
