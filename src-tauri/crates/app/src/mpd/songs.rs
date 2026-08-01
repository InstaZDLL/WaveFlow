//! Reading the queue in the shape MPD wants.
//!
//! [`crate::queue::list_queue`] already returns the queue, but it is
//! built for the frontend: it carries artwork hashes and audio-quality
//! fields MPD has no use for, and — critically — it projects
//! `track.id`, not `queue_item.id`.
//!
//! MPD's `Id` must be **stable per queue entry**, not per track: the
//! same file can sit in the queue twice, and `deleteid` / `moveid` /
//! `playid` must still tell those two entries apart. `queue_item.id` is
//! an `INTEGER PRIMARY KEY` and survives reordering, so it maps onto
//! MPD's song id exactly. Hence a dedicated query rather than widening
//! the shared struct that every frontend payload flows through.

use serde::Serialize;

use crate::error::AppResult;

/// One queue entry, projected for the MPD wire format.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MpdSong {
    /// `queue_item.id` — MPD's `Id`. Stable across moves.
    pub queue_id: i64,
    /// 0-based `queue_item.position` — MPD's `Pos`.
    pub position: i64,
    pub file_path: String,
    pub title: String,
    pub duration_ms: i64,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub album_artist_name: Option<String>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    /// Epoch **milliseconds** (the scanner writes `modified_ms`), for
    /// `Last-Modified`.
    pub file_modified: i64,
}

/// Multi-artist display follows the same `GROUP_CONCAT` over
/// `track_artist` ordered by `position` that every other track query in
/// the codebase uses — a track credited to three artists must not show
/// up under only its primary one just because it came through MPD.
const SELECT_SONGS: &str = r#"
    SELECT q.id       AS queue_id,
           q.position AS position,
           t.file_path,
           t.title,
           t.duration_ms,
           (SELECT GROUP_CONCAT(name, ', ') FROM (
              SELECT ar2.name FROM track_artist ta2
              JOIN artist ar2 ON ar2.id = ta2.artist_id
              WHERE ta2.track_id = t.id
              ORDER BY ta2.position
           )) AS artist_name,
           al.title   AS album_title,
           aa.name    AS album_artist_name,
           t.track_number,
           t.disc_number,
           t.year,
           t.file_modified
      FROM queue_item q
      JOIN track t        ON t.id = q.track_id
      LEFT JOIN album al  ON al.id = t.album_id
      LEFT JOIN artist aa ON aa.id = al.artist_id
"#;

