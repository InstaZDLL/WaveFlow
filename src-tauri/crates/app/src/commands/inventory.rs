//! The library's "needs attention" inventory (issue #589).
//!
//! There was no way to ask the library what was wrong with it. Somebody
//! who had just imported two thousand files had no starting point other
//! than scrolling and noticing.
//!
//! # Shape
//!
//! Counted categories you can click into. Clicking loads exactly those
//! tracks into the library's own table, so the inventory is an entry
//! point and not a report — which is why every category is expressed as
//! an extra `WHERE` on [`browse::library_tracks_sql_where`], the same
//! query the Tracks tab and the folder browser already render.
//!
//! # Three traps, and where each is answered
//!
//! - **Album identity is `(canonical_title, album_artist_id)`**, not the
//!   raw title. The album-level checks group on `album.id`, which is
//!   that pair by construction, rather than on `album.title`. Note the
//!   column holding the album artist is `album.artist_id` — there is no
//!   `album_artist_id`, and `album.album_artist` beside it is the free
//!   text, not the link.
//! - **Multi-artist credits are rebuilt from `track_artist`**, so
//!   "missing artist" asks whether the credit list is empty, not
//!   whether a display string is.
//! - **The counts run over the whole library and must not block behind
//!   a scan.** They are reads on the profile pool, and nothing here
//!   writes, so they queue behind SQLite's single writer for the length
//!   of a read and no longer.
//!
//! # What is *not* here
//!
//! Byte-identical duplicates. [`super::duplicates`] already finds those
//! by hashing content, which is a different and stronger question than
//! the one this module asks. The category below is deliberately
//! *probable* duplicates — same title and artist, near-identical
//! length — which catches the re-encodes and re-rips that content
//! hashing can never group.

use serde::Serialize;
use sqlx::Row;

use waveflow_core::inventory::{chain_by_duration, DURATION_TOLERANCE_MS};
use waveflow_core::metadata::name_match::normalize_name;

use super::browse::{
    expand_library_track_rows, library_track_order_clause, library_tracks_sql_where,
    LibraryTrackRawRow, ListLibraryTracksResponse,
};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// One clickable line of the inventory.
#[derive(Debug, Clone, Serialize)]
pub struct InventoryCategory {
    /// Stable slug; the frontend turns it into localized copy. As
    /// stable as an event name.
    pub key: &'static str,
    /// How many tracks fall in it.
    pub count: i64,
}

