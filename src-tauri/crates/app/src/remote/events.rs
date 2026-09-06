//! Keeping the mirror current between sweeps, from the server's own feed.
//!
//! ## The change the sweep structurally cannot see
//!
//! [`mirror`](super::mirror) skips an album whose `song_count` still matches,
//! and that is what makes a second walk cheap. It is also a blind spot with a
//! precise shape: a **correction** does not move the count. Somebody retitles
//! a track through `PATCH /tracks/{id}`, the album's count is unchanged, the
//! album is skipped, and the mirror keeps showing the old title until some
//! unrelated change forces a re-fetch. `GET /libraries/{id}/events` is what
//! sees those, so this module is not a faster sweep — it covers a class of
//! change the sweep was never able to report.
//!
//! ## And the one the reconciliation depends on
//!
//! A track upsert carries `full_hash`. Nothing else tells a client that a file
//! was retagged **outside** the API: the track keeps its identifier while its
//! bytes move, so an `exact_full_hash` link built on the old bytes stays
//! `confirmed` while being wrong. Reading the feed is what lets that link be
//! marked `stale` — RFC-006 named this feed for exactly that reason.
//!
//! ## Why a refused cursor is not an error
//!
//! The server purges old events and keeps a watermark of what it cut. A cursor
//! below it has missed events, and rather than hand back the surviving tail —
//! which would look like a successful catch-up while silently skipping the gap
//! — the server refuses. The answer is to forget the cursor and let the sweep
//! rebuild the mirror, which is what this application did before this module
//! existed. So the failure mode is "no faster than before", never "wrong".
//!
//! **The refusal arrives as `conflict`, not `cursor_expired`.** The sync
//! journal uses the second code for the same situation; this feed maps its
//! refusal through `ServiceError::Conflict` and answers the first. Reaching
//! for [`RemoteFailure::is_cursor_expired`] here would never match, and the
//! pass would treat a permanent refusal as a passing error and retry it
//! forever.
//!
//! [`RemoteFailure::is_cursor_expired`]: super::client::RemoteFailure::is_cursor_expired

use serde::Deserialize;
use sqlx::SqlitePool;

use crate::{
    error::{AppError, AppResult},
    remote::{
        client::{RemoteClient, RemoteFailure},
        mirror, projection,
    },
};

/// What can end one library's pass.
///
/// The two are kept apart because only one of them is a verdict about the
/// cursor. Folding a SQLite failure into a [`RemoteFailure`] would let a
/// local write error be read as a server refusal, and answered by throwing
/// away the mirror.
#[derive(Debug)]
enum PassError {
    Remote(RemoteFailure),
    Local(AppError),
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::Remote(failure) => write!(f, "{failure}"),
            PassError::Local(error) => write!(f, "{error}"),
        }
    }
}

impl From<RemoteFailure> for PassError {
    fn from(failure: RemoteFailure) -> Self {
        PassError::Remote(failure)
    }
}

impl From<AppError> for PassError {
    fn from(error: AppError) -> Self {
        PassError::Local(error)
    }
}

impl From<sqlx::Error> for PassError {
    fn from(error: sqlx::Error) -> Self {
        PassError::Local(error.into())
    }
}

/// Events asked for per request. The server caps pages itself; this only
/// bounds how much one round trip carries.
const PAGE: i64 = 500;

/// Pages read per library in one pass.
///
/// A bound rather than a policy: the pass runs again, and a device returning
/// after a long absence should not hold a profile lease across an entire
/// history. What it does not read stays behind its cursor and is read next
/// time.
const MAX_PAGES: usize = 20;

#[derive(Debug, Deserialize)]
struct EventPage {
    events: Vec<LibraryEvent>,
    has_more: bool,
}

