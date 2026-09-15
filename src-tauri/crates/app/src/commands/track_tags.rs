//! The custom tags the user's own files carry (#588).
//!
//! Two reads, deliberately separate because they answer different
//! questions at different moments:
//!
//! - [`list_track_tag_keys`] — "which keys does this library hold, and
//!   on how many tracks". Feeds the column picker, and the count is
//!   what makes that list usable: it separates the tag on every track
//!   from the one on three. Offering the *theoretical* list of frames
//!   each format allows would bury the five useful ones under
//!   thirty-five nobody has.
//! - [`list_track_tag_values`] — the values of the chosen keys, for
//!   every track at once. Fetched only while a `tag:` column is shown,
//!   which is the uncommon case.
//!
//! # Why the values are not in the listing query
//!
//! `browse::library_tracks_sql_where` builds one compound select whose
//! column list has to stay in step with a struct. A variable number of
//! joined tag columns cannot live there without generating the struct
//! too, so the values come across separately and the table stitches
//! them on by track id. The cost is one extra query per list load,
//! against an index built for exactly this shape.

use serde::Serialize;
use sqlx::Row;

use crate::error::AppResult;
use crate::state::AppState;

/// How many distinct keys the picker will offer.
///
/// A library assembled by several taggers over fifteen years can hold
/// hundreds; past the first few dozen the list stops being a choice and
/// becomes a search problem, which is a different feature.
const MAX_KEYS: i64 = 80;

#[derive(Debug, Clone, Serialize)]
pub struct TrackTagKey {
    pub key: String,
    pub count: i64,
}

#[tauri::command]
pub async fn list_track_tag_keys(state: tauri::State<'_, AppState>) -> AppResult<Vec<TrackTagKey>> {
    let pool = state.require_profile_pool().await?;
    // Only tags of tracks that are still on disk: a key surviving only
    // on rows the scanner has marked unavailable would offer a column
    // that renders empty everywhere.
    let rows = sqlx::query(
        "SELECT tt.key AS key, COUNT(*) AS count
           FROM track_tag tt
           JOIN track t ON t.id = tt.track_id AND t.is_available = 1
          GROUP BY tt.key
          ORDER BY count DESC, tt.key COLLATE NOCASE
          LIMIT ?",
    )
    .bind(MAX_KEYS)
    .fetch_all(&*pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(TrackTagKey {
                key: row.try_get("key")?,
                count: row.try_get("count")?,
            })
        })
        .collect()
}

/// `track_id` (as text, matching the listing's ids) → key → value.
pub type TagValues = std::collections::HashMap<String, std::collections::HashMap<String, String>>;

#[tauri::command]
pub async fn list_track_tag_values(
    state: tauri::State<'_, AppState>,
    keys: Vec<String>,
) -> AppResult<TagValues> {
    // Deduplicated and capped before a placeholder is built for each:
    // `keys` is the frontend's stored column layout, and SQLite refuses
    // a statement with more than 999 bound parameters. A layout with a
    // thousand tag columns is absurd, but the failure it would cause is
    // a query error rather than a missing column, which is far harder
    // to read. `MAX_KEYS` is the same ceiling the picker offers.
    let mut seen = std::collections::HashSet::new();
    let keys: Vec<String> = keys
        .into_iter()
        .filter(|key| !key.is_empty() && seen.insert(key.clone()))
        .take(MAX_KEYS as usize)
        .collect();
    if keys.is_empty() {
        return Ok(TagValues::new());
    }
    let pool = state.require_profile_pool().await?;

    // A bound placeholder per key rather than an interpolated list:
    // these strings come from the frontend's stored column layout, and
    // a tag key can hold anything a tagger chose to write.
    // `repeat().take()` and not `repeat_n`: the latter is Rust 1.82 and
    // the MSRV declared in `clippy.toml` is 1.80.
    let placeholders = std::iter::repeat("?")
        .take(keys.len())
        .collect::<Vec<_>>()
        .join(",");
    // Every matching row, deliberately not a page and not capped.
    //
    // A `LIMIT` here would not make the answer smaller, it would make it
    // wrong: the rows past the cap become blank cells in a column whose
    // only job is to show that tag, and nothing on screen would say the
    // value exists. Slow is recoverable, a confident blank is not.
    //
    // Paging by what is on screen trades that for a round trip on every
    // scroll and every sort -- and the fit-to-content measurement reads
    // a sample well past the viewport, so it would have to fetch again
    // anyway.
    //
    // The bound is narrower than "the whole library" reads: only tracks
    // that actually carry one of the chosen keys produce a row, only the
    // keys shown as columns are asked for (`MAX_KEYS` of them), and
    // nothing is fetched at all until a `tag:` column is added -- which
    // the caller notes is the uncommon case.
    let sql = format!(
        "SELECT tt.track_id AS track_id, tt.key AS key, tt.value AS value
           FROM track_tag tt
           JOIN track t ON t.id = tt.track_id AND t.is_available = 1
          WHERE tt.key IN ({placeholders})"
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    for key in &keys {
        query = query.bind(key);
    }
    let rows = query.fetch_all(&*pool).await?;

    let mut out = TagValues::new();
    for row in rows {
        let track_id: i64 = row.try_get("track_id")?;
        let key: String = row.try_get("key")?;
        let value: String = row.try_get("value")?;
        out.entry(track_id.to_string())
            .or_default()
            .insert(key, value);
    }
    Ok(out)
}
