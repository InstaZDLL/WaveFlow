use super::*;
use crate::remote::dto::SyncPage;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Row, SqlitePool};

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

fn change(entity_type: &str, action: &str, entity_id: &str, payload: Value) -> SyncChange {
    SyncChange {
        cursor: 1,
        entity_type: entity_type.into(),
        entity_id: entity_id.into(),
        action: action.into(),
        payload,
        origin_device_id: None,
        changed_at: 1_000,
    }
}

async fn playlist_row(conn: &mut SqliteConnection, id: &str) -> (String, Option<String>, i64) {
    let row =
        sqlx::query("SELECT name, comment, is_public FROM remote_playlist WHERE remote_id = ?")
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    (
        row.try_get("name").unwrap(),
        row.try_get("comment").unwrap(),
        row.try_get("is_public").unwrap(),
    )
}

#[tokio::test]
async fn creating_a_playlist_after_an_update_does_not_blank_what_the_update_set() {
    // The bug this guards: `create_playlist` emits neither `comment`
    // nor `public`, so an apply that wrote every field would erase
    // both every time a playlist is re-created or replayed.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "p1",
            serde_json::json!({
                "id": "p1", "name": "Mix", "comment": "notes",
                "public": true, "track_ids": ["t1"]
            }),
        ),
    )
    .await
    .unwrap();

    // Now the create-shaped payload: three keys only.
    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "p1",
            serde_json::json!({"id": "p1", "name": "Mix renamed", "track_ids": ["t1", "t2"]}),
        ),
    )
    .await
    .unwrap();

    let (name, comment, public) = playlist_row(&mut conn, "p1").await;
    assert_eq!(name, "Mix renamed");
    assert_eq!(comment.as_deref(), Some("notes"), "comment was blanked");
    assert_eq!(public, 1, "public was reset");
}

#[tokio::test]
async fn a_null_comment_is_a_real_clear() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "p1",
            serde_json::json!({"id": "p1", "name": "Mix", "comment": "notes"}),
        ),
    )
    .await
    .unwrap();
    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "p1",
            serde_json::json!({"id": "p1", "name": "Mix", "comment": null}),
        ),
    )
    .await
    .unwrap();

    let (_, comment, _) = playlist_row(&mut conn, "p1").await;
    assert_eq!(comment, None, "an explicit null must clear, not be ignored");
}

#[tokio::test]
async fn a_playlist_keeps_a_track_that_appears_twice() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "p1",
            serde_json::json!({"id": "p1", "name": "M", "track_ids": ["t1", "t1", "t2"]}),
        ),
    )
    .await
    .unwrap();

    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT track_remote_id FROM remote_playlist_track
              WHERE playlist_remote_id = 'p1' ORDER BY position",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(ids, vec!["t1", "t1", "t2"]);
}

#[tokio::test]
async fn updating_a_share_description_does_not_empty_it() {
    // `update_share` emits `{id, description, expires_at}` and no
    // track list. Writing tracks unconditionally would drop them.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "share",
            "upsert",
            "s1",
            serde_json::json!({"id": "s1", "track_ids": ["t1", "t2"], "description": "a"}),
        ),
    )
    .await
    .unwrap();
    apply_change(
        &mut conn,
        &change(
            "share",
            "upsert",
            "s1",
            serde_json::json!({"id": "s1", "description": "b", "expires_at": null}),
        ),
    )
    .await
    .unwrap();

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_share_track WHERE share_remote_id='s1'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(count, 2, "the share lost its tracks on a description edit");
}

#[tokio::test]
async fn a_zero_rating_clears_instead_of_storing_zero() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "rating",
            "upsert",
            "t1",
            serde_json::json!({"entity_type": "track", "entity_id": "t1", "rating": 4}),
        ),
    )
    .await
    .unwrap();
    apply_change(
        &mut conn,
        &change(
            "rating",
            "delete",
            "t1",
            serde_json::json!({"entity_type": "track", "entity_id": "t1", "rating": 0}),
        ),
    )
    .await
    .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_rating")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn an_out_of_range_rating_is_clamped_rather_than_aborting() {
    // Aborting would force a fresh snapshot whose replay hits the
    // same row — an unbreakable loop over one bad field.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "rating",
            "upsert",
            "t1",
            serde_json::json!({"entity_type": "track", "entity_id": "t1", "rating": 99}),
        ),
    )
    .await
    .unwrap();

    let rating: i64 = sqlx::query_scalar("SELECT rating FROM remote_rating")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(rating, 5);
}

