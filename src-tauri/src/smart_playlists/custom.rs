//! User-defined smart playlists.
//!
//! Mirrors the structured filters from [`crate::commands::track::SearchFilters`]
//! but persists them as JSON inside `playlist.smart_rules` so the same
//! row can be re-materialized on demand. Track membership lives in the
//! regular `playlist_track` table — once materialized, downstream views
//! treat the playlist exactly like a manual one.
//!
//! Refresh strategy: the rule set is re-evaluated on demand
//! ([`regenerate`] command) and once at app startup so any tracks added
//! to the library since the last app run get picked up. Live re-materialize
//! on every library write would be wasteful (a 10k import would burn it
//! once per file); the on-startup pass is the practical compromise.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::{AppError, AppResult};

/// Sort order applied to the rule set's results before truncation. The
/// SQL fragments are hand-rolled because dynamic ORDER BY through binds
/// isn't allowed in SQLite — keep this enum in sync with [`order_by_sql`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomSort {
    AddedDesc,
    AddedAsc,
    YearDesc,
    YearAsc,
    TitleAsc,
    ArtistAsc,
    Random,
}

impl Default for CustomSort {
    fn default() -> Self {
        CustomSort::AddedDesc
    }
}

fn order_by_sql(sort: &CustomSort) -> &'static str {
    match sort {
        CustomSort::AddedDesc => "t.added_at DESC",
        CustomSort::AddedAsc => "t.added_at ASC",
        CustomSort::YearDesc => "COALESCE(t.year, 0) DESC, t.title ASC",
        CustomSort::YearAsc => "COALESCE(t.year, 9999) ASC, t.title ASC",
        CustomSort::TitleAsc => "t.title ASC",
        CustomSort::ArtistAsc =>
            "COALESCE((SELECT name FROM artist WHERE id = t.primary_artist), '') ASC, \
             t.title ASC",
        CustomSort::Random => "RANDOM()",
    }
}

/// Editable rule set. Every field is optional so the user can pile up
/// only the predicates they care about. An empty rule set materializes
/// every available track in the library — useful for "all my music
/// shuffled" but documented in the UI so it isn't a footgun.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustomRules {
    /// Substring match on track title (case-insensitive).
    #[serde(default)]
    pub title_contains: Option<String>,
    /// Substring match on any of the linked artist names.
    #[serde(default)]
    pub artist_contains: Option<String>,
    /// Substring match on the album title.
    #[serde(default)]
    pub album_contains: Option<String>,
    /// Whitelist of genre IDs (OR-combined). Tracks must link to at
    /// least one of these via `track_genre`.
    #[serde(default)]
    pub genre_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub year_min: Option<i64>,
    #[serde(default)]
    pub year_max: Option<i64>,
    #[serde(default)]
    pub bpm_min: Option<f64>,
    #[serde(default)]
    pub bpm_max: Option<f64>,
    #[serde(default)]
    pub duration_min_ms: Option<i64>,
    #[serde(default)]
    pub duration_max_ms: Option<i64>,
    /// Whitelist of file extensions (lowercase, no dot). OR-combined.
    #[serde(default)]
    pub formats: Option<Vec<String>>,
    /// Hi-Res = sample rate ≥ 88.2 kHz OR bit depth ≥ 24.
    #[serde(default)]
    pub hi_res_only: Option<bool>,
    #[serde(default)]
    pub liked_only: Option<bool>,
    /// Minimum POPM rating (0-255) the track must carry. Maps from the
    /// editor's 1-5 star picker via `stars * 255 / 5` so a "3 stars
    /// or more" filter matches everything POPM ≥ 153. `None` = no
    /// rating filter (default).
    #[serde(default)]
    pub rating_min: Option<i64>,
    /// Sort applied before truncation. `None` defaults to AddedDesc.
    #[serde(default)]
    pub sort: Option<CustomSort>,
    /// Hard cap on track count after sorting. `None` = no cap. Capped
    /// at 5_000 server-side so a typo doesn't blow up the queue.
    #[serde(default)]
    pub limit: Option<i64>,
}

const HARD_LIMIT: i64 = 5_000;

