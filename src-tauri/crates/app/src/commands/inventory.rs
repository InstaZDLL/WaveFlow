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

use waveflow_core::scanner::canonical_name;

use super::artist_split::split_fragments;
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
        // Two shapes, because the scanner substitutes the file stem
        // when a file has no title tag at all: without the flag this
        // category could only ever find the rarer fault, a title tag
        // that is present and empty. The flag is set at scan time --
        // see `ExtractedFile::title_from_filename` for why it cannot be
        // worked out afterwards from the path.
        //
        // Reached through a subquery on `track` rather than named
        // directly: the clause is applied to the projection
        // `library_tracks_sql_where` builds, which unions local and
        // remote rows and exposes only the columns both can answer.
        // Widening that projection for one category would change the
        // shape of every library listing in the app.
        "missing_title" => {
            "AND source = 'local' AND (
                 trim(title) = ''
                 OR CAST(id AS INTEGER) IN (
                     SELECT t.id FROM track t WHERE t.title_from_filename = 1
                 )
             )"
        }
        "missing_artist" => "AND source = 'local' AND artist_name IS NULL",
        "missing_album" => "AND source = 'local' AND album_id IS NULL",
        // Year `0` is what several taggers write for "unknown", so it is
        // missing rather than a date.
        "missing_year" => "AND source = 'local' AND (year IS NULL OR year = 0)",
        // `<= 0` and not only NULL: several taggers write `0` for
        // "unknown", the same convention the year check above already
        // allows for. The album-level checks below exclude those too,
        // or a disc full of zeroes would read as a pile of duplicates
        // and a gap spanning the whole numbering.
        "missing_track_number" => {
            "AND source = 'local' AND (track_number IS NULL OR track_number <= 0)"
        }
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
                  WHERE t.album_id IS NOT NULL AND t.track_number > 0
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
                  WHERE t.album_id IS NOT NULL AND t.track_number > 0
                    AND t.is_available = 1
                    AND (t.album_id, COALESCE(t.disc_number, 1)) IN (
                        SELECT album_id, COALESCE(disc_number, 1) FROM track
                         WHERE album_id IS NOT NULL AND track_number > 0
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
    PHANTOM,
];

/// The key whose members are computed in Rust rather than in SQL.
const PROBABLE: &str = "probable_duplicate";

/// The one category made of artists rather than tracks (#719): names that
/// look like several artists joined by commas. Listed by
/// [`inventory_phantom_artists`]; [`inventory_tracks`] has nothing for it.
const PHANTOM: &str = "phantom_artist";

/// `profile_setting` key: JSON array of the canonical names the user said
/// are one artist ("don't split"), so the list stops asking about them.
pub const DISMISSED_PHANTOMS_KEY: &str = "inventory.dismissed_phantoms";

/// One name a phantom would split into.
#[derive(Debug, Clone, Serialize)]
pub struct PhantomFragment {
    pub name: String,
    /// The artist already in the library under that name, if any — the
    /// row the split will reuse.
    pub artist_id: Option<i64>,
}

/// An artist whose name looks like several joined by commas.
#[derive(Debug, Clone, Serialize)]
pub struct PhantomArtist {
    pub id: i64,
    pub name: String,
    /// What `don't split` stores, so it matches what this list reads.
    pub canonical_name: String,
    pub track_count: i64,
    pub fragments: Vec<PhantomFragment>,
}

