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

use waveflow_core::album_completeness::{album_completeness, AlbumCompleteness, NumberedTrack};
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
        // Lossless files whose highs stop at a lossy encoder's wall: a
        // FLAC or WAV made from an MP3, AAC or Opus. `cutoff_hz` and the
        // threshold: `waveflow_core::analysis::bandwidth`.
        "probable_fake_lossless" => concat!(
            "AND source = 'local' AND CAST(id AS INTEGER) IN (
                 SELECT t.id FROM track t
                   JOIN track_analysis ta ON ta.track_id = t.id
                  WHERE ",
            waveflow_core::probable_fake_lossless_sql!(),
            ")"
        ),
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
        // album from a ten-track album missing its opener. That is the
        // category after this one, computed in Rust
        // ([`albums_missing_ends_ids`]) from the totals the files carry.
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
    "probable_fake_lossless",
    "album_year_conflict",
    "compilation_no_album_artist",
    "duplicate_track_number",
    "track_number_gap",
    MISSING_ENDS,
    "probable_duplicate",
    PHANTOM,
];

/// The key whose members are computed in Rust rather than in SQL.
const PROBABLE: &str = "probable_duplicate";

/// Albums missing a first or last track, or a whole disc: computed in
/// Rust too, by [`waveflow_core::album_completeness`].
const MISSING_ENDS: &str = "album_missing_tracks";

/// The members of a category computed in Rust, `None` for the others.
async fn computed_ids(pool: &sqlx::SqlitePool, key: &str) -> AppResult<Option<Vec<i64>>> {
    Ok(match key {
        PROBABLE => Some(probable_duplicate_ids(pool).await?),
        MISSING_ENDS => Some(albums_missing_ends_ids(pool).await?),
        _ => None,
    })
}

/// The numbering of an album's available tracks, as the completeness
/// check reads it. One row per track; `album_id` first so a caller
/// reading every album can group a single ordered pass.
const NUMBERING_COLUMNS: &str = "album_id, disc_number, track_number, track_total, disc_total, id";

fn numbered(row: &sqlx::sqlite::SqliteRow) -> Result<NumberedTrack, sqlx::Error> {
    Ok(NumberedTrack {
        disc_number: row.try_get("disc_number")?,
        track_number: row.try_get("track_number")?,
        track_total: row.try_get("track_total")?,
        disc_total: row.try_get("disc_total")?,
    })
}

/// What one album is missing — the album page's indication.
pub(crate) async fn album_completeness_of(
    pool: &sqlx::SqlitePool,
    album_id: i64,
) -> AppResult<AlbumCompleteness> {
    let sql =
        format!("SELECT {NUMBERING_COLUMNS} FROM track WHERE album_id = ? AND is_available = 1");
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(album_id)
        .fetch_all(pool)
        .await?;
    let tracks = rows.iter().map(numbered).collect::<Result<Vec<_>, _>>()?;
    Ok(album_completeness(&tracks))
}

/// The tracks of every album missing a first or last track or a disc.
///
/// One query over the library, grouped here in a single pass — not a
/// query per album. Holes in the middle are left to `track_number_gap`,
/// which already lists them; an album with both is in both, as it should
/// be, since each names a different fix.
async fn albums_missing_ends_ids(pool: &sqlx::SqlitePool) -> AppResult<Vec<i64>> {
    let sql = format!(
        "SELECT {NUMBERING_COLUMNS} FROM track
          WHERE album_id IS NOT NULL AND is_available = 1
          ORDER BY album_id"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await?;

    let mut ids = Vec::new();
    let mut album: Option<i64> = None;
    let mut tracks: Vec<NumberedTrack> = Vec::new();
    let mut members: Vec<i64> = Vec::new();
    let mut flush = |tracks: &mut Vec<NumberedTrack>, members: &mut Vec<i64>| {
        if album_completeness(tracks).missing_at_the_ends() {
            ids.append(members);
        }
        tracks.clear();
        members.clear();
    };
    for row in &rows {
        let album_id: i64 = row.try_get("album_id")?;
        if album != Some(album_id) {
            flush(&mut tracks, &mut members);
            album = Some(album_id);
        }
        tracks.push(numbered(row)?);
        members.push(row.try_get("id")?);
    }
    flush(&mut tracks, &mut members);
    // Sorted for the same reason as the duplicates: a stable `IN` list.
    ids.sort_unstable();
    Ok(ids)
}

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
          -- The characters of `NAME_SEPARATORS` (artist_split.rs), #851.
          WHERE instr(a.name, ',') > 0 OR instr(a.name, '，') > 0
             OR instr(a.name, '、') > 0 OR instr(a.name, '､') > 0
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
///
/// `expected_profile_id` is the profile the caller will label the rows
/// with, so they can never be read from one profile and acted on as
/// another's — see [`super::artist_split::split_artist`].
pub async fn inventory_phantom_artists(
    state: tauri::State<'_, AppState>,
    expected_profile_id: Option<i64>,
) -> AppResult<Vec<PhantomArtist>> {
    let pool = state.require_profile_pool_for(expected_profile_id).await?;
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
        let count = if let Some(ids) = computed_ids(&pool, key).await? {
            ids.len() as i64
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
    let clause: &str = if let Some(ids) = computed_ids(&pool, &category).await? {
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
mod tests;