/// Every queue entry, in play order.
pub async fn list(pool: &sqlx::SqlitePool) -> AppResult<Vec<MpdSong>> {
    let rows = sqlx::query_as::<_, MpdSong>(sqlx::AssertSqlSafe(format!(
        "{SELECT_SONGS} ORDER BY q.position"
    )))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A half-open slice of the queue by position, for `playlistinfo A:B`.
pub async fn list_range(pool: &sqlx::SqlitePool, start: u32, end: u32) -> AppResult<Vec<MpdSong>> {
    let rows = sqlx::query_as::<_, MpdSong>(sqlx::AssertSqlSafe(format!(
        "{SELECT_SONGS} WHERE q.position >= ? AND q.position < ? ORDER BY q.position"
    )))
    .bind(start as i64)
    .bind(end as i64)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A single entry by its `queue_item.id`, for `playlistid` / `seekid`.
pub async fn by_queue_id(pool: &sqlx::SqlitePool, queue_id: u32) -> AppResult<Option<MpdSong>> {
    let row = sqlx::query_as::<_, MpdSong>(sqlx::AssertSqlSafe(format!(
        "{SELECT_SONGS} WHERE q.id = ?"
    )))
    .bind(queue_id as i64)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// A single entry by queue position.
pub async fn by_position(pool: &sqlx::SqlitePool, position: i64) -> AppResult<Option<MpdSong>> {
    let row = sqlx::query_as::<_, MpdSong>(sqlx::AssertSqlSafe(format!(
        "{SELECT_SONGS} WHERE q.position = ?"
    )))
    .bind(position)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Library-wide counters for `stats`. Clients show these in an "info"
/// pane; ncmpcpp refuses to draw one at all if the command ACKs.
#[derive(Debug, Clone, Copy, Default)]
pub struct MpdStats {
    pub artists: i64,
    pub albums: i64,
    pub songs: i64,
    /// Total duration of the library, in seconds.
    pub db_playtime: i64,
}

pub async fn stats(pool: &sqlx::SqlitePool) -> AppResult<MpdStats> {
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT (SELECT COUNT(*) FROM artist),
               (SELECT COUNT(*) FROM album),
               (SELECT COUNT(*) FROM track WHERE is_available = 1),
               (SELECT COALESCE(SUM(duration_ms), 0) / 1000
                  FROM track WHERE is_available = 1)
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok(MpdStats {
        artists: row.0,
        albums: row.1,
        songs: row.2,
        db_playtime: row.3,
    })
}

impl MpdSong {
    /// Render into MPD's song fields.
    ///
    /// MPD sends seconds: `Time` as a whole number (legacy) and
    /// `duration` as a float (since 0.20). Clients read whichever they
    /// support, so both go out.
    pub fn write_into(&self, out: &mut super::protocol::Response) {
        out.push("file", &self.file_path);
        out.push("Last-Modified", format_last_modified(self.file_modified));
        out.push("Time", self.duration_ms.max(0) / 1000);
        out.push(
            "duration",
            format!("{:.3}", self.duration_ms.max(0) as f64 / 1000.0),
        );
        out.push("Title", &self.title);
        out.push_opt("Artist", self.artist_name.as_ref());
        out.push_opt("Album", self.album_title.as_ref());
        out.push_opt("AlbumArtist", self.album_artist_name.as_ref());
        out.push_opt("Track", self.track_number);
        out.push_opt("Disc", self.disc_number);
        out.push_opt("Date", self.year);
        out.push("Pos", self.position);
        out.push("Id", self.queue_id);
    }
}

/// `Last-Modified` is ISO-8601 UTC with a `Z` suffix.
///
/// `track.file_modified` holds epoch **milliseconds** — the scanner
/// writes `extracted.modified_ms` — so it is divided down before being
/// handed to chrono. Reading it as seconds would date every file to
/// somewhere in the far future.
fn format_last_modified(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> MpdSong {
        MpdSong {
            queue_id: 42,
            position: 3,
            file_path: "/music/a.flac".into(),
            title: "Creep".into(),
            duration_ms: 238_000,
            artist_name: Some("Radiohead".into()),
            album_title: Some("Pablo Honey".into()),
            album_artist_name: Some("Radiohead".into()),
            track_number: Some(2),
            disc_number: Some(1),
            year: Some(1993),
            file_modified: 0,
        }
    }

    #[test]
    fn writes_the_mpd_song_fields() {
        let mut out = super::super::protocol::Response::new();
        song().write_into(&mut out);
        let encoded = out.encode();
        assert!(encoded.contains("file: /music/a.flac\n"));
        assert!(encoded.contains("Title: Creep\n"));
        assert!(encoded.contains("Artist: Radiohead\n"));
        assert!(encoded.contains("Pos: 3\n"));
        assert!(encoded.contains("Id: 42\n"));
    }

    #[test]
    fn sends_both_the_legacy_and_float_duration() {
        let mut out = super::super::protocol::Response::new();
        song().write_into(&mut out);
        let encoded = out.encode();
        assert!(encoded.contains("Time: 238\n"));
        assert!(encoded.contains("duration: 238.000\n"));
    }

    #[test]
    fn omits_absent_tags_rather_than_sending_them_blank() {
        let mut bare = song();
        bare.album_title = None;
        bare.track_number = None;
        let mut out = super::super::protocol::Response::new();
        bare.write_into(&mut out);
        let encoded = out.encode();
        assert!(!encoded.contains("Album:"), "{encoded}");
        assert!(!encoded.contains("Track:"), "{encoded}");
        // AlbumArtist is a distinct tag and must survive Album going away.
        assert!(encoded.contains("AlbumArtist: Radiohead\n"));
    }

    #[test]
    fn last_modified_reads_the_column_as_milliseconds() {
        // `track.file_modified` is epoch millis, not seconds. Treating
        // it as seconds would put every file tens of thousands of years
        // out — this test is the guard on that unit.
        assert_eq!(format_last_modified(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_last_modified(1_700_000_000_000),
            "2023-11-14T22:13:20Z"
        );
    }
}
