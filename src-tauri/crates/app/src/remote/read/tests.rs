use super::*;
use crate::remote::dto::SyncSnapshot;
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

/// The same capture the projection tests replay: a real server's
/// snapshot of a seeded library.
const REAL_SNAPSHOT: &str = include_str!("../testdata/snapshot.json");

#[tokio::test]
async fn the_overview_counts_a_real_snapshot() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let snapshot: SyncSnapshot = serde_json::from_str(REAL_SNAPSHOT).unwrap();
    crate::remote::projection::apply_snapshot(&mut conn, &snapshot)
        .await
        .unwrap();

    let overview = overview(&mut conn).await.unwrap();
    assert_eq!(overview.playlists, 1);
    assert_eq!(overview.favorites, 2);
    assert_eq!(overview.ratings, 2);
    assert_eq!(overview.history, 2);
    assert_eq!(overview.shares, 1);
    assert_eq!(overview.queue_tracks, 3);
    assert_eq!(overview.tracks_awaiting_metadata, 0);
    assert_eq!(overview.pending_changes, 0);
}

#[tokio::test]
async fn an_unbound_profile_reads_as_all_zeroes_rather_than_failing() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    assert_eq!(
        overview(&mut conn).await.unwrap(),
        RemoteOverview::default()
    );
    assert!(playlists(&mut conn).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_track_without_metadata_keeps_its_position() {
    // The bug this guards: an inner join would drop the row and
    // renumber everything after it, so a playlist would silently
    // look shorter than it is while a backfill is outstanding.
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('p1', 'Mix');
             INSERT INTO remote_playlist_track VALUES ('p1', 0, 'known'), ('p1', 1, 'unknown'),
                                                      ('p1', 2, 'known2');
             INSERT INTO remote_track (remote_id, title, duration_ms, cached_at)
                VALUES ('known', 'First', 1000, 0), ('known2', 'Third', 3000, 0);",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    let tracks = playlist_tracks(&mut conn, "p1").await.unwrap();
    assert_eq!(tracks.len(), 3);
    assert_eq!(tracks[0].title.as_deref(), Some("First"));
    assert_eq!(tracks[1].title, None, "the placeholder lost its slot");
    assert_eq!(tracks[1].id, "unknown");
    assert_eq!(tracks[2].title.as_deref(), Some("Third"));
}

#[tokio::test]
async fn a_summary_understates_rather_than_refusing_a_duration() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_playlist (remote_id, name) VALUES ('p1', 'Mix');
             INSERT INTO remote_playlist_track VALUES ('p1', 0, 'known'), ('p1', 1, 'unknown');
             INSERT INTO remote_track (remote_id, title, duration_ms, cached_at)
                VALUES ('known', 'First', 1000, 0);",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    let summaries = playlists(&mut conn).await.unwrap();
    assert_eq!(summaries[0].track_count, 2, "both entries are counted");
    assert_eq!(summaries[0].duration_ms, 1000, "only the known one adds up");
    assert!(!summaries[0].pending_creation);
}

#[tokio::test]
async fn a_playlist_created_offline_is_flagged_as_such() {
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    let local_id = crate::remote::mutation::new_placeholder();
    sqlx::query("INSERT INTO remote_playlist (remote_id, name) VALUES (?, 'Offline')")
        .bind(&local_id)
        .execute(&mut *conn)
        .await
        .unwrap();

    let summaries = playlists(&mut conn).await.unwrap();
    assert!(
        summaries[0].pending_creation,
        "the UI cannot tell the user this one has not left the machine"
    );
}

#[tokio::test]
async fn failed_changes_are_counted_apart_from_pending_ones() {
    // They mean different things to a user: one is "not yet", the
    // other is "never, and nobody will tell you unless we do".
    let pool = pool().await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO remote_mutation (operation_id, kind, payload, created_at)
                VALUES ('a', 'k', '{}', 0), ('b', 'k', '{}', 0);
             UPDATE remote_mutation SET failed_at = 1 WHERE operation_id = 'b';",
    )
    .execute(&mut *conn)
    .await
    .unwrap();

    let overview = overview(&mut conn).await.unwrap();
    assert_eq!(overview.pending_changes, 1);
    assert_eq!(overview.failed_changes, 1);
}