/// The `WHERE` fragment that selects a category, in terms of the outer
/// query's aliases.
///
/// Every fragment is local-only. A server track has no file to fix, no
/// tags to write and no cover to embed, so listing one under "needs
/// attention" would offer the user an action they cannot take.
///
/// `id` is `TEXT` in the outer query (the union carries a UUID on the
/// remote side), hence the casts back to integer where a subquery joins
/// on a rowid.
fn where_for(key: &str) -> Option<&'static str> {
    Some(match key {
        // The scanner falls back to the file stem when a file carries no
        // title, so an empty string is the only shape "no title" can
        // take here.
        "missing_title" => "AND source = 'local' AND trim(title) = ''",
        "missing_artist" => "AND source = 'local' AND artist_name IS NULL",
        "missing_album" => "AND source = 'local' AND album_id IS NULL",
        // Year `0` is what several taggers write for "unknown", so it is
        // missing rather than a date.
        "missing_year" => "AND source = 'local' AND (year IS NULL OR year = 0)",
        "missing_track_number" => "AND source = 'local' AND track_number IS NULL",
        "missing_cover" => "AND source = 'local' AND artwork_hash IS NULL",
        // Formats we cannot write tags into. Somebody deciding how to
        // fix their library deserves to know which files they cannot fix
        // in place — and this list is shorter than the issue assumed:
        // `.dsf` became writable, so only `.dff` remains.
        "untaggable" => "AND source = 'local' AND lower(file_path) LIKE '%.dff'",
        // Albums whose tracks disagree about the year. Grouped on
        // `album.id`, which *is* `(canonical_title, album_artist_id)`.
        "album_year_conflict" => {
            "AND source = 'local' AND album_id IN (
                 SELECT CAST(album_id AS TEXT) FROM track
                  WHERE album_id IS NOT NULL AND year IS NOT NULL AND year <> 0
                    AND is_available = 1
                  GROUP BY album_id
                 HAVING COUNT(DISTINCT year) > 1
             )"
        }
        "compilation_no_album_artist" => {
            "AND source = 'local' AND album_id IN (
                 SELECT CAST(id AS TEXT) FROM album
                  WHERE is_compilation = 1
                    AND artist_id IS NULL
                    AND (album_artist IS NULL OR trim(album_artist) = '')
             )"
        }
        // Two tracks claiming the same slot. Per disc, because track 3
        // on disc 1 and track 3 on disc 2 is a double album, not a
        // defect — `COALESCE` so the discless case still groups.
        "duplicate_track_number" => {
            "AND source = 'local' AND CAST(id AS INTEGER) IN (
                 SELECT t.id FROM track t
                  WHERE t.album_id IS NOT NULL AND t.track_number IS NOT NULL
                    AND t.is_available = 1
                    AND EXISTS (
                        SELECT 1 FROM track o
                         WHERE o.album_id = t.album_id
                           AND COALESCE(o.disc_number, 1) = COALESCE(t.disc_number, 1)
                           AND o.track_number = t.track_number
                           AND o.is_available = 1
                           AND o.id <> t.id
                    )
             )"
        }
        // A hole *inside* the observed numbering: the disc spans
        // MIN..MAX but holds fewer distinct numbers than that span.
        //
        // Deliberately not `MAX > COUNT(DISTINCT)`, which is the obvious
        // spelling and is wrong: it assumes every disc starts at 1, so a
        // box set whose third disc is numbered 20-22 gets reported as
        // missing nineteen tracks. Measured against a real database
        // before this was noticed.
        //
        // The cost is that a missing *first* or *last* track is
        // invisible here — numbering alone cannot tell a nine-track
        // album from a ten-track album missing its opener. That needs
        // `album.total_tracks`, which most files do not carry, so it is
        // left out rather than half-implemented.
        //
        // Distinct numbers rather than rows, so a disc with two tracks
        // claiming slot 8 is reported by the category above and not
        // again by this one.
        "track_number_gap" => {
            "AND source = 'local' AND CAST(id AS INTEGER) IN (
                 SELECT t.id FROM track t
                  WHERE t.album_id IS NOT NULL AND t.track_number IS NOT NULL
                    AND t.is_available = 1
                    AND (t.album_id, COALESCE(t.disc_number, 1)) IN (
                        SELECT album_id, COALESCE(disc_number, 1) FROM track
                         WHERE album_id IS NOT NULL AND track_number IS NOT NULL
                           AND is_available = 1
                         GROUP BY album_id, COALESCE(disc_number, 1)
                        HAVING MAX(track_number) - MIN(track_number) + 1
                               > COUNT(DISTINCT track_number)
                    )
             )"
        }
        _ => return None,
    })
}

/// Categories in the order the panel renders them: the per-track
/// problems first, then the ones only an album view can see, then the
/// probable duplicates.
const CATEGORIES: &[&str] = &[
    "missing_title",
    "missing_artist",
    "missing_album",
    "missing_year",
    "missing_track_number",
    "missing_cover",
    "untaggable",
    "album_year_conflict",
    "compilation_no_album_artist",
    "duplicate_track_number",
    "track_number_gap",
    "probable_duplicate",
];

/// The key whose members are computed in Rust rather than in SQL.
const PROBABLE: &str = "probable_duplicate";

