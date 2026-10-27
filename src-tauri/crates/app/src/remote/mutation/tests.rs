use super::*;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

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

fn favorite(starred: bool) -> Mutation {
    Mutation::SetFavorite {
        entity_type: "track".into(),
        entity_id: "t1".into(),
        starred,
    }
}

#[tokio::test]
async fn every_enqueue_draws_its_own_operation_id() {
    // Two identical gestures are two mutations. Reusing one
    // identifier for both would make the server treat the second as
    // a replay of the first and drop it.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let first = enqueue(&mut conn, &favorite(true)).await.unwrap();
    let second = enqueue(&mut conn, &favorite(true)).await.unwrap();
    assert_ne!(first, second);
}

#[tokio::test]
async fn pending_comes_back_in_the_order_it_was_queued() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &favorite(true)).await.unwrap();
    enqueue(&mut conn, &favorite(false)).await.unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(queued.len(), 2);
    assert_eq!(queued[0].mutation, favorite(true));
    assert_eq!(queued[1].mutation, favorite(false));
}

#[tokio::test]
async fn a_permanently_failed_entry_leaves_the_queue_without_blocking_it() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &favorite(true)).await.unwrap();
    enqueue(&mut conn, &favorite(false)).await.unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    mark_failed(&mut conn, queued[0].id, "422 conflict")
        .await
        .unwrap();

    let still_pending = pending(&mut conn, 10).await.unwrap();
    assert_eq!(still_pending.len(), 1);
    assert_eq!(still_pending[0].mutation, favorite(false));

    // Still readable for diagnostics, just not for the drain.
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_mutation WHERE failed_at IS NOT NULL")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(failed, 1);
}

#[tokio::test]
async fn a_transient_attempt_keeps_the_entry_and_counts_the_try() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    enqueue(&mut conn, &favorite(true)).await.unwrap();
    let queued = pending(&mut conn, 10).await.unwrap();

    record_attempt(&mut conn, queued[0].id, "503")
        .await
        .unwrap();
    record_attempt(&mut conn, queued[0].id, "503")
        .await
        .unwrap();

    let again = pending(&mut conn, 10).await.unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].attempt_count, 2);
}

#[tokio::test]
async fn a_payload_this_build_cannot_read_stops_the_pass_and_is_marked_failed() {
    // A row written by a newer build can never be sent by this one.
    // FIFO is load-bearing, so it must not be skipped over — the pass
    // stops at it, it is marked permanently failed, and the next pass
    // resumes past it. That way the later entry is still sent, just one
    // pass later, and never *before* something it might depend on.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query(
        "INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
             VALUES ('op-future', 'teleport', '{\"kind\":\"teleport\"}', 0)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    enqueue(&mut conn, &favorite(true)).await.unwrap();

    // First pass stops at the unreadable row — nothing behind it slips
    // past — and marks it failed.
    let first = pending(&mut conn, 10).await.unwrap();
    assert!(first.is_empty(), "the pass must stop at the unreadable row");
    let failed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_mutation WHERE failed_at IS NOT NULL")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(failed, 1);

    // Next pass resumes past it and returns the readable entry.
    let second = pending(&mut conn, 10).await.unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].mutation, favorite(true));
}

