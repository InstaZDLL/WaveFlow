//! The engine's database half: what is waiting, what was decided, and
//! what a decision filled.
//!
//! # Fill the gaps, never overwrite
//!
//! A value the library already has — from the file's tags or typed in
//! the tag editor — is the user's, and the engine does not replace it.
//! Every write here is conditional on the field being empty (`year IS
//! NULL`, a track with no genre row, an album with no cover), and what
//! was written is recorded beside the decision (`filled_*`,
//! `musicbrainz_filled_genre`) so an undo takes back exactly that.
//!
//! # Surviving the scanner
//!
//! The scanner rewrites a track's row, and empties and refills its
//! genres, whenever the file changes — and a rating or a tag WaveFlow
//! writes changes the file. A file without a genre tag therefore loses
//! the engine's genre on the next scan. [`reapply_all`] fills the gaps
//! again from what the decisions stored, with no network, and the app
//! runs it at the start of every pass.
//!
//! Every function takes a connection, so the caller owns the
//! transaction — the single-writer rule of the rest of the codebase.

use sqlx::SqliteConnection;

use crate::error::CoreResult;
use crate::metadata::album_match::TrackSignals;
use crate::scanner::upserts::upsert_genre;

use super::{AlbumFacts, ArtistFacts, ArtistProposal, LocalAlbum, LocalTrack, MatchedTrack};

/// What the engine may fill. Off means the field is never written,
/// automatically or on review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Fields {
    pub genres: bool,
    pub dates: bool,
    pub covers: bool,
    pub credits: bool,
}

impl Default for Fields {
    fn default() -> Self {
        Self {
            genres: true,
            dates: true,
            covers: true,
            credits: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Applied,
    Review,
    Rejected,
    NoMatch,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Review => "review",
            Self::Rejected => "rejected",
            Self::NoMatch => "no_match",
        }
    }
}

/// The value `album.artwork_source` carries for a cover the engine
/// fetched. `artwork.source`'s CHECK predates the archive and cannot be
/// widened (the table is a parent); provenance lives on the link since
/// #401, and this is what the link says.
pub const COVER_SOURCE: &str = "coverartarchive";

/// Albums with no decision yet, and `no_match` decisions older than
/// `no_match_before` (a catalogue grows; an album unknown last month may
/// be known now). Only albums with an available track: a decision about
/// files that are gone helps nobody.
///
/// Read in id order from `after_id` on, so a pass walks forward: an
/// album whose lookup failed stays pending for the next pass without
/// being handed back to this one.
pub async fn pending_albums(
    conn: &mut SqliteConnection,
    no_match_before: i64,
    after_id: i64,
    limit: i64,
) -> CoreResult<Vec<LocalAlbum>> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT al.id
           FROM album al
           LEFT JOIN musicbrainz_album m ON m.album_id = al.id
          WHERE (m.album_id IS NULL
                 OR (m.status = 'no_match' AND m.matched_at < ?))
            AND al.id > ?
            AND EXISTS (SELECT 1 FROM track t
                         WHERE t.album_id = al.id AND t.is_available = 1)
          ORDER BY al.id
          LIMIT ?",
    )
    .bind(no_match_before)
    .bind(after_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(album) = load_album(conn, id).await? {
            out.push(album);
        }
    }
    Ok(out)
}

/// How many albums [`pending_albums`] would return without a limit.
pub async fn count_pending_albums(
    conn: &mut SqliteConnection,
    no_match_before: i64,
) -> CoreResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM album al
           LEFT JOIN musicbrainz_album m ON m.album_id = al.id
          WHERE (m.album_id IS NULL
                 OR (m.status = 'no_match' AND m.matched_at < ?))
            AND EXISTS (SELECT 1 FROM track t
                         WHERE t.album_id = al.id AND t.is_available = 1)",
    )
    .bind(no_match_before)
    .fetch_one(&mut *conn)
    .await?)
}

