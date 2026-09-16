//! Fetching an album's tags from Deezer, for review (#599).
//!
//! Deezer enrichment already fills artwork and artist pages; this is
//! the other half — "these tags are wrong, fetch them and let me
//! approve the result". It is two steps, deliberately:
//!
//! 1. [`search_album_tag_sources`] offers the catalogue albums that
//!    might be this record. Choosing is the user's, because a title
//!    and an artist name match several releases of the same album —
//!    an original, a remaster, a deluxe edition with four more tracks
//!    — and they carry different track lists.
//! 2. [`fetch_album_tag_proposals`] pairs the chosen release's tracks
//!    with the local files through
//!    [`waveflow_core::metadata::album_match`] and hands back both
//!    sides, field by field.
//!
//! # Nothing here writes
//!
//! No command in this module touches a file or a row. The proposals go
//! to the review screen, and what the user accepts is applied through
//! [`crate::commands::edit::update_track_tags`] — the path that pauses
//! playback before opening the file, writes through the concrete tag
//! so non-standard frames survive, re-hashes, and relinks the album and
//! artist rows. Writing across a whole album is exactly where a second,
//! simpler write path would turn one bad moment into a folder in an
//! unknown state.
//!
//! # What Deezer cannot fill
//!
//! No composer, and no genre at track level. Neither is offered: a
//! review screen that lists a field the source cannot fill invites the
//! user to accept a blank over something they typed themselves.
//! `disk_number` is returned by the API but is unreliable on box sets,
//! so it is not offered either — the field it would overwrite is
//! usually right where the catalogue's is wrong.

use serde::Serialize;
use sqlx::SqlitePool;
use waveflow_core::metadata::{
    album_match::{self, Confidence, TrackSignals},
    deezer::DeezerClient,
};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// How many catalogue releases to offer for one local album.
///
/// Enough that a remaster and a deluxe edition both appear, few enough
/// that the choice stays a choice.
const MAX_SOURCES: usize = 8;

/// One catalogue release that might be this record.
#[derive(Debug, Serialize)]
pub struct AlbumSource {
    pub deezer_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub track_count: Option<i64>,
    pub year: Option<i64>,
    pub cover_url: Option<String>,
}

#[tauri::command]
pub async fn search_album_tag_sources(
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<Vec<AlbumSource>> {
    // Before the reads, as in `fetch_album_tag_proposals`: everything
    // this command does afterwards is in service of a network call it
    // is not going to make.
    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on — turn it off to fetch tags".into(),
        ));
    }

    let pool = state.require_profile_pool().await?;
    let (title, artist) = album_identity(&pool, album_id).await?;

    let query = match artist.as_deref() {
        Some(artist) => format!("{title} {artist}"),
        None => title.clone(),
    };
    let client = DeezerClient::new();
    let hits = client
        .search_album(&query)
        .await
        .map_err(|e| AppError::Other(format!("Deezer album search failed: {e}")))?;

    Ok(hits
        .into_iter()
        .take(MAX_SOURCES)
        .map(|hit| AlbumSource {
            deezer_id: hit.id,
            title: hit.title,
            artist: hit.artist.map(|a| a.name),
            track_count: hit.nb_tracks,
            // `release_date` is `YYYY-MM-DD`; only the year is worth
            // offering, and a malformed one is dropped rather than
            // guessed at.
            year: hit
                .release_date
                .as_deref()
                .and_then(|d| d.get(0..4))
                .and_then(|y| y.parse::<i64>().ok()),
            cover_url: hit.cover_medium.or(hit.cover_big),
        })
        .collect())
}

/// The values a tag fetch can offer for one track.
///
/// Every field is optional on both sides: the local file may not carry
/// it, and the catalogue may not either. The review screen compares
/// them field by field, so it needs the pair rather than a diff.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TagValues {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
}

/// One local file, with what the catalogue says about it.
#[derive(Debug, Serialize)]
pub struct TrackProposal {
    pub track_id: i64,
    /// Shown for a track the matcher could not pair, where the title
    /// alone would not tell the user which file is meant.
    pub file_name: String,
    pub current: TagValues,
    /// `None` when nothing in the release matched this file well
    /// enough — a real answer, and the reason the screen must not
    /// silently apply "the best available".
    pub fetched: Option<TagValues>,
    pub score: Option<f64>,
    pub confidence: Option<Confidence>,
}

#[derive(Debug, Serialize)]
pub struct AlbumProposals {
    pub album_id: i64,
    pub deezer_id: i64,
    /// Tracks in the album's own order, matched or not.
    pub tracks: Vec<TrackProposal>,
    /// Catalogue tracks no local file claimed — a deluxe edition's
    /// extras, or the songs a partial rip is missing.
    pub unmatched_remote: Vec<String>,
}

