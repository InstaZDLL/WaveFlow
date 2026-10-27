//! The library's track list, from the device and from the bound server.

use std::path::PathBuf;

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// One track of the library, whichever source it comes from.
///
/// Carries only what the library table renders. A server track has no local
/// row, so it has no rating, no like, no file and no tags — the fields that
/// describe those are absent rather than defaulted, because a `0` rating and
/// "not rated" are different things.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryTrackRow {
    pub source: String,
    /// Local rowid rendered as text, or the server's track UUID.
    pub id: String,
    /// Local only — the library a track belongs to. A server track belongs to
    /// one of the *server's* libraries, which is not one of these.
    pub library_id: Option<i64>,
    pub title: String,
    pub album_id: Option<String>,
    pub album_title: Option<String>,
    pub artist_id: Option<String>,
    pub artist_name: Option<String>,
    /// Comma-joined artist ids, local only — the server credits one artist per
    /// track in its listings, so a remote row has nothing to split.
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
    pub channels: Option<i64>,
    pub codec: Option<String>,
    pub musical_key: Option<String>,
    /// Local only, and the reason a remote row cannot be edited, rated or
    /// hashed: there is no file here to do any of it to.
    pub file_path: Option<String>,
    pub file_size: Option<i64>,
    pub added_at: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Local only. `None` on a remote row means "this cannot be rated here",
    /// not "unrated".
    pub rating: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLibraryTracksResponse {
    pub artwork_base: String,
    pub items: Vec<LibraryTrackRow>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct LibraryTrackRawRow {
    source: String,
    id: String,
    library_id: Option<i64>,
    title: String,
    album_id: Option<String>,
    album_title: Option<String>,
    artist_id: Option<String>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    year: Option<i64>,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
    channels: Option<i64>,
    codec: Option<String>,
    musical_key: Option<String>,
    file_path: Option<String>,
    file_size: Option<i64>,
    added_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    rating: Option<i64>,
}

/// The SQL expression a column sorts on, or `None` when the key is not
/// a sortable column.
///
/// A **whitelist**, and the only reason the caller may hand its result
/// to `AssertSqlSafe`: `order_by` reaches here from the frontend, so
/// anything that is not matched here must produce the default clause
/// rather than reaching a query.
///
/// Text columns sort case-insensitively (names under `FOLD`, which sets
/// accents aside too, so "Émilie" sits among the E); numeric ones do not, and every
/// nullable one puts its NULLs last in both directions — an untagged
/// track is not a track with the smallest value, and burying a run of
/// blanks at the top of an ascending sort is the fastest way to make a
/// column look broken (#588).
pub(super) fn track_sort_expr(key: &str) -> Option<(&'static str, bool)> {
    // (expression, nulls_last)
    Some(match key {
        "title" => ("title COLLATE FOLD", false),
        "artist" => ("sort_artist COLLATE NAME", true),
        "album" => ("sort_album COLLATE NAME", true),
        "duration_ms" => ("duration_ms", false),
        "year" => ("year", true),
        "added_at" => ("added_at", false),
        // Local-only: a server track has no rating, and an unratable row
        // is not a badly-rated one.
        "rating" => ("rating", true),
        "track_number" => ("track_number", true),
        "disc_number" => ("disc_number", true),
        "bitrate" => ("bitrate", true),
        "sample_rate" => ("sample_rate", true),
        "bit_depth" => ("bit_depth", true),
        "channels" => ("channels", true),
        "codec" => ("codec COLLATE NOCASE", true),
        "musical_key" => ("musical_key COLLATE NOCASE", true),
        "file_size" => ("file_size", true),
        "file_path" => ("file_path COLLATE NOCASE", true),
        _ => return None,
    })
}

/// Columns that read as "most first": clicking one of these the first
/// time should give the newest, the longest, the best rated — not the
/// oldest.
fn sorts_descending_by_default(key: Option<&str>) -> bool {
    matches!(
        key,
        Some("duration_ms")
            | Some("added_at")
            | Some("year")
            | Some("rating")
            | Some("bitrate")
            | Some("sample_rate")
            | Some("bit_depth")
            | Some("file_size")
    )
}

/// The `ORDER BY` for a track listing.
///
/// Returns an owned string rather than a `&'static str` since #588: the
/// sortable set is every column the table can show, and spelling out
/// two match arms per column was how the previous shape quietly stopped
/// at seven of them.
///
/// Artist and album sort on the normalised keys both halves now carry; the
/// title sorts on the display string on both sides, because the local half has
/// no canonical form for it either. Consistency per column is what matters —
/// normalising one side of a comparison and not the other is exactly how an
/// artist ends up in two places.
pub(crate) fn library_track_order_clause(
    order_by: Option<&str>,
    direction: Option<&str>,
) -> String {
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if sorts_descending_by_default(order_by) {
                "DESC"
            } else {
                "ASC"
            }
        }
    };

    // The library's natural reading order, and the answer for any key
    // that is not a sortable column.
    // `source, id` here too, for the reason the explicit clauses carry
    // it: this is the order most lists are rendered in, so it is the one
    // where a pair of rows swapping places between two refreshes would
    // be seen most often.
    const DEFAULT: &str = "ORDER BY sort_artist COLLATE NAME,
                  sort_album COLLATE NAME,
                  disc_number,
                  track_number,
                  title COLLATE FOLD,
                  source,
                  id";

    let Some((expr, nulls_last)) = order_by.and_then(track_sort_expr) else {
        return DEFAULT.to_string();
    };

    let mut clause = String::from("ORDER BY ");
    if nulls_last {
        // SQLite has `NULLS LAST` from 3.30, but `expr IS NULL` works on
        // every build and is what the rest of this file already uses.
        clause.push_str(expr);
        clause.push_str(" IS NULL, ");
    }
    clause.push_str(expr);
    clause.push(' ');
    clause.push_str(dir);

    // A stable tie-break, so two rows that compare equal do not swap
    // places between two identical queries. Album sorts read down the
    // disc, everything else falls back to the title.
    match order_by {
        // `sort_artist` first, exactly as DEFAULT above orders it:
        // album titles are not unique across artists (`Greatest Hits`,
        // `Live`, an untitled rip), and without it two such albums
        // interleave track 1 against track 1 all the way down.
        Some("album") => clause.push_str(", sort_artist COLLATE NAME, disc_number, track_number"),
        Some("title") => {}
        _ => clause.push_str(", title COLLATE FOLD"),
    }
    // ...and then a **total** order. Two tracks of the same album with
    // the same disc and track number, or two files with the same title,
    // are ordinary in a library with duplicates -- which is precisely
    // the library whose owner is most likely to be sorting a column to
    // find them. `source` before `id` because the two halves of the
    // union number independently: a local rowid and a server UUID can
    // read as equal text.
    clause.push_str(", source, id");
    clause
}

