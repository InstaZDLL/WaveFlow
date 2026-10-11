//! The library's Years tab: tracks grouped by the year their tags give,
//! and the tracks of one year.
//!
//! A year outside 1–9999 is a tag nobody meant (`0`, a full date pasted
//! into the field) and is counted with the tracks that have none, under
//! "Unknown year", rather than as a year of its own.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;
use sqlx::FromRow;

use crate::commands::browse::{detail_track, detail_tracks_sql, DetailTrackRaw};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The year a track is filed under, or `NULL` for "Unknown year".
const TRACK_YEAR: &str = "CASE WHEN t.year BETWEEN 1 AND 9999 THEN t.year END";

/// One year of the Years tab: how much it holds, and the cover of the
/// album with the most tracks in it to stand for it.
#[derive(Debug, Clone, Serialize)]
pub struct YearRow {
    /// `None` gathers the tracks without a usable year.
    pub year: Option<i64>,
    pub track_count: i64,
    pub album_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
}

/// Same `artwork_base` economy as `ListGenresResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct ListYearsResponse {
    pub artwork_base: String,
    pub items: Vec<YearRow>,
}

#[derive(FromRow)]
struct YearRawRow {
    year: Option<i64>,
    track_count: i64,
    album_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// Newest first, "Unknown year" last. The cover is the album with the most
/// of the year's tracks, the lowest id breaking a tie so it does not change
/// between two loads. Built from constants only.
fn list_years_sql() -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!(
        r#"
        WITH dated AS (
          SELECT t.album_id, {TRACK_YEAR} AS year
            FROM track t
           WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
        ),
        years AS (
          SELECT year,
                 COUNT(*)                 AS track_count,
                 COUNT(DISTINCT album_id) AS album_count
            FROM dated
           GROUP BY year
        ),
        covers AS (
          SELECT d.year, al.artwork_id,
                 ROW_NUMBER() OVER (
                   PARTITION BY d.year ORDER BY COUNT(*) DESC, al.id
                 ) AS rank
            FROM dated d
            JOIN album al ON al.id = d.album_id
           WHERE al.artwork_id IS NOT NULL
           GROUP BY d.year, al.id
        )
        SELECT y.year, y.track_count, y.album_count,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format
          FROM years y
          LEFT JOIN covers  c  ON c.year IS y.year AND c.rank = 1
          LEFT JOIN artwork aw ON aw.id = c.artwork_id
         ORDER BY y.year IS NULL, y.year DESC
        "#
    ))
}

/// Every year that has at least one available track in the given library
/// (all of them when `None`).
#[tauri::command]
pub async fn list_years(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
) -> AppResult<ListYearsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let raw = sqlx::query_as::<_, YearRawRow>(list_years_sql())
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&*pool)
        .await?;

    let items = expand_year_rows(raw, artwork_dir.clone()).await?;
    Ok(ListYearsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Thumbnail-existence flags, off the async thread like `expand_genre_rows`.
async fn expand_year_rows(raw: Vec<YearRawRow>, artwork_dir: PathBuf) -> AppResult<Vec<YearRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|row| {
                let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                YearRow {
                    year: row.year,
                    track_count: row.track_count,
                    album_count: row.album_count,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("year row expand join: {e}")))
}

#[derive(Debug, Clone, Serialize)]
pub struct YearDetail {
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub tracks: Vec<crate::commands::track::Track>,
}

/// The tracks of one year across the active profile (`None`: those with
/// no usable year), with the cover the album holding most of them carries.
#[tauri::command]
pub async fn get_year_detail(
    state: tauri::State<'_, AppState>,
    year: Option<i64>,
) -> AppResult<YearDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let rows =
        sqlx::query_as::<_, DetailTrackRaw>(detail_tracks_sql("", &format!("({TRACK_YEAR}) IS ?")))
            .bind(year)
            .fetch_all(&*pool)
            .await?;

    let tracks: Vec<crate::commands::track::Track> = rows
        .into_iter()
        .map(|row| detail_track(row, &artwork_dir))
        .collect();

    let cover = most_common_cover(&tracks);
    Ok(YearDetail {
        year,
        track_count: tracks.len() as i64,
        total_duration_ms: tracks.iter().map(|t| t.duration_ms).sum(),
        artwork_path: cover.and_then(|t| t.artwork_path.clone()),
        artwork_path_1x: cover.and_then(|t| t.artwork_path_1x.clone()),
        artwork_path_2x: cover.and_then(|t| t.artwork_path_2x.clone()),
        tracks,
    })
}