/// One album and its available tracks, in disc then track order.
pub async fn load_album(
    conn: &mut SqliteConnection,
    album_id: i64,
) -> CoreResult<Option<LocalAlbum>> {
    let head: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT al.title, COALESCE(al.album_artist, ar.name)
           FROM album al LEFT JOIN artist ar ON ar.id = al.artist_id
          WHERE al.id = ?",
    )
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((title, artist)) = head else {
        return Ok(None);
    };
    type Row = (i64, String, i64, Option<i64>, Option<i64>, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, title, duration_ms, track_number, disc_number, file_path
           FROM track
          WHERE album_id = ? AND is_available = 1
          ORDER BY COALESCE(disc_number, 1), COALESCE(track_number, 2147483647), file_path",
    )
    .bind(album_id)
    .fetch_all(&mut *conn)
    .await?;
    let tracks = rows
        .into_iter()
        .map(
            |(track_id, title, duration, number, disc, file_path)| LocalTrack {
                track_id,
                signals: TrackSignals {
                    title,
                    // A zero length is "not measured", not a zero-second song.
                    duration_ms: (duration > 0).then_some(duration),
                    track_number: number,
                    disc_number: disc,
                },
                file_path,
            },
        )
        .collect();
    Ok(Some(LocalAlbum {
        album_id,
        title,
        artist,
        tracks,
    }))
}

/// What [`apply_album`] filled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub year: bool,
    pub release_date: bool,
    /// Tracks that received a year.
    pub track_years: u64,
    /// Tracks that received genres.
    pub genre_tracks: u64,
    /// The album has no cover and the covers field is on: the caller
    /// fetches one and links it with [`link_cover`].
    pub wants_cover: bool,
}

