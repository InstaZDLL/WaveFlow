//! SQLite implementation of [`TrackRepository`].

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::{
    domain::track::TrackRow,
    error::CoreResult,
    repository::track::{
        SortDirection, TrackListFilter, TrackRepository, TrackSort, TrackSortColumn, TrackSource,
    },
};

#[derive(Debug, Clone)]
pub struct SqliteTrackRepository {
    pool: SqlitePool,
}

impl SqliteTrackRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

/// Shared `SELECT … FROM track t LEFT JOIN album / artist / artwork`
/// projection. Every method that returns [`TrackRow`] glues this
/// prefix in front of its `WHERE … ORDER BY …` tail.
const SELECT_TRACK_ROW: &str = r#"
SELECT t.id, t.library_id, t.title,
       t.album_id,
       al.title AS album_title,
       t.primary_artist AS artist_id,
       (SELECT GROUP_CONCAT(name, ', ') FROM (
          SELECT ar2.name FROM track_artist ta2
          JOIN artist ar2 ON ar2.id = ta2.artist_id
          WHERE ta2.track_id = t.id
          ORDER BY ta2.position
       )) AS artist_name,
       (SELECT GROUP_CONCAT(id, ',') FROM (
          SELECT ta2.artist_id AS id FROM track_artist ta2
          WHERE ta2.track_id = t.id
          ORDER BY ta2.position
       )) AS artist_ids,
       t.duration_ms, t.track_number, t.disc_number, t.year,
       t.bitrate, t.sample_rate, t.channels,
       t.bit_depth, t.codec, t.musical_key,
       t.file_path, t.file_size, t.added_at,
       aw.hash   AS artwork_hash,
       aw.format AS artwork_format,
       t.rating  AS rating
"#;

const FROM_TRACK_BASE: &str = r#"
FROM track t
LEFT JOIN album   al ON al.id = t.album_id
LEFT JOIN artist  ar ON ar.id = t.primary_artist
LEFT JOIN artwork aw ON aw.id = al.artwork_id
"#;

/// Map a [`TrackSort`] to a whitelisted `ORDER BY` clause — never
/// interpolate user input, the enum guarantees a finite output set.
fn order_clause(sort: TrackSort) -> &'static str {
    use SortDirection::{Asc, Desc};
    use TrackSortColumn as C;

    let dir = sort.direction.unwrap_or(match sort.column {
        C::Rating | C::DurationMs | C::AddedAt | C::Year => Desc,
        _ => Asc,
    });

    match (sort.column, dir) {
        (C::Default, _) => {
            "ORDER BY ar.canonical_name COLLATE NOCASE,\n                  al.canonical_title COLLATE NOCASE,\n                  t.disc_number,\n                  t.track_number,\n                  t.title COLLATE NOCASE"
        }
        (C::Title, Asc) => "ORDER BY t.title COLLATE NOCASE ASC",
        (C::Title, Desc) => "ORDER BY t.title COLLATE NOCASE DESC",
        (C::Artist, Asc) => {
            "ORDER BY ar.canonical_name COLLATE NOCASE ASC, t.title COLLATE NOCASE"
        }
        (C::Artist, Desc) => {
            "ORDER BY ar.canonical_name COLLATE NOCASE DESC, t.title COLLATE NOCASE"
        }
        (C::Album, Asc) => {
            "ORDER BY al.canonical_title COLLATE NOCASE ASC, t.disc_number, t.track_number"
        }
        (C::Album, Desc) => {
            "ORDER BY al.canonical_title COLLATE NOCASE DESC, t.disc_number, t.track_number"
        }
        (C::DurationMs, Asc) => "ORDER BY t.duration_ms ASC",
        (C::DurationMs, Desc) => "ORDER BY t.duration_ms DESC",
        (C::Year, Asc) => "ORDER BY t.year ASC, t.title COLLATE NOCASE",
        (C::Year, Desc) => "ORDER BY t.year DESC, t.title COLLATE NOCASE",
        (C::AddedAt, Asc) => "ORDER BY t.added_at ASC",
        (C::AddedAt, Desc) => "ORDER BY t.added_at DESC",
        (C::Rating, Asc) => "ORDER BY t.rating ASC, t.title COLLATE NOCASE",
        (C::Rating, Desc) => "ORDER BY t.rating DESC, t.title COLLATE NOCASE",
    }
}

