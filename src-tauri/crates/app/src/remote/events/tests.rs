use super::*;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

/// The real migrator against a real database with `foreign_keys` on, so
/// the constraints under test are the ones that ship rather than a
/// fixture's idea of them. Same shape as `remote::hashing`'s.
async fn pool() -> SqlitePool {
    let options = SqliteConnectOptions::from_str(":memory:")
        .unwrap()
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/profile")
        .run(&pool)
        .await
        .unwrap();
    pool
}

async fn library(pool: &SqlitePool, cursor: Option<i64>) {
    sqlx::query(
        "INSERT INTO remote_library (remote_id, name, mirrored_at, events_cursor)
             VALUES ('lib', 'Library', 1, ?)",
    )
    .bind(cursor)
    .execute(pool)
    .await
    .unwrap();
}

/// The libraries a pass would read, using production's own predicate.
async fn selectable(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT remote_id FROM remote_library
              WHERE mirrored_at IS NOT NULL
                AND (events_cursor IS NULL OR events_cursor >= 0)",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn cursor_of(pool: &SqlitePool) -> Option<i64> {
    sqlx::query_scalar("SELECT events_cursor FROM remote_library WHERE remote_id = 'lib'")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_cursor_is_never_moved_backwards() {
    // Two passes racing would otherwise let the slower one rewind the
    // faster one's position and replay events already applied.
    let pool = pool().await;
    library(&pool, Some(0)).await;

    save_cursor(&pool, "lib", 42).await.unwrap();
    save_cursor(&pool, "lib", 7).await.unwrap();

    assert_eq!(cursor_of(&pool).await, Some(42));
}

async fn album(pool: &SqlitePool, id: &str, library_id: Option<&str>) {
    sqlx::query(
        "INSERT INTO remote_album (remote_id, library_id, title, song_count, mirrored_at)
             VALUES (?, ?, 'A', 3, 1)",
    )
    .bind(id)
    .bind(library_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn album_walked(pool: &SqlitePool, id: &str) -> bool {
    let at: Option<i64> =
        sqlx::query_scalar("SELECT mirrored_at FROM remote_album WHERE remote_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    at.is_some()
}

#[tokio::test]
async fn giving_up_on_a_feed_makes_the_walk_re_read_its_albums() {
    // The part that does the work, and the part the first version got
    // wrong. Freshness lives in `remote_album.mirrored_at` against
    // `song_count`, so clearing the library's sweep date changed nothing
    // about which albums the walk fetches — and the events we missed are
    // corrections, which leave `song_count` alone. The walk would have
    // skipped precisely the albums that needed re-reading.
    let pool = pool().await;
    library(&pool, Some(99)).await;
    album(&pool, "a-ours", Some("lib")).await;
    album(&pool, "a-unattributed", None).await;
    album(&pool, "a-elsewhere", Some("other-lib")).await;

    forget_cursor(&pool, "lib").await.unwrap();

    assert!(!album_walked(&pool, "a-ours").await);
    // Nullable column: an album we cannot attribute is one we cannot
    // prove is unaffected, and re-walking it costs a request while
    // skipping it would leave wrong metadata with nothing to correct it.
    assert!(!album_walked(&pool, "a-unattributed").await);
    assert!(
        album_walked(&pool, "a-elsewhere").await,
        "another library's albums were invalidated"
    );
}

#[tokio::test]
async fn a_refused_feed_is_not_asked_again_on_the_next_pass() {
    // Without the mark the pass loops: forget the cursor, ask from 0
    // again, be refused again, invalidate the mirror again — a full
    // re-walk on every pass, forever. The server exposes no watermark,
    // so there is no cursor to adopt instead; not asking is the only
    // honest answer.
    let pool = pool().await;
    library(&pool, Some(99)).await;

    forget_cursor(&pool, "lib").await.unwrap();

    assert_eq!(cursor_of(&pool).await, Some(CURSOR_UNREACHABLE));
    assert!(
        selectable(&pool).await.is_empty(),
        "a feed known to be out of reach was queued for another attempt"
    );

    // Emptying the mirror is the way back: the catalogue that cursor
    // described is gone, so asking from zero means something again.
    sqlx::query("UPDATE remote_library SET events_cursor = NULL")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(selectable(&pool).await, vec!["lib".to_string()]);
}

#[tokio::test]
async fn the_librarys_own_sweep_date_survives() {
    // It is what the interface shows as "last copied", and this library
    // has been copied. Its albums are what went stale.
    let pool = pool().await;
    library(&pool, Some(99)).await;

    forget_cursor(&pool, "lib").await.unwrap();

    let swept: Option<i64> =
        sqlx::query_scalar("SELECT mirrored_at FROM remote_library WHERE remote_id = 'lib'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(swept, Some(1));
}

async fn link(pool: &SqlitePool, hash: &str, status: &str) {
    // A library and a track, every NOT NULL column filled and the
    // foreign key satisfied. The fixture runs the real migrator with
    // `foreign_keys` on, so the real constraints apply — a hand-trimmed
    // insert reads fine and fails in CI, which is how the first version
    // of this fixture went out.
    sqlx::raw_sql(
        "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/t.flac', 'local-hash', 1, 0, 'T', 1000, 0, 1, 0, 0, 0, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO remote_track_link
                (local_track_id, remote_track_id, method, verified_full_hash, status,
                 confirmed_at, verified_at)
             VALUES (1, 'rt', 'exact_full_hash', ?, ?, 0, 0)",
    )
    .bind(hash)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

async fn status_of(pool: &SqlitePool) -> String {
    sqlx::query_scalar("SELECT status FROM remote_track_link WHERE remote_track_id = 'rt'")
        .fetch_one(pool)
        .await
        .unwrap()
}

const OLD: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
const NEW: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

#[tokio::test]
async fn bytes_that_moved_unlink_the_pair() {
    // The whole reason this feed matters to RFC-006: a file retagged
    // outside the API keeps its identifier while its bytes move, so a link
    // proved by an exact hash goes on asserting two files are the same
    // when they are not.
    let pool = pool().await;
    link(&pool, OLD, "confirmed").await;

    let changed = mark_link_stale(&pool, "rt", Some(NEW)).await.unwrap();

    assert!(changed);
    assert_eq!(status_of(&pool).await, "stale");
}

#[tokio::test]
async fn bytes_that_agree_leave_the_link_alone() {
    let pool = pool().await;
    link(&pool, OLD, "confirmed").await;

    let changed = mark_link_stale(&pool, "rt", Some(OLD)).await.unwrap();

    assert!(!changed);
    assert_eq!(status_of(&pool).await, "confirmed");
}

#[tokio::test]
async fn an_event_without_a_hash_says_nothing_about_the_bytes() {
    // An upsert whose payload carries no hash is not evidence that the
    // file changed, and treating a missing field as a mismatch would
    // unlink every pair the first time the server omitted one.
    let pool = pool().await;
    link(&pool, OLD, "confirmed").await;

    let changed = mark_link_stale(&pool, "rt", None).await.unwrap();

    assert!(!changed);
    assert_eq!(status_of(&pool).await, "confirmed");
}

#[test]
fn the_feed_refuses_a_stale_cursor_as_conflict_not_as_expired() {
    // The trap this guard exists for: the sync journal answers
    // `cursor_expired` for the same situation, and a reader watching only
    // for that code would retry a permanent refusal forever.
    let conflict = RemoteFailure {
        kind: crate::remote::client::FailureKind::Permanent,
        status: Some(409),
        code: Some("conflict".into()),
        message: "refused".into(),
    };
    assert!(is_cursor_refused(&conflict));

    let elsewhere = RemoteFailure {
        kind: crate::remote::client::FailureKind::Transient,
        status: Some(503),
        code: None,
        message: "later".into(),
    };
    assert!(!is_cursor_refused(&elsewhere));
}

#[test]
fn a_payload_without_a_hash_reads_as_absent() {
    assert_eq!(hash_of(&serde_json::json!({})), None);
    assert_eq!(hash_of(&serde_json::json!({ "full_hash": 7 })), None);
    assert_eq!(
        hash_of(&serde_json::json!({ "full_hash": "abc" })),
        Some("abc")
    );
}