/// Re-materialize the playlist's tracks from its rule set. Wipes
/// `playlist_track` rows for the playlist, runs the rule query, then
/// re-inserts the results in the sorted order. The rule set is read
/// from `playlist.smart_rules` so this command is idempotent (calling
/// it twice yields the same membership unless the library changed).
pub async fn materialize(pool: &SqlitePool, playlist_id: i64, rules: &CustomRules) -> AppResult<i64> {
    // 1. Resolve the matching track ids in the requested sort order.
    let track_ids = run_query(pool, rules).await?;

    // 2. Replace the membership in a single transaction so a partial
    //    failure doesn't leave the playlist half-empty.
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM playlist_track WHERE playlist_id = ?")
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;

    let now = chrono::Utc::now().timestamp_millis();
    for (idx, track_id) in track_ids.iter().enumerate() {
        sqlx::query(
            "INSERT INTO playlist_track (playlist_id, track_id, position, added_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(playlist_id)
        .bind(track_id)
        .bind(idx as i64)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(playlist_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(track_ids.len() as i64)
}

/// Resolve the rule set into a list of track ids in the canonical sort
/// order. Public for the dry-run "Preview" button in the rule editor.
pub async fn run_query(pool: &SqlitePool, rules: &CustomRules) -> AppResult<Vec<i64>> {
    let mut sql = String::from(
        "SELECT DISTINCT t.id
           FROM track t
           LEFT JOIN album al ON al.id = t.album_id",
    );
    let mut joins = Vec::<&'static str>::new();
    let mut where_clauses = Vec::<String>::new();
    let mut binds = Vec::<BindValue>::new();

    where_clauses.push("t.is_available = 1".to_string());

    if rules.title_contains.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
        where_clauses.push("t.title LIKE ? COLLATE NOCASE".to_string());
        binds.push(BindValue::Text(format!(
            "%{}%",
            rules.title_contains.as_ref().unwrap().trim()
        )));
    }
    if rules.artist_contains.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
        joins.push(
            "LEFT JOIN track_artist ta_s ON ta_s.track_id = t.id \
             LEFT JOIN artist ar_s ON ar_s.id = ta_s.artist_id",
        );
        where_clauses.push("ar_s.name LIKE ? COLLATE NOCASE".to_string());
        binds.push(BindValue::Text(format!(
            "%{}%",
            rules.artist_contains.as_ref().unwrap().trim()
        )));
    }
    if rules.album_contains.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
        where_clauses.push("al.title LIKE ? COLLATE NOCASE".to_string());
        binds.push(BindValue::Text(format!(
            "%{}%",
            rules.album_contains.as_ref().unwrap().trim()
        )));
    }
    if let Some(ids) = rules.genre_ids.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = vec!["?"; ids.len()].join(",");
        where_clauses.push(format!(
            "EXISTS (SELECT 1 FROM track_genre tg WHERE tg.track_id = t.id AND tg.genre_id IN ({}))",
            placeholders
        ));
        for id in ids {
            binds.push(BindValue::Int(*id));
        }
    }
    if let Some(y) = rules.year_min {
        where_clauses.push("t.year IS NOT NULL AND t.year >= ?".to_string());
        binds.push(BindValue::Int(y));
    }
    if let Some(y) = rules.year_max {
        where_clauses.push("t.year IS NOT NULL AND t.year <= ?".to_string());
        binds.push(BindValue::Int(y));
    }
    if rules.bpm_min.is_some() || rules.bpm_max.is_some() {
        joins.push("LEFT JOIN track_analysis ana ON ana.track_id = t.id");
        if let Some(bpm) = rules.bpm_min {
            where_clauses.push("ana.bpm IS NOT NULL AND ana.bpm >= ?".to_string());
            binds.push(BindValue::Real(bpm));
        }
        if let Some(bpm) = rules.bpm_max {
            where_clauses.push("ana.bpm IS NOT NULL AND ana.bpm <= ?".to_string());
            binds.push(BindValue::Real(bpm));
        }
    }
    if let Some(d) = rules.duration_min_ms {
        where_clauses.push("t.duration_ms >= ?".to_string());
        binds.push(BindValue::Int(d));
    }
    if let Some(d) = rules.duration_max_ms {
        where_clauses.push("t.duration_ms <= ?".to_string());
        binds.push(BindValue::Int(d));
    }
    if let Some(formats) = rules.formats.as_ref().filter(|v| !v.is_empty()) {
        let placeholders = vec!["LOWER(?)"; formats.len()].join(",");
        where_clauses.push(format!(
            "LOWER(t.codec) IN ({})",
            placeholders
        ));
        for fmt in formats {
            binds.push(BindValue::Text(fmt.to_lowercase()));
        }
    }
    if rules.hi_res_only == Some(true) {
        where_clauses
            .push("(t.sample_rate >= 88200 OR t.bit_depth >= 24)".to_string());
    }
    if rules.liked_only == Some(true) {
        where_clauses.push(
            "EXISTS (SELECT 1 FROM liked_track lt WHERE lt.track_id = t.id)".to_string(),
        );
    }
    if let Some(rating) = rules.rating_min.filter(|r| *r > 0) {
        where_clauses.push("t.rating IS NOT NULL AND t.rating >= ?".to_string());
        binds.push(BindValue::Int(rating.clamp(1, 255)));
    }

    // Splice JOINs in (deduped — adding the same JOIN twice would
    // explode row counts via the cross product).
    let mut seen = std::collections::HashSet::new();
    for j in &joins {
        if seen.insert(*j) {
            sql.push(' ');
            sql.push_str(j);
        }
    }
    if !where_clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY ");
    sql.push_str(order_by_sql(rules.sort.as_ref().unwrap_or(&CustomSort::AddedDesc)));

    let limit = rules.limit.unwrap_or(HARD_LIMIT).clamp(1, HARD_LIMIT);
    sql.push_str(" LIMIT ?");
    binds.push(BindValue::Int(limit));

    let mut q = sqlx::query_scalar::<_, i64>(&sql);
    for b in binds {
        q = match b {
            BindValue::Int(v) => q.bind(v),
            BindValue::Real(v) => q.bind(v),
            BindValue::Text(v) => q.bind(v),
        };
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Other(format!("custom smart playlist query failed: {e}")))?;
    Ok(rows)
}

enum BindValue {
    Int(i64),
    Real(f64),
    Text(String),
}