/// Artists whose name is several names joined by commas, most likely
/// phantoms first.
///
/// **Every comma-joined name is listed**, not only those whose fragments
/// already exist as artists — which is what the issue proposed, and what
/// measuring against real libraries ruled out: in one of them, 18 of 39
/// comma names had no fragment in the library at all, and every one of
/// them was a real duo (`Drake, Lil Durk`). The fragments that do exist
/// are shown and sort a name up, as evidence rather than as a gate.
///
/// A comma is still only a hint (`Tyler, The Creator`), so the user can
/// say "don't split" once; that is remembered per profile, by canonical
/// name so a rescan that recreates the row does not bring it back.
///
/// Only artists credited on an available track: a phantom nothing plays
/// is not worth an entry.
async fn phantom_artists(pool: &sqlx::SqlitePool) -> AppResult<Vec<PhantomArtist>> {
    let candidates: Vec<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT a.id, a.name, a.canonical_name, COUNT(DISTINCT t.id)
           FROM artist a
           JOIN track_artist ta ON ta.artist_id = a.id
           JOIN track t ON t.id = ta.track_id AND t.is_available = 1
          WHERE instr(a.name, ',') > 0
          GROUP BY a.id",
    )
    .fetch_all(pool)
    .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let dismissed: Vec<String> =
        sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = ?")
            .bind(DISMISSED_PHANTOMS_KEY)
            .fetch_optional(pool)
            .await?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

    let known: std::collections::HashMap<String, i64> =
        sqlx::query_as::<_, (String, i64)>("SELECT canonical_name, id FROM artist")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();

    let mut out = Vec::new();
    for (id, name, canonical, track_count) in candidates {
        if dismissed.contains(&canonical) {
            continue;
        }
        let fragments: Vec<PhantomFragment> = split_fragments(&name)
            .into_iter()
            .map(|fragment| {
                let artist_id = known
                    .get(&canonical_name(&fragment))
                    .copied()
                    .filter(|found| *found != id);
                PhantomFragment {
                    name: fragment,
                    artist_id,
                }
            })
            .collect();
        // The same test the split applies: at least two distinct names,
        // none of them the artist itself.
        let mut distinct: Vec<String> = fragments.iter().map(|f| canonical_name(&f.name)).collect();
        distinct.sort();
        distinct.dedup();
        distinct.retain(|c| !c.is_empty() && *c != canonical);
        if distinct.len() < 2 {
            continue;
        }
        out.push(PhantomArtist {
            id,
            name,
            canonical_name: canonical,
            track_count,
            fragments,
        });
    }

    let known_count =
        |p: &PhantomArtist| p.fragments.iter().filter(|f| f.artist_id.is_some()).count();
    out.sort_by(|a, b| {
        known_count(b)
            .cmp(&known_count(a))
            .then(b.track_count.cmp(&a.track_count))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

/// The artists the "artists to split" category holds (#719).
#[tauri::command]
pub async fn inventory_phantom_artists(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<PhantomArtist>> {
    let pool = state.require_profile_pool().await?;
    phantom_artists(&pool).await
}

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

    // Each count runs `COUNT(*)` over the whole projection, correlated
    // GROUP_CONCATs and all, once per category. That is more work than
    // counting needs, and on a large library it is the slowest thing
    // this tab does.
    //
    // Not fixed by giving each category its own lean query: the clauses
    // in `where_for` are written against the PROJECTION -- that is why
    // `missing_title` reaches the flag through a subquery rather than
    // naming the column. A second set of filters would be a second
    // place for those semantics to drift, and this file already paid
    // for that once. Tracked separately instead.
    for key in CATEGORIES {
        let count = if *key == PROBABLE {
            probable_duplicate_ids(&pool).await?.len() as i64
        } else if *key == PHANTOM {
            phantom_artists(&pool).await?.len() as i64
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
    // One snapshot rather than two resolutions: taken separately, a
    // profile switch landing between them hands this query profile A's
    // pool and profile B's artwork directory, and every cover path in
    // the answer points at the wrong profile.
    let (pool, profile_id) = state.require_profile_snapshot().await?;
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
            //
            // The cost is that the SQL text varies with the number of
            // ids, so SQLite prepares it afresh per distinct count.
            // That is one prepare per click on this category, against
            // pulling in a `json_each` dependency for the only query in
            // the app that would use it.
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

    let sql = library_tracks_sql_where(clause, &order_clause);
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::SqlitePool;
    use std::str::FromStr;

    /// The repo's own profile migrations, with `foreign_keys` on.
    ///
    /// A hand-written fixture would let these clauses pass against a
    /// schema the app never has -- which is how `album.album_artist_id`
    /// survived being written down at all: the column does not exist.
    async fn pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
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

    /// Four discs chosen for what they must *not* trigger:
    ///
    /// - album 1 disc 3 numbered 20-22: complete, and the case that
    ///   `MAX > COUNT(DISTINCT)` reports as missing nineteen tracks;
    /// - album 1 disc 1 numbered 1-2: proof the rule splits per disc
    ///   rather than per album;
    /// - album 2 disc 1 numbered 1, 2, 4: the genuine hole;
    /// - album 3 disc 1 with two tracks both claiming slot 8: a clash,
    ///   and deliberately not also a gap.
    async fn seed(pool: &SqlitePool) {
        for statement in [
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
            "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')",
            "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (1, 'Box Set', 'box set', 1, 1999, 0)",
            "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (2, 'Gappy', 'gappy', 1, 2000, 0)",
            "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (3, 'Clashing', 'clashing', 1, 2001, 0)",
        ] {
            sqlx::raw_sql(statement).execute(pool).await.unwrap();
        }

        for (id, album, disc, number) in [
            (1i64, 1i64, 3i64, 20i64),
            (2, 1, 3, 21),
            (3, 1, 3, 22),
            (4, 1, 1, 1),
            (5, 1, 1, 2),
            (6, 2, 1, 1),
            (7, 2, 1, 2),
            (8, 2, 1, 4),
            (9, 3, 1, 8),
            (10, 3, 1, 8),
        ] {
            sqlx::query(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, album_id, disc_number,
                                    track_number, primary_artist, duration_ms, added_at,
                                    is_available, hlc_wall, hlc_logical, rating_hlc_wall,
                                    rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, ?, ?, 1, 300000, 0, 1, 0, 0, 0, 0)",
            )
            .bind(id)
            .bind(format!("/m/{id}.flac"))
            .bind(format!("h{id}"))
            .bind(format!("T{id}"))
            .bind(album)
            .bind(disc)
            .bind(number)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO track_artist (track_id, artist_id, position) VALUES (?, 1, 0)",
            )
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// The ids one category holds, through the exact query the command
    /// runs -- clause, wrapper and bound parameters alike.
    async fn ids_for(pool: &SqlitePool, key: &str) -> Vec<i64> {
        let clause = where_for(key).expect("known category");
        let sql = format!(
            "SELECT CAST(id AS INTEGER) FROM ({}) ORDER BY 1",
            library_tracks_sql_where(clause, "")
        );
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
            .bind(Option::<i64>::None)
            .bind(Option::<i64>::None)
            .bind(Option::<i64>::None)
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_gap_is_measured_inside_the_observed_range() {
        let pool = pool().await;
        seed(&pool).await;
        // Album 2's disc only. A disc starting at 20 is complete, and a
        // disc whose only fault is a repeated number is not a hole.
        assert_eq!(ids_for(&pool, "track_number_gap").await, vec![6, 7, 8]);
    }

    /// The SQL half of the probable-duplicate rule: what lands in a
    /// bucket together. The chaining itself is tested in
    /// `waveflow_core::inventory`; what is tested here is that two
    /// recordings reach the same bucket at all.
    async fn seed_duplicates(pool: &SqlitePool) {
        for statement in [
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
            "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'A', 'a')",
            "INSERT INTO artist (id, name, canonical_name) VALUES (2, 'B', 'b')",
        ] {
            sqlx::raw_sql(statement).execute(pool).await.unwrap();
        }

        // 1-3: one recording three times, 180 s / 181.5 s / 183 s. The
        // outer pair is 3 s apart, so a pairwise tolerance splits them;
        // the chain must not. Their titles differ only by the case and
        // punctuation `normalize_name` folds, and their two artists are
        // credited in opposite orders -- the bucket key sorts by artist
        // id, so a credit is a set and not a display string.
        //
        // 4-5: no credited artist at all, same title and length. Two
        // untagged copies of one file are exactly what this should
        // surface, so they share the empty-credit bucket rather than
        // being skipped.
        //
        // 6: same title, far too long to be the same recording.
        let tracks: &[(i64, &str, i64, &[(i64, i64)])] = &[
            (1, "Blue Monday", 180_000, &[(1, 0), (2, 1)]),
            (2, "blue monday!", 181_500, &[(2, 0), (1, 1)]),
            (3, "Blue  Monday", 183_000, &[(1, 0), (2, 1)]),
            (4, "Untitled", 200_000, &[]),
            (5, "Untitled", 200_500, &[]),
            (6, "Blue Monday", 400_000, &[(1, 0)]),
        ];
        for (id, title, duration, credits) in tracks {
            sqlx::query(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, duration_ms, added_at,
                                    is_available, hlc_wall, hlc_logical, rating_hlc_wall,
                                    rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, 0, 1, 0, 0, 0, 0)",
            )
            .bind(id)
            .bind(format!("/d/{id}.flac"))
            .bind(format!("d{id}"))
            .bind(title)
            .bind(duration)
            .execute(pool)
            .await
            .unwrap();
            for (artist_id, position) in *credits {
                sqlx::query(
                    "INSERT INTO track_artist (track_id, artist_id, position)
                     VALUES (?, ?, ?)",
                )
                .bind(id)
                .bind(artist_id)
                .bind(position)
                .execute(pool)
                .await
                .unwrap();
            }
        }
    }

    /// Both shapes of "no title", and nothing else.
    #[tokio::test]
    async fn an_untitled_file_is_found_whichever_way_it_is_untitled() {
        let pool = pool().await;
        sqlx::raw_sql(
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // 1: properly titled. 2: the title tag was there and empty.
        // 3: no title tag at all, so the scanner wrote the file stem --
        // the common case, and the one a text test cannot see.
        for (id, title, from_filename) in [
            (1i64, "Real Title", 0i64),
            (2, "", 0),
            (3, "03 - untagged", 1),
        ] {
            sqlx::query(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, title_from_filename, duration_ms,
                                    added_at, is_available, hlc_wall, hlc_logical,
                                    rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, ?, ?, 1000, 0, 1, 0, 0, 0, 0)",
            )
            .bind(id)
            .bind(format!("/t/{id}.flac"))
            .bind(format!("t{id}"))
            .bind(title)
            .bind(from_filename)
            .execute(&pool)
            .await
            .unwrap();
        }

        assert_eq!(ids_for(&pool, "missing_title").await, vec![2, 3]);
    }

    #[tokio::test]
    async fn probable_duplicates_chain_and_ignore_credit_order() {
        let pool = pool().await;
        seed_duplicates(&pool).await;
        let ids = probable_duplicate_ids(&pool).await.unwrap();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn a_clash_is_scoped_to_one_disc_of_one_album() {
        let pool = pool().await;
        seed(&pool).await;
        // Both sides of the clash, and nothing from the box set -- whose
        // discs 1 and 3 each hold a track numbered differently but would
        // collide if the check grouped by album alone.
        assert_eq!(ids_for(&pool, "duplicate_track_number").await, vec![9, 10]);
    }

    /// #719, against the real migrations. Four comma names:
    ///
    /// - `Ice Spice, Central Cee`, one fragment already an artist: listed
    ///   first, with that fragment linked;
    /// - `Drake, Lil Durk`, no fragment in the library: still listed —
    ///   measured on a real library, that is what most real duos look like;
    /// - `Tyler, The Creator`, dismissed by the user: not listed;
    /// - `Solo,` which splits into one name only: not a split at all.
    ///
    /// And `Unplayed, Duo`, credited on no available track: not listed.
    #[tokio::test]
    async fn comma_names_are_listed_unless_dismissed_or_unsplittable() {
        let pool = pool().await;
        sqlx::raw_sql(
            r#"INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0);
             INSERT INTO artist (id, name, canonical_name) VALUES
                 (1, 'Ice Spice, Central Cee', 'ice spice central cee'),
                 (2, 'Ice Spice', 'ice spice'),
                 (3, 'Drake, Lil Durk', 'drake lil durk'),
                 (4, 'Tyler, The Creator', 'tyler the creator'),
                 (5, 'Solo,', 'solo'),
                 (6, 'Unplayed, Duo', 'unplayed duo');
             INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('inventory.dismissed_phantoms', '["tyler the creator"]', 'json', 0);"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        for (id, artist, available) in [
            (1i64, 1i64, 1i64),
            (2, 1, 1),
            (3, 3, 1),
            (4, 4, 1),
            (5, 5, 1),
            (6, 6, 0),
        ] {
            sqlx::query(
                "INSERT INTO track (id, library_id, file_path, file_hash, file_size,
                                    file_modified, title, primary_artist, duration_ms,
                                    added_at, is_available, hlc_wall, hlc_logical,
                                    rating_hlc_wall, rating_hlc_logical)
                 VALUES (?, 1, ?, ?, 1, 0, 'T', ?, 300000, 0, ?, 0, 0, 0, 0)",
            )
            .bind(id)
            .bind(format!("/p/{id}.flac"))
            .bind(format!("p{id}"))
            .bind(artist)
            .bind(available)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO track_artist (track_id, artist_id, position) VALUES (?, ?, 0)",
            )
            .bind(id)
            .bind(artist)
            .execute(&pool)
            .await
            .unwrap();
        }

        let listed = phantom_artists(&pool).await.unwrap();
        let names: Vec<&str> = listed.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Ice Spice, Central Cee", "Drake, Lil Durk"]);
        assert_eq!(listed[0].track_count, 2);
        assert_eq!(listed[0].fragments[0].artist_id, Some(2));
        assert_eq!(listed[0].fragments[1].artist_id, None);
    }
}