/// Track ids that are probably the same recording as some other track.
///
/// Two passes on purpose. SQL groups by an exact normalized
/// title+artist, which it can do over the whole library in one
/// round-trip; the duration chaining then runs per group in Rust,
/// because it is a transitive rule and SQL has no clean way to express
/// one. See [`waveflow_core::inventory::chain_by_duration`] for why
/// chaining rather than pairwise comparison is what makes the list
/// usable.
async fn probable_duplicate_ids(pool: &sqlx::SqlitePool) -> AppResult<Vec<i64>> {
    // The credit list, not a display string: `"A; B"` and `"B; A"` are
    // the same credit and must land in the same bucket, so the artists
    // are concatenated in id order rather than in `position` order.
    let rows = sqlx::query(
        r#"
        SELECT t.id                AS id,
               t.title             AS title,
               t.duration_ms       AS duration_ms,
               (SELECT GROUP_CONCAT(artist_id, '|') FROM (
                   SELECT ta.artist_id FROM track_artist ta
                    WHERE ta.track_id = t.id
                    ORDER BY ta.artist_id
               )) AS credit
          FROM track t
         WHERE t.is_available = 1
           AND trim(t.title) <> ''
           AND t.duration_ms > 0
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut buckets: std::collections::HashMap<(String, String), Vec<(i64, i64)>> =
        std::collections::HashMap::new();
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let title: String = row.try_get("title")?;
        let duration: i64 = row.try_get("duration_ms")?;
        // A track with no credited artist joins the "no artist" bucket
        // rather than being skipped: two untagged copies of the same
        // file are exactly what this list should surface.
        let credit: String = row
            .try_get::<Option<String>, _>("credit")?
            .unwrap_or_default();
        buckets
            .entry((normalize_name(&title), credit))
            .or_default()
            .push((id, duration));
    }

    let mut ids: Vec<i64> = buckets
        .values()
        .flat_map(|items| chain_by_duration(items, DURATION_TOLERANCE_MS))
        .flatten()
        .collect();
    // Sorted so the `IN` list below — and therefore the SQL string, and
    // therefore SQLite's statement cache — is stable between two calls
    // on an unchanged library.
    ids.sort_unstable();
    Ok(ids)
}

#[tauri::command]
pub async fn inventory_summary(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<InventoryCategory>> {
    let pool = state.require_profile_pool().await?;
    let mut out = Vec::with_capacity(CATEGORIES.len());

    for key in CATEGORIES {
        let count = if *key == PROBABLE {
            probable_duplicate_ids(&pool).await?.len() as i64
        } else {
            let Some(clause) = where_for(key) else {
                continue;
            };
            let sql = format!(
                "SELECT COUNT(*) FROM ({})",
                library_tracks_sql_where(clause, "")
            );
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
                .bind(Option::<i64>::None)
                .bind(Option::<i64>::None)
                .bind(Option::<i64>::None)
                .bind(Option::<String>::None)
                .bind(Option::<String>::None)
                .fetch_one(&*pool)
                .await?
        };
        out.push(InventoryCategory { key, count });
    }
    Ok(out)
}

/// The tracks of one category, in the library table's own shape.
#[tauri::command]
pub async fn inventory_tracks(
    state: tauri::State<'_, AppState>,
    category: String,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryTracksResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let order_clause = library_track_order_clause(order_by.as_deref(), direction.as_deref());

    let owned;
    let clause: &str = if category == PROBABLE {
        let ids = probable_duplicate_ids(&pool).await?;
        if ids.is_empty() {
            // `IN ()` is a syntax error in SQLite, and `IN (NULL)` would
            // quietly match nothing *and* read as a mistake. Say it.
            owned = "AND 0".to_string();
        } else {
            // Interpolated rather than bound: the list is variable
            // length, and these are `i64` values this process just read
            // out of its own database — there is no string to escape.
            let list = ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            owned = format!("AND source = 'local' AND CAST(id AS INTEGER) IN ({list})");
        }
        owned.as_str()
    } else {
        where_for(&category)
            .ok_or_else(|| AppError::Other(format!("unknown inventory category: {category}")))?
    };

    let sql = library_tracks_sql_where(clause, order_clause);
    let raw = sqlx::query_as::<_, LibraryTrackRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(Option::<i64>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<i64>::None)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_track_rows(raw, artwork_dir.clone()).await?;
    Ok(ListLibraryTracksResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}
