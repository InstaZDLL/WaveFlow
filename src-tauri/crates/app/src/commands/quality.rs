//! The library's Quality tab: what the audio of each file says about it,
//! rather than what its container claims.
//!
//! Reads only. The verdict is [`waveflow_core::quality_verdict_sql!`],
//! computed where it is read from what the analysis stored
//! (`track_analysis.cutoff_hz`, `bandwidth_version`), so a threshold that
//! moves is a code change and never a re-analysis — and the tab, Track
//! Properties, the "needs attention" category and the `fake_lossless`
//! rule cannot disagree about a file. Asking for the files to be measured
//! again goes through the analysis sweep
//! ([`super::analysis::reanalyze_lossless`]).
//!
//! Local tracks only, and available ones: a server track has no analysis
//! here, and a file that has gone has nothing to measure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqlx::{Row, SqlitePool};

use waveflow_core::analysis::bandwidth::QUALITY_VERDICTS;
use waveflow_core::audio_format::format_filter::formats_condition;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The most rows one page may ask for. The tab asks for fewer; this is
/// the bound on a caller that asks for more.
const MAX_PAGE: i64 = 500;

/// One verdict and how many files carry it.
#[derive(Debug, Clone, Serialize)]
pub struct QualityCount {
    pub verdict: &'static str,
    pub count: i64,
}

/// The counts the tab's header shows, over the whole library.
#[derive(Debug, Clone, Serialize)]
pub struct QualitySummary {
    /// Every verdict, in [`QUALITY_VERDICTS`] order, zeros included: a
    /// list whose shape changes between two visits is hard to learn.
    pub verdicts: Vec<QualityCount>,
    /// Lossless files: the only ones the measure concerns.
    pub lossless: i64,
    /// Of those, the ones measured by the current pass.
    pub measured: i64,
    /// When the last lossless file was measured, in epoch milliseconds.
    pub last_analyzed_at: Option<i64>,
}

/// One row of the tab's table.
#[derive(Debug, Clone, Serialize)]
pub struct QualityTrackRow {
    pub id: i64,
    pub title: String,
    /// The credit, every artist in order.
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub codec: Option<String>,
    pub bit_depth: Option<i64>,
    pub sample_rate: Option<i64>,
    pub bitrate: Option<i64>,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    /// Where the highs stop, in hertz; `None` when they never hit a wall
    /// or were never measured — the verdict tells the two apart.
    pub cutoff_hz: Option<f64>,
    pub verdict: String,
}

/// One page of the table, and how many rows the filter holds in all.
#[derive(Debug, Clone, Serialize)]
pub struct QualityTracksPage {
    pub total: i64,
    pub items: Vec<QualityTrackRow>,
}

/// Every available local track with its verdict, as the subquery the
/// two commands read from. `t` and `ta` are the aliases the verdict
/// macros expect.
const VERDICTS_FROM: &str = concat!(
    "SELECT t.id AS id, ",
    waveflow_core::quality_verdict_sql!(),
    " AS verdict, ta.cutoff_hz AS cutoff_hz, ta.analyzed_at AS analyzed_at
       FROM track t
       LEFT JOIN track_analysis ta ON ta.track_id = t.id
      WHERE t.is_available = 1"
);

/// The counts per verdict, the lossless files measured out of all, and
/// when the last was.
#[tauri::command]
pub async fn library_quality_summary(
    state: tauri::State<'_, AppState>,
) -> AppResult<QualitySummary> {
    let pool = state.require_profile_pool().await?;
    read_summary(&pool).await
}

/// [`library_quality_summary`], on a pool.
async fn read_summary(pool: &SqlitePool) -> AppResult<QualitySummary> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT verdict, COUNT(*) AS n,
                MAX(CASE WHEN verdict NOT IN ('lossy', 'not_measured', 'pending')
                         THEN analyzed_at END) AS last
           FROM ({VERDICTS_FROM})
          GROUP BY verdict"
    )))
    .fetch_all(pool)
    .await?;
    let mut counts: HashMap<String, i64> = HashMap::new();
    let mut last_analyzed_at: Option<i64> = None;
    for row in rows {
        let verdict: String = row.try_get("verdict")?;
        counts.insert(verdict, row.try_get("n")?);
        let last: Option<i64> = row.try_get("last")?;
        last_analyzed_at = last_analyzed_at.max(last);
    }
    Ok(summarize(&counts, last_analyzed_at))
}

/// The summary from the counts per verdict. Split out so the arithmetic
/// is tested without a profile.
fn summarize(counts: &HashMap<String, i64>, last_analyzed_at: Option<i64>) -> QualitySummary {
    let count = |verdict: &str| counts.get(verdict).copied().unwrap_or(0);
    let verdicts = QUALITY_VERDICTS
        .iter()
        .map(|&verdict| QualityCount {
            verdict,
            count: count(verdict),
        })
        .collect();
    // Lossless is everything the measure applies to; measured is all of
    // it but the files still waiting.
    let lossless: i64 = QUALITY_VERDICTS
        .iter()
        .filter(|v| !matches!(**v, "lossy" | "not_measured"))
        .map(|v| count(v))
        .sum();
    QualitySummary {
        verdicts,
        lossless,
        measured: lossless - count("pending"),
        last_analyzed_at,
    }
}