/// One fact from a library's feed.
///
/// Only `id` and the discriminants are required: a field the server adds later
/// must not make a whole page undecodable.
#[derive(Debug, Deserialize)]
struct LibraryEvent {
    cursor: i64,
    entity_type: String,
    entity_id: String,
    action: String,
    #[serde(default)]
    payload: serde_json::Value,
    /// The device that asked for the change, when a client did. Absent when a
    /// scan wrote it.
    #[serde(default)]
    origin_device_id: Option<String>,
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventsReport {
    /// Events read and acted on.
    pub applied: usize,
    /// Events skipped because this device is what caused them.
    pub skipped_own: usize,
    /// Reconciliation links marked stale because the server's bytes moved.
    pub unlinked: usize,
    /// Libraries whose cursor the server refused, and which now wait for a
    /// full sweep.
    pub restarted: usize,
}

/// Read every mirrored library's feed and apply what it reports.
///
/// Libraries that have never been swept are skipped: they have no mirror to
/// keep current, and replaying their history would duplicate the walk that is
/// about to run anyway.
pub async fn catch_up(client: &RemoteClient<'_>, pool: &SqlitePool) -> AppResult<EventsReport> {
    let mut report = EventsReport::default();
    let libraries: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT remote_id, events_cursor FROM remote_library WHERE mirrored_at IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;

    for (library_id, cursor) in libraries {
        // A library swept before this feed was ever read starts at 0. That
        // replays the whole retained history, which is safe — every apply
        // below is an upsert or a delete keyed by identity — and on a server
        // that has purged, it is refused and falls back to the sweep.
        match catch_up_library(client, pool, &library_id, cursor.unwrap_or(0), &mut report).await {
            Ok(()) => {}
            Err(PassError::Remote(failure)) if is_cursor_refused(&failure) => {
                forget_cursor(pool, &library_id).await?;
                report.restarted += 1;
            }
            Err(error) => {
                // One library's feed being unreachable says nothing about the
                // next one's, and none of this is load-bearing: the sweep
                // still runs. Logged rather than propagated so a single
                // failure cannot cost the whole pass.
                tracing::debug!(
                    library = %library_id,
                    %error,
                    "could not read a library's change feed"
                );
            }
        }
    }
    Ok(report)
}

/// A refusal that will never succeed on retry, however long we wait.
///
/// The feed answers `conflict` where the sync journal answers
/// `cursor_expired`, so this reads the code the *server* sends rather than the
/// one the situation resembles.
fn is_cursor_refused(failure: &RemoteFailure) -> bool {
    failure.is_conflict() || failure.is_cursor_expired()
}

/// Forget where we were, so the next pass starts from scratch behind the
/// sweep.
///
/// The sweep date goes with it: a mirror that missed events is stale in ways
/// the cursor cannot describe, and leaving `mirrored_at` set would let the
/// incremental walk skip every album whose count happens to match.
async fn forget_cursor(pool: &SqlitePool, library_id: &str) -> AppResult<()> {
    sqlx::query(
        "UPDATE remote_library SET events_cursor = NULL, mirrored_at = NULL WHERE remote_id = ?",
    )
    .bind(library_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn catch_up_library(
    client: &RemoteClient<'_>,
    pool: &SqlitePool,
    library_id: &str,
    from: i64,
    report: &mut EventsReport,
) -> Result<(), PassError> {
    let mut cursor = from;
    for _ in 0..MAX_PAGES {
        let path = format!("/api/v2/libraries/{library_id}/events?after={cursor}&limit={PAGE}");
        let page: EventPage = client.send_json(client.get(&path)).await?;
        if page.events.is_empty() {
            break;
        }
        for event in &page.events {
            // Applied one at a time, and the cursor advances only behind an
            // event that landed. A fetch that fails mid-page leaves the rest
            // for the next pass instead of skipping it: the alternative is a
            // gap nothing can detect, since the cursor is the only record of
            // what was read.
            if let Err(error) = apply(client, pool, event, report).await {
                tracing::debug!(
                    library = %library_id,
                    cursor = event.cursor,
                    %error,
                    "stopping a feed pass on the event that failed"
                );
                break;
            }
            cursor = event.cursor;
        }
        save_cursor(pool, library_id, cursor).await?;
        if !page.has_more || cursor < page.events[page.events.len() - 1].cursor {
            // Either the server has nothing more, or an event failed and the
            // loop above stopped short of the page's end. Asking for another
            // page in the second case would read past what was applied.
            break;
        }
    }
    // Told after the writes, and only about what was written. The server uses
    // it to decide what it may purge, so claiming a cursor whose events were
    // not applied would authorise it to delete events this device still needs.
    acknowledge(client, library_id, cursor).await;
    Ok(())
}

async fn save_cursor(pool: &SqlitePool, library_id: &str, cursor: i64) -> AppResult<()> {
    // Never lowered. Two passes racing would otherwise let the slower one
    // rewind the faster one's position and replay events it already applied.
    sqlx::query(
        "UPDATE remote_library
            SET events_cursor = MAX(COALESCE(events_cursor, 0), ?)
          WHERE remote_id = ?",
    )
    .bind(cursor)
    .bind(library_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Tell the server how far this device has read.
///
/// Best-effort on purpose: the acknowledgement only informs the server's
/// retention policy. Failing to send it costs the server some disk, and costs
/// this device nothing — so it must never fail a pass whose writes already
/// committed.
async fn acknowledge(client: &RemoteClient<'_>, library_id: &str, cursor: i64) {
    let Some(device_id) = client.device_id() else {
        // A binding with no device identifier cannot acknowledge: the server
        // records the position per device, and there is no device to record.
        return;
    };
    let path = format!("/api/v2/libraries/{library_id}/events/ack");
    let request = client
        .request(reqwest::Method::PUT, &path)
        .json(&serde_json::json!({ "device_id": device_id, "cursor": cursor }));
    if let Err(failure) = client.send_ok(request).await {
        tracing::debug!(library = %library_id, cursor, %failure, "could not acknowledge a feed");
    }
}

/// Act on one event.
async fn apply(
    client: &RemoteClient<'_>,
    pool: &SqlitePool,
    event: &LibraryEvent,
    report: &mut EventsReport,
) -> Result<(), PassError> {
    // Our own writes come back through the feed, and the drain has already
    // applied them from the reply — which is the better source, since it is
    // the state the server settled on rather than a notification that it
    // changed. Skipping them also stops an upload from arriving as a track
    // this device just discovered.
    if event.origin_device_id.is_some() && event.origin_device_id.as_deref() == client.device_id() {
        report.skipped_own += 1;
        return Ok(());
    }

    match (event.entity_type.as_str(), event.action.as_str()) {
        ("track", "upsert") => {
            // The payload carries `full_hash` and nothing else, so the row
            // itself has to be re-read. Compared *before* the fetch: the point
            // is whether the bytes moved since the link was proved, and the
            // fetch cannot change that answer.
            let unlinked = mark_link_stale(pool, &event.entity_id, hash_of(&event.payload)).await?;
            report.unlinked += usize::from(unlinked);

            let path = format!("/api/v2/tracks/{}", event.entity_id);
            let song: super::dto::SongItem = client.send_json(client.get(&path)).await?;
            let mut conn = pool.acquire().await?;
            projection::cache_song(&mut conn, &song).await?;
            report.applied += 1;
        }
        ("track", "delete") => {
            let mut conn = pool.acquire().await?;
            mirror::forget_track(&mut conn, &event.entity_id).await?;
            report.applied += 1;
        }
        ("album", _) => {
            // Nothing is fetched here. An album event means the walk's
            // freshness check can no longer be trusted for it, and the cheapest
            // truthful answer is to say so and let the walk re-read it — the
            // same invalidation the mirror does when a count changes.
            sqlx::query("UPDATE remote_album SET mirrored_at = NULL WHERE remote_id = ?")
                .bind(&event.entity_id)
                .execute(pool)
                .await?;
            report.applied += 1;
        }
        _ => {
            // `artist` is in the feed's vocabulary but nothing emits it today,
            // and an action this build does not know is a server that grew a
            // verb. Counting it as applied is correct: the cursor moves past
            // it, and a client that stalled on every unrecognised event would
            // stop reading the feed the first time the server learnt a new
            // word.
            report.applied += 1;
        }
    }
    Ok(())
}

/// The `full_hash` a track upsert carries, if it carries one.
fn hash_of(payload: &serde_json::Value) -> Option<&str> {
    payload.get("full_hash").and_then(serde_json::Value::as_str)
}

/// Mark the reconciliation link stale when the server's bytes have moved.
///
/// A link proved by an exact hash is a claim about **bytes**, and a retag
/// outside the API changes them while the track keeps its identifier. Left
/// alone the link stays `confirmed` and goes on asserting that a local file
/// and a server track are the same file when they are no longer.
///
/// Only ever downgrades. A hash that matches proves the link is still good,
/// but a link marked stale for some other reason is not re-confirmed here:
/// this event says the bytes agree, not that everything else does.
async fn mark_link_stale(
    pool: &SqlitePool,
    remote_track_id: &str,
    full_hash: Option<&str>,
) -> AppResult<bool> {
    let Some(full_hash) = full_hash else {
        return Ok(false);
    };
    let affected = sqlx::query(
        "UPDATE remote_track_link
            SET status = 'stale'
          WHERE remote_track_id = ?
            AND status = 'confirmed'
            AND verified_full_hash IS NOT NULL
            AND verified_full_hash != ?",
    )
    .bind(remote_track_id)
    .bind(full_hash)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected > 0)
}

#[cfg(test)]
mod tests {
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

    #[tokio::test]
    async fn forgetting_a_cursor_also_forgets_the_sweep() {
        // A mirror that missed events is stale in ways the cursor cannot
        // describe. Leaving `mirrored_at` set would let the incremental walk
        // skip every album whose count happens to match, and the gap would
        // never be closed.
        let pool = pool().await;
        library(&pool, Some(99)).await;

        forget_cursor(&pool, "lib").await.unwrap();

        assert_eq!(cursor_of(&pool).await, None);
        let swept: Option<i64> =
            sqlx::query_scalar("SELECT mirrored_at FROM remote_library WHERE remote_id = 'lib'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(swept, None, "the sweep date outlived the cursor");
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
}
