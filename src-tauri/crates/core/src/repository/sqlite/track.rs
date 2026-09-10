//! SQLite implementation of [`TrackRepository`].

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::{
    domain::track::TrackRow,
    error::CoreResult,
    repository::track::{
        SortDirection, TrackListFilter, TrackRepository, TrackSort, TrackSortColumn, TrackSource,
    },
    search::{SearchPlan, LIKE_TERM_BINDS, LIKE_TERM_CLAUSE},
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

    async fn search(&self, plan: &SearchPlan, limit: i64) -> CoreResult<Vec<TrackRow>> {
        // Clamp `limit` so a caller passing 0 or a negative value (SQLite
        // treats negative LIMIT as "no limit") can't accidentally fetch
        // the entire index. The upper bound stays open since legitimate
        // callers want to pick their own ceiling.
        let bounded = limit.max(1);
        match plan {
            SearchPlan::Match(expr) => {
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
                    .bind(expr)
                    .bind(bounded)
                    .fetch_all(&self.pool)
                    .await?;
                Ok(rows)
            }
            SearchPlan::Like(patterns) => {
                // Straight off `track`, not off `track_fts`: below three
                // characters no index applies either way, and the real
                // columns are already joined here. `ORDER BY rank` is not
                // available on this route — there is no FTS match to rank
                // — so the order is the deterministic one.
                let mut sql = String::with_capacity(SELECT_TRACK_ROW.len() + 512);
                sql.push_str(SELECT_TRACK_ROW);
                sql.push_str(FROM_TRACK_BASE);
                sql.push_str("WHERE t.is_available = 1\n");
                for _ in patterns {
                    sql.push_str("  AND ");
                    sql.push_str(LIKE_TERM_CLAUSE);
                    sql.push('\n');
                }
                sql.push_str("ORDER BY t.title COLLATE NOCASE\nLIMIT ?");

                let mut q = sqlx::query_as::<_, TrackRow>(sqlx::AssertSqlSafe(sql));
                for p in patterns {
                    for _ in 0..LIKE_TERM_BINDS {
                        q = q.bind(p);
                    }
                }
                let rows = q.bind(bounded).fetch_all(&self.pool).await?;
                Ok(rows)
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::plan_search;

    /// Enough of the profile schema for `SELECT_TRACK_ROW` to resolve,
    /// plus the FTS index the search actually reads.
    ///
    /// The `track_fts` definition here MUST stay identical to
    /// `migrations/profile/20260910090000_track_fts_trigram.sql`. The
    /// migrations live in the app crate, out of reach from
    /// `waveflow-core` (same constraint the scanner fixtures note), so
    /// this is a copy — if the tokenizer changes there, change it here.
    async fn fixture_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE artwork (
                 id INTEGER PRIMARY KEY,
                 hash TEXT NOT NULL,
                 format TEXT NOT NULL
             );
             CREATE TABLE artist (
                 id INTEGER PRIMARY KEY,
                 name TEXT NOT NULL
             );
             CREATE TABLE album (
                 id INTEGER PRIMARY KEY,
                 title TEXT NOT NULL,
                 artist_id INTEGER REFERENCES artist(id),
                 artwork_id INTEGER REFERENCES artwork(id)
             );
             CREATE TABLE track (
                 id INTEGER PRIMARY KEY,
                 library_id INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 album_id INTEGER REFERENCES album(id),
                 primary_artist INTEGER REFERENCES artist(id),
                 duration_ms INTEGER NOT NULL DEFAULT 0,
                 track_number INTEGER, disc_number INTEGER, year INTEGER,
                 bitrate INTEGER, sample_rate INTEGER, channels INTEGER,
                 bit_depth INTEGER, codec TEXT, musical_key TEXT,
                 file_path TEXT NOT NULL, file_size INTEGER NOT NULL DEFAULT 0,
                 added_at INTEGER NOT NULL DEFAULT 0,
                 rating INTEGER,
                 is_available INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE track_artist (
                 track_id INTEGER NOT NULL,
                 artist_id INTEGER NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE VIRTUAL TABLE track_fts USING fts5(
                 title, album_title, artist_name,
                 tokenize='trigram remove_diacritics 1'
             );",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// One Chinese track, one Latin track, one unavailable track.
    async fn seed(pool: &SqlitePool) {
        sqlx::raw_sql(
            "INSERT INTO artist (id, name) VALUES (1, '周杰伦'), (2, 'U2');
             INSERT INTO album (id, title, artist_id) VALUES (1, '叶惠美', 1), (2, 'War', 2);
             INSERT INTO track (id, library_id, title, album_id, primary_artist, file_path)
             VALUES (1, 1, '中国人民解放军进行曲', 1, 1, '/a.flac'),
                    (2, 1, 'Sunday Bloody Sunday', 2, 2, '/b.flac');
             INSERT INTO track (id, library_id, title, album_id, primary_artist,
                                file_path, is_available)
             VALUES (3, 1, '中国人民解放军进行曲 (live)', 1, 1, '/c.flac', 0);
             INSERT INTO track_artist (track_id, artist_id, position)
             VALUES (1, 1, 0), (2, 2, 0), (3, 1, 0);
             INSERT INTO track_fts (rowid, title, album_title, artist_name)
             SELECT t.id, t.title,
                    COALESCE(al.title, ''), COALESCE(ar.name, '')
               FROM track t
               LEFT JOIN album  al ON al.id = t.album_id
               LEFT JOIN artist ar ON ar.id = t.primary_artist;",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn ids_for(pool: &SqlitePool, query: &str) -> Vec<i64> {
        let plan = plan_search(query).expect("query should produce a plan");
        let repo = SqliteTrackRepository::new(pool.clone());
        repo.search(&plan, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// The whole point of #579: characters from the MIDDLE of a Chinese
    /// title find it. Before trigram this returned nothing.
    #[tokio::test]
    async fn a_chinese_title_is_found_by_its_middle() {
        let pool = fixture_pool().await;
        seed(&pool).await;
        assert_eq!(ids_for(&pool, "人民解").await, vec![1]);
        // ...and by its start, which already worked and must not break.
        assert_eq!(ids_for(&pool, "中国人").await, vec![1]);
    }

    /// The fallback route is not a formality: two Han characters is the
    /// common shape of a Chinese word, and `MATCH` cannot serve it.
    #[tokio::test]
    async fn a_two_character_query_takes_the_like_route_and_still_matches() {
        let pool = fixture_pool().await;
        seed(&pool).await;
        assert_eq!(ids_for(&pool, "人民").await, vec![1]);
        // Short Latin names go the same way rather than regressing.
        assert_eq!(ids_for(&pool, "u2").await, vec![2]);
    }

    /// Album and artist are indexed alongside the title on the MATCH
    /// route, and searched alongside it on the LIKE one.
    #[tokio::test]
    async fn both_routes_reach_album_and_artist() {
        let pool = fixture_pool().await;
        seed(&pool).await;
        assert_eq!(ids_for(&pool, "叶惠美").await, vec![1]); // album, MATCH
        assert_eq!(ids_for(&pool, "惠美").await, vec![1]); // album, LIKE
        assert_eq!(ids_for(&pool, "周杰伦").await, vec![1]); // artist, MATCH
    }

    /// Multiple terms are an AND on both routes, not an OR.
    #[tokio::test]
    async fn terms_are_combined_with_and() {
        let pool = fixture_pool().await;
        seed(&pool).await;
        assert_eq!(ids_for(&pool, "sunday bloody").await, vec![2]);
        assert!(ids_for(&pool, "sunday 人民解").await.is_empty());
    }

    /// A track whose file went missing must not surface on either route
    /// — the seed has one, indexed, deliberately.
    #[tokio::test]
    async fn unavailable_tracks_stay_out_of_both_routes() {
        let pool = fixture_pool().await;
        seed(&pool).await;
        assert_eq!(ids_for(&pool, "解放军进").await, vec![1]);
        assert_eq!(ids_for(&pool, "人民").await, vec![1]);
    }

    /// An unescaped `%` on the LIKE route would return the whole
    /// library. It reaches SQLite as a literal instead.
    #[tokio::test]
    async fn a_percent_sign_is_not_a_wildcard() {
        let pool = fixture_pool().await;
        seed(&pool).await;
        assert!(ids_for(&pool, "%").await.is_empty());
        assert!(ids_for(&pool, "_").await.is_empty());
    }
}