#[tauri::command]
pub async fn fetch_album_tag_proposals(
    state: tauri::State<'_, AppState>,
    album_id: i64,
    deezer_album_id: i64,
) -> AppResult<AlbumProposals> {
    let pool = state.require_profile_pool().await?;

    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on — turn it off to fetch tags".into(),
        ));
    }

    let locals = local_tracks(&pool, album_id).await?;
    if locals.is_empty() {
        return Err(AppError::Other(format!("album {album_id} has no tracks")));
    }

    let client = DeezerClient::new();
    let album = client
        .get_album(deezer_album_id)
        .await
        .map_err(|e| AppError::Other(format!("Deezer album fetch failed: {e}")))?;
    let remote_tracks = client
        .get_album_tracks(deezer_album_id)
        .await
        .map_err(|e| AppError::Other(format!("Deezer track listing failed: {e}")))?;
    if remote_tracks.is_empty() {
        return Err(AppError::Other(
            "this release has no track listing on Deezer".into(),
        ));
    }

    let album_year = album
        .release_date
        .as_deref()
        .and_then(|d| d.get(0..4))
        .and_then(|y| y.parse::<i64>().ok());
    let album_title = album.title.clone();

    let local_signals: Vec<TrackSignals> = locals
        .iter()
        .map(|l| TrackSignals {
            title: l.title.clone(),
            duration_ms: Some(l.duration_ms),
            track_number: l.track_number,
            disc_number: l.disc_number,
        })
        .collect();
    let remote_signals: Vec<TrackSignals> = remote_tracks
        .iter()
        .map(|r| TrackSignals {
            title: r.title.clone(),
            duration_ms: r.duration_ms(),
            track_number: r.track_position,
            // Read for matching only. It is still not offered as a
            // value to write: the catalogue's disc numbers are
            // unreliable on box sets, which is where the local ones are
            // usually right — and a signal that costs 0.15 when it
            // disagrees is a different risk from a value that
            // overwrites a correct field.
            disc_number: r.disk_number,
        })
        .collect();

    let assignments = album_match::assign(&local_signals, &remote_signals);
    // Indexed by local position so the album's order is preserved
    // whatever order the matcher settled the pairs in.
    let mut by_local: std::collections::HashMap<usize, &album_match::Assignment> =
        std::collections::HashMap::new();
    for a in &assignments {
        by_local.insert(a.local, a);
    }
    let claimed: std::collections::HashSet<usize> = assignments.iter().map(|a| a.remote).collect();

    let tracks = locals
        .iter()
        .enumerate()
        .map(|(idx, local)| {
            let matched = by_local.get(&idx);
            TrackProposal {
                track_id: local.track_id,
                file_name: file_name_of(&local.file_path),
                current: TagValues {
                    title: Some(local.title.clone()),
                    artist: local.artist.clone(),
                    album: local.album.clone(),
                    year: local.year,
                    track_number: local.track_number,
                },
                fetched: matched.map(|a| {
                    let r = &remote_tracks[a.remote];
                    TagValues {
                        title: Some(r.title.clone()),
                        // The track's own credit where the catalogue
                        // gives one: a compilation's tracks are not all
                        // by the album's artist, and taking the album's
                        // would rewrite every guest credit on it.
                        artist: r.artist.as_ref().map(|a| a.name.clone()),
                        album: Some(album_title.clone()),
                        year: album_year,
                        track_number: r.track_position,
                    }
                }),
                score: matched.map(|a| a.score),
                confidence: matched.map(|a| a.confidence),
            }
        })
        .collect();

    let unmatched_remote = remote_tracks
        .iter()
        .enumerate()
        .filter(|(i, _)| !claimed.contains(i))
        .map(|(_, r)| r.title.clone())
        .collect();

    Ok(AlbumProposals {
        album_id,
        deezer_id: deezer_album_id,
        tracks,
        unmatched_remote,
    })
}

// ── Reads ───────────────────────────────────────────────────────────

/// The album's title and its artist's name, for the search query.
async fn album_identity(pool: &SqlitePool, album_id: i64) -> AppResult<(String, Option<String>)> {
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT al.title, ar.name
           FROM album al
           LEFT JOIN artist ar ON ar.id = al.artist_id
          WHERE al.id = ?",
    )
    .bind(album_id)
    .fetch_optional(pool)
    .await?;
    row.ok_or_else(|| AppError::Other(format!("album {album_id} not found")))
}

struct LocalTrack {
    track_id: i64,
    file_path: String,
    title: String,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    year: Option<i64>,
    artist: Option<String>,
    album: Option<String>,
}

