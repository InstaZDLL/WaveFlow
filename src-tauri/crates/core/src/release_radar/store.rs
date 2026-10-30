//! The radar's two tables (`release_radar_artist`, `release_radar_release`)
//! and the reads that join them to the library.
//!
//! Every write is one short transaction for one artist, so a scan that
//! runs for a minute never holds the single writer for longer than an
//! upsert of a few rows.

use sqlx::{FromRow, SqlitePool};

use super::{LocalTrack, ReleaseGroup, ReleaseRef};
use crate::error::CoreResult;
use crate::scanner::upserts::VARIOUS_ARTISTS_LABEL;

/// How far back the plays that weigh an artist go.
const PLAYS_WINDOW_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// An artist the radar follows.
#[derive(Debug, Clone, FromRow)]
pub struct FollowedArtist {
    pub artist_id: i64,
    pub name: String,
    /// The MusicBrainz id, when one is known.
    pub mbid: Option<String>,
    pub checked_at: Option<i64>,
    pub weight: i64,
}

/// The library's artists the radar follows, heaviest first, at most
/// `limit` of them.
///
/// An artist weighs its available tracks, plus one for every two plays
/// over the last 90 days, plus three for every liked track: what the
/// user keeps, listens to and loves, the first of which is all a library
/// that was never played has. Every credit counts, not only the first,
/// so a featured artist the user collects is followed too. "Various
/// Artists" is a label, not an artist, and is never followed.
///
/// **Where an MBID comes from.** The id is the one the radar learnt
/// from a release credit (`release_radar_artist.mbid`). This is the one
/// place to read the library's own MusicBrainz ids once the library has
/// them: `COALESCE(<the library's id>, rra.mbid)` in the `mbid` column
/// below, and every query goes by id from the first scan on.
pub async fn followed_artists(
    pool: &SqlitePool,
    now_ms: i64,
    limit: i64,
) -> CoreResult<Vec<FollowedArtist>> {
    Ok(sqlx::query_as::<_, FollowedArtist>(
        r#"
        WITH held AS (
            SELECT ta.artist_id, COUNT(DISTINCT t.id) AS n
              FROM track_artist ta
              JOIN track t ON t.id = ta.track_id
             WHERE t.is_available = 1
             GROUP BY ta.artist_id
        ),
        -- DISTINCT: a track can credit one artist under two roles.
        played AS (
            SELECT ta.artist_id, COUNT(DISTINCT pe.id) AS n
              FROM play_event pe
              JOIN track_artist ta ON ta.track_id = pe.track_id
             WHERE pe.played_at >= ?
             GROUP BY ta.artist_id
        ),
        liked AS (
            SELECT ta.artist_id, COUNT(DISTINCT lt.track_id) AS n
              FROM liked_track lt
              JOIN track_artist ta ON ta.track_id = lt.track_id
             GROUP BY ta.artist_id
        )
        SELECT ar.id                          AS artist_id,
               ar.name                        AS name,
               rra.mbid                       AS mbid,
               rra.checked_at                 AS checked_at,
               held.n + COALESCE(played.n, 0) / 2 + 3 * COALESCE(liked.n, 0) AS weight
          FROM held
          JOIN artist ar ON ar.id = held.artist_id
          LEFT JOIN played ON played.artist_id = ar.id
          LEFT JOIN liked  ON liked.artist_id  = ar.id
          LEFT JOIN release_radar_artist rra ON rra.artist_id = ar.id
         WHERE ar.name <> ? COLLATE NOCASE
           AND TRIM(ar.name) <> ''
         ORDER BY weight DESC, ar.canonical_name ASC, ar.id ASC
         LIMIT ?
        "#,
    )
    .bind(now_ms - PLAYS_WINDOW_MS)
    .bind(VARIOUS_ARTISTS_LABEL)
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Forget when every artist was last asked about, and the ids learnt
/// for them, so the next scan asks about all of them by name again. The
/// releases stay. The ids go too because a full rescan is the one way
/// back from an id learnt wrong — a `COALESCE` keeps it otherwise.
pub async fn reset_checks(pool: &SqlitePool) -> CoreResult<()> {
    sqlx::query("UPDATE release_radar_artist SET checked_at = NULL, mbid = NULL")
        .execute(pool)
        .await?;
    Ok(())
}

/// Drop the releases that came out before `cutoff` (`YYYY-MM-DD`): the
/// radar is about what is new.
///
/// Compared on the padded date ([`super::date_key`]) so "2026" is not
/// read as older than "2026-01-01".
pub async fn prune_older_than(pool: &SqlitePool, cutoff: &str) -> CoreResult<u64> {
    let done = sqlx::query(
        r#"
        DELETE FROM release_radar_release
         WHERE (CASE LENGTH(first_release_date)
                    WHEN 4 THEN first_release_date || '-01-01'
                    WHEN 7 THEN first_release_date || '-01'
                    ELSE first_release_date
                END) < ?
        "#,
    )
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Record one artist's answer: the artist is checked as of `now_ms`, its
/// id is kept when the library confirmed one ([`super::confirmed_mbid`]),
/// and its releases are upserted.
/// One transaction, so a scan stopped between two artists leaves each of
/// them either recorded or still due.
///
/// A release credited to two followed artists stays with the first that
/// found it.
pub async fn record_artist(
    pool: &SqlitePool,
    artist_id: i64,
    learned_mbid: Option<&str>,
    releases: &[ReleaseGroup],
    now_ms: i64,
) -> CoreResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO release_radar_artist (artist_id, mbid, checked_at)
        VALUES (?, ?, ?)
        ON CONFLICT(artist_id) DO UPDATE SET
            mbid       = COALESCE(excluded.mbid, release_radar_artist.mbid),
            checked_at = excluded.checked_at
        "#,
    )
    .bind(artist_id)
    .bind(learned_mbid)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;

    for release in releases {
        let secondary =
            (!release.secondary_types.is_empty()).then(|| release.secondary_types.join("; "));
        sqlx::query(
            r#"
            INSERT INTO release_radar_release
                (id, artist_id, title, artist_credit, first_release_date,
                 primary_type, secondary_types, discovered_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                title              = excluded.title,
                artist_credit      = excluded.artist_credit,
                first_release_date = excluded.first_release_date,
                primary_type       = excluded.primary_type,
                secondary_types    = excluded.secondary_types
            "#,
        )
        .bind(&release.id)
        .bind(artist_id)
        .bind(&release.title)
        .bind(&release.artist_credit)
        .bind(&release.first_release_date)
        .bind(release.primary_type.as_deref())
        .bind(secondary)
        .bind(now_ms)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// The releases whose cover is still to be asked for: never asked, or
/// asked without an answer more than `retry_after_ms` ago.
pub async fn covers_due(
    pool: &SqlitePool,
    now_ms: i64,
    retry_after_ms: i64,
) -> CoreResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT id FROM release_radar_release
         WHERE cover_hash IS NULL
           AND (cover_checked_at IS NULL OR cover_checked_at < ?)
         ORDER BY first_release_date DESC, id
        "#,
    )
    .bind(now_ms - retry_after_ms)
    .fetch_all(pool)
    .await?)
}

/// Record the answer to a cover request: the cached file's hash, or
/// `None` when there was no cover to have.
pub async fn record_cover(
    pool: &SqlitePool,
    release_id: &str,
    cover_hash: Option<&str>,
    now_ms: i64,
) -> CoreResult<()> {
    sqlx::query(
        "UPDATE release_radar_release
            SET cover_hash = COALESCE(?, cover_hash), cover_checked_at = ?
          WHERE id = ?",
    )
    .bind(cover_hash)
    .bind(now_ms)
    .bind(release_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// One stored release with the library artist it belongs to.
#[derive(Debug, Clone, FromRow)]
pub struct StoredRelease {
    pub id: String,
    pub artist_id: i64,
    pub artist_name: String,
    pub title: String,
    pub artist_credit: String,
    pub first_release_date: String,
    pub primary_type: Option<String>,
    pub secondary_types: Option<String>,
    pub cover_hash: Option<String>,
    pub discovered_at: i64,
}

impl StoredRelease {
    pub fn as_ref(&self) -> ReleaseRef {
        ReleaseRef {
            id: self.id.clone(),
            artist_id: self.artist_id,
            title: self.title.clone(),
            primary_type: self.primary_type.clone(),
        }
    }
}

/// Every stored release out on or after `cutoff`, newest first.
pub async fn recent_releases(pool: &SqlitePool, cutoff: &str) -> CoreResult<Vec<StoredRelease>> {
    Ok(sqlx::query_as::<_, StoredRelease>(
        r#"
        SELECT r.id, r.artist_id, ar.name AS artist_name, r.title,
               r.artist_credit, r.first_release_date, r.primary_type,
               r.secondary_types, r.cover_hash, r.discovered_at
          FROM release_radar_release r
          JOIN artist ar ON ar.id = r.artist_id
         WHERE (CASE LENGTH(r.first_release_date)
                    WHEN 4 THEN r.first_release_date || '-01-01'
                    WHEN 7 THEN r.first_release_date || '-01'
                    ELSE r.first_release_date
                END) >= ?
         ORDER BY (CASE LENGTH(r.first_release_date)
                       WHEN 4 THEN r.first_release_date || '-01-01'
                       WHEN 7 THEN r.first_release_date || '-01'
                       ELSE r.first_release_date
                   END) DESC,
                  r.title COLLATE NOCASE ASC, r.id ASC
        "#,
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?)
}

#[derive(FromRow)]
struct LocalTrackRow {
    track_id: i64,
    artist_id: i64,
    title: String,
    album_id: Option<i64>,
    album_title: Option<String>,
    disc_number: Option<i64>,
    track_number: Option<i64>,
}

/// The available tracks of one artist, shaped as [`local_tracks`]: what a
/// fresh answer is matched against before the library may confirm the
/// artist's id.
pub async fn local_tracks_of(pool: &SqlitePool, artist_id: i64) -> CoreResult<Vec<LocalTrack>> {
    let rows = sqlx::query_as::<_, LocalTrackRow>(
        r#"
        SELECT t.id AS track_id, ta.artist_id AS artist_id, t.title AS title,
               t.album_id AS album_id, al.title AS album_title,
               t.disc_number AS disc_number, t.track_number AS track_number
          FROM track t
          JOIN track_artist ta ON ta.track_id = t.id
          LEFT JOIN album al ON al.id = t.album_id
         WHERE t.is_available = 1
           AND ta.artist_id = ?1
        UNION
        SELECT t.id, al.artist_id, t.title, t.album_id, al.title,
               t.disc_number, t.track_number
          FROM track t
          JOIN album al ON al.id = t.album_id
         WHERE t.is_available = 1
           AND al.artist_id = ?1
        "#,
    )
    .bind(artist_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(LocalTrack::from).collect())
}

/// The available tracks of the artists the stored releases belong to,
/// once per (track, artist) pairing — by any of the track's credits, or
/// by its album's artist. What [`super::match_releases`] matches against.
pub async fn local_tracks(pool: &SqlitePool) -> CoreResult<Vec<LocalTrack>> {
    let rows = sqlx::query_as::<_, LocalTrackRow>(
        r#"
        WITH radar AS (SELECT DISTINCT artist_id FROM release_radar_release)
        SELECT t.id AS track_id, ta.artist_id AS artist_id, t.title AS title,
               t.album_id AS album_id, al.title AS album_title,
               t.disc_number AS disc_number, t.track_number AS track_number
          FROM track t
          JOIN track_artist ta ON ta.track_id = t.id
          LEFT JOIN album al ON al.id = t.album_id
         WHERE t.is_available = 1
           AND ta.artist_id IN (SELECT artist_id FROM radar)
        UNION
        SELECT t.id, al.artist_id, t.title, t.album_id, al.title,
               t.disc_number, t.track_number
          FROM track t
          JOIN album al ON al.id = t.album_id
         WHERE t.is_available = 1
           AND al.artist_id IN (SELECT artist_id FROM radar)
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(LocalTrack::from).collect())
}

impl From<LocalTrackRow> for LocalTrack {
    fn from(r: LocalTrackRow) -> Self {
        LocalTrack {
            track_id: r.track_id,
            artist_id: r.artist_id,
            title: r.title,
            album_id: r.album_id,
            album_title: r.album_title,
            disc_number: r.disc_number,
            track_number: r.track_number,
        }
    }
}