#[tokio::test]
async fn resolving_a_placeholder_the_projection_already_created_is_idempotent() {
    // A `/changes` echo can materialize the real playlist under its
    // server id before the drain resolves the placeholder. Resolving
    // must then not collide on the primary key or duplicate tracks —
    // it drops the placeholder side and keeps the projected row.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let local_id = new_placeholder();
    // Placeholder projection (what the user saw offline).
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline mix')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO remote_playlist_track VALUES (?, 0, 't1')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    // The real row, already projected from the server's echo.
    sqlx::query(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('server-uuid', 'Offline mix')",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("INSERT INTO remote_playlist_track VALUES ('server-uuid', 0, 't1')")
        .execute(&mut *conn)
        .await
        .unwrap();

    resolve_placeholder(&mut conn, &local_id, "server-uuid")
        .await
        .expect("resolving over an already-projected row must not error");

    // Exactly one playlist row (the real one), one track, no placeholder.
    let playlists: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_playlist")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(playlists, 1);
    let placeholder_gone: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_playlist WHERE remote_id = ?")
            .bind(&local_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(placeholder_gone, 0);
    let tracks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM remote_playlist_track WHERE playlist_remote_id = 'server-uuid'",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(tracks, 1, "the projected track must not be duplicated");
}

#[tokio::test]
async fn resolving_a_share_the_projection_already_created_still_captures_the_url() {
    // The share URL is learned only from the creation response. If the
    // real share was already projected (without a URL — the journal
    // never carries one), resolving must still write the URL in.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let local_id = new_placeholder();
    sqlx::query("INSERT INTO remote_share (remote_id, description) VALUES (?, 'My mix')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    // Real share already projected, URL still null.
    sqlx::query(
        "INSERT INTO remote_share (remote_id, description) VALUES ('share-uuid', 'My mix')",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    resolve_share_placeholder(&mut conn, &local_id, "share-uuid", Some("https://x/y"))
        .await
        .expect("resolving over an already-projected share must not error");

    let url: Option<String> =
        sqlx::query_scalar("SELECT url FROM remote_share WHERE remote_id = 'share-uuid'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(url.as_deref(), Some("https://x/y"));
    let placeholder_gone: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_share WHERE remote_id = ?")
            .bind(&local_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(placeholder_gone, 0);
}

#[tokio::test]
async fn resolving_a_placeholder_rewrites_the_projection_and_the_queue() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    // Pinned: this test only proves anything with the constraint on,
    // and it caught a real ordering bug precisely because it was.
    let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(fk, 1, "foreign keys are off; this test proves nothing");

    let local_id = new_placeholder();
    assert!(is_placeholder(&local_id));

    // The optimistic projection a user sees immediately.
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline mix')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO remote_playlist_track VALUES (?, 0, 't1')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();

    enqueue(
        &mut conn,
        &Mutation::CreatePlaylist {
            local_id: local_id.clone(),
            name: "Offline mix".into(),
            track_ids: vec!["t1".into()],
        },
    )
    .await
    .unwrap();
    // An edit made before the creation ever reached the server.
    enqueue(
        &mut conn,
        &Mutation::UpdatePlaylist {
            playlist_id: local_id.clone(),
            name: Some("Renamed offline".into()),
            comment: None,
            public: None,
            add: vec![],
            remove_indexes: vec![],
            clear_comment: false,
        },
    )
    .await
    .unwrap();

    resolve_placeholder(&mut conn, &local_id, "server-uuid")
        .await
        .unwrap();

    let name: String =
        sqlx::query_scalar("SELECT name FROM remote_playlist WHERE remote_id = 'server-uuid'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(name, "Offline mix");

    let tracks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM remote_playlist_track WHERE playlist_remote_id = 'server-uuid'",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(tracks, 1, "the ordered tracks did not follow the rename");

    let queued = pending(&mut conn, 10).await.unwrap();
    match &queued[1].mutation {
        Mutation::UpdatePlaylist { playlist_id, .. } => {
            assert_eq!(
                playlist_id, "server-uuid",
                "the queued edit still names a playlist the server has never heard of"
            );
        }
        other => panic!("unexpected mutation: {other:?}"),
    }
}

#[tokio::test]
async fn resolving_a_share_captures_the_url_the_journal_never_carries() {
    // The token is keyed on a server-side secret, so the creation
    // response is the only moment this device can learn the link.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let local_id = new_placeholder();

    sqlx::query("INSERT INTO remote_share (remote_id) VALUES (?)")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO remote_share_track VALUES (?, 0, 't1')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();
    enqueue(
        &mut conn,
        &Mutation::DeleteShare {
            share_id: local_id.clone(),
        },
    )
    .await
    .unwrap();

    resolve_share_placeholder(
        &mut conn,
        &local_id,
        "share-uuid",
        Some("https://music.example/share/abc"),
    )
    .await
    .unwrap();

    let url: Option<String> =
        sqlx::query_scalar("SELECT url FROM remote_share WHERE remote_id = 'share-uuid'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(url.as_deref(), Some("https://music.example/share/abc"));

    let tracks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM remote_share_track WHERE share_remote_id = 'share-uuid'",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(tracks, 1);

    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[0].mutation,
        Mutation::DeleteShare {
            share_id: "share-uuid".into()
        }
    );
}

#[tokio::test]
async fn resolving_leaves_unrelated_entries_alone() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let mine = new_placeholder();
    let other = new_placeholder();

    enqueue(
        &mut conn,
        &Mutation::DeletePlaylist {
            playlist_id: other.clone(),
        },
    )
    .await
    .unwrap();

    resolve_placeholder(&mut conn, &mine, "server-uuid")
        .await
        .unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[0].mutation,
        Mutation::DeletePlaylist { playlist_id: other }
    );
}

#[tokio::test]
async fn setting_a_field_and_clearing_it_are_two_operations() {
    // The clear is part of the server's operation fingerprint, so
    // "set an expiry" and "remove it" cannot share a replay
    // identifier — presenting one id with the other's payload is
    // rejected as a conflict.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let set = enqueue(
        &mut conn,
        &Mutation::UpdateShare {
            share_id: "s1".into(),
            description: None,
            expires_at: Some(9_999),
            clear_description: false,
            clear_expires_at: false,
        },
    )
    .await
    .unwrap();
    let cleared = enqueue(
        &mut conn,
        &Mutation::UpdateShare {
            share_id: "s1".into(),
            description: None,
            expires_at: None,
            clear_description: false,
            clear_expires_at: true,
        },
    )
    .await
    .unwrap();

    assert_ne!(set, cleared);
    assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_row_queued_before_clearing_existed_still_reads() {
    // Backwards compatibility with payloads written by an earlier
    // build: the absent flags mean "clear nothing".
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query(
        "INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
             VALUES ('op-old', 'update_share', ?, 0)",
    )
    .bind(r#"{"kind":"update_share","share_id":"s1","description":"a","expires_at":null}"#)
    .execute(&mut *conn)
    .await
    .unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[0].mutation,
        Mutation::UpdateShare {
            share_id: "s1".into(),
            description: Some("a".into()),
            expires_at: None,
            clear_description: false,
            clear_expires_at: false,
        }
    );
}

#[test]
fn a_server_identifier_is_never_mistaken_for_a_placeholder() {
    assert!(!is_placeholder("31be1c98-3984-40ff-8352-73fc6e2c29b5"));
    assert!(is_placeholder(&new_placeholder()));
}

#[test]
fn only_playlist_mutations_carry_a_placeholder_dependency() {
    assert!(favorite(true).placeholder_dependency().is_none());
    let id = new_placeholder();
    assert_eq!(
        Mutation::DeletePlaylist {
            playlist_id: id.clone()
        }
        .placeholder_dependency(),
        Some(id.as_str())
    );
    // A playlist the server already knows is not a dependency.
    assert!(Mutation::DeletePlaylist {
        playlist_id: "real-uuid".into()
    }
    .placeholder_dependency()
    .is_none());
}

#[test]
fn the_stored_shape_round_trips() {
    for mutation in [
        favorite(true),
        Mutation::SetRating {
            entity_type: "album".into(),
            entity_id: "a1".into(),
            rating: 4,
        },
        Mutation::ClearRating {
            entity_type: "track".into(),
            entity_id: "t1".into(),
        },
        Mutation::Scrobble {
            track_id: "t1".into(),
            submission: true,
            played_at: 1_000,
        },
        Mutation::SaveQueue {
            track_ids: vec!["t1".into()],
            current: Some("t1".into()),
            position_ms: 4200,
            client: Some("WaveFlow".into()),
        },
    ] {
        let json = serde_json::to_string(&mutation).unwrap();
        assert_eq!(serde_json::from_str::<Mutation>(&json).unwrap(), mutation);
    }
}

/// Refused entries go, pending ones stay.
///
/// The count of refused changes is shown in Settings and is never retried,
/// so before this existed the only way to clear it was to forget the
/// server — which also discards the mirror and everything still queued.
#[tokio::test]
async fn discarding_refused_changes_spares_the_pending_ones() {
    let pool = pool().await;
    sqlx::raw_sql(
        "INSERT INTO remote_mutation (id, operation_id, kind, payload, created_at,
                                          attempt_count, failed_at)
             VALUES (1, 'op-1', 'k', '{}', 0, 1, 123),
                    (2, 'op-2', 'k', '{}', 0, 0, NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(discard_failed(&mut conn).await.unwrap(), 1);

    // Through the connection already held: the fixture's pool allows one,
    // so asking it for a second while this one is alive waits forever.
    let left: Vec<String> = sqlx::query_scalar("SELECT operation_id FROM remote_mutation")
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        left,
        vec!["op-2".to_string()],
        "a pending change is still owed to the server"
    );
}
