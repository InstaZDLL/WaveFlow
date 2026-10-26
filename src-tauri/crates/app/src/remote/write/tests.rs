use super::*;
use crate::remote::mutation::pending;
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

async fn count(conn: &mut SqliteConnection, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&mut *conn)
        .await
        .unwrap()
}

#[tokio::test]
async fn starring_writes_the_projection_and_queues_the_mutation() {
    // The pairing is the module's reason to exist: one without the
    // other either loses the change or shows the wrong value.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    set_favorite_in_tx(&mut conn, "track", "t1", true)
        .await
        .unwrap();

    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_favorite").await,
        1
    );
    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[0].mutation,
        Mutation::SetFavorite {
            entity_type: "track".into(),
            entity_id: "t1".into(),
            starred: true
        }
    );
}

#[tokio::test]
async fn unstarring_removes_the_row_and_still_queues() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    set_favorite_in_tx(&mut conn, "track", "t1", true)
        .await
        .unwrap();
    set_favorite_in_tx(&mut conn, "track", "t1", false)
        .await
        .unwrap();

    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_favorite").await,
        0
    );
    assert_eq!(
        pending(&mut conn, 10).await.unwrap().len(),
        2,
        "the removal has to travel too"
    );
}

#[tokio::test]
async fn a_zero_rating_clears_locally_and_queues_a_clear() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    set_rating_in_tx(&mut conn, "track", "t1", 4).await.unwrap();
    set_rating_in_tx(&mut conn, "track", "t1", 0).await.unwrap();

    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_rating").await,
        0
    );
    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[1].mutation,
        Mutation::ClearRating {
            entity_type: "track".into(),
            entity_id: "t1".into()
        },
        "a zero must not travel as a rating of zero"
    );
}

#[tokio::test]
async fn a_rating_above_five_is_refused_before_anything_is_written() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    assert!(set_rating_in_tx(&mut conn, "track", "t1", 6).await.is_err());

    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_rating").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_mutation").await,
        0,
        "a refused gesture must not leave a queued mutation behind"
    );
}

#[tokio::test]
async fn a_new_playlist_is_visible_immediately_and_queued() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let id = create_playlist_in_tx(&mut conn, "  Offline mix  ", &["t1".into(), "t2".into()])
        .await
        .unwrap();

    assert!(mutation::is_placeholder(&id));
    let name: String = sqlx::query_scalar("SELECT name FROM remote_playlist WHERE remote_id = ?")
        .bind(&id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(name, "Offline mix", "the name was not trimmed");
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_playlist_track").await,
        2
    );
    assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_nameless_playlist_is_refused() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(create_playlist_in_tx(&mut conn, "   ", &[]).await.is_err());
}

#[tokio::test]
async fn clearing_a_comment_wins_over_setting_one() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let id = create_playlist_in_tx(&mut conn, "Mix", &[]).await.unwrap();
    update_playlist_in_tx(&mut conn, &id, None, Some("notes".into()), None, false)
        .await
        .unwrap();

    // Contradictory input: a value and a clear. The clear is the
    // more explicit intent and must not silently lose.
    update_playlist_in_tx(&mut conn, &id, None, Some("ignored".into()), None, true)
        .await
        .unwrap();

    let comment: Option<String> =
        sqlx::query_scalar("SELECT comment FROM remote_playlist WHERE remote_id = ?")
            .bind(&id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(comment, None);

    let queued = pending(&mut conn, 10).await.unwrap();
    match &queued.last().unwrap().mutation {
        Mutation::UpdatePlaylist {
            comment,
            clear_comment,
            ..
        } => {
            assert!(clear_comment);
            assert_eq!(
                comment, &None,
                "sending both would let the server coalesce the value back in"
            );
        }
        other => panic!("unexpected mutation: {other:?}"),
    }
}

#[tokio::test]
async fn deleting_an_unsent_playlist_drops_its_queued_creation() {
    // Sending a creation and then a delete describes something that
    // existed for nobody. Both are dropped instead.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let id = create_playlist_in_tx(&mut conn, "Offline", &["t1".into()])
        .await
        .unwrap();
    assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);

    delete_playlist_in_tx(&mut conn, &id).await.unwrap();

    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_playlist").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_playlist_track").await,
        0
    );
    assert!(
        pending(&mut conn, 10).await.unwrap().is_empty(),
        "the creation should not travel for something already deleted"
    );
}