/// The `ORDER BY` position of each verdict: worst news first, then the
/// lowest wall, so the likeliest fakes open the list.
fn verdict_rank_sql() -> String {
    let mut sql = String::from("CASE q.verdict");
    for (rank, verdict) in QUALITY_VERDICTS.iter().enumerate() {
        // Literals from a constant list, never from the caller.
        sql.push_str(&format!(" WHEN '{verdict}' THEN {rank}"));
    }
    sql.push_str(&format!(" ELSE {} END", QUALITY_VERDICTS.len()));
    sql
}

/// One page of the files with `verdict` (every verdict when `None`) in
/// `format` (every format when `None`), worst news first. `format` is a
/// name as the search panel's chips use it (`FLAC`, `ALAC`, `OGG`…),
/// read through [`formats_condition`]. The count and the page are read
/// in one transaction, so the total always describes the rows beside it.
#[tauri::command]
pub async fn library_quality_tracks(
    state: tauri::State<'_, AppState>,
    verdict: Option<String>,
    format: Option<String>,
    offset: i64,
    limit: i64,
) -> AppResult<QualityTracksPage> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    read_page(
        &pool,
        artwork_dir,
        verdict.as_deref(),
        format.as_deref(),
        offset,
        limit,
    )
    .await
}

/// [`library_quality_tracks`], on a pool.
async fn read_page(
    pool: &SqlitePool,
    artwork_dir: PathBuf,
    verdict: Option<&str>,
    format: Option<&str>,
    offset: i64,
    limit: i64,
) -> AppResult<QualityTracksPage> {
    if let Some(v) = verdict {
        if !QUALITY_VERDICTS.contains(&v) {
            return Err(AppError::Other(format!("unknown quality verdict: {v}")));
        }
    }
    let offset = offset.max(0);
    let limit = limit.clamp(1, MAX_PAGE);

    // The format condition speaks of `t`, so the filter joins the track
    // again rather than widening the verdict subquery.
    let (format_sql, format_values) = match format {
        Some(name) if !name.is_empty() => formats_condition(&[name]),
        _ => ("1".to_string(), Vec::new()),
    };
    let filter = format!(
        "FROM ({VERDICTS_FROM}) q
         JOIN track t ON t.id = q.id
         LEFT JOIN album al ON al.id = t.album_id
         LEFT JOIN artwork aw ON aw.id = COALESCE(al.artwork_id, t.artwork_id)
        WHERE (? IS NULL OR q.verdict = ?)
          AND {format_sql}"
    );

    let mut tx = pool.begin().await?;
    let mut count =
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) {filter}")))
            .bind(verdict)
            .bind(verdict);
    for value in &format_values {
        count = count.bind(value);
    }
    let total = count.fetch_one(&mut *tx).await?;

    let sql = format!(
        "SELECT t.id, t.title, al.title AS album_title,
                (SELECT GROUP_CONCAT(name, ', ') FROM (
                   SELECT ar.name FROM track_artist tar
                   JOIN artist ar ON ar.id = tar.artist_id
                   WHERE tar.track_id = t.id
                   ORDER BY tar.position
                )) AS artist_name,
                t.codec, t.bit_depth, t.sample_rate, t.bitrate,
                aw.hash AS artwork_hash, aw.format AS artwork_format,
                q.cutoff_hz, q.verdict
           {filter}
          ORDER BY {rank}, q.cutoff_hz IS NULL, q.cutoff_hz, t.title COLLATE FOLD, t.id
          LIMIT ? OFFSET ?",
        rank = verdict_rank_sql(),
    );
    let mut page = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(verdict)
        .bind(verdict);
    for value in &format_values {
        page = page.bind(value);
    }
    let rows = page.bind(limit).bind(offset).fetch_all(&mut *tx).await?;
    tx.commit().await?;

    let items = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .map(|row| to_row(&row, &artwork_dir))
            .collect::<Result<Vec<_>, sqlx::Error>>()
    })
    .await
    .map_err(|e| AppError::Other(format!("quality rows task panicked: {e}")))??;

    Ok(QualityTracksPage { total, items })
}

/// A database row as the table's, its cover's thumbnails looked up on
/// disk (hence off the reactor).
fn to_row(
    row: &sqlx::sqlite::SqliteRow,
    artwork_dir: &Path,
) -> Result<QualityTrackRow, sqlx::Error> {
    let hash: Option<String> = row.try_get("artwork_hash")?;
    let format: Option<String> = row.try_get("artwork_format")?;
    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (hash, format) {
        (Some(hash), Some(format)) => {
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, &hash);
            let full = artwork_dir.join(format!("{hash}.{format}"));
            (Some(full.to_string_lossy().into_owned()), p1, p2)
        }
        _ => (None, None, None),
    };
    Ok(QualityTrackRow {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        artist_name: row.try_get("artist_name")?,
        album_title: row.try_get("album_title")?,
        codec: row.try_get("codec")?,
        bit_depth: row.try_get("bit_depth")?,
        sample_rate: row.try_get("sample_rate")?,
        bitrate: row.try_get("bitrate")?,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
        cutoff_hz: row.try_get("cutoff_hz")?,
        verdict: row.try_get("verdict")?,
    })
}

#[cfg(test)]
mod tests;
