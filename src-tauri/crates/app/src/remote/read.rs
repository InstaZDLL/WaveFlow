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
    /// Server id of the primary artist, for navigating to a remote artist
    /// view. `None` when the server has no primary artist for the track.
    pub artist_id: Option<String>,
    pub album: Option<String>,
    /// Server id of the album, for navigating to a remote album view.
    pub album_id: Option<String>,
    pub duration_ms: Option<i64>,
    pub artwork_hash: Option<String>,
    /// Whether this track is starred, from the projected `remote_favorite`
    /// table (the synced source of truth, including pending local changes).
    pub starred: bool,
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
        "SELECT t.track_remote_id, rt.title, rt.artist, rt.artist_id, rt.album,
                rt.album_id, rt.duration_ms, rt.artwork_hash,
                EXISTS(SELECT 1 FROM remote_favorite f
                        WHERE f.entity_type = 'track' AND f.entity_id = t.track_remote_id) AS starred
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
        "SELECT q.track_remote_id, rt.title, rt.artist, rt.artist_id, rt.album,
                rt.album_id, rt.duration_ms, rt.artwork_hash,
                EXISTS(SELECT 1 FROM remote_favorite f
                        WHERE f.entity_type = 'track' AND f.entity_id = q.track_remote_id) AS starred
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
        artist_id: row.try_get("artist_id")?,
        album: row.try_get("album")?,
        album_id: row.try_get("album_id")?,
        duration_ms: row.try_get("duration_ms")?,
        artwork_hash: row.try_get("artwork_hash")?,
        starred: row.try_get::<i64, _>("starred")? != 0,
    })
}

/// The fields a server track's tag editor can correct.
///
/// Its own read rather than fields on the unified track row: the genre
/// is the reason. Locally a genre lives in `track_genre` and never on
/// the track row, so carrying it on the shared row would mean either an
/// extra join on every library listing or a column only half the rows
/// could fill. The editor needs it once, when it opens.
#[derive(Debug, Serialize)]
pub struct RemoteTrackTags {
    pub title: String,
    pub artist: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
}

/// What the mirror holds for one server track, or `None` when it holds
/// nothing — a track known only as an identifier has no form to fill.
pub async fn track_tags(
    conn: &mut SqliteConnection,
    track_id: &str,
) -> AppResult<Option<RemoteTrackTags>> {
    let row = sqlx::query(
        "SELECT title, artist, genre, year, track_no, disc_no
           FROM remote_track
          WHERE remote_id = ?",
    )
    .bind(track_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    Ok(Some(RemoteTrackTags {
        title: row.try_get("title")?,
        artist: row.try_get("artist")?,
        genre: row.try_get("genre")?,
        year: row.try_get("year")?,
        track_number: row.try_get("track_no")?,
        disc_number: row.try_get("disc_no")?,
    }))
}

#[cfg(test)]
mod tests;
