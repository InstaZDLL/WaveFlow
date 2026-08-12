//! Reading the projection back out, for the UI.
//!
//! Pure queries over the `remote_*` tables — no HTTP, no client. What
//! the projection holds is already the answer; nothing here needs the
//! server to be reachable, which is the point of keeping a local
//! projection at all.
//!
//! ## Tracks may be identifiers with no metadata yet
//!
//! A playlist that arrived through `/sync/changes` carries bare track
//! identifiers until the backfill fetches them. Rather than hide those
//! rows or block on a fetch, the queries below return them with
//! `title: None`, so the caller can render a placeholder in the right
//! position instead of a playlist that is silently short a few entries.

use serde::Serialize;
use sqlx::{Row, SqliteConnection};

use crate::error::AppResult;

/// Enough to tell, at a glance, whether synchronization is working.
#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct RemoteOverview {
    pub playlists: i64,
    pub favorites: i64,
    pub ratings: i64,
    pub history: i64,
    pub shares: i64,
    /// Tracks in the saved queue, if the account has one.
    pub queue_tracks: i64,
    /// Cached track metadata rows.
    pub cached_tracks: i64,
    /// Identifiers the projection references but has no metadata for.
    /// A non-zero value is normal right after a catch-up and should
    /// fall back to zero on the next pass.
    pub tracks_awaiting_metadata: i64,
    /// Local changes not yet accepted by the server.
    pub pending_changes: i64,
    /// Local changes the server refused permanently. These need a
    /// human: they will never be retried.
    pub failed_changes: i64,
}

/// A playlist as the list view needs it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemotePlaylistSummary {
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub is_public: bool,
    pub track_count: i64,
    /// Total duration of the tracks we have metadata for. Understates
    /// while a backfill is outstanding, which is better than refusing
    /// to show a number at all.
    pub duration_ms: i64,
    /// `true` while this playlist exists only here — created offline,
    /// its creation still queued. It has no server identifier yet.
    pub pending_creation: bool,
}

/// One entry of a playlist, in position order.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RemoteTrack {
    pub id: String,
    /// `None` when the identifier arrived through a change event and
    /// the metadata has not been fetched yet.
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<i64>,
    pub artwork_hash: Option<String>,
}

/// Count everything worth counting, in one round-trip.
pub async fn overview(conn: &mut SqliteConnection) -> AppResult<RemoteOverview> {
    let row = sqlx::query(
        "SELECT
            (SELECT count(*) FROM remote_playlist)      AS playlists,
            (SELECT count(*) FROM remote_favorite)      AS favorites,
            (SELECT count(*) FROM remote_rating)        AS ratings,
            (SELECT count(*) FROM remote_history)       AS history,
            (SELECT count(*) FROM remote_share)         AS shares,
            (SELECT count(*) FROM remote_queue_track)   AS queue_tracks,
            (SELECT count(*) FROM remote_track)         AS cached_tracks,
            (SELECT count(*) FROM remote_mutation WHERE failed_at IS NULL)     AS pending_changes,
            (SELECT count(*) FROM remote_mutation WHERE failed_at IS NOT NULL) AS failed_changes",
    )
    .fetch_one(&mut *conn)
    .await?;

    Ok(RemoteOverview {
        playlists: row.try_get("playlists")?,
        favorites: row.try_get("favorites")?,
        ratings: row.try_get("ratings")?,
        history: row.try_get("history")?,
        shares: row.try_get("shares")?,
        queue_tracks: row.try_get("queue_tracks")?,
        cached_tracks: row.try_get("cached_tracks")?,
        tracks_awaiting_metadata: super::projection::missing_track_ids(&mut *conn)
            .await?
            .len() as i64,
        pending_changes: row.try_get("pending_changes")?,
        failed_changes: row.try_get("failed_changes")?,
    })
}

/// Every projected playlist, newest activity first.
pub async fn playlists(conn: &mut SqliteConnection) -> AppResult<Vec<RemotePlaylistSummary>> {
    let rows = sqlx::query(
        "SELECT p.remote_id, p.name, p.comment, p.is_public,
                (SELECT count(*) FROM remote_playlist_track t
                  WHERE t.playlist_remote_id = p.remote_id) AS track_count,
                COALESCE((SELECT SUM(rt.duration_ms)
                            FROM remote_playlist_track t
                            JOIN remote_track rt ON rt.remote_id = t.track_remote_id
                           WHERE t.playlist_remote_id = p.remote_id), 0) AS duration_ms
           FROM remote_playlist p
          ORDER BY COALESCE(p.updated_at, p.created_at, 0) DESC, p.name",
    )
    .fetch_all(&mut *conn)
    .await?;

    rows.into_iter()
        .map(|row| {
            let id: String = row.try_get("remote_id")?;
            Ok(RemotePlaylistSummary {
                pending_creation: super::mutation::is_placeholder(&id),
                id,
                name: row.try_get("name")?,
                comment: row.try_get("comment")?,
                is_public: row.try_get::<i64, _>("is_public")? != 0,
                track_count: row.try_get("track_count")?,
                duration_ms: row.try_get("duration_ms")?,
            })
        })
        .collect()
}

/// A playlist's tracks, in position order, placeholders included.
///
/// The join is a LEFT JOIN on purpose: an identifier without metadata
/// still occupies its position. Dropping it would renumber everything
/// after it and make a playlist look shorter than it is.
pub async fn playlist_tracks(
    conn: &mut SqliteConnection,
    playlist_id: &str,
) -> AppResult<Vec<RemoteTrack>> {
    let rows = sqlx::query(
        "SELECT t.track_remote_id, rt.title, rt.artist, rt.album, rt.duration_ms, rt.artwork_hash
           FROM remote_playlist_track t
           LEFT JOIN remote_track rt ON rt.remote_id = t.track_remote_id
          WHERE t.playlist_remote_id = ?
          ORDER BY t.position",
    )
    .bind(playlist_id)
    .fetch_all(&mut *conn)
    .await?;

    rows.into_iter().map(track_from_row).collect()
}

/// The account's saved queue, in order.
pub async fn queue_tracks(conn: &mut SqliteConnection) -> AppResult<Vec<RemoteTrack>> {
    let rows = sqlx::query(
        "SELECT q.track_remote_id, rt.title, rt.artist, rt.album, rt.duration_ms, rt.artwork_hash
           FROM remote_queue_track q
           LEFT JOIN remote_track rt ON rt.remote_id = q.track_remote_id
          ORDER BY q.position",
    )
    .fetch_all(&mut *conn)
    .await?;

    rows.into_iter().map(track_from_row).collect()
}

fn track_from_row(row: sqlx::sqlite::SqliteRow) -> AppResult<RemoteTrack> {
    Ok(RemoteTrack {
        id: row.try_get("track_remote_id")?,
        title: row.try_get("title")?,
        artist: row.try_get("artist")?,
        album: row.try_get("album")?,
        duration_ms: row.try_get("duration_ms")?,
        artwork_hash: row.try_get("artwork_hash")?,
    })
}

#[cfg(test)]
mod tests {
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
        for migration in [
            include_str!(
                "../../../../migrations/profile/20260810120000_remote_source_projection.sql"
            ),
            include_str!("../../../../migrations/profile/20260810140000_remote_track_cache.sql"),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        pool
    }

    /// The same capture the projection tests replay: a real server's
    /// snapshot of a seeded library.
    const REAL_SNAPSHOT: &str = include_str!("testdata/snapshot.json");

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
        assert_eq!(overview(&mut conn).await.unwrap(), RemoteOverview::default());
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
}