/// Both halves of the track listing, as one compound select.
///
/// Split out of the command for the reason on [`library_albums_sql`].
pub(super) fn library_tracks_sql(order_clause: &str) -> String {
    library_tracks_sql_where("", order_clause)
}

/// The same listing, with `extra_where` appended to the **outer** filter
/// (#578). One builder rather than two because the column list has to
/// stay in step with [`LibraryTrackRawRow`], and two copies of forty
/// columns drift.
///
/// `extra_where` is composed at the call site, never from user input: it
/// holds `AND ...` fragments whose values are bound, and its `?`
/// placeholders bind *after* the union's own five.
pub(crate) fn library_tracks_sql_where(extra_where: &str, order_clause: &str) -> String {
    let musical_key = waveflow_core::effective_musical_key_sql!();
    format!(
        r#"
        SELECT source, id, library_id, title, album_id, album_title, artist_id, artist_name,
               artist_ids, duration_ms, track_number, disc_number, year, bitrate, sample_rate,
               bit_depth, channels, codec, musical_key, file_path, file_size, added_at,
               artwork_hash, artwork_format, rating
          FROM (
            SELECT 'local'                    AS source,
                   CAST(t.id AS TEXT)         AS id,
                   t.library_id               AS library_id,
                   t.title                    AS title,
                   CAST(t.album_id AS TEXT)   AS album_id,
                   al.title                   AS album_title,
                   CAST(t.primary_artist AS TEXT) AS artist_id,
                   (SELECT GROUP_CONCAT(name, ', ') FROM (
                      SELECT ar2.name FROM track_artist ta2
                      JOIN artist ar2 ON ar2.id = ta2.artist_id
                      WHERE ta2.track_id = t.id
                      ORDER BY ta2.position
                   ))                         AS artist_name,
                   (SELECT GROUP_CONCAT(id, ',') FROM (
                      SELECT ta2.artist_id AS id FROM track_artist ta2
                      WHERE ta2.track_id = t.id
                      ORDER BY ta2.position
                   ))                         AS artist_ids,
                   t.duration_ms              AS duration_ms,
                   t.track_number             AS track_number,
                   t.disc_number              AS disc_number,
                   t.year                     AS year,
                   t.bitrate                  AS bitrate,
                   t.sample_rate              AS sample_rate,
                   t.bit_depth                AS bit_depth,
                   t.channels                 AS channels,
                   t.codec                    AS codec,
                   {musical_key}              AS musical_key,
                   t.file_path                AS file_path,
                   t.file_size                AS file_size,
                   t.added_at                 AS added_at,
                   aw.hash                    AS artwork_hash,
                   aw.format                  AS artwork_format,
                   t.rating                   AS rating,
                   -- Coalesced on both sides or on neither. The remote half
                   -- falls back to its display string, so leaving the local
                   -- one bare would file every track without a primary artist
                   -- ahead of the entire list, NULL sorting first.
                   COALESCE(ar.canonical_name, al.album_artist) AS sort_artist,
                   al.canonical_title         AS sort_album
              FROM track t
              LEFT JOIN album   al ON al.id = t.album_id
              LEFT JOIN artist  ar ON ar.id = t.primary_artist
              LEFT JOIN artwork aw ON aw.id = al.artwork_id
             WHERE (? IS NULL OR t.library_id = ?)
               AND t.is_available = 1
            UNION ALL
            SELECT 'remote',
                   rt.remote_id,
                   NULL,
                   rt.title,
                   rt.album_id,
                   rt.album,
                   rt.artist_id,
                   rt.artist,
                   NULL,
                   rt.duration_ms,
                   rt.track_no,
                   rt.disc_no,
                   rt.year,
                   rt.bitrate,
                   NULL,
                   NULL,
                   NULL,
                   rt.suffix,
                   NULL,
                   NULL,
                   rt.size,
                   rt.cached_at,
                   rt.artwork_hash,
                   NULL,
                   NULL,
                   COALESCE(rt.sort_artist, rt.artist),
                   COALESCE(rt.sort_album, rt.album)
              FROM remote_track rt
             WHERE rt.in_catalogue = 1
             -- A server track proven to be the same bytes as a local one is
             -- not a second track. Without this the list shows the same
             -- recording twice the moment a reconciliation pass or an import
             -- establishes the link -- which is exactly when the user has the
             -- most reason to expect one row.
             --
             -- Two narrowings, and neither is decoration. `confirmed` only:
             -- a stale link is a guess, and hiding a track on a guess loses
             -- it. And the local row must still be available -- when its file
             -- has gone the local half already filtered it out, so dropping
             -- the remote half too would remove from the library a track the
             -- server can still play.
               AND NOT EXISTS (
                     SELECT 1 FROM remote_track_link l
                       JOIN track lt ON lt.id = l.local_track_id
                      WHERE l.remote_track_id = rt.remote_id
                        AND l.status = 'confirmed'
                        AND lt.is_available = 1
                   )
             -- A local library filter is a filter over local libraries; see
             -- `list_library_albums`.
               AND ? IS NULL
          )
         WHERE (? IS NULL OR source = ?)
           {extra_where}
         {order_clause}
"#
    )
}