#[tokio::test]
async fn removing_a_track_compacts_the_projection_and_queues_the_index() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('p', 'Mix');
             INSERT INTO remote_playlist_track VALUES ('p', 0, 'a'), ('p', 1, 'b'), ('p', 2, 'c');",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    remove_playlist_tracks_in_tx(&mut conn, "p", &[1])
        .await
        .unwrap();

    // The survivors keep their order with dense positions, so a later
    // index-based edit still lines up with the server's.
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT position, track_remote_id FROM remote_playlist_track
              WHERE playlist_remote_id = 'p' ORDER BY position",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(rows, vec![(0, "a".into()), (1, "c".into())]);

    let queued = pending(&mut conn, 10).await.unwrap();
    match &queued.last().unwrap().mutation {
        Mutation::UpdatePlaylist { remove_indexes, .. } => {
            assert_eq!(remove_indexes, &vec![1]);
        }
        other => panic!("unexpected mutation: {other:?}"),
    }
}

#[tokio::test]
async fn reordering_rewrites_the_order_and_queues_a_full_replace() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('p', 'Mix');
             INSERT INTO remote_playlist_track VALUES ('p', 0, 'a'), ('p', 1, 'b'), ('p', 2, 'c');",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    // Move the last track to the front: [a,b,c] -> [c,a,b].
    reorder_playlist_in_tx(&mut conn, "p", 2, 0).await.unwrap();

    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT position, track_remote_id FROM remote_playlist_track
              WHERE playlist_remote_id = 'p' ORDER BY position",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(0, "c".into()), (1, "a".into()), (2, "b".into())]
    );

    // The server has no move, so it's expressed as remove-all + re-add
    // in the new order.
    let queued = pending(&mut conn, 10).await.unwrap();
    match &queued.last().unwrap().mutation {
        Mutation::UpdatePlaylist {
            add,
            remove_indexes,
            ..
        } => {
            assert_eq!(remove_indexes, &vec![0, 1, 2]);
            assert_eq!(add, &vec!["c".to_string(), "a".into(), "b".into()]);
        }
        other => panic!("unexpected mutation: {other:?}"),
    }
}

#[tokio::test]
async fn deleting_a_playlist_the_server_knows_queues_the_delete() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES ('server-id', 'Mix')")
        .execute(&mut *conn)
        .await
        .unwrap();

    delete_playlist_in_tx(&mut conn, "server-id").await.unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[0].mutation,
        Mutation::DeletePlaylist {
            playlist_id: "server-id".into()
        }
    );
}

#[tokio::test]
async fn a_failed_write_leaves_neither_half_behind() {
    // The transaction is what guarantees this. Rolling back after a
    // projection write must take the queued mutation with it.
    let pool = pool().await;
    let mut tx = pool.begin().await.unwrap();
    set_favorite_in_tx(&mut tx, "track", "t1", true)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_favorite").await,
        0
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_mutation").await,
        0
    );
}

#[tokio::test]
async fn a_now_playing_ping_travels_without_entering_the_history() {
    // It is a transient state, not a play. Writing it locally would
    // show a listen that never happened.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    scrobble_in_tx(&mut conn, "t1", false, Some(1_000))
        .await
        .unwrap();
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_history").await,
        0
    );
    assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);

    scrobble_in_tx(&mut conn, "t1", true, Some(2_000))
        .await
        .unwrap();
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_history").await,
        1
    );
}

#[tokio::test]
async fn saving_the_queue_replaces_it_rather_than_appending() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    save_queue_in_tx(
        &mut conn,
        &["a".into(), "b".into()],
        Some("a".into()),
        0,
        None,
    )
    .await
    .unwrap();
    save_queue_in_tx(&mut conn, &["c".into()], Some("c".into()), 4200, None)
        .await
        .unwrap();

    let ids: Vec<String> =
        sqlx::query_scalar("SELECT track_remote_id FROM remote_queue_track ORDER BY position")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert_eq!(ids, vec!["c"]);
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_queue").await,
        1
    );
}

#[tokio::test]
async fn a_negative_position_never_reaches_storage() {
    // The column has a CHECK, so a stray negative would abort the
    // whole gesture rather than degrade.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    save_queue_in_tx(&mut conn, &["a".into()], None, -5, None)
        .await
        .unwrap();
    let position: i64 = sqlx::query_scalar("SELECT position_ms FROM remote_queue")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(position, 0);
}