#[tokio::test]
async fn an_unstarred_favorite_upsert_removes_the_row() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "favorite",
            "upsert",
            "t1",
            serde_json::json!({"entity_type": "track", "entity_id": "t1", "starred": true}),
        ),
    )
    .await
    .unwrap();
    apply_change(
        &mut conn,
        &change(
            "favorite",
            "upsert",
            "t1",
            serde_json::json!({"entity_type": "track", "entity_id": "t1", "starred": false}),
        ),
    )
    .await
    .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_favorite")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn two_plays_of_one_track_are_distinct_but_a_replay_is_not() {
    // There is no scrobble identifier in the protocol; the natural
    // key is the only thing that separates two plays.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    for played_at in [1_000, 2_000, 1_000] {
        apply_change(
            &mut conn,
            &change(
                "scrobble",
                "append",
                "t1",
                serde_json::json!({"track_id": "t1", "submission": true, "played_at": played_at}),
            ),
        )
        .await
        .unwrap();
    }

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_history")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn an_unknown_entity_is_skipped_not_failed() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let outcome = apply_change(
        &mut conn,
        &change("something_new", "upsert", "x", serde_json::json!({})),
    )
    .await
    .unwrap();
    assert_eq!(outcome, Outcome::Ignored);

    let outcome = apply_change(
        &mut conn,
        &change("playlist", "reorder_v2", "p1", serde_json::json!({})),
    )
    .await
    .unwrap();
    assert_eq!(outcome, Outcome::Ignored);
}

#[tokio::test]
async fn a_snapshot_replaces_everything_and_caches_its_songs() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    // Something stale to be swept away.
    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "old",
            serde_json::json!({"id": "old", "name": "Stale", "track_ids": ["z"]}),
        ),
    )
    .await
    .unwrap();

    let snapshot: SyncSnapshot = serde_json::from_value(serde_json::json!({
        "cursor": 12,
        "playlists": [{
            "id": "p1", "name": "Mix", "public": false, "created_at": 1, "updated_at": 2,
            "songs": [{"id": "t1", "title": "Song", "artist": "A", "duration_ms": 1000}]
        }],
        "favorites": [{"entity_type": "track", "entity_id": "t1", "starred_at": 5}],
        "ratings": [{"entity_type": "track", "entity_id": "t1", "rating": 3, "updated_at": 5}],
        "history": [{"track_id": "t1", "submission": true, "played_at": 9}],
        "queue": {"songs": [{"id": "t1", "title": "Song"}], "current": "t1",
                  "position_ms": 4200, "updated_at": 7},
        "shares": [{"id": "s1", "track_ids": ["t1"], "visit_count": 2}]
    }))
    .unwrap();

    apply_snapshot(&mut conn, &snapshot).await.unwrap();

    let stale: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_playlist WHERE remote_id = 'old'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(stale, 0, "the snapshot did not replace the old projection");

    let title: String = sqlx::query_scalar("SELECT title FROM remote_track WHERE remote_id = 't1'")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(title, "Song", "the snapshot's song metadata was not cached");

    // Nothing is missing: the snapshot carried every song it
    // referenced.
    assert!(missing_track_ids(&mut conn).await.unwrap().is_empty());
}