/// Every track the library can show, from the device and from the bound
/// server, as one sorted list.
///
/// Not merged, on the same terms as the albums and the artists. A server track
/// carries none of the local user data — no rating, no like, no tags — because
/// none of it exists for a row that has no local counterpart.
#[tauri::command]
pub async fn list_library_tracks(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    source: Option<String>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryTracksResponse> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = library_track_order_clause(order_by.as_deref(), direction.as_deref());
    let sql = library_tracks_sql(&order_clause);

    let raw = sqlx::query_as::<_, LibraryTrackRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source.as_deref())
        .bind(source.as_deref())
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_track_rows(raw, artwork_dir.clone()).await?;

    Ok(ListLibraryTracksResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto the local half only. See
/// [`expand_library_album_rows`].
pub(crate) async fn expand_library_track_rows(
    raw: Vec<LibraryTrackRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<LibraryTrackRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|row| {
                let local = row.source == "local";
                let (artwork_has_1x, artwork_has_2x) = match row.artwork_hash.as_deref() {
                    Some(hash) if local => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    _ => (false, false),
                };
                LibraryTrackRow {
                    source: row.source,
                    id: row.id,
                    library_id: row.library_id,
                    title: row.title,
                    album_id: row.album_id,
                    album_title: row.album_title,
                    artist_id: row.artist_id,
                    artist_name: row.artist_name,
                    artist_ids: row.artist_ids,
                    duration_ms: row.duration_ms,
                    track_number: row.track_number,
                    disc_number: row.disc_number,
                    year: row.year,
                    bitrate: row.bitrate,
                    sample_rate: row.sample_rate,
                    bit_depth: row.bit_depth,
                    channels: row.channels,
                    codec: row.codec,
                    musical_key: row.musical_key,
                    file_path: row.file_path,
                    file_size: row.file_size,
                    added_at: row.added_at,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    rating: row.rating,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("library track row expand join: {e}")))
}