#[tokio::test]
async fn a_share_is_visible_at_once_but_has_no_link_yet() {
    // The token is derived from a server-side secret, so the link
    // cannot exist before the creation lands.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let id = create_share_in_tx(&mut conn, &["t1".into()], Some("notes".into()), None)
        .await
        .unwrap();

    assert!(mutation::is_placeholder(&id));
    let url: Option<String> =
        sqlx::query_scalar("SELECT url FROM remote_share WHERE remote_id = ?")
            .bind(&id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(url, None);
    assert_eq!(pending(&mut conn, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_empty_share_is_refused() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    assert!(create_share_in_tx(&mut conn, &[], None, None)
        .await
        .is_err());
}

#[tokio::test]
async fn clearing_an_expiry_empties_it_locally_and_says_so_upstream() {
    // The case that motivated the whole clear verb: without it an
    // expiry set by mistake is permanent.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let id = create_share_in_tx(&mut conn, &["t1".into()], None, Some(9_999))
        .await
        .unwrap();

    update_share_in_tx(&mut conn, &id, None, None, false, true)
        .await
        .unwrap();

    let expires: Option<i64> =
        sqlx::query_scalar("SELECT expires_at FROM remote_share WHERE remote_id = ?")
            .bind(&id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(expires, None);

    match &pending(&mut conn, 10)
        .await
        .unwrap()
        .last()
        .unwrap()
        .mutation
    {
        Mutation::UpdateShare {
            clear_expires_at,
            expires_at,
            ..
        } => {
            assert!(clear_expires_at);
            assert_eq!(expires_at, &None, "sending both would coalesce it back");
        }
        other => panic!("unexpected mutation: {other:?}"),
    }
}

#[tokio::test]
async fn deleting_an_unsent_share_drops_its_queued_creation() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let id = create_share_in_tx(&mut conn, &["t1".into()], None, None)
        .await
        .unwrap();

    delete_share_in_tx(&mut conn, &id).await.unwrap();

    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_share").await,
        0
    );
    assert!(pending(&mut conn, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn every_gesture_leaves_the_projection_and_the_queue_in_step() {
    // A sweep rather than a per-gesture assertion: any future
    // gesture that writes one half and forgets the other fails here.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    set_favorite_in_tx(&mut conn, "track", "t1", true)
        .await
        .unwrap();
    set_rating_in_tx(&mut conn, "track", "t1", 3).await.unwrap();
    let playlist = create_playlist_in_tx(&mut conn, "Mix", &[]).await.unwrap();
    update_playlist_in_tx(
        &mut conn,
        &playlist,
        Some("Renamed".into()),
        None,
        None,
        false,
    )
    .await
    .unwrap();
    scrobble_in_tx(&mut conn, "t1", true, Some(1_000))
        .await
        .unwrap();
    save_queue_in_tx(&mut conn, &["t1".into()], None, 0, None)
        .await
        .unwrap();
    create_share_in_tx(&mut conn, &["t1".into()], None, None)
        .await
        .unwrap();

    let queued = pending(&mut conn, 20).await.unwrap();
    assert_eq!(queued.len(), 7, "a gesture wrote without queuing");
    let rows = count(&mut conn, "SELECT count(*) FROM remote_favorite").await
        + count(&mut conn, "SELECT count(*) FROM remote_rating").await
        + count(&mut conn, "SELECT count(*) FROM remote_playlist").await
        + count(&mut conn, "SELECT count(*) FROM remote_history").await
        + count(&mut conn, "SELECT count(*) FROM remote_queue").await
        + count(&mut conn, "SELECT count(*) FROM remote_share").await;
    assert_eq!(rows, 6, "a gesture queued without writing");
}

/// A mirrored track to correct. `cache_song` would do, but it needs
/// a whole `SongItem`, and what these tests care about is the row.
async fn mirror_track(conn: &mut SqliteConnection) {
    sqlx::query(
        "INSERT INTO remote_track
                (remote_id, title, artist, genre, year, track_no, disc_no, duration_ms, cached_at)
             VALUES ('t1', 'Intro', 'Bjork', 'Trip Hop', 1995, 1, 1, 1000, 0)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
}

async fn text(conn: &mut SqliteConnection, column: &str) -> Option<String> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {column} FROM remote_track WHERE remote_id = 't1'"
    )))
    .fetch_one(&mut *conn)
    .await
    .unwrap()
}

#[tokio::test]
async fn correcting_a_track_writes_the_mirror_and_queues_the_patch() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    mirror_track(&mut conn).await;

    set_track_metadata_in_tx(
        &mut conn,
        "t1",
        &TrackMetadataEdit {
            title: "Army of Me".into(),
            artist: "Björk; Skunk Anansie".into(),
            genre: "Trip Hop".into(),
            year: Some(1995),
            track_number: Some(1),
            disc_number: Some(1),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        text(&mut conn, "title").await.as_deref(),
        Some("Army of Me")
    );
    assert_eq!(
        text(&mut conn, "artist").await.as_deref(),
        Some("Björk; Skunk Anansie")
    );
    // Derived from the artist string, or the unified listing sorts
    // this row under the name it no longer carries.
    assert_eq!(
        text(&mut conn, "sort_artist").await,
        Some(waveflow_core::metadata::name_match::normalize_name(
            "Björk; Skunk Anansie"
        ))
    );

    let queued = pending(&mut conn, 10).await.unwrap();
    assert_eq!(
        queued[0].mutation,
        Mutation::UpdateTrackMetadata {
            track_id: "t1".into(),
            title: Some("Army of Me".into()),
            // The joined form stops here: the server takes the list
            // so that nobody has to guess where a name ends.
            artists: Some(vec!["Björk".into(), "Skunk Anansie".into()]),
            genres: Some(vec!["Trip Hop".into()]),
            year: Some(1995),
            track_number: Some(1),
            disc_number: Some(1),
        }
    );
}

#[tokio::test]
async fn a_cleared_field_removes_the_correction_rather_than_emptying_it() {
    // `Some(vec![])` would be a stored claim that the track credits
    // nobody. An empty box means the user is undoing a correction,
    // and reading it the other way would make the blank permanent.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    mirror_track(&mut conn).await;

    set_track_metadata_in_tx(
        &mut conn,
        "t1",
        &TrackMetadataEdit {
            title: "Army of Me".into(),
            artist: "   ".into(),
            genre: String::new(),
            year: None,
            track_number: None,
            disc_number: None,
        },
    )
    .await
    .unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    let Mutation::UpdateTrackMetadata {
        artists,
        genres,
        year,
        ..
    } = &queued[0].mutation
    else {
        panic!("the queued entry is not a metadata patch");
    };
    assert_eq!(*artists, None, "a blank field asked for no correction");
    assert_eq!(*genres, None);
    assert_eq!(*year, None);

    // And the mirror shows the clearing straight away. An upsert
    // could not: `cache_song` coalesces, so it would keep showing
    // the value the user just deleted.
    assert_eq!(text(&mut conn, "artist").await, None);
    assert_eq!(text(&mut conn, "genre").await, None);
}

#[tokio::test]
async fn a_name_holding_the_bare_separator_survives_the_split() {
    // "AC;DC" is one band. Splitting on the character rather than on
    // the separator would send the server two artists that never
    // existed, under a wholesale patch that makes them the truth.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    mirror_track(&mut conn).await;

    set_track_metadata_in_tx(
        &mut conn,
        "t1",
        &TrackMetadataEdit {
            artist: "AC;DC".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    let Mutation::UpdateTrackMetadata { artists, .. } = &queued[0].mutation else {
        panic!("the queued entry is not a metadata patch");
    };
    assert_eq!(artists.as_deref(), Some(&["AC;DC".to_string()][..]));
}

#[tokio::test]
async fn a_name_holding_a_comma_survives_the_split() {
    // The local editor accepts a comma on input; `"; "` is the only
    // separator that crosses the wire, so "Earth, Wind & Fire"
    // stays one artist.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    mirror_track(&mut conn).await;

    set_track_metadata_in_tx(
        &mut conn,
        "t1",
        &TrackMetadataEdit {
            artist: "Earth, Wind & Fire; Deniece Williams".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let queued = pending(&mut conn, 10).await.unwrap();
    let Mutation::UpdateTrackMetadata { artists, .. } = &queued[0].mutation else {
        panic!("the queued entry is not a metadata patch");
    };
    assert_eq!(
        artists.as_deref(),
        Some(
            &[
                "Earth, Wind & Fire".to_string(),
                "Deniece Williams".to_string()
            ][..]
        )
    );
}

#[tokio::test]
async fn a_placeholder_track_is_refused_before_anything_is_written() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let error = set_track_metadata_in_tx(
        &mut conn,
        &mutation::new_placeholder(),
        &TrackMetadataEdit::default(),
    )
    .await;

    assert!(
        error.is_err(),
        "a track the server cannot resolve was queued"
    );
    assert_eq!(
        count(&mut conn, "SELECT count(*) FROM remote_mutation").await,
        0,
        "a patch that can only ever fail was left in the queue"
    );
}