/// A server that names a playlist or share inside the placeholder
/// namespace — here, the very placeholder of an offline playlist — must
/// neither overwrite that playlist nor fail the snapshot on its key.
#[tokio::test]
async fn a_server_identifier_in_the_local_namespace_is_kept_out() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let local_id = crate::remote::mutation::new_placeholder();
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline mix')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();

    let snapshot: SyncSnapshot = serde_json::from_value(serde_json::json!({
        "cursor": 3,
        "playlists": [{"id": local_id, "name": "Hijack", "public": false, "songs": []}],
        "shares": [{"id": "local:share", "track_ids": []}]
    }))
    .unwrap();
    apply_snapshot(&mut conn, &snapshot).await.unwrap();

    let name: String = sqlx::query_scalar("SELECT name FROM remote_playlist WHERE remote_id = ?")
        .bind(&local_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(name, "Offline mix");
    let shares: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_share")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(shares, 0);

    for (entity_type, action) in [("playlist", "upsert"), ("playlist", "delete")] {
        let outcome = apply_change(
            &mut conn,
            &change(
                entity_type,
                action,
                &local_id,
                serde_json::json!({"id": local_id, "name": "Hijack"}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(outcome, Outcome::Ignored, "{entity_type} {action}");
    }
    let name: String = sqlx::query_scalar("SELECT name FROM remote_playlist WHERE remote_id = ?")
        .bind(&local_id)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(name, "Offline mix", "a change reached the offline playlist");
}

/// The snapshot spares placeholders by the same test `is_placeholder`
/// applies: case-sensitive. `LIKE` folded case, so a server playlist
/// named `LOCAL:…` outlived every snapshot that no longer listed it.
#[tokio::test]
async fn a_snapshot_spares_only_what_is_placeholder() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES ('LOCAL:x', 'Server')")
        .execute(&mut *conn)
        .await
        .unwrap();
    let snapshot: SyncSnapshot = serde_json::from_str(r#"{"cursor": 4}"#).unwrap();
    apply_snapshot(&mut conn, &snapshot).await.unwrap();
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_playlist")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_snapshot_does_not_destroy_a_playlist_created_offline() {
    // The creation is still queued, so the server has never heard of
    // this playlist and its snapshot cannot be evidence that it was
    // deleted.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let local_id = crate::remote::mutation::new_placeholder();

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

    let snapshot: SyncSnapshot = serde_json::from_str(r#"{"cursor": 9}"#).unwrap();
    apply_snapshot(&mut conn, &snapshot).await.unwrap();

    let survived: i64 =
        sqlx::query_scalar("SELECT count(*) FROM remote_playlist WHERE remote_id = ?")
            .bind(&local_id)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(survived, 1, "an unsent offline playlist was destroyed");

    let tracks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM remote_playlist_track WHERE playlist_remote_id = ?",
    )
    .bind(&local_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(tracks, 1, "its tracks went with it");
}

#[tokio::test]
async fn identifiers_from_a_change_event_show_up_as_missing_metadata() {
    // A change carries bare identifiers, so this is the gap the
    // orchestrator has to close by fetching.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    apply_change(
        &mut conn,
        &change(
            "playlist",
            "upsert",
            "p1",
            serde_json::json!({"id": "p1", "name": "M", "track_ids": ["t9"]}),
        ),
    )
    .await
    .unwrap();

    assert_eq!(missing_track_ids(&mut conn).await.unwrap(), vec!["t9"]);
}

/// Captured verbatim from a running server (2026-08-10) seeded with
/// a real six-track library, then driven through every entity the
/// protocol carries: two favourites (a track and an album), two
/// ratings, two scrobbles, a saved queue, a playlist created then
/// updated, and a share.
///
/// Bytes the server produced, not a shape we imagined — the only
/// kind of fixture that can catch us having imagined the wrong one.
const REAL_SNAPSHOT: &str = include_str!("../testdata/snapshot.json");
const REAL_CHANGES: &str = include_str!("../testdata/changes.json");

/// Everything the projection holds, in a form two feeds can be
/// compared on.
async fn projected_state(conn: &mut SqliteConnection) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for (label, sql) in [
        (
            "playlist",
            "SELECT remote_id || '|' || name || '|' || COALESCE(comment, '~') || '|' || is_public
                   FROM remote_playlist ORDER BY remote_id",
        ),
        (
            "playlist_track",
            "SELECT playlist_remote_id || '|' || position || '|' || track_remote_id
                   FROM remote_playlist_track ORDER BY playlist_remote_id, position",
        ),
        (
            "favorite",
            "SELECT entity_type || '|' || entity_id FROM remote_favorite
                  ORDER BY entity_type, entity_id",
        ),
        (
            "rating",
            "SELECT entity_type || '|' || entity_id || '|' || rating FROM remote_rating
                  ORDER BY entity_type, entity_id",
        ),
        (
            "history",
            "SELECT track_remote_id || '|' || played_at || '|' || submission
                   FROM remote_history ORDER BY played_at",
        ),
        (
            "queue",
            "SELECT COALESCE(current_remote_id, '~') || '|' || position_ms FROM remote_queue",
        ),
        (
            "queue_track",
            "SELECT position || '|' || track_remote_id FROM remote_queue_track ORDER BY position",
        ),
        (
            "share",
            "SELECT remote_id || '|' || COALESCE(description, '~') FROM remote_share
                  ORDER BY remote_id",
        ),
        (
            "share_track",
            "SELECT share_remote_id || '|' || position || '|' || track_remote_id
                   FROM remote_share_track ORDER BY share_remote_id, position",
        ),
    ] {
        let values: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        for value in values {
            rows.push((label.to_string(), value));
        }
    }
    rows
}

#[tokio::test]
async fn the_servers_own_snapshot_applies_across_every_entity() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let snapshot: SyncSnapshot = serde_json::from_str(REAL_SNAPSHOT).unwrap();

    apply_snapshot(&mut conn, &snapshot).await.unwrap();

    let state = projected_state(&mut conn).await;
    let count = |label: &str| state.iter().filter(|(l, _)| l == label).count();
    assert_eq!(count("playlist"), 1);
    assert_eq!(count("playlist_track"), 3);
    assert_eq!(count("favorite"), 2, "a track and an album were starred");
    assert_eq!(count("rating"), 2);
    assert_eq!(count("history"), 2);
    assert_eq!(count("queue"), 1);
    assert_eq!(count("queue_track"), 3);
    assert_eq!(count("share"), 1);
    assert_eq!(count("share_track"), 2);

    // The snapshot carries whole song objects, so nothing is left
    // waiting for metadata.
    assert!(missing_track_ids(&mut conn).await.unwrap().is_empty());
    let cached: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_track")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert!(cached >= 3, "the snapshot's songs were not cached");
}

#[tokio::test]
async fn replaying_the_servers_own_journal_reaches_the_snapshot_state() {
    // The two feeds must agree. If they do not, what the user sees
    // depends on whether they bootstrapped or caught up — which is
    // the kind of divergence nobody can debug from a screenshot.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();

    let page: SyncPage = serde_json::from_str(REAL_CHANGES).unwrap();
    assert_eq!(page.changes.len(), 10, "the capture lost events");
    for change in &page.changes {
        assert_eq!(
            apply_change(&mut conn, change).await.unwrap(),
            Outcome::Applied,
            "event {} ({}/{}) was not understood",
            change.cursor,
            change.entity_type,
            change.action
        );
    }
    let from_journal = projected_state(&mut conn).await;

    // Same database, replaced by the snapshot.
    let snapshot: SyncSnapshot = serde_json::from_str(REAL_SNAPSHOT).unwrap();
    apply_snapshot(&mut conn, &snapshot).await.unwrap();
    let from_snapshot = projected_state(&mut conn).await;

    assert_eq!(
        from_journal, from_snapshot,
        "walking the journal and taking a snapshot produced different state"
    );
}

#[tokio::test]
async fn a_sparser_sighting_does_not_blank_richer_metadata() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let rich: SongItem = serde_json::from_value(serde_json::json!({
        "id": "t1", "title": "Song", "artist": "A", "album": "B", "duration_ms": 1234
    }))
    .unwrap();
    let sparse: SongItem =
        serde_json::from_value(serde_json::json!({"id": "t1", "title": "Song"})).unwrap();

    cache_song(&mut conn, &rich).await.unwrap();
    cache_song(&mut conn, &sparse).await.unwrap();

    let row = sqlx::query("SELECT artist, duration_ms FROM remote_track WHERE remote_id='t1'")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(
        row.try_get::<Option<String>, _>("artist")
            .unwrap()
            .as_deref(),
        Some("A")
    );
    assert_eq!(row.try_get::<i64, _>("duration_ms").unwrap(), 1234);
}