/// The first track of the album with the most tracks that has a cover, the
/// same choice `list_years` makes, so the page shows the tile's picture.
fn most_common_cover(
    tracks: &[crate::commands::track::Track],
) -> Option<&crate::commands::track::Track> {
    let mut counts: HashMap<i64, (usize, &crate::commands::track::Track)> = HashMap::new();
    for track in tracks {
        if let (Some(album), Some(_)) = (track.album_id, track.artwork_path.as_ref()) {
            counts.entry(album).or_insert((0, track)).0 += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|(a_id, (a_n, _)), (b_id, (b_n, _))| a_n.cmp(b_n).then(b_id.cmp(a_id)))
        .map(|(_, (_, track))| track)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use std::str::FromStr;

    /// The real migrator, `foreign_keys` on, as the browse tests do.
    async fn pool() -> SqlitePool {
        let options = waveflow_core::repository::sqlite::collation::register(
            SqliteConnectOptions::from_str(":memory:")
                .unwrap()
                .foreign_keys(true),
        );
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

    /// Two albums in 1997, the second with more tracks and a cover; one
    /// track in 2003; one with no year, one tagged `0`, one unavailable.
    async fn seed(pool: &SqlitePool) {
        let mut statements = vec![
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)"
                .to_string(),
            "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')".to_string(),
            "INSERT INTO artwork (id, hash, format, width, height, source, created_at)
             VALUES (1, 'c0ffee', 'jpg', 1, 1, 'embedded', 0)"
                .to_string(),
            "INSERT INTO album (id, title, canonical_title, artist_id, is_compilation)
             VALUES (1, 'One', 'one', 1, 0), (2, 'Two', 'two', 1, 0)"
                .to_string(),
            "UPDATE album SET artwork_id = 1 WHERE id = 2".to_string(),
        ];
        let tracks: [(i64, Option<i64>, Option<i64>, i64); 7] = [
            (1, Some(1997), Some(1), 1),
            (2, Some(1997), Some(2), 1),
            (3, Some(1997), Some(2), 1),
            (4, Some(2003), Some(1), 1),
            (5, None, None, 1),
            (6, Some(0), None, 1),
            (7, Some(1997), Some(1), 0),
        ];
        for (id, year, album, available) in tracks {
            let year = year.map_or("NULL".into(), |y| y.to_string());
            let album = album.map_or("NULL".into(), |a| a.to_string());
            statements.push(format!(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                    title, album_id, year, duration_ms, added_at, is_available,
                                    hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical,
                                    primary_artist)
                 VALUES ({id}, 1, '/m/{id}.flac', 'h{id}', 1, 0, 'T{id}', {album}, {year},
                         1000, 0, {available}, 0, 0, 0, 0, 1)"
            ));
        }
        for statement in statements {
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
                .execute(pool)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn years_newest_first_unknown_last() {
        let pool = pool().await;
        seed(&pool).await;
        let rows = sqlx::query_as::<_, YearRawRow>(list_years_sql())
            .bind(None::<i64>)
            .bind(None::<i64>)
            .fetch_all(&pool)
            .await
            .unwrap();
        let summary: Vec<_> = rows
            .iter()
            .map(|r| (r.year, r.track_count, r.album_count, r.artwork_hash.clone()))
            .collect();
        assert_eq!(
            summary,
            [
                (Some(2003), 1, 1, None),
                // The unavailable track is not counted; album 2 holds most
                // of the year and lends its cover.
                (Some(1997), 3, 2, Some("c0ffee".to_string())),
                // No year and a year of 0 are the same bucket.
                (None, 2, 0, None),
            ]
        );
    }

    #[tokio::test]
    async fn one_year_and_the_unknown_bucket() {
        let pool = pool().await;
        seed(&pool).await;
        let sql = || detail_tracks_sql("", &format!("({TRACK_YEAR}) IS ?"));
        let ids = |rows: Vec<DetailTrackRaw>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();

        let mut in_1997 = ids(sqlx::query_as::<_, DetailTrackRaw>(sql())
            .bind(Some(1997_i64))
            .fetch_all(&pool)
            .await
            .unwrap());
        in_1997.sort();
        assert_eq!(in_1997, [1, 2, 3]);

        let mut unknown = ids(sqlx::query_as::<_, DetailTrackRaw>(sql())
            .bind(None::<i64>)
            .fetch_all(&pool)
            .await
            .unwrap());
        unknown.sort();
        assert_eq!(unknown, [5, 6]);
    }
}