/// The album's tracks, in the order the record plays.
///
/// The artist credit is rebuilt from `track_artist` with `"; "`, the
/// library's one spelling for a multi-artist credit — reading
/// `track.primary_artist` instead would offer to replace a full credit
/// with its first name, which is a silent loss dressed as a fix.
///
/// The ordering goes in an **inner subquery**, the shape the rest of
/// the codebase uses: an `ORDER BY` in the aggregate's own query runs
/// after the aggregation and orders one row, so the names inside the
/// string come out in whatever order the scan happened to reach them.
/// Ordering inside the aggregate call is SQLite 3.44, newer than what
/// we can require.
async fn local_tracks(pool: &SqlitePool, album_id: i64) -> AppResult<Vec<LocalTrack>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: i64,
        file_path: String,
        title: String,
        duration_ms: i64,
        track_number: Option<i64>,
        disc_number: Option<i64>,
        year: Option<i64>,
        artists: Option<String>,
        album: Option<String>,
    }

    let rows: Vec<Row> = sqlx::query_as::<_, Row>(
        r#"
        SELECT t.id            AS id,
               t.file_path     AS file_path,
               t.title         AS title,
               t.duration_ms   AS duration_ms,
               t.track_number  AS track_number,
               t.disc_number   AS disc_number,
               t.year          AS year,
               (SELECT GROUP_CONCAT(name, '; ') FROM (
                   SELECT ar.name AS name
                     FROM track_artist ta
                     JOIN artist ar ON ar.id = ta.artist_id
                    WHERE ta.track_id = t.id
                    ORDER BY ta.position
               )) AS artists,
               al.title        AS album
          FROM track t
          LEFT JOIN album al ON al.id = t.album_id
         WHERE t.album_id = ?
           AND t.is_available = 1
         ORDER BY COALESCE(t.disc_number, 1), COALESCE(t.track_number, 9999), t.title
        "#,
    )
    .bind(album_id)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| LocalTrack {
            track_id: r.id,
            file_path: r.file_path,
            title: r.title,
            duration_ms: r.duration_ms,
            track_number: r.track_number,
            disc_number: r.disc_number,
            year: r.year,
            artist: r.artists,
            album: r.album,
        })
        .collect())
}

/// The last path segment, whichever separator the scanning OS used.
fn file_name_of(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    #[test]
    fn file_name_handles_both_separators() {
        assert_eq!(file_name_of(r"E:\Music\a\b.flac"), "b.flac");
        assert_eq!(file_name_of("/home/u/Music/a/b.flac"), "b.flac");
        assert_eq!(file_name_of("bare.flac"), "bare.flac");
    }

    /// The repo's own profile migrations, with `foreign_keys` on.
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

    /// Both reads run against the real schema, and the credit comes
    /// back in `track_artist.position` order.
    ///
    /// Neither query is compile-time checked, and the credit is built
    /// by a correlated `GROUP_CONCAT` over an ordered subquery — the
    /// shape that is easy to write in a way that compiles, runs, and
    /// quietly returns the names in whatever order the scan reached
    /// them.
    #[tokio::test]
    async fn the_reads_run_and_the_credit_keeps_its_order() {
        let pool = pool().await;
        sqlx::raw_sql(
            "INSERT INTO library (id, name, created_at, updated_at)
                  VALUES (1, 'l', 0, 0);
             INSERT INTO artist (id, name, canonical_name)
                  VALUES (1, 'Second', 'second'), (2, 'First', 'first');
             INSERT INTO album (id, title, canonical_title, artist_id)
                  VALUES (1, 'Record', 'record', 2);
             INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                file_modified, title, duration_ms, added_at,
                                album_id, track_number, year)
                  VALUES (1, 1, '/l/a.flac', 'h1', 1, 0, 'Song', 200000, 0, 1, 1, 1999);
             -- Inserted out of order on purpose: position decides, not
             -- the insertion order and not the artist id.
             INSERT INTO track_artist (track_id, artist_id, position)
                  VALUES (1, 1, 1), (1, 2, 0);",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (title, artist) = album_identity(&pool, 1).await.unwrap();
        assert_eq!(title, "Record");
        assert_eq!(artist.as_deref(), Some("First"));

        let tracks = local_tracks(&pool, 1).await.unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].artist.as_deref(), Some("First; Second"));
        assert_eq!(tracks[0].album.as_deref(), Some("Record"));
        assert_eq!(tracks[0].year, Some(1999));
    }

    /// An album nobody has is an error, not an empty answer: the
    /// review screen would otherwise open on nothing and say nothing.
    #[tokio::test]
    async fn a_missing_album_is_an_error() {
        let pool = pool().await;
        assert!(album_identity(&pool, 404).await.is_err());
    }
}