#[tokio::test]
async fn a_patch_reply_can_blank_what_the_upsert_would_have_kept() {
    // The one behaviour that stops `apply_track_metadata` being a
    // call to `cache_song`. Removing a correction hands back a
    // *smaller* value, and coalescing would keep showing the year
    // and genre the user just deleted, with no later pass able to
    // put it right.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let corrected: SongItem = serde_json::from_value(serde_json::json!({
        "id": "t1", "title": "Army of Me", "artist": "Björk",
        "genre": "Trip Hop", "year": 1995, "duration_ms": 1234
    }))
    .unwrap();
    cache_song(&mut conn, &corrected).await.unwrap();

    // What the server answers once the corrections are withdrawn:
    // the file's own tags, which carry neither.
    let uncorrected: SongItem = serde_json::from_value(serde_json::json!({
        "id": "t1", "title": "Army of Me", "artist": "Björk"
    }))
    .unwrap();
    apply_track_metadata(&mut conn, &uncorrected).await.unwrap();

    let row =
        sqlx::query("SELECT genre, year, size, duration_ms FROM remote_track WHERE remote_id='t1'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(row.try_get::<Option<String>, _>("genre").unwrap(), None);
    assert_eq!(row.try_get::<Option<i64>, _>("year").unwrap(), None);
    // And nothing outside the corrected fields moved: a metadata
    // patch does not touch the file, so it cannot change its size
    // or its duration.
    assert_eq!(row.try_get::<i64, _>("duration_ms").unwrap(), 1234);
}

#[tokio::test]
async fn a_patch_reply_for_an_unmirrored_track_creates_nothing() {
    // Minting a row from a reply would enter a track into the
    // catalogue by a path that never established it belongs to a
    // mirrored library.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let song: SongItem =
        serde_json::from_value(serde_json::json!({"id": "ghost", "title": "Nowhere"})).unwrap();

    apply_track_metadata(&mut conn, &song).await.unwrap();

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_track")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