#[async_trait]
impl TrackRepository for SqliteTrackRepository {
    async fn get(&self, id: i64) -> CoreResult<Option<TrackRow>> {
        let sql = format!("{SELECT_TRACK_ROW}{FROM_TRACK_BASE} WHERE t.id = ?");
        let row = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    async fn list(&self, filter: TrackListFilter, sort: TrackSort) -> CoreResult<Vec<TrackRow>> {
        let order = order_clause(sort);
        let sql = format!(
            "{SELECT_TRACK_ROW}{FROM_TRACK_BASE} \
             WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1\n{order}"
        );
        let rows = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql))
            .bind(filter.library_id)
            .bind(filter.library_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn list_in_playlist(&self, playlist_id: i64) -> CoreResult<Vec<TrackRow>> {
        let sql = format!(
            "{SELECT_TRACK_ROW} \
             FROM playlist_track pt \
             JOIN track   t  ON t.id  = pt.track_id \
             LEFT JOIN album   al ON al.id = t.album_id \
             LEFT JOIN artist  ar ON ar.id = t.primary_artist \
             LEFT JOIN artwork aw ON aw.id = al.artwork_id \
             WHERE pt.playlist_id = ? AND t.is_available = 1 \
             ORDER BY pt.position ASC"
        );
        let rows = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql))
            .bind(playlist_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn list_liked(&self) -> CoreResult<Vec<TrackRow>> {
        let sql = format!(
            "{SELECT_TRACK_ROW} \
             FROM liked_track lt \
             JOIN track   t  ON t.id  = lt.track_id \
             LEFT JOIN album   al ON al.id = t.album_id \
             LEFT JOIN artist  ar ON ar.id = t.primary_artist \
             LEFT JOIN artwork aw ON aw.id = al.artwork_id \
             WHERE t.is_available = 1 \
             ORDER BY lt.liked_at DESC"
        );
        let rows = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn search_fts(&self, fts_query: &str, limit: i64) -> CoreResult<Vec<TrackRow>> {
        // Clamp `limit` so a caller passing 0 or a negative value (SQLite
        // treats negative LIMIT as "no limit") can't accidentally fetch
        // the entire FTS index. The upper bound stays open since legitimate
        // callers want to pick their own ceiling.
        let bounded = limit.max(1);
        let sql = format!(
            "{SELECT_TRACK_ROW} \
             FROM track_fts fts \
             JOIN track   t  ON t.id  = fts.rowid \
             LEFT JOIN album   al ON al.id = t.album_id \
             LEFT JOIN artist  ar ON ar.id = t.primary_artist \
             LEFT JOIN artwork aw ON aw.id = al.artwork_id \
             WHERE track_fts MATCH ? AND t.is_available = 1 \
             ORDER BY rank \
             LIMIT ?"
        );
        let rows = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql))
            .bind(fts_query)
            .bind(bounded)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    async fn list_ids_in_source(&self, source: TrackSource) -> CoreResult<Vec<i64>> {
        let (sql, id) = match source {
            TrackSource::Folder(id) => (
                "SELECT id FROM track WHERE folder_id = ? AND is_available = 1
                 ORDER BY disc_number, track_number, title COLLATE NOCASE",
                id,
            ),
            TrackSource::Album(id) => (
                "SELECT id FROM track WHERE album_id = ? AND is_available = 1
                 ORDER BY disc_number, track_number, title COLLATE NOCASE",
                id,
            ),
            TrackSource::Artist(id) => (
                "SELECT id FROM track WHERE primary_artist = ? AND is_available = 1
                 ORDER BY title COLLATE NOCASE",
                id,
            ),
        };
        let ids = sqlx::query_scalar(sql)
            .bind(id)
            .fetch_all(&self.pool)
            .await?;
        Ok(ids)
    }

    async fn liked_ids(&self) -> CoreResult<Vec<i64>> {
        let ids = sqlx::query_scalar("SELECT track_id FROM liked_track ORDER BY liked_at DESC")
            .fetch_all(&self.pool)
            .await?;
        Ok(ids)
    }

    async fn is_liked(&self, track_id: i64) -> CoreResult<bool> {
        let row: Option<i64> =
            sqlx::query_scalar("SELECT track_id FROM liked_track WHERE track_id = ?")
                .bind(track_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    async fn like(&self, track_id: i64, now_ms: i64) -> CoreResult<()> {
        sqlx::query("INSERT OR IGNORE INTO liked_track (track_id, liked_at) VALUES (?, ?)")
            .bind(track_id)
            .bind(now_ms)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn unlike(&self, track_id: i64) -> CoreResult<()> {
        sqlx::query("DELETE FROM liked_track WHERE track_id = ?")
            .bind(track_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn get_file_path(&self, track_id: i64) -> CoreResult<Option<String>> {
        let path: Option<String> = sqlx::query_scalar("SELECT file_path FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(path)
    }

    async fn set_rating(&self, track_id: i64, rating: Option<u8>) -> CoreResult<()> {
        sqlx::query("UPDATE track SET rating = ? WHERE id = ?")
            .bind(rating.map(|r| r as i64))
            .bind(track_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