/// Write a decision with its evidence. An `Applied` decision also stores
/// the pairing and the credits, and fills the gaps (see [`apply_album`]);
/// a `Review` one stores only what the review screen shows — accepting
/// it looks the release up again, so nothing stale is applied.
pub async fn record_match(
    conn: &mut SqliteConnection,
    album_id: i64,
    status: Status,
    score: f64,
    facts: &AlbumFacts,
    tracks: &[MatchedTrack],
    fields: Fields,
    now: i64,
) -> CoreResult<ApplyReport> {
    // A decision replaced by another starts from nothing the old one
    // filled: its values are taken back while its provenance still says
    // which they are, then its pairing and credits go.
    let previous: Option<String> =
        sqlx::query_scalar("SELECT status FROM musicbrainz_album WHERE album_id = ?")
            .bind(album_id)
            .fetch_optional(&mut *conn)
            .await?;
    if previous.as_deref() == Some(Status::Applied.as_str()) {
        undo_album(conn, album_id, now).await?;
    }
    clear_pairing(conn, album_id).await?;
    let genres = serde_json::to_string(&facts.genres).unwrap_or_else(|_| "[]".into());
    sqlx::query(
        "INSERT INTO musicbrainz_album
            (album_id, status, score, release_mbid, release_group_mbid,
             release_title, release_artist, release_country, release_track_count,
             original_date, original_year, genres, has_front_cover,
             filled_year, filled_release_date, filled_cover, matched_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, ?)
         ON CONFLICT(album_id) DO UPDATE SET
            status = excluded.status,
            score = excluded.score,
            release_mbid = excluded.release_mbid,
            release_group_mbid = excluded.release_group_mbid,
            release_title = excluded.release_title,
            release_artist = excluded.release_artist,
            release_country = excluded.release_country,
            release_track_count = excluded.release_track_count,
            original_date = excluded.original_date,
            original_year = excluded.original_year,
            genres = excluded.genres,
            has_front_cover = excluded.has_front_cover,
            filled_year = 0,
            filled_release_date = 0,
            filled_cover = 0,
            matched_at = excluded.matched_at",
    )
    .bind(album_id)
    .bind(status.as_str())
    .bind(score)
    .bind(&facts.release_mbid)
    .bind(&facts.release_group_mbid)
    .bind(&facts.release_title)
    .bind(&facts.release_artist)
    .bind(&facts.release_country)
    .bind(facts.release_track_count)
    .bind(&facts.original_date)
    .bind(facts.original_year)
    .bind(&genres)
    .bind(facts.has_front_cover)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    if status != Status::Applied {
        return Ok(ApplyReport::default());
    }
    for track in tracks {
        sqlx::query(
            "INSERT INTO musicbrainz_track (track_id, album_id, recording_mbid, score)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(track_id) DO UPDATE SET
                album_id = excluded.album_id,
                recording_mbid = excluded.recording_mbid,
                score = excluded.score,
                filled_year = 0",
        )
        .bind(track.track_id)
        .bind(album_id)
        .bind(&track.recording_mbid)
        .bind(track.score)
        .execute(&mut *conn)
        .await?;
        if fields.credits {
            for (position, credit) in track.credits.iter().enumerate() {
                sqlx::query(
                    "INSERT OR IGNORE INTO track_credit
                        (track_id, role, name, detail, artist_mbid, position)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(track.track_id)
                .bind(&credit.role)
                .bind(&credit.name)
                .bind(&credit.detail)
                .bind(&credit.artist_mbid)
                .bind(position as i64)
                .execute(&mut *conn)
                .await?;
            }
        }
    }
    apply_album(conn, album_id, fields).await
}

/// Record that nothing plausible was found. A review or an applied
/// decision is never downgraded to this by a later pass.
pub async fn record_no_match(
    conn: &mut SqliteConnection,
    album_id: i64,
    now: i64,
) -> CoreResult<()> {
    sqlx::query(
        "INSERT INTO musicbrainz_album (album_id, status, matched_at)
         VALUES (?, 'no_match', ?)
         ON CONFLICT(album_id) DO UPDATE SET matched_at = excluded.matched_at
          WHERE musicbrainz_album.status = 'no_match'",
    )
    .bind(album_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Fill the album's gaps from its applied decision. Idempotent: a field
/// already filled is not empty any more, so a second call changes
/// nothing — which is what lets [`reapply_all`] run on every pass.
pub async fn apply_album(
    conn: &mut SqliteConnection,
    album_id: i64,
    fields: Fields,
) -> CoreResult<ApplyReport> {
    let decision: Option<(Option<String>, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT original_date, original_year, genres
           FROM musicbrainz_album WHERE album_id = ? AND status = 'applied'",
    )
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((original_date, original_year, genres)) = decision else {
        return Ok(ApplyReport::default());
    };
    let mut report = ApplyReport::default();

    if fields.dates {
        if let Some(year) = original_year {
            report.year = sqlx::query("UPDATE album SET year = ? WHERE id = ? AND year IS NULL")
                .bind(year)
                .bind(album_id)
                .execute(&mut *conn)
                .await?
                .rows_affected()
                > 0;
            // The provenance first, while the gap still identifies the
            // tracks it is about.
            sqlx::query(
                "UPDATE musicbrainz_track SET filled_year = 1
                  WHERE album_id = ?
                    AND track_id IN (SELECT id FROM track WHERE album_id = ? AND year IS NULL)",
            )
            .bind(album_id)
            .bind(album_id)
            .execute(&mut *conn)
            .await?;
            report.track_years = sqlx::query(
                "UPDATE track SET year = ?
                  WHERE album_id = ? AND year IS NULL
                    AND id IN (SELECT track_id FROM musicbrainz_track WHERE album_id = ?)",
            )
            .bind(year)
            .bind(album_id)
            .bind(album_id)
            .execute(&mut *conn)
            .await?
            .rows_affected();
        }
        if let Some(date) = original_date.as_deref() {
            report.release_date = sqlx::query(
                "UPDATE album SET release_date = ? WHERE id = ? AND release_date IS NULL",
            )
            .bind(date)
            .bind(album_id)
            .execute(&mut *conn)
            .await?
            .rows_affected()
                > 0;
        }
        sqlx::query(
            "UPDATE musicbrainz_album
                SET filled_year = filled_year OR ?,
                    filled_release_date = filled_release_date OR ?
              WHERE album_id = ?",
        )
        .bind(report.year)
        .bind(report.release_date)
        .bind(album_id)
        .execute(&mut *conn)
        .await?;
    }

    if fields.genres {
        let names: Vec<String> = genres
            .as_deref()
            .and_then(|g| serde_json::from_str(g).ok())
            .unwrap_or_default();
        let mut genre_ids = Vec::with_capacity(names.len());
        for name in &names {
            if let Some(id) = upsert_genre(&mut *conn, name).await? {
                genre_ids.push(id);
            }
        }
        if !genre_ids.is_empty() {
            // Paired tracks with no genre at all: a track that has one,
            // from its file or from the user, keeps it alone.
            // Only tracks still on the album: one retagged elsewhere keeps
            // its pairing row until the next decision, and must not take
            // this album's genres along.
            let bare: Vec<i64> = sqlx::query_scalar(
                "SELECT mt.track_id FROM musicbrainz_track mt
                   JOIN track t ON t.id = mt.track_id AND t.album_id = mt.album_id
                  WHERE mt.album_id = ?
                    AND NOT EXISTS (SELECT 1 FROM track_genre tg WHERE tg.track_id = mt.track_id)",
            )
            .bind(album_id)
            .fetch_all(&mut *conn)
            .await?;
            for track_id in &bare {
                for genre_id in &genre_ids {
                    sqlx::query(
                        "INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?, ?)",
                    )
                    .bind(track_id)
                    .bind(genre_id)
                    .execute(&mut *conn)
                    .await?;
                    sqlx::query(
                        "INSERT OR IGNORE INTO musicbrainz_filled_genre (track_id, genre_id)
                         VALUES (?, ?)",
                    )
                    .bind(track_id)
                    .bind(genre_id)
                    .execute(&mut *conn)
                    .await?;
                }
            }
            report.genre_tracks = bare.len() as u64;
        }
    }

    if fields.covers {
        let artwork: Option<Option<i64>> =
            sqlx::query_scalar("SELECT artwork_id FROM album WHERE id = ?")
                .bind(album_id)
                .fetch_optional(&mut *conn)
                .await?;
        report.wants_cover = matches!(artwork, Some(None));
    }
    Ok(report)
}

/// Link a fetched cover, if the album still has none — a scan may have
/// found one while the image was downloading.
pub async fn link_cover(
    conn: &mut SqliteConnection,
    album_id: i64,
    artwork_id: i64,
) -> CoreResult<bool> {
    let linked = sqlx::query(
        "UPDATE album SET artwork_id = ?, artwork_source = ?
          WHERE id = ? AND artwork_id IS NULL",
    )
    .bind(artwork_id)
    .bind(COVER_SOURCE)
    .bind(album_id)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0;
    if linked {
        sqlx::query("UPDATE musicbrainz_album SET filled_cover = 1 WHERE album_id = ?")
            .bind(album_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(linked)
}

/// Applied albums whose cover is still missing although their release
/// has one: the cover fetch of a pass that failed or was stopped, picked
/// up by the next one. Only those the archive said it holds a front
/// cover for — an album with none anywhere would otherwise cost a
/// request on every pass, forever.
pub async fn albums_wanting_cover(
    conn: &mut SqliteConnection,
) -> CoreResult<Vec<(i64, String, Option<String>, bool)>> {
    Ok(sqlx::query_as(
        "SELECT m.album_id, m.release_mbid, m.release_group_mbid, m.has_front_cover
           FROM musicbrainz_album m JOIN album al ON al.id = m.album_id
          WHERE m.status = 'applied' AND al.artwork_id IS NULL
            AND m.release_mbid IS NOT NULL AND m.has_front_cover = 1
          ORDER BY m.album_id",
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// The albums with an applied decision, for a refill one album at a time.
pub async fn applied_album_ids(conn: &mut SqliteConnection) -> CoreResult<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT album_id FROM musicbrainz_album WHERE status = 'applied'")
            .fetch_all(&mut *conn)
            .await?,
    )
}

/// Fill again what the scanner emptied on one applied album. `true` when
/// something was filled.
pub async fn reapply_album(
    conn: &mut SqliteConnection,
    album_id: i64,
    fields: Fields,
) -> CoreResult<bool> {
    // Covers are not this function's business: fetching one is network
    // work, and `albums_wanting_cover` hands those to the caller.
    let fields = Fields {
        covers: false,
        ..fields
    };
    let report = apply_album(conn, album_id, fields).await?;
    Ok(report.year || report.release_date || report.track_years > 0 || report.genre_tracks > 0)
}

/// [`reapply_album`] over every applied decision, in the caller's one
/// transaction. Returns how many albums had something filled. The app
/// refills one album per transaction instead; this is the whole-library
/// form, for a caller with no other writer to make room for.
pub async fn reapply_all(conn: &mut SqliteConnection, fields: Fields) -> CoreResult<u64> {
    let mut touched = 0;
    for id in applied_album_ids(conn).await? {
        if reapply_album(conn, id, fields).await? {
            touched += 1;
        }
    }
    Ok(touched)
}

/// Delete the pairing and the credits of an album's decision, and the
/// genres it gave its tracks.
async fn clear_pairing(conn: &mut SqliteConnection, album_id: i64) -> CoreResult<()> {
    sqlx::query(
        "DELETE FROM track_genre
          WHERE EXISTS (SELECT 1 FROM musicbrainz_filled_genre fg
                          JOIN musicbrainz_track mt ON mt.track_id = fg.track_id
                         WHERE mt.album_id = ?
                           AND fg.track_id = track_genre.track_id
                           AND fg.genre_id = track_genre.genre_id)",
    )
    .bind(album_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM musicbrainz_filled_genre
          WHERE track_id IN (SELECT track_id FROM musicbrainz_track WHERE album_id = ?)",
    )
    .bind(album_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "DELETE FROM track_credit
          WHERE track_id IN (SELECT track_id FROM musicbrainz_track WHERE album_id = ?)",
    )
    .bind(album_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM musicbrainz_track WHERE album_id = ?")
        .bind(album_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Take back everything an applied decision filled, and mark it
/// rejected so no later pass applies it again. A value the user changed
/// since is theirs and stays: each field is reset only while it still
/// holds what the engine wrote.
pub async fn undo_album(conn: &mut SqliteConnection, album_id: i64, now: i64) -> CoreResult<()> {
    let decision: Option<(Option<String>, Option<i64>, bool, bool, bool)> = sqlx::query_as(
        "SELECT original_date, original_year, filled_year, filled_release_date, filled_cover
           FROM musicbrainz_album WHERE album_id = ?",
    )
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((date, year, filled_year, filled_date, filled_cover)) = decision else {
        return Ok(());
    };
    if filled_year {
        sqlx::query("UPDATE album SET year = NULL WHERE id = ? AND year IS ?")
            .bind(album_id)
            .bind(year)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query(
        "UPDATE track SET year = NULL
          WHERE year IS ?
            AND id IN (SELECT track_id FROM musicbrainz_track
                        WHERE album_id = ? AND filled_year = 1)",
    )
    .bind(year)
    .bind(album_id)
    .execute(&mut *conn)
    .await?;
    if filled_date {
        sqlx::query("UPDATE album SET release_date = NULL WHERE id = ? AND release_date IS ?")
            .bind(album_id)
            .bind(&date)
            .execute(&mut *conn)
            .await?;
    }
    if filled_cover {
        sqlx::query(
            "UPDATE album SET artwork_id = NULL, artwork_source = NULL
              WHERE id = ? AND artwork_source = ?",
        )
        .bind(album_id)
        .bind(COVER_SOURCE)
        .execute(&mut *conn)
        .await?;
    }
    clear_pairing(conn, album_id).await?;
    set_album_status(conn, album_id, Status::Rejected, now).await
}

/// Refuse a proposal: it is never made again on its own.
pub async fn reject_album(conn: &mut SqliteConnection, album_id: i64, now: i64) -> CoreResult<()> {
    set_album_status(conn, album_id, Status::Rejected, now).await
}

/// Forget a decision entirely, so the next pass looks the album up
/// again: what "try again" means for a rejected or unmatched album.
pub async fn forget_album(conn: &mut SqliteConnection, album_id: i64, now: i64) -> CoreResult<()> {
    undo_album(conn, album_id, now).await?;
    sqlx::query("DELETE FROM musicbrainz_album WHERE album_id = ?")
        .bind(album_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn set_album_status(
    conn: &mut SqliteConnection,
    album_id: i64,
    status: Status,
    now: i64,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE musicbrainz_album
            SET status = ?, matched_at = ?,
                filled_year = 0, filled_release_date = 0, filled_cover = 0
          WHERE album_id = ?",
    )
    .bind(status.as_str())
    .bind(now)
    .bind(album_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One album decision as the review screen lists it: the local album
/// beside the release it was matched with.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct AlbumDecision {
    pub album_id: i64,
    pub status: String,
    pub score: Option<f64>,
    pub local_title: String,
    pub local_artist: Option<String>,
    pub local_tracks: i64,
    pub local_year: Option<i64>,
    pub release_mbid: Option<String>,
    pub release_title: Option<String>,
    pub release_artist: Option<String>,
    pub release_country: Option<String>,
    pub release_track_count: Option<i64>,
    pub original_date: Option<String>,
    pub genres: Option<String>,
    pub matched_at: i64,
}

/// Decisions with `status`, the newest first.
pub async fn album_decisions(
    conn: &mut SqliteConnection,
    status: Status,
    limit: i64,
) -> CoreResult<Vec<AlbumDecision>> {
    Ok(sqlx::query_as(
        "SELECT m.album_id, m.status, m.score,
                al.title AS local_title,
                COALESCE(al.album_artist, ar.name) AS local_artist,
                (SELECT COUNT(*) FROM track t
                  WHERE t.album_id = al.id AND t.is_available = 1) AS local_tracks,
                al.year AS local_year,
                m.release_mbid, m.release_title, m.release_artist, m.release_country,
                m.release_track_count, m.original_date, m.genres, m.matched_at
           FROM musicbrainz_album m
           JOIN album al ON al.id = m.album_id
           LEFT JOIN artist ar ON ar.id = al.artist_id
          WHERE m.status = ?
          ORDER BY m.matched_at DESC, m.album_id
          LIMIT ?",
    )
    .bind(status.as_str())
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?)
}

/// How many decisions of each kind there are.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Stats {
    pub albums_applied: i64,
    pub albums_review: i64,
    pub albums_rejected: i64,
    pub albums_no_match: i64,
    pub artists_applied: i64,
    pub artists_review: i64,
    pub artists_rejected: i64,
    pub artists_no_match: i64,
}

pub async fn stats(conn: &mut SqliteConnection) -> CoreResult<Stats> {
    let mut out = Stats::default();
    let albums: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM musicbrainz_album GROUP BY status")
            .fetch_all(&mut *conn)
            .await?;
    for (status, n) in albums {
        match status.as_str() {
            "applied" => out.albums_applied = n,
            "review" => out.albums_review = n,
            "rejected" => out.albums_rejected = n,
            _ => out.albums_no_match = n,
        }
    }
    let artists: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM musicbrainz_artist GROUP BY status")
            .fetch_all(&mut *conn)
            .await?;
    for (status, n) in artists {
        match status.as_str() {
            "applied" => out.artists_applied = n,
            "review" => out.artists_review = n,
            "rejected" => out.artists_rejected = n,
            _ => out.artists_no_match = n,
        }
    }
    Ok(out)
}

/// One stored credit of a track.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct StoredCredit {
    pub role: String,
    pub name: String,
    pub detail: String,
    pub artist_mbid: Option<String>,
}

pub async fn track_credits(
    conn: &mut SqliteConnection,
    track_id: i64,
) -> CoreResult<Vec<StoredCredit>> {
    Ok(sqlx::query_as(
        "SELECT role, name, detail, artist_mbid FROM track_credit
          WHERE track_id = ? ORDER BY position, role, name",
    )
    .bind(track_id)
    .fetch_all(&mut *conn)
    .await?)
}

// ── Artists ─────────────────────────────────────────────────────────

/// Library artists with no decision yet, and `no_match` decisions older
/// than `no_match_before`, in id order from `after_id` on (see
/// [`pending_albums`]). Only artists credited on an available track, as
/// its main artist or as its album's artist; the compilation label is
/// not an artist.
pub async fn pending_artists(
    conn: &mut SqliteConnection,
    no_match_before: i64,
    various_artists: &str,
    after_id: i64,
    limit: i64,
) -> CoreResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT a.id, a.name
           FROM artist a
           LEFT JOIN musicbrainz_artist m ON m.artist_id = a.id
          WHERE (m.artist_id IS NULL
                 OR (m.status = 'no_match' AND m.matched_at < ?))
            AND a.name <> ?
            AND a.id > ?
            AND (EXISTS (SELECT 1 FROM track_artist ta JOIN track t ON t.id = ta.track_id
                          WHERE ta.artist_id = a.id AND ta.role = 'main' AND t.is_available = 1)
                 OR EXISTS (SELECT 1 FROM album al JOIN track t ON t.album_id = al.id
                             WHERE al.artist_id = a.id AND t.is_available = 1))
          ORDER BY a.id
          LIMIT ?",
    )
    .bind(no_match_before)
    .bind(various_artists)
    .bind(after_id)
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?)
}

/// How many artists [`pending_artists`] would return without a cursor or
/// a limit.
pub async fn count_pending_artists(
    conn: &mut SqliteConnection,
    no_match_before: i64,
    various_artists: &str,
) -> CoreResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM artist a
           LEFT JOIN musicbrainz_artist m ON m.artist_id = a.id
          WHERE (m.artist_id IS NULL
                 OR (m.status = 'no_match' AND m.matched_at < ?))
            AND a.name <> ?
            AND (EXISTS (SELECT 1 FROM track_artist ta JOIN track t ON t.id = ta.track_id
                          WHERE ta.artist_id = a.id AND ta.role = 'main' AND t.is_available = 1)
                 OR EXISTS (SELECT 1 FROM album al JOIN track t ON t.album_id = al.id
                             WHERE al.artist_id = a.id AND t.is_available = 1))",
    )
    .bind(no_match_before)
    .bind(various_artists)
    .fetch_one(&mut *conn)
    .await?)
}

/// The library artists an album credits: its album artist and the main
/// artists of its tracks. Where a credited release artist is looked for.
pub async fn album_artists(
    conn: &mut SqliteConnection,
    album_id: i64,
) -> CoreResult<Vec<(i64, String)>> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT a.id, a.name FROM artist a
          WHERE a.id = (SELECT artist_id FROM album WHERE id = ?)
             OR a.id IN (SELECT ta.artist_id FROM track_artist ta
                           JOIN track t ON t.id = ta.track_id
                          WHERE t.album_id = ? AND ta.role = 'main')
          ORDER BY a.id",
    )
    .bind(album_id)
    .bind(album_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Propose `proposal` for review. A decision the user took (applied,
/// rejected) is never replaced; a pending proposal for the same artist
/// gains the evidence, one for another artist is kept unless this one
/// has more of it.
pub async fn propose_artist(
    conn: &mut SqliteConnection,
    artist_id: i64,
    proposal: &ArtistProposal,
    now: i64,
) -> CoreResult<()> {
    sqlx::query(
        "INSERT INTO musicbrainz_artist
            (artist_id, status, score, artist_mbid, name, disambiguation, kind, country,
             album_evidence, matched_at)
         VALUES (?, 'review', ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(artist_id) DO UPDATE SET
            status = 'review',
            score = excluded.score,
            artist_mbid = excluded.artist_mbid,
            name = excluded.name,
            disambiguation = excluded.disambiguation,
            kind = excluded.kind,
            country = excluded.country,
            album_evidence = CASE
                WHEN musicbrainz_artist.artist_mbid = excluded.artist_mbid
                THEN musicbrainz_artist.album_evidence + excluded.album_evidence
                ELSE excluded.album_evidence END,
            matched_at = excluded.matched_at
          WHERE musicbrainz_artist.status = 'no_match'
             OR (musicbrainz_artist.status = 'review'
                 AND (musicbrainz_artist.artist_mbid = excluded.artist_mbid
                      OR musicbrainz_artist.album_evidence < excluded.album_evidence))",
    )
    .bind(artist_id)
    .bind(proposal.score)
    .bind(&proposal.mbid)
    .bind(&proposal.name)
    .bind(&proposal.disambiguation)
    .bind(&proposal.kind)
    .bind(&proposal.country)
    .bind(proposal.album_evidence)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn record_artist_no_match(
    conn: &mut SqliteConnection,
    artist_id: i64,
    now: i64,
) -> CoreResult<()> {
    sqlx::query(
        "INSERT INTO musicbrainz_artist (artist_id, status, matched_at)
         VALUES (?, 'no_match', ?)
         ON CONFLICT(artist_id) DO UPDATE SET matched_at = excluded.matched_at
          WHERE musicbrainz_artist.status = 'no_match'",
    )
    .bind(artist_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Accept the proposal, with the facts looked up for it.
pub async fn accept_artist(
    conn: &mut SqliteConnection,
    artist_id: i64,
    facts: &ArtistFacts,
    now: i64,
) -> CoreResult<bool> {
    Ok(sqlx::query(
        "UPDATE musicbrainz_artist
            SET status = 'applied',
                name = COALESCE(?, name),
                disambiguation = COALESCE(?, disambiguation),
                kind = COALESCE(?, kind),
                country = COALESCE(?, country),
                area = ?, begin_date = ?, end_date = ?, genres = ?,
                matched_at = ?
          WHERE artist_id = ? AND status = 'review' AND artist_mbid = ?",
    )
    .bind(&facts.name)
    .bind(&facts.disambiguation)
    .bind(&facts.kind)
    .bind(&facts.country)
    .bind(&facts.area)
    .bind(&facts.begin_date)
    .bind(&facts.end_date)
    .bind(&facts.genres)
    .bind(now)
    .bind(artist_id)
    .bind(&facts.artist_mbid)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        > 0)
}

/// Reject a proposal, or take back an accepted artist.
pub async fn reject_artist(
    conn: &mut SqliteConnection,
    artist_id: i64,
    now: i64,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE musicbrainz_artist
            SET status = 'rejected', area = NULL, begin_date = NULL, end_date = NULL,
                genres = NULL, matched_at = ?
          WHERE artist_id = ?",
    )
    .bind(now)
    .bind(artist_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Forget an artist decision, so the next pass proposes again.
pub async fn forget_artist(conn: &mut SqliteConnection, artist_id: i64) -> CoreResult<()> {
    sqlx::query("DELETE FROM musicbrainz_artist WHERE artist_id = ?")
        .bind(artist_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// One artist decision as the review screen lists it.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ArtistDecision {
    pub artist_id: i64,
    pub local_name: String,
    pub status: String,
    pub score: Option<f64>,
    pub artist_mbid: Option<String>,
    pub name: Option<String>,
    pub disambiguation: Option<String>,
    pub kind: Option<String>,
    pub country: Option<String>,
    pub album_evidence: i64,
    pub matched_at: i64,
}

pub async fn artist_decisions(
    conn: &mut SqliteConnection,
    status: Status,
    limit: i64,
) -> CoreResult<Vec<ArtistDecision>> {
    Ok(sqlx::query_as(
        "SELECT m.artist_id, a.name AS local_name, m.status, m.score, m.artist_mbid, m.name,
                m.disambiguation, m.kind, m.country, m.album_evidence, m.matched_at
           FROM musicbrainz_artist m JOIN artist a ON a.id = m.artist_id
          WHERE m.status = ?
          ORDER BY m.album_evidence DESC, m.score DESC, a.name
          LIMIT ?",
    )
    .bind(status.as_str())
    .bind(limit)
    .fetch_all(&mut *conn)
    .await?)
}

/// The facts of an accepted artist; `None` when it has none.
pub async fn artist_facts(
    conn: &mut SqliteConnection,
    artist_id: i64,
) -> CoreResult<Option<ArtistFacts>> {
    Ok(sqlx::query_as(
        "SELECT artist_mbid, name, disambiguation, kind, country, area, begin_date, end_date, genres
           FROM musicbrainz_artist WHERE artist_id = ? AND status = 'applied'",
    )
    .bind(artist_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// The MusicBrainz identifier of a pending artist proposal.
pub async fn proposed_artist_mbid(
    conn: &mut SqliteConnection,
    artist_id: i64,
) -> CoreResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT artist_mbid FROM musicbrainz_artist WHERE artist_id = ? AND status = 'review'",
    )
    .bind(artist_id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten())
}

/// The release identifier of a pending album proposal.
pub async fn proposed_release_mbid(
    conn: &mut SqliteConnection,
    album_id: i64,
) -> CoreResult<Option<String>> {
    Ok(sqlx::query_scalar(
        "SELECT release_mbid FROM musicbrainz_album WHERE album_id = ? AND status = 'review'",
    )
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten())
}

#[cfg(test)]
mod tests;
