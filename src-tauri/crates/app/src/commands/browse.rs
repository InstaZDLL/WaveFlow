//! Aggregate "browse" queries for a library: albums, artists, genres, folders.
//!
//! These commands back the Albums / Artistes / Genres / Dossiers tabs in the
//! library view. They all take a `library_id` and filter content to rows that
//! have at least one available track in that library — important because
//! `album`, `artist` and `genre` are profile-wide tables shared across
//! libraries.

use std::io::Read;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Slim album row shipped by `list_albums` — artwork is represented by
/// `(hash, format, has_1x, has_2x)` so the response-level
/// `artwork_base` carries the per-profile prefix once instead of
/// repeating it on every row.
#[derive(Debug, Clone, Serialize)]
pub struct AlbumRow {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Best-quality bit depth across the album's tracks. Drives the
    /// Hi-Res cover badge — if any track in the album is mastered at
    /// 24-bit, the badge shows on the cover. `None` when no track
    /// has a known bit depth (e.g. all MP3s).
    pub max_bit_depth: Option<i64>,
    pub max_sample_rate: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListAlbumsResponse {
    /// Per-profile artwork dir. Stitch `<base>/<hash>.<format>` for the
    /// full image, `<base>/<hash>_1x.jpg` / `<base>/<hash>_2x.jpg` for
    /// thumbnails (the thumbnail pipeline always emits JPEG regardless
    /// of the source extension).
    pub artwork_base: String,
    pub items: Vec<AlbumRow>,
}

/// Private SQL row — the public `AlbumRow` derives `artwork_path` from the
/// per-profile data dir in Rust.
#[derive(FromRow)]
struct AlbumRawRow {
    id: i64,
    title: String,
    artist_name: Option<String>,
    year: Option<i64>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    max_bit_depth: Option<i64>,
    max_sample_rate: Option<i64>,
}

/// Slim artist row — same wire-format contract as `AlbumRow`. Two
/// hash families (local `artwork_*` and Deezer-cached `picture_*`)
/// because the UI prefers the extracted local image and only falls
/// back to the Deezer cache when the local one is missing.
#[derive(Debug, Clone, Serialize)]
pub struct ArtistRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    /// Cached-Deezer picture hash. Files are stored under the shared
    /// `metadata_artwork_base`, always as `<hash>.jpg`.
    pub picture_hash: Option<String>,
    pub picture_has_1x: bool,
    pub picture_has_2x: bool,
    /// Deezer CDN URL — last-resort fallback when no local file is
    /// available (e.g. when the cache was wiped or the picture is on a
    /// remote profile being browsed offline).
    pub picture_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListArtistsResponse {
    pub artwork_base: String,
    pub metadata_artwork_base: String,
    pub items: Vec<ArtistRow>,
}

#[derive(FromRow)]
struct ArtistRowRaw {
    id: i64,
    name: String,
    track_count: i64,
    album_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_url: Option<String>,
    picture_hash: Option<String>,
}

/// Slim genre row shipped by `list_genres` — same `artwork_base` economy
/// as `AlbumRow`: hash/format ride on the row, the per-profile prefix
/// rides once on the response.
#[derive(Debug, Clone, Serialize)]
pub struct GenreRow {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListGenresResponse {
    pub artwork_base: String,
    pub items: Vec<GenreRow>,
}

#[derive(FromRow)]
struct GenreRawRow {
    id: i64,
    name: String,
    track_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    pub last_scanned_at: Option<i64>,
    pub is_watched: i64,
    pub track_count: i64,
}

/// Profile-wide counters shown in the sidebar "Playlists" section.
/// Computed on demand; cheap enough to refetch on every
/// `player:track-changed` event.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileStats {
    pub liked_count: i64,
    pub recent_plays_count: i64,
}

/// Return the count of liked tracks and distinct recently-played
/// tracks (applying the same 15 s / completed filter as
/// [`list_recent_plays`] so the numbers stay in sync with the
/// view).
#[tauri::command]
pub async fn get_profile_stats(state: tauri::State<'_, AppState>) -> AppResult<ProfileStats> {
    let pool = state.require_profile_pool().await?;

    let liked_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM liked_track")
        .fetch_one(&*pool)
        .await
        .unwrap_or(0);

    let recent_plays_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT track_id) FROM play_event
          WHERE completed = 1 OR listened_ms >= 15000",
    )
    .fetch_one(&*pool)
    .await
    .unwrap_or(0);

    Ok(ProfileStats {
        liked_count,
        recent_plays_count,
    })
}

/// Row shape returned by `list_recent_plays` — one deduplicated
/// entry per track with its most recent play timestamp. `played_at`
/// and `artwork_path` are resolved post-query.
#[derive(Debug, Clone, Serialize)]
pub struct RecentPlay {
    pub track_id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub played_at: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub file_path: String,
}

/// Internal row shape — the SQL query returns the artwork hash and
/// format separately, and the Rust code resolves the absolute path
/// using the active profile's artwork directory.
#[derive(FromRow)]
struct RecentPlayRaw {
    track_id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    album_id: Option<i64>,
    album_title: Option<String>,
    duration_ms: i64,
    played_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    file_path: String,
}

/// Whitelisted ORDER BY clause builder for `list_albums`. Falls back to
/// the default "Artist → Album" sort whenever the spec isn't recognized.
fn album_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("year") | Some("added_at"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("title"), "ASC") => "ORDER BY al.canonical_title COLLATE NOCASE ASC",
        (Some("title"), "DESC") => "ORDER BY al.canonical_title COLLATE NOCASE DESC",
        (Some("artist"), "ASC") => "ORDER BY ar.canonical_name COLLATE NOCASE ASC, al.canonical_title COLLATE NOCASE",
        (Some("artist"), "DESC") => "ORDER BY ar.canonical_name COLLATE NOCASE DESC, al.canonical_title COLLATE NOCASE",
        (Some("year"), "ASC") => "ORDER BY al.year ASC, al.canonical_title COLLATE NOCASE",
        (Some("year"), "DESC") => "ORDER BY al.year DESC, al.canonical_title COLLATE NOCASE",
        (Some("added_at"), "ASC") => "ORDER BY MIN(t.added_at) ASC",
        (Some("added_at"), "DESC") => "ORDER BY MIN(t.added_at) DESC",
        _ => "ORDER BY ar.canonical_name COLLATE NOCASE,\n                  al.canonical_title COLLATE NOCASE",
    }
}

/// Whitelisted ORDER BY clause builder for `list_artists`.
fn artist_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("albums_count") | Some("tracks_count"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("name"), "ASC") => "ORDER BY ar.canonical_name COLLATE NOCASE ASC",
        (Some("name"), "DESC") => "ORDER BY ar.canonical_name COLLATE NOCASE DESC",
        (Some("albums_count"), "ASC") => {
            "ORDER BY album_count ASC, ar.canonical_name COLLATE NOCASE"
        }
        (Some("albums_count"), "DESC") => {
            "ORDER BY album_count DESC, ar.canonical_name COLLATE NOCASE"
        }
        (Some("tracks_count"), "ASC") => {
            "ORDER BY track_count ASC, ar.canonical_name COLLATE NOCASE"
        }
        (Some("tracks_count"), "DESC") => {
            "ORDER BY track_count DESC, ar.canonical_name COLLATE NOCASE"
        }
        _ => "ORDER BY ar.canonical_name COLLATE NOCASE",
    }
}

/// List every album that has at least one available track in the given
/// library, sorted by artist → album title. Track count and total duration
/// are computed on the fly so the UI can display "Album · N titres · h:mm".
#[tauri::command]
pub async fn list_albums(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = album_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT al.id,
               al.title,
               COALESCE(ar.name, al.album_artist) AS artist_name,
               al.year,
               COUNT(t.id)                     AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash                         AS artwork_hash,
               aw.format                       AS artwork_format,
               MAX(t.bit_depth)                AS max_bit_depth,
               MAX(t.sample_rate)              AS max_sample_rate
          FROM album al
          JOIN track t        ON t.album_id = al.id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
         GROUP BY al.id
         {order_clause}
"#
    );

    let raw = sqlx::query_as::<_, AlbumRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&*pool)
        .await?;

    let items = expand_album_rows(raw, artwork_dir.clone()).await?;

    Ok(ListAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch the thumbnail-existence flags onto a batch of raw album rows.
///
/// Per-row mapping does N synchronous `Path::exists` probes against the
/// artwork dir (via `thumbnail_paths_for`). At 850+ albums × 2 checks
/// that's enough sustained syscalls to noticeably stall the tokio
/// runtime, so we hand the whole batch off to the blocking pool in one
/// shot — single hop, no per-row overhead. Shared by `list_albums` and
/// `search_albums`.
async fn expand_album_rows(
    raw: Vec<AlbumRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<AlbumRow>> {
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
                AlbumRow {
                    id: row.id,
                    title: row.title,
                    artist_name: row.artist_name,
                    year: row.year,
                    track_count: row.track_count,
                    total_duration_ms: row.total_duration_ms,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    max_bit_depth: row.max_bit_depth,
                    max_sample_rate: row.max_sample_rate,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("album row expand join: {e}")))
}

/// One album of the library, whichever source it comes from.
///
/// Deliberately not an [`AlbumRow`]: the two sources do not share an
/// identifier type. A local album is a rowid, a server album is a UUID, and
/// widening the local one to a string across every existing consumer would
/// be a large change to say something small. Here the identifier is text and
/// `source` says how to read it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryAlbumRow {
    /// `"local"` or `"remote"`. The discriminant, not decoration: it decides
    /// how `id` is resolved, where the cover comes from, and which detail
    /// view a click opens.
    pub source: String,
    /// Local rowid rendered as text, or the server's album UUID.
    pub id: String,
    pub title: String,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    /// Content hash of the cover. A local hash names a file in the profile's
    /// artwork directory; a remote one is resolved through the server's cover
    /// cache. Same field, two resolutions — hence `source`.
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    pub max_bit_depth: Option<i64>,
    pub max_sample_rate: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLibraryAlbumsResponse {
    pub artwork_base: String,
    pub items: Vec<LibraryAlbumRow>,
}

#[derive(sqlx::FromRow)]
struct LibraryAlbumRawRow {
    source: String,
    id: String,
    title: String,
    artist_name: Option<String>,
    year: Option<i64>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    max_bit_depth: Option<i64>,
    max_sample_rate: Option<i64>,
}

/// Ordering for the unified listing.
///
/// Separate from [`album_order_clause`] because it has to be: that one sorts
/// on the inner tables' own columns (`al.canonical_title`, `MIN(t.added_at)`),
/// which do not exist outside the local half of the union. This one sorts on
/// the columns the union itself projects, so both halves obey one comparison
/// and one collation — a list sorted differently depending on which source a
/// row came from would be worse than two lists.
fn library_album_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("year") | Some("added_at"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("title"), "ASC") => "ORDER BY sort_title COLLATE NOCASE ASC",
        (Some("title"), "DESC") => "ORDER BY sort_title COLLATE NOCASE DESC",
        (Some("artist"), "ASC") => {
            "ORDER BY sort_artist COLLATE NOCASE ASC, sort_title COLLATE NOCASE"
        }
        (Some("artist"), "DESC") => {
            "ORDER BY sort_artist COLLATE NOCASE DESC, sort_title COLLATE NOCASE"
        }
        (Some("year"), "ASC") => "ORDER BY year ASC, sort_title COLLATE NOCASE",
        (Some("year"), "DESC") => "ORDER BY year DESC, sort_title COLLATE NOCASE",
        (Some("added_at"), "ASC") => "ORDER BY added_at ASC",
        (Some("added_at"), "DESC") => "ORDER BY added_at DESC",
        _ => "ORDER BY sort_artist COLLATE NOCASE, sort_title COLLATE NOCASE",
    }
}

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
struct LibraryTrackRawRow {
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

/// Ordering for the unified track listing.
///
/// Artist and album sort on the normalised keys both halves now carry; the
/// title sorts on the display string on both sides, because the local half has
/// no canonical form for it either. Consistency per column is what matters —
/// normalising one side of a comparison and not the other is exactly how an
/// artist ends up in two places.
fn library_track_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    // `duration_ms` is the column name, and it is what the sort dropdown and
    // the persisted preference both carry. Matching on "duration" here sent
    // every duration sort to the fallback clause instead.
    let dir_default_desc = matches!(
        order_by,
        Some("duration_ms") | Some("added_at") | Some("year") | Some("rating")
    );
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("title"), "ASC") => "ORDER BY title COLLATE NOCASE ASC",
        (Some("title"), "DESC") => "ORDER BY title COLLATE NOCASE DESC",
        (Some("artist"), "ASC") => {
            "ORDER BY sort_artist COLLATE NOCASE ASC, title COLLATE NOCASE"
        }
        (Some("artist"), "DESC") => {
            "ORDER BY sort_artist COLLATE NOCASE DESC, title COLLATE NOCASE"
        }
        (Some("album"), "ASC") => {
            "ORDER BY sort_album COLLATE NOCASE ASC, disc_number, track_number"
        }
        (Some("album"), "DESC") => {
            "ORDER BY sort_album COLLATE NOCASE DESC, disc_number, track_number"
        }
        (Some("duration_ms"), "ASC") => "ORDER BY duration_ms ASC",
        (Some("duration_ms"), "DESC") => "ORDER BY duration_ms DESC",
        (Some("year"), "ASC") => "ORDER BY year ASC, title COLLATE NOCASE",
        (Some("year"), "DESC") => "ORDER BY year DESC, title COLLATE NOCASE",
        (Some("added_at"), "ASC") => "ORDER BY added_at ASC",
        (Some("added_at"), "DESC") => "ORDER BY added_at DESC",
        // Rating is local-only, so a server track has none. NULLs last in
        // either direction: an unratable row is not a badly-rated one.
        (Some("rating"), "ASC") => {
            "ORDER BY rating IS NULL, rating ASC, title COLLATE NOCASE"
        }
        (Some("rating"), "DESC") => {
            "ORDER BY rating IS NULL, rating DESC, title COLLATE NOCASE"
        }
        _ => {
            "ORDER BY sort_artist COLLATE NOCASE,\n                  sort_album COLLATE NOCASE,\n                  disc_number,\n                  track_number,\n                  title COLLATE NOCASE"
        }
    }
}

/// Both halves of the track listing, as one compound select.
///
/// Split out of the command for the reason on [`library_albums_sql`].
fn library_tracks_sql(order_clause: &str) -> String {
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
                   t.musical_key              AS musical_key,
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
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = library_track_order_clause(order_by.as_deref(), direction.as_deref());
    let sql = library_tracks_sql(order_clause);

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
async fn expand_library_track_rows(
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

/// Both halves of the album listing, as one compound select.
///
/// Split out of the command so the SQL can be exercised on its own: the
/// command needs an `AppState`, the query needs only a database, and the query
/// is the part that can be wrong.
/// A local track whose bytes have been read and recorded — RFC-006's
/// "examined" side of the completeness frontier.
///
/// Without it, "no link" is ambiguous: it means either "different bytes" or
/// "nobody has looked yet", and pairing on the second is how a rule hides
/// something it has no evidence about. `local_full_hash` answers the
/// difference, and only while its entry still describes the file on disk.
macro_rules! track_examined {
    ($track:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM local_full_hash h
                      WHERE h.track_id = ",
            $track,
            ".id
                        AND h.file_size = ",
            $track,
            ".file_size
                        AND h.file_modified = ",
            $track,
            ".file_modified)"
        )
    };
}

/// A server album that is the same release as a local one, and may therefore
/// be rendered once rather than twice — [RFC-006](../../../../docs/rfcs/RFC-006-deduplicating-the-two-catalogues.md)
/// decisions 2 and 3.
///
/// A **complete, non-empty bijection** over examined sets: every track on each
/// side confirmed-linked to one on the other, and at least one such link. The
/// one-to-one part is free — `remote_track_link` is a primary key on the local
/// id and `UNIQUE` on the remote one — so "each track has a partner" already
/// means "exactly one".
///
/// Two links and unanimity would **not** do here, which is what an earlier
/// draft of that RFC got wrong: a compilation sharing two recordings with an
/// album satisfies it while being a different release. An album is a closed
/// set, so the set is the evidence.
///
/// The emptiness guard is not decoration either: a bijection over two empty
/// sets holds vacuously, so without it a local album would pair with a server
/// walk that returned nothing.
macro_rules! album_pair_proven {
    () => {
        concat!(
            "ra.mirrored_at IS NOT NULL AND EXISTS (
           SELECT 1 FROM album al
            WHERE EXISTS (
                    SELECT 1 FROM remote_track rt
                      JOIN remote_track_link l
                        ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                      JOIN track t ON t.id = l.local_track_id
                     WHERE rt.album_id = ra.remote_id AND rt.in_catalogue = 1
                       AND t.album_id = al.id AND t.is_available = 1)
              AND NOT EXISTS (
                    SELECT 1 FROM remote_track rt
                     WHERE rt.album_id = ra.remote_id AND rt.in_catalogue = 1
                       AND NOT EXISTS (
                             SELECT 1 FROM remote_track_link l
                               JOIN track t ON t.id = l.local_track_id
                              WHERE l.remote_track_id = rt.remote_id
                                AND l.status = 'confirmed'
                                AND t.album_id = al.id AND t.is_available = 1))
              AND NOT EXISTS (
                    SELECT 1 FROM track t
                     WHERE t.album_id = al.id AND t.is_available = 1
                       AND (NOT EXISTS (
                              SELECT 1 FROM remote_track_link l
                                JOIN remote_track rt ON rt.remote_id = l.remote_track_id
                               WHERE l.local_track_id = t.id AND l.status = 'confirmed'
                                AND rt.album_id = ra.remote_id AND rt.in_catalogue = 1)
                            OR NOT ",
            track_examined!("t"),
            "))
         )"
        )
    };
}

/// A server artist that is the same person as a local one — RFC-006 decision 2,
/// and deliberately **not** the album rule.
///
/// An artist is an open grouping: nobody's discography is complete on either
/// side, so demanding a bijection would pair nobody. The evidence is about the
/// person, not the extent of their work, so a sample suffices — **unanimity,
/// and at least two confirmed links**.
///
/// Unanimity disposes of *Various Artists* with no special case: a compilation
/// has tracks linked to a dozen different server artists, so the second
/// disagreeing link ends the question. Two links because one guest appearance,
/// credited to the featured artist on one side and the host on the other, is a
/// coincidence rather than evidence.
///
/// Both sides of the disagreement are checked. Looking only from the server
/// side would fold a local *Various Artists* into a real artist whenever the
/// compilation's own tracks happened to agree.
macro_rules! artist_pair_proven {
    () => {
        concat!(
            "EXISTS (
           SELECT 1 FROM artist la
            WHERE (SELECT COUNT(*)
                     FROM remote_track rt
                     JOIN remote_track_link l
                       ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                     JOIN track t ON t.id = l.local_track_id
                    WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1
                      AND t.primary_artist = la.id AND t.is_available = 1
                      AND ",
            track_examined!("t"),
            ") >= 2
              AND NOT EXISTS (
                    SELECT 1 FROM remote_track rt
                      JOIN remote_track_link l
                        ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                      JOIN track t ON t.id = l.local_track_id
                     WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1
                       AND t.is_available = 1
                       AND (t.primary_artist IS NULL OR t.primary_artist != la.id))
              AND NOT EXISTS (
                    SELECT 1 FROM track t
                      JOIN remote_track_link l
                        ON l.local_track_id = t.id AND l.status = 'confirmed'
                      JOIN remote_track rt ON rt.remote_id = l.remote_track_id
                     WHERE t.primary_artist = la.id AND t.is_available = 1
                       AND rt.in_catalogue = 1
                       AND (rt.artist_id IS NULL OR rt.artist_id != ra.remote_id))
         )"
        )
    };
}

fn library_albums_sql(order_clause: &str) -> String {
    let album_pair = album_pair_proven!();
    format!(
        r#"
        SELECT source, id, title, artist_name, year, track_count, total_duration_ms,
               artwork_hash, artwork_format, max_bit_depth, max_sample_rate
          FROM (
            SELECT 'local'                             AS source,
                   CAST(al.id AS TEXT)                 AS id,
                   al.title                            AS title,
                   COALESCE(ar.name, al.album_artist)  AS artist_name,
                   al.year                             AS year,
                   COUNT(t.id)                         AS track_count,
                   COALESCE(SUM(t.duration_ms), 0)     AS total_duration_ms,
                   aw.hash                             AS artwork_hash,
                   aw.format                           AS artwork_format,
                   MAX(t.bit_depth)                    AS max_bit_depth,
                   MAX(t.sample_rate)                  AS max_sample_rate,
                   al.canonical_title                  AS sort_title,
                   COALESCE(ar.canonical_name, al.album_artist) AS sort_artist,
                   MIN(t.added_at)                     AS added_at
              FROM album al
              JOIN track t         ON t.album_id = al.id
              LEFT JOIN artist ar  ON ar.id = al.artist_id
              LEFT JOIN artwork aw ON aw.id = al.artwork_id
             WHERE (? IS NULL OR t.library_id = ?)
               AND t.is_available = 1
             GROUP BY al.id
            UNION ALL
            SELECT 'remote',
                   ra.remote_id,
                   ra.title,
                   ra.artist,
                   ra.year,
                   ra.song_count,
                   ra.duration_ms,
                   ra.artwork_hash,
                   NULL,
                   NULL,
                   NULL,
                   COALESCE(ra.sort_title, ra.title),
                   COALESCE(ra.sort_artist, ra.artist),
                   ra.created_at
              FROM remote_album ra
             -- A local library filter is a filter over *local* libraries; a
             -- server album belongs to none of them. Keeping the remote half
             -- visible while the user has narrowed to one local library reads
             -- as the filter having failed.
             WHERE ? IS NULL
             -- One release, one entry: a server album proven to be the same
             -- release as a local one is rendered from the local half rather
             -- than beside it. See `album_pair_proven!`.
               AND NOT ({album_pair})
          )
         WHERE (? IS NULL OR source = ?)
         {order_clause}
"#
    )
}

/// Every album the library can show, from the device and from the bound
/// server, as one sorted list.
///
/// The two halves are **not** merged: an album held both locally and on the
/// server appears twice, tagged twice, which is what RFC-005 decision 1 says
/// and what the source chip explains. Unifying the navigation is not
/// deduplicating the catalogue.
///
/// `source` filters the list to one half; `None` means both. The remote half
/// comes from the mirrored catalogue, so it is exactly as complete as the
/// last walk left it — and empty, at no cost, on a build without `sync_v2`.
#[tauri::command]
pub async fn list_library_albums(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    source: Option<String>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let order_clause = library_album_order_clause(order_by.as_deref(), direction.as_deref());

    // The sort keys are projected rather than computed in the ORDER BY: the
    // two halves spell them differently (`canonical_title` against
    // `sort_name`) and only the aliases exist outside the union.
    let sql = library_albums_sql(order_clause);

    let raw = sqlx::query_as::<_, LibraryAlbumRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source.as_deref())
        .bind(source.as_deref())
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_album_rows(raw, artwork_dir.clone()).await?;

    Ok(ListLibraryAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto the local half only.
///
/// A remote hash names nothing in the profile's artwork directory, so probing
/// for it would be `is_file` calls guaranteed to fail — once per remote album,
/// twice each, on the blocking pool. The frontend resolves those covers
/// through the server cover cache instead.
async fn expand_library_album_rows(
    raw: Vec<LibraryAlbumRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<LibraryAlbumRow>> {
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
                LibraryAlbumRow {
                    source: row.source,
                    id: row.id,
                    title: row.title,
                    artist_name: row.artist_name,
                    year: row.year,
                    track_count: row.track_count,
                    total_duration_ms: row.total_duration_ms,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    max_bit_depth: row.max_bit_depth,
                    max_sample_rate: row.max_sample_rate,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("library album row expand join: {e}")))
}

/// One artist of the library, whichever source they come from. Same shape
/// contract as [`LibraryAlbumRow`]: text identifier, `source` says how to read
/// it.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryArtistRow {
    pub source: String,
    pub id: String,
    pub name: String,
    pub track_count: i64,
    pub album_count: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    pub picture_hash: Option<String>,
    pub picture_has_1x: bool,
    pub picture_has_2x: bool,
    pub picture_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListLibraryArtistsResponse {
    pub artwork_base: String,
    pub metadata_artwork_base: String,
    pub items: Vec<LibraryArtistRow>,
}

#[derive(sqlx::FromRow)]
struct LibraryArtistRawRow {
    source: String,
    id: String,
    name: String,
    track_count: i64,
    album_count: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_url: Option<String>,
    picture_hash: Option<String>,
}

/// Ordering for the unified artist listing — on the union's own columns, for
/// the reason spelled out on [`library_album_order_clause`].
fn library_artist_order_clause(order_by: Option<&str>, direction: Option<&str>) -> &'static str {
    let dir_default_desc = matches!(order_by, Some("albums_count") | Some("tracks_count"));
    let dir = match direction {
        Some(d) if d.eq_ignore_ascii_case("asc") => "ASC",
        Some(d) if d.eq_ignore_ascii_case("desc") => "DESC",
        _ => {
            if dir_default_desc {
                "DESC"
            } else {
                "ASC"
            }
        }
    };
    match (order_by, dir) {
        (Some("name"), "ASC") => "ORDER BY sort_name COLLATE NOCASE ASC",
        (Some("name"), "DESC") => "ORDER BY sort_name COLLATE NOCASE DESC",
        (Some("albums_count"), "ASC") => "ORDER BY album_count ASC, sort_name COLLATE NOCASE",
        (Some("albums_count"), "DESC") => "ORDER BY album_count DESC, sort_name COLLATE NOCASE",
        (Some("tracks_count"), "ASC") => "ORDER BY track_count ASC, sort_name COLLATE NOCASE",
        (Some("tracks_count"), "DESC") => "ORDER BY track_count DESC, sort_name COLLATE NOCASE",
        _ => "ORDER BY sort_name COLLATE NOCASE",
    }
}

/// Both halves of the artist listing, as one compound select.
///
/// Split out of the command so the SQL can be exercised on its own: the
/// command needs an `AppState`, the query needs only a database, and the query
/// is the part that can be wrong.
fn library_artists_sql(order_clause: &str) -> String {
    let artist_pair = artist_pair_proven!();
    format!(
        r#"
        SELECT source, id, name, track_count, album_count,
               artwork_hash, artwork_format, picture_url, picture_hash
          FROM (
            SELECT 'local'                     AS source,
                   CAST(ar.id AS TEXT)         AS id,
                   ar.name                     AS name,
                   COUNT(DISTINCT t.id)        AS track_count,
                   COUNT(DISTINCT t.album_id)  AS album_count,
                   aw.hash                     AS artwork_hash,
                   aw.format                   AS artwork_format,
                   da.picture_url              AS picture_url,
                   da.picture_hash             AS picture_hash,
                   ar.canonical_name           AS sort_name
              FROM artist ar
              JOIN track_artist ta ON ta.artist_id = ar.id
              JOIN track t         ON t.id = ta.track_id
              LEFT JOIN artwork aw ON aw.id = ar.artwork_id
              LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
             WHERE (? IS NULL OR t.library_id = ?)
               AND t.is_available = 1
             GROUP BY ar.id
            UNION ALL
            SELECT 'remote',
                   ra.remote_id,
                   ra.name,
                   (SELECT count(*) FROM remote_track rt
                     WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1),
                   (SELECT count(*) FROM remote_album al
                     WHERE al.artist_id = ra.remote_id),
                   ra.artwork_hash,
                   NULL,
                   NULL,
                   NULL,
                   COALESCE(ra.sort_key, ra.name)
              FROM remote_artist ra
             -- A local library filter is a filter over local libraries; see
             -- `list_library_albums`.
             WHERE ? IS NULL
             -- One person, one entry. See `artist_pair_proven!` — a different
             -- rule from the albums', because an artist is an open grouping.
               AND NOT ({artist_pair})
          )
         WHERE (? IS NULL OR source = ?)
         {order_clause}
"#
    )
}

/// Every artist the library can show, from the device and from the bound
/// server, as one sorted list.
///
/// Not merged, on the same terms as the albums: an artist credited on both
/// sides appears twice. The remote half comes from the mirrored catalogue, and
/// its counts are derived from the albums and tracks already mirrored rather
/// than stored — a stored count would go stale the moment an album is walked.
#[tauri::command]
pub async fn list_library_artists(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    source: Option<String>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListLibraryArtistsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();

    let order_clause = library_artist_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = library_artists_sql(order_clause);

    let raw = sqlx::query_as::<_, LibraryArtistRawRow>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .bind(library_id)
        .bind(source.as_deref())
        .bind(source.as_deref())
        .fetch_all(&*pool)
        .await?;

    let items = expand_library_artist_rows(raw, artwork_dir.clone(), metadata_dir.clone()).await?;

    Ok(ListLibraryArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto the local half only — a remote hash
/// names nothing in either local directory. See
/// [`expand_library_album_rows`].
async fn expand_library_artist_rows(
    raw: Vec<LibraryArtistRawRow>,
    artwork_dir: PathBuf,
    metadata_dir: PathBuf,
) -> AppResult<Vec<LibraryArtistRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|r| {
                let local = r.source == "local";
                let (artwork_has_1x, artwork_has_2x) = match r.artwork_hash.as_deref() {
                    Some(hash) if local => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    _ => (false, false),
                };
                let (picture_has_1x, picture_has_2x) = match r.picture_hash.as_deref() {
                    Some(hash) if local => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&metadata_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    _ => (false, false),
                };
                LibraryArtistRow {
                    source: r.source,
                    id: r.id,
                    name: r.name,
                    track_count: r.track_count,
                    album_count: r.album_count,
                    artwork_hash: r.artwork_hash,
                    artwork_format: r.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    picture_hash: r.picture_hash,
                    picture_has_1x,
                    picture_has_2x,
                    picture_url: r.picture_url,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("library artist row expand join: {e}")))
}

/// List every primary artist that has at least one available track in the
/// given library, with track and album counts.
#[tauri::command]
pub async fn list_artists(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    order_by: Option<String>,
    direction: Option<String>,
) -> AppResult<ListArtistsResponse> {
    let pool = state.require_profile_pool().await?;

    let order_clause = artist_order_clause(order_by.as_deref(), direction.as_deref());

    let sql = format!(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               aw.hash                    AS artwork_hash,
               aw.format                  AS artwork_format,
               da.picture_url             AS picture_url,
               da.picture_hash            AS picture_hash
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY ar.id
         {order_clause}
        "#
    );

    let raw = sqlx::query_as::<_, ArtistRowRaw>(sqlx::AssertSqlSafe(sql))
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&*pool)
        .await?;

    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();
    let items = expand_artist_rows(raw, artwork_dir.clone(), metadata_dir.clone()).await?;

    Ok(ListArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch local + Deezer thumbnail-existence flags onto a batch of raw
/// artist rows. Same blocking-pool offload as [`expand_album_rows`]:
/// each row triggers up to 5 `Path::exists` probes (1 Deezer-full + 2
/// local thumbs + 2 Deezer thumbs) — at 900 artists that's ~4 500
/// syscalls in a tight loop, well past the threshold where stalling the
/// tokio runtime starts to matter. Shared by `list_artists` and
/// `search_artists`.
async fn expand_artist_rows(
    raw: Vec<ArtistRowRaw>,
    artwork_dir: PathBuf,
    metadata_dir: PathBuf,
) -> AppResult<Vec<ArtistRow>> {
    tokio::task::spawn_blocking(move || {
        raw.into_iter()
            .map(|r| {
                let (artwork_has_1x, artwork_has_2x) = match r.artwork_hash.as_deref() {
                    Some(hash) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                // For the Deezer cache the "full" file uses the same
                // `<hash>.jpg` naming pattern, so we can drop a `picture_hash`
                // when the source file is missing — the frontend won't have
                // anything to point a thumbnail variant at either.
                let picture_hash = r
                    .picture_hash
                    .filter(|h| crate::metadata_artwork::existing_path(&metadata_dir, h).is_some());
                let (picture_has_1x, picture_has_2x) = match picture_hash.as_deref() {
                    Some(h) => {
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&metadata_dir, h);
                        (p1.is_some(), p2.is_some())
                    }
                    None => (false, false),
                };
                ArtistRow {
                    id: r.id,
                    name: r.name,
                    track_count: r.track_count,
                    album_count: r.album_count,
                    artwork_hash: r.artwork_hash,
                    artwork_format: r.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                    picture_hash,
                    picture_has_1x,
                    picture_has_2x,
                    picture_url: r.picture_url,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("artist row expand join: {e}")))
}

/// Search albums by name for the global top-bar search. Matches the
/// query's [`canonical_name`](waveflow_core::scanner::canonical_name)
/// form as a substring of `album.canonical_title` (case / accent
/// insensitive via the canonical column, no FTS — `track_fts` is
/// track-scoped). Prefix matches rank first. Returns the same slim
/// `{ artwork_base, items }` shape as [`list_albums`] so the frontend
/// reuses `expandAlbumRow`.
#[tauri::command]
pub async fn search_albums(
    state: tauri::State<'_, AppState>,
    query: String,
    library_id: Option<i64>,
    limit: Option<i64>,
) -> AppResult<ListAlbumsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let canon = waveflow_core::scanner::canonical_name(query.trim());
    if canon.is_empty() {
        return Ok(ListAlbumsResponse {
            artwork_base: artwork_dir.to_string_lossy().into_owned(),
            items: Vec::new(),
        });
    }
    // Small dropdown sections — clamp to keep the payload + the
    // per-row thumbnail probes bounded.
    let limit = limit.unwrap_or(8).clamp(1, 50);

    let raw = sqlx::query_as::<_, AlbumRawRow>(
        r#"
        SELECT al.id,
               al.title,
               COALESCE(ar.name, al.album_artist) AS artist_name,
               al.year,
               COUNT(t.id)                     AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash                         AS artwork_hash,
               aw.format                       AS artwork_format,
               MAX(t.bit_depth)                AS max_bit_depth,
               MAX(t.sample_rate)              AS max_sample_rate
          FROM album al
          JOIN track t        ON t.album_id = al.id
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND instr(al.canonical_title, ?) > 0
         GROUP BY al.id
         ORDER BY (instr(al.canonical_title, ?) = 1) DESC,
                  al.canonical_title COLLATE NOCASE
         LIMIT ?
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .bind(&canon)
    .bind(&canon)
    .bind(limit)
    .fetch_all(&*pool)
    .await?;

    let items = expand_album_rows(raw, artwork_dir.clone()).await?;

    Ok(ListAlbumsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Search artists by name for the global top-bar search. Mirror of
/// [`search_albums`] over `artist.canonical_name`; returns the same slim
/// shape as [`list_artists`] so the frontend reuses `expandArtistRow`.
#[tauri::command]
pub async fn search_artists(
    state: tauri::State<'_, AppState>,
    query: String,
    library_id: Option<i64>,
    limit: Option<i64>,
) -> AppResult<ListArtistsResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let metadata_dir = state.paths.metadata_artwork_dir.clone();

    let canon = waveflow_core::scanner::canonical_name(query.trim());
    if canon.is_empty() {
        return Ok(ListArtistsResponse {
            artwork_base: artwork_dir.to_string_lossy().into_owned(),
            metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
            items: Vec::new(),
        });
    }
    let limit = limit.unwrap_or(8).clamp(1, 50);

    let raw = sqlx::query_as::<_, ArtistRowRaw>(
        r#"
        SELECT ar.id,
               ar.name,
               COUNT(DISTINCT t.id)       AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count,
               aw.hash                    AS artwork_hash,
               aw.format                  AS artwork_format,
               da.picture_url             AS picture_url,
               da.picture_hash            AS picture_hash
          FROM artist ar
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND instr(ar.canonical_name, ?) > 0
         GROUP BY ar.id
         ORDER BY (instr(ar.canonical_name, ?) = 1) DESC,
                  ar.canonical_name COLLATE NOCASE
         LIMIT ?
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .bind(&canon)
    .bind(&canon)
    .bind(limit)
    .fetch_all(&*pool)
    .await?;

    let items = expand_artist_rows(raw, artwork_dir.clone(), metadata_dir.clone()).await?;

    Ok(ListArtistsResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        metadata_artwork_base: metadata_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// List every genre that tags at least one available track in the given
/// library, with a track count.
#[tauri::command]
pub async fn list_genres(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
) -> AppResult<ListGenresResponse> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let raw = sqlx::query_as::<_, GenreRawRow>(
        r#"
        SELECT g.id,
               g.name,
               COUNT(DISTINCT t.id) AS track_count,
               aw.hash              AS artwork_hash,
               aw.format            AS artwork_format
          FROM genre g
          JOIN track_genre tg ON tg.genre_id = g.id
          JOIN track t         ON t.id = tg.track_id
          LEFT JOIN artwork aw ON aw.id = g.artwork_id
         WHERE (? IS NULL OR t.library_id = ?) AND t.is_available = 1
         GROUP BY g.id
         ORDER BY g.canonical_name COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .fetch_all(&*pool)
    .await?;

    let items = expand_genre_rows(raw, artwork_dir.clone()).await?;

    Ok(ListGenresResponse {
        artwork_base: artwork_dir.to_string_lossy().into_owned(),
        items,
    })
}

/// Stitch thumbnail-existence flags onto a batch of raw genre rows —
/// same off-thread batching rationale as `expand_album_rows`.
async fn expand_genre_rows(
    raw: Vec<GenreRawRow>,
    artwork_dir: PathBuf,
) -> AppResult<Vec<GenreRow>> {
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
                GenreRow {
                    id: row.id,
                    name: row.name,
                    track_count: row.track_count,
                    artwork_hash: row.artwork_hash,
                    artwork_format: row.artwork_format,
                    artwork_has_1x,
                    artwork_has_2x,
                }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("genre row expand join: {e}")))
}

/// Manually upload an image file as a genre's picture (issue #424) — a
/// local jpg/png/webp the user picks themselves; genres have no
/// automatic/embedded artwork source of their own, unlike album/artist.
/// Same magic-byte validation as `set_artist_artwork_from_file`.
#[tauri::command]
pub async fn set_genre_artwork_from_file(
    state: tauri::State<'_, AppState>,
    genre_id: i64,
    file_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let profile_artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // Off the async runtime: create_dir_all/read/write are all blocking
    // syscalls, and the read is bounded to MAX_IMAGE_BYTES + 1 so an
    // oversized file is rejected without first loading the whole thing
    // into memory.
    let dir_for_blocking = profile_artwork_dir.clone();
    let (hash, format): (String, &'static str) = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dir_for_blocking)?;

        let mut file = std::fs::File::open(&file_path)?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(crate::commands::deezer::MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > crate::commands::deezer::MAX_IMAGE_BYTES {
            return Err(AppError::Other(format!(
                "file too large (max {} bytes)",
                crate::commands::deezer::MAX_IMAGE_BYTES
            )));
        }
        let format = crate::commands::deezer::detect_image_format(&bytes).ok_or_else(|| {
            AppError::Other("unsupported image format (expected jpg/png/webp)".into())
        })?;

        let hash = blake3::hash(&bytes).to_hex().to_string();
        let target = dir_for_blocking.join(format!("{hash}.{format}"));
        if !target.exists() {
            std::fs::write(&target, &bytes)?;
        }
        Ok::<_, AppError>((hash, format))
    })
    .await
    .map_err(|e| AppError::Other(format!("genre artwork blocking task join: {e}")))??;

    let target = profile_artwork_dir.join(format!("{hash}.{format}"));
    crate::thumbnails::spawn_thumbnail_job(target, profile_artwork_dir.clone(), hash.clone());

    let mut tx = pool.begin().await?;
    let artwork_id =
        waveflow_core::scanner::upsert_artwork(&mut tx, &hash, format, "manual").await?;
    let res = sqlx::query("UPDATE genre SET artwork_id = ? WHERE id = ?")
        .bind(artwork_id)
        .bind(genre_id)
        .execute(&mut *tx)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("genre {genre_id} not found")));
    }
    tx.commit().await?;

    Ok(())
}

/// Detach a genre's manual picture. The orphaned `artwork` row (if no
/// longer referenced) is left in place — same future-GC-pass note as
/// `clear_artist_artwork`.
#[tauri::command]
pub async fn clear_genre_artwork(
    state: tauri::State<'_, AppState>,
    genre_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let res = sqlx::query("UPDATE genre SET artwork_id = NULL WHERE id = ?")
        .bind(genre_id)
        .execute(&*pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!("genre {genre_id} not found")));
    }
    Ok(())
}

/// List the most-recently-played tracks for a library, deduplicated
/// to one entry per track (taking the max `played_at` across all
/// `play_event` rows for that track). Used by the "Récemment joués"
/// view in the sidebar.
#[tauri::command]
pub async fn list_recent_plays(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
    limit: i64,
) -> AppResult<Vec<RecentPlay>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let artwork_dir = profile_id.map(|pid| state.paths.profile_artwork_dir(pid));

    let raw = sqlx::query_as::<_, RecentPlayRaw>(
        r#"
        SELECT t.id                         AS track_id,
               t.title                      AS title,
               t.primary_artist             AS artist_id,
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
               t.album_id                   AS album_id,
               al.title                     AS album_title,
               t.duration_ms                AS duration_ms,
               MAX(pe.played_at)            AS played_at,
               aw.hash                      AS artwork_hash,
               aw.format                    AS artwork_format,
               t.file_path                  AS file_path
          FROM play_event pe
          JOIN track t        ON t.id = pe.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artist ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE (? IS NULL OR t.library_id = ?)
           AND t.is_available = 1
           AND (pe.completed = 1 OR pe.listened_ms >= 15000)
         GROUP BY t.id
         ORDER BY played_at DESC
         LIMIT ?
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .bind(limit)
    .fetch_all(&*pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
                row.artwork_hash.as_deref(),
                row.artwork_format.as_deref(),
                artwork_dir.as_ref(),
            ) {
                (Some(hash), Some(format), Some(dir)) => {
                    let full = dir
                        .join(format!("{hash}.{format}"))
                        .to_string_lossy()
                        .to_string();
                    let (p1, p2) = crate::thumbnails::thumbnail_paths_for(dir, hash);
                    (Some(full), p1, p2)
                }
                _ => (None, None, None),
            };
            RecentPlay {
                track_id: row.track_id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                album_id: row.album_id,
                album_title: row.album_title,
                duration_ms: row.duration_ms,
                played_at: row.played_at,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                file_path: row.file_path,
            }
        })
        .collect();

    Ok(rows)
}

/// List every folder registered under the given library, along with the
/// number of tracks found inside it at the last scan.
#[tauri::command]
pub async fn list_folders(
    state: tauri::State<'_, AppState>,
    library_id: Option<i64>,
) -> AppResult<Vec<FolderRow>> {
    let pool = state.require_profile_pool().await?;

    let rows = sqlx::query_as::<_, FolderRow>(
        r#"
        SELECT lf.id,
               lf.path,
               lf.last_scanned_at,
               lf.is_watched,
               COALESCE(COUNT(t.id), 0) AS track_count
          FROM library_folder lf
          LEFT JOIN track t
            ON t.folder_id = lf.id AND t.is_available = 1
         WHERE (? IS NULL OR lf.library_id = ?)
         GROUP BY lf.id
         ORDER BY lf.path COLLATE NOCASE
        "#,
    )
    .bind(library_id)
    .bind(library_id)
    .fetch_all(&*pool)
    .await?;

    Ok(rows)
}

// ── Album detail ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct AlbumDetail {
    pub id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub label: Option<String>,
    pub release_date: Option<String>,
    pub genres: Vec<String>,
    pub tracks: Vec<AlbumTrack>,
}

#[derive(FromRow)]
struct AlbumDetailRaw {
    id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    year: Option<i64>,
    release_date: Option<String>,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlbumTrack {
    pub id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub file_path: String,
    /// Per-track quality fields surfaced for the inline Hi-Res
    /// badge on the AlbumDetailView track list.
    pub bit_depth: Option<i64>,
    pub sample_rate: Option<i64>,
    /// Codec label from the scanner (e.g. "FLAC", "MP3", "DSD128").
    /// Lets the inline Hi-Res badge swap to a "DSD64/128/…" label
    /// for DSF/DFF tracks where bit_depth=1 would otherwise look
    /// like junk to the badge logic.
    pub codec: Option<String>,
    // The remaining fields exist so the Properties modal opened from
    // this view shows the same Audio / File sections it shows
    // everywhere else. AlbumDetailView has to synthesise a full
    // `Track` for the context menu, and anything missing here became
    // a hard-coded null there — which is exactly what left those
    // sections blank on the album page only (issue #458).
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub channels: Option<i64>,
    pub musical_key: Option<String>,
    pub file_size: i64,
    pub added_at: i64,
    /// Half-star rating (POPM round-trip). Selected so the context
    /// menu's rating submenu reflects reality here: it is enabled by
    /// default, so a hard-coded `null` made an already-rated track
    /// read as unrated on this view.
    pub rating: Option<i64>,
}

#[derive(FromRow)]
struct AlbumTrackRaw {
    id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    file_path: String,
    bit_depth: Option<i64>,
    sample_rate: Option<i64>,
    codec: Option<String>,
    year: Option<i64>,
    bitrate: Option<i64>,
    channels: Option<i64>,
    musical_key: Option<String>,
    file_size: i64,
    added_at: i64,
    rating: Option<i64>,
}

/// Return full album detail: header (with Deezer-cached label), genres,
/// and tracks ordered by disc then track number.
#[tauri::command]
pub async fn get_album_detail(
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<AlbumDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let header = sqlx::query_as::<_, AlbumDetailRaw>(
        r#"
        SELECT al.id, al.title, al.artist_id,
               COALESCE(ar.name, al.album_artist) AS artist_name,
               al.year, al.release_date,
               aw.hash AS artwork_hash, aw.format AS artwork_format,
               da.label
          FROM album al
          LEFT JOIN artist ar ON ar.id = al.artist_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
          LEFT JOIN app.metadata_album da ON da.deezer_id = al.deezer_id
         WHERE al.id = ?
        "#,
    )
    .bind(album_id)
    .fetch_optional(&*pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("album not found".into()))?;

    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        header.artwork_hash.as_deref(),
        header.artwork_format.as_deref(),
    ) {
        (Some(hash), Some(format)) => {
            let full = artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
            (Some(full), p1, p2)
        }
        _ => (None, None, None),
    };

    let genres: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT DISTINCT g.name
          FROM genre g
          JOIN track_genre tg ON tg.genre_id = g.id
          JOIN track t ON t.id = tg.track_id
         WHERE t.album_id = ?
         ORDER BY g.name COLLATE NOCASE
        "#,
    )
    .bind(album_id)
    .fetch_all(&*pool)
    .await?;

    // Collapse duplicate files (same album/disc/track_number) — e.g. when the
    // same song was scanned in both FLAC and MP3 form. We keep the highest-
    // quality variant per slot: bit_depth desc, then sample_rate desc, then
    // file_size desc, then id asc as a stable tie-breaker. Tracks without a
    // track_number get their own slot (-id) so they're never collapsed
    // together blindly.
    //
    // DSD nuance: DSF/DFF tracks report `bit_depth = 1` (one bit per sample,
    // not 1-bit lossy). A naïve `bit_depth DESC` would rank them BELOW
    // 16-bit MP3, dropping the higher-quality DSD variant from a mixed
    // DSD/PCM album. The first sort key promotes `bit_depth = 1` rows ahead
    // of every PCM row so DSD always wins the collapse when present.
    let tracks_raw = sqlx::query_as::<_, AlbumTrackRaw>(
        r#"
        WITH ranked AS (
            SELECT t.id,
                   ROW_NUMBER() OVER (
                       PARTITION BY COALESCE(t.disc_number, 1),
                                    COALESCE(t.track_number, -t.id)
                       ORDER BY (t.bit_depth IS NULL),
                                (t.bit_depth = 1) DESC,
                                t.bit_depth DESC,
                                t.sample_rate DESC,
                                t.file_size DESC,
                                t.id ASC
                   ) AS rn
              FROM track t
             WHERE t.album_id = ? AND t.is_available = 1
        )
        SELECT t.id, t.title,
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
               t.duration_ms, t.track_number, t.disc_number,
               t.file_path,
               t.bit_depth, t.sample_rate, t.codec,
               t.year, t.bitrate, t.channels, t.musical_key,
               t.file_size, t.added_at, t.rating,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM ranked r
          JOIN track t ON t.id = r.id
          LEFT JOIN album al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE r.rn = 1
         ORDER BY t.disc_number, t.track_number
        "#,
    )
    .bind(album_id)
    .fetch_all(&*pool)
    .await?;

    let tracks: Vec<AlbumTrack> = tracks_raw
        .into_iter()
        .map(|row| {
            let (track_artwork, track_artwork_1x, track_artwork_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            AlbumTrack {
                id: row.id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                duration_ms: row.duration_ms,
                track_number: row.track_number,
                disc_number: row.disc_number,
                artwork_path: track_artwork,
                artwork_path_1x: track_artwork_1x,
                artwork_path_2x: track_artwork_2x,
                file_path: row.file_path,
                bit_depth: row.bit_depth,
                sample_rate: row.sample_rate,
                codec: row.codec,
                year: row.year,
                bitrate: row.bitrate,
                channels: row.channels,
                musical_key: row.musical_key,
                file_size: row.file_size,
                added_at: row.added_at,
                rating: row.rating,
            }
        })
        .collect();

    let track_count = tracks.len() as i64;
    let total_duration_ms = tracks.iter().map(|t| t.duration_ms).sum();

    Ok(AlbumDetail {
        id: header.id,
        title: header.title,
        artist_id: header.artist_id,
        artist_name: header.artist_name,
        year: header.year,
        track_count,
        total_duration_ms,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
        label: header.label,
        release_date: header.release_date,
        genres,
        tracks,
    })
}

// ── Artist detail ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ArtistDetail {
    pub id: i64,
    pub name: String,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub picture_url: Option<String>,
    pub picture_path: Option<String>,
    pub picture_path_1x: Option<String>,
    pub picture_path_2x: Option<String>,
    pub fans_count: Option<i64>,
    pub bio_short: Option<String>,
    pub bio_full: Option<String>,
    /// Wide TheAudioDB fanart backing the artist hero (issue #482).
    /// Served straight from the metadata cache so the hero paints on the
    /// first frame instead of waiting for `enrich_artist_deezer`.
    pub background_url: Option<String>,
    pub background_path: Option<String>,
    pub track_count: i64,
    pub album_count: i64,
    pub albums: Vec<ArtistAlbumRow>,
}

#[derive(FromRow)]
struct ArtistDetailRaw {
    id: i64,
    name: String,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    picture_url: Option<String>,
    picture_hash: Option<String>,
    fans_count: Option<i64>,
    bio_short: Option<String>,
    bio_full: Option<String>,
    background_url: Option<String>,
    background_hash: Option<String>,
    track_count: i64,
    album_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtistAlbumRow {
    pub id: i64,
    pub title: String,
    pub year: Option<i64>,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
}

#[derive(FromRow)]
struct ArtistAlbumRawRow {
    id: i64,
    title: String,
    year: Option<i64>,
    track_count: i64,
    total_duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

/// Return full artist detail: header, discography, and track count.
#[tauri::command]
pub async fn get_artist_detail(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<ArtistDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let header = sqlx::query_as::<_, ArtistDetailRaw>(
        r#"
        SELECT ar.id, ar.name,
               aw.hash AS artwork_hash, aw.format AS artwork_format,
               da.picture_url  AS picture_url,
               da.picture_hash AS picture_hash,
               da.fans_count   AS fans_count,
               da.bio_short    AS bio_short,
               da.bio_full     AS bio_full,
               da.background_url  AS background_url,
               da.background_hash AS background_hash,
               COUNT(DISTINCT t.id) AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count
          FROM artist ar
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
          JOIN track_artist ta ON ta.artist_id = ar.id
          JOIN track t ON t.id = ta.track_id AND t.is_available = 1
         WHERE ar.id = ?
         GROUP BY ar.id
        "#,
    )
    .bind(artist_id)
    .fetch_optional(&*pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("artist not found".into()))?;

    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        header.artwork_hash.as_deref(),
        header.artwork_format.as_deref(),
    ) {
        (Some(hash), Some(format)) => {
            let full = artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
            (Some(full), p1, p2)
        }
        _ => (None, None, None),
    };

    let albums_raw = sqlx::query_as::<_, ArtistAlbumRawRow>(
        r#"
        SELECT al.id, al.title, al.year,
               COUNT(DISTINCT t.id) AS track_count,
               COALESCE(SUM(t.duration_ms), 0) AS total_duration_ms,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM album al
          JOIN track t ON t.album_id = al.id AND t.is_available = 1
          JOIN track_artist ta ON ta.track_id = t.id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE ta.artist_id = ?
         GROUP BY al.id
         ORDER BY al.year DESC, al.canonical_title COLLATE NOCASE
        "#,
    )
    .bind(artist_id)
    .fetch_all(&*pool)
    .await?;

    let albums = albums_raw
        .into_iter()
        .map(|row| {
            let (album_artwork, album_artwork_1x, album_artwork_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(fmt)) => {
                        let full = artwork_dir
                            .join(format!("{hash}.{fmt}"))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            ArtistAlbumRow {
                id: row.id,
                title: row.title,
                year: row.year,
                track_count: row.track_count,
                total_duration_ms: row.total_duration_ms,
                artwork_path: album_artwork,
                artwork_path_1x: album_artwork_1x,
                artwork_path_2x: album_artwork_2x,
            }
        })
        .collect();

    let metadata_dir = &state.paths.metadata_artwork_dir;
    let picture_path = header
        .picture_hash
        .as_deref()
        .and_then(|h| crate::metadata_artwork::existing_path(metadata_dir, h));
    let (picture_path_1x, picture_path_2x) = match header.picture_hash.as_deref() {
        Some(h) => crate::thumbnails::thumbnail_paths_for(metadata_dir, h),
        None => (None, None),
    };
    let background_path = header
        .background_hash
        .as_deref()
        .and_then(|h| crate::metadata_artwork::existing_path(metadata_dir, h));

    Ok(ArtistDetail {
        id: header.id,
        name: header.name,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
        picture_url: header.picture_url,
        picture_path,
        picture_path_1x,
        picture_path_2x,
        fans_count: header.fans_count,
        bio_short: header.bio_short,
        bio_full: header.bio_full,
        background_url: header.background_url,
        background_path,
        track_count: header.track_count,
        album_count: header.album_count,
        albums,
    })
}

// ── Genre detail ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GenreDetail {
    pub id: i64,
    pub name: String,
    pub track_count: i64,
    pub total_duration_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub tracks: Vec<crate::commands::track::Track>,
}

#[derive(FromRow)]
struct GenreHeaderRaw {
    id: i64,
    name: String,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
}

#[derive(FromRow)]
struct GenreTrackRaw {
    id: i64,
    library_id: i64,
    title: String,
    album_id: Option<i64>,
    album_title: Option<String>,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    duration_ms: i64,
    track_number: Option<i64>,
    disc_number: Option<i64>,
    year: Option<i64>,
    bitrate: Option<i64>,
    sample_rate: Option<i64>,
    channels: Option<i64>,
    bit_depth: Option<i64>,
    codec: Option<String>,
    musical_key: Option<String>,
    file_path: String,
    file_size: i64,
    added_at: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    rating: Option<i64>,
}

/// Return full genre detail: header (name, totals) and every track tagged
/// with this genre across the active profile, ordered by artist → album →
/// disc → track number to match `list_tracks`'s default layout.
#[tauri::command]
pub async fn get_genre_detail(
    state: tauri::State<'_, AppState>,
    genre_id: i64,
) -> AppResult<GenreDetail> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    let header = sqlx::query_as::<_, GenreHeaderRaw>(
        r#"
        SELECT g.id, g.name,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM genre g
          LEFT JOIN artwork aw ON aw.id = g.artwork_id
         WHERE g.id = ?
        "#,
    )
    .bind(genre_id)
    .fetch_optional(&*pool)
    .await?
    .ok_or_else(|| crate::error::AppError::Other("genre not found".into()))?;

    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        header.artwork_hash.as_deref(),
        header.artwork_format.as_deref(),
    ) {
        (Some(hash), Some(format)) => {
            let full = artwork_dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
            (Some(full), p1, p2)
        }
        _ => (None, None, None),
    };

    let rows = sqlx::query_as::<_, GenreTrackRaw>(
        r#"
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
          FROM track t
          JOIN track_genre tg ON tg.track_id = t.id
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE tg.genre_id = ? AND t.is_available = 1
         ORDER BY ar.canonical_name COLLATE NOCASE,
                  al.canonical_title COLLATE NOCASE,
                  t.disc_number,
                  t.track_number,
                  t.title COLLATE NOCASE
        "#,
    )
    .bind(genre_id)
    .fetch_all(&*pool)
    .await?;

    let tracks: Vec<crate::commands::track::Track> = rows
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) =
                match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
                    (Some(hash), Some(format)) => {
                        let full = artwork_dir
                            .join(format!("{}.{}", hash, format))
                            .to_string_lossy()
                            .to_string();
                        let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&artwork_dir, hash);
                        (Some(full), p1, p2)
                    }
                    _ => (None, None, None),
                };
            crate::commands::track::Track {
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
                channels: row.channels,
                bit_depth: row.bit_depth,
                codec: row.codec,
                musical_key: row.musical_key,
                file_path: row.file_path,
                file_size: row.file_size,
                added_at: row.added_at,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                rating: row.rating,
            }
        })
        .collect();

    let track_count = tracks.len() as i64;
    let total_duration_ms = tracks.iter().map(|t| t.duration_ms).sum();

    Ok(GenreDetail {
        id: header.id,
        name: header.name,
        track_count,
        total_duration_ms,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
        tracks,
    })
}

// ─── Play history (Last.fm-style chronological scrubber) ─────────
//
// Distinct from `list_recent_plays`, which deduplicates per track.
// The history view wants every individual play_event as its own row
// so the user can actually see "I played X three times this evening".

/// One row per `play_event` (no per-track dedup), reverse-chronological.
#[derive(Debug, Clone, Serialize)]
pub struct PlayHistoryRow {
    pub event_id: i64,
    pub played_at: i64,
    pub listened_ms: i64,
    pub completed: bool,
    pub track_id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    pub file_path: String,
}

#[derive(FromRow)]
struct PlayHistoryRaw {
    event_id: i64,
    played_at: i64,
    listened_ms: i64,
    completed: i64,
    track_id: i64,
    title: String,
    artist_id: Option<i64>,
    artist_name: Option<String>,
    artist_ids: Option<String>,
    album_id: Option<i64>,
    album_title: Option<String>,
    duration_ms: i64,
    artwork_hash: Option<String>,
    artwork_format: Option<String>,
    file_path: String,
}

/// Returns one row per play_event in reverse-chronological order.
/// `before_ms` is an exclusive upper bound on `played_at` — pass the
/// `played_at` of the last row from the previous page to paginate
/// without windowing artefacts when new plays land mid-scroll.
/// `after_ms` is an inclusive lower bound for date-range filtering
/// (e.g. "show me only plays since 2026-01-01"). Both are optional.
#[tauri::command]
pub async fn list_play_history(
    state: tauri::State<'_, AppState>,
    before_ms: Option<i64>,
    after_ms: Option<i64>,
    limit: i64,
) -> AppResult<Vec<PlayHistoryRow>> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let artwork_dir = profile_id.map(|pid| state.paths.profile_artwork_dir(pid));

    let raw = sqlx::query_as::<_, PlayHistoryRaw>(
        r#"
        SELECT pe.id                        AS event_id,
               pe.played_at                 AS played_at,
               pe.listened_ms               AS listened_ms,
               pe.completed                 AS completed,
               t.id                         AS track_id,
               t.title                      AS title,
               t.primary_artist             AS artist_id,
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
               t.album_id                   AS album_id,
               al.title                     AS album_title,
               t.duration_ms                AS duration_ms,
               aw.hash                      AS artwork_hash,
               aw.format                    AS artwork_format,
               t.file_path                  AS file_path
          FROM play_event pe
          JOIN track t        ON t.id = pe.track_id
          LEFT JOIN album al  ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE t.is_available = 1
           AND (?1 IS NULL OR pe.played_at < ?1)
           AND (?2 IS NULL OR pe.played_at >= ?2)
         ORDER BY pe.played_at DESC, pe.id DESC
         LIMIT ?3
        "#,
    )
    .bind(before_ms)
    .bind(after_ms)
    .bind(limit)
    .fetch_all(&*pool)
    .await?;

    let rows = raw
        .into_iter()
        .map(|row| {
            let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
                row.artwork_hash.as_deref(),
                row.artwork_format.as_deref(),
                artwork_dir.as_ref(),
            ) {
                (Some(hash), Some(format), Some(dir)) => {
                    let full = dir
                        .join(format!("{hash}.{format}"))
                        .to_string_lossy()
                        .to_string();
                    let (p1, p2) = crate::thumbnails::thumbnail_paths_for(dir, hash);
                    (Some(full), p1, p2)
                }
                _ => (None, None, None),
            };
            PlayHistoryRow {
                event_id: row.event_id,
                played_at: row.played_at,
                listened_ms: row.listened_ms,
                completed: row.completed != 0,
                track_id: row.track_id,
                title: row.title,
                artist_id: row.artist_id,
                artist_name: row.artist_name,
                artist_ids: row.artist_ids,
                album_id: row.album_id,
                album_title: row.album_title,
                duration_ms: row.duration_ms,
                artwork_path,
                artwork_path_1x,
                artwork_path_2x,
                file_path: row.file_path,
            }
        })
        .collect();

    Ok(rows)
}

/// One bucket per (year, month) for the play-history scrubber. Returns
/// the aggregated play count so the UI can render a sparkline-style
/// indicator next to each month label. Sorted oldest → newest because
/// the scrubber renders top-to-bottom and the user expects the latest
/// month at the bottom (next to where the page anchors on first load).
#[derive(Debug, Clone, Serialize)]
pub struct PlayHistoryMonth {
    pub year: i32,
    pub month: u32,
    /// Unix epoch ms at the first instant of this month (UTC).
    pub start_ms: i64,
    pub plays: i64,
}

#[derive(FromRow)]
struct PlayHistoryMonthRaw {
    bucket: String,
    plays: i64,
}

#[tauri::command]
pub async fn play_history_months(
    state: tauri::State<'_, AppState>,
) -> AppResult<Vec<PlayHistoryMonth>> {
    let pool = state.require_profile_pool().await?;
    let raw = sqlx::query_as::<_, PlayHistoryMonthRaw>(
        r#"
        SELECT strftime('%Y-%m', played_at / 1000, 'unixepoch', 'localtime') AS bucket,
               COUNT(*)                                                       AS plays
          FROM play_event
         GROUP BY bucket
         ORDER BY bucket ASC
        "#,
    )
    .fetch_all(&*pool)
    .await?;

    let mut out = Vec::with_capacity(raw.len());
    for r in raw {
        // bucket = "YYYY-MM" — split & convert to (year, month) plus
        // the first-of-month epoch ms. The SQL `strftime(..., 'localtime')`
        // above bucketed by **local** time, so the reconstructed midnight
        // must also be interpreted as local time. Using `and_utc()` here
        // would push the start_ms off by the local UTC offset (visible at
        // a glance as the scrubber showing the wrong month label for
        // events that happened near midnight on the boundary days).
        use chrono::{LocalResult, NaiveDate, TimeZone};
        let mut parts = r.bucket.splitn(2, '-');
        let year: i32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1970);
        let month: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let start_ms = NaiveDate::from_ymd_opt(year, month, 1)
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .and_then(|naive| match chrono::Local.from_local_datetime(&naive) {
                LocalResult::Single(dt) => Some(dt.timestamp_millis()),
                // The first of a month falls inside a DST spring-forward
                // gap (`None`) or fall-back ambiguity (`Ambiguous`) only
                // in vanishingly rare jurisdictions. Pick the earlier
                // interpretation so the scrubber stays monotonic.
                LocalResult::Ambiguous(early, _late) => Some(early.timestamp_millis()),
                LocalResult::None => None,
            })
            .unwrap_or(0);
        out.push(PlayHistoryMonth {
            year,
            month,
            start_ms,
            plays: r.plays,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use sqlx::{Row, SqlitePool};
    use std::str::FromStr;

    /// The real migrator against a real database, `foreign_keys` on — the only
    /// fixture that proves a compound select over fourteen tables actually
    /// runs. The unified listings join the attached `app` database too, so the
    /// fixture attaches one and creates the single table they read.
    async fn pool() -> SqlitePool {
        let options = SqliteConnectOptions::from_str(":memory:")
            .unwrap()
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            // One connection: `ATTACH` is per-connection, and a second one
            // would not see the attached database.
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("ATTACH DATABASE ':memory:' AS app")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE app.metadata_artist (
                deezer_id    INTEGER PRIMARY KEY,
                picture_url  TEXT,
                picture_hash TEXT
             )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// One local artist with one album and one track, and one server artist
    /// with one album and two tracks. The two artists share a name that only
    /// the normaliser makes comparable.
    async fn seed(pool: &SqlitePool) {
        for statement in [
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
            "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'Björk', 'bjork')",
            "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (1, 'Vespertine', 'vespertine', 1, 2001, 0)",
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, 'T1', 1, 300000, 500, 1, 0, 0, 0, 0)",
            // The scanner always stamps a primary artist; a fixture that omits
            // it would exercise a shape the library never holds.
            "UPDATE track SET primary_artist = 1 WHERE id = 1",
            "INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 1, 0)",
            "INSERT INTO remote_artist (remote_id, name, artwork_hash, sort_key, mirrored_at)
             VALUES ('ar-1', 'Aphex Twin', 'aa11', 'aphex twin', 1)",
            "INSERT INTO remote_album (remote_id, title, artist, artist_id, song_count,
                                       duration_ms, created_at, sort_title, sort_artist)
             VALUES ('al-1', 'Drukqs', 'Aphex Twin', 'ar-1', 2, 1000, 100, 'drukqs',
                     'aphex twin')",
            "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-1', 'R1', 'ar-1', 0, 1, 1)",
            "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-2', 'R2', 'ar-1', 0, 1, 1)",
            // Fully described, so the track listing has something to sort and
            // render: the two above are bare identifiers on purpose.
            "UPDATE remote_track
                SET artist = 'Aphex Twin', album = 'Drukqs', album_id = 'al-1',
                    sort_artist = 'aphex twin', sort_album = 'drukqs',
                    track_no = 1, disc_no = 1, artwork_hash = 'bb22'
              WHERE remote_id IN ('t-1', 't-2')",
            // Cached for a playlist but never walked: outside the catalogue,
            // so the library must not list it.
            "INSERT INTO remote_track (remote_id, title, artist, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-3', 'Not in the catalogue', 'Someone', 0, 1, 0)",
        ] {
            sqlx::raw_sql(statement).execute(pool).await.unwrap();
        }
    }

    async fn albums(
        pool: &SqlitePool,
        library_id: Option<i64>,
        source: Option<&str>,
        order: &str,
    ) -> Vec<(String, String)> {
        sqlx::query(sqlx::AssertSqlSafe(library_albums_sql(order)))
            .bind(library_id)
            .bind(library_id)
            .bind(library_id)
            .bind(source)
            .bind(source)
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("source"), row.get("title")))
            .collect()
    }

    async fn artists(
        pool: &SqlitePool,
        library_id: Option<i64>,
        source: Option<&str>,
        order: &str,
    ) -> Vec<(String, String, i64, i64)> {
        sqlx::query(sqlx::AssertSqlSafe(library_artists_sql(order)))
            .bind(library_id)
            .bind(library_id)
            .bind(library_id)
            .bind(source)
            .bind(source)
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                (
                    row.get("source"),
                    row.get("name"),
                    row.get("track_count"),
                    row.get("album_count"),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn the_album_listing_sorts_both_halves_against_each_other() {
        let pool = pool().await;
        seed(&pool).await;

        let by_artist = albums(&pool, None, None, library_album_order_clause(None, None)).await;
        // "aphex twin" before "bjork": the remote half's normalised key sorts
        // against the local half's canonical one, which is the whole point.
        assert_eq!(
            by_artist,
            vec![
                ("remote".into(), "Drukqs".into()),
                ("local".into(), "Vespertine".into()),
            ]
        );

        let by_title_desc = albums(
            &pool,
            None,
            None,
            library_album_order_clause(Some("title"), Some("desc")),
        )
        .await;
        assert_eq!(
            by_title_desc.first().map(|row| row.1.as_str()),
            Some("Vespertine")
        );
    }

    #[tokio::test]
    async fn the_source_filter_returns_one_half_only() {
        let pool = pool().await;
        seed(&pool).await;
        let order = library_album_order_clause(None, None);

        assert_eq!(
            albums(&pool, None, Some("local"), order).await,
            vec![("local".to_string(), "Vespertine".to_string())]
        );
        assert_eq!(
            albums(&pool, None, Some("remote"), order).await,
            vec![("remote".to_string(), "Drukqs".to_string())]
        );
    }

    /// The picker chooses among *local* libraries, and a server album belongs
    /// to none of them.
    #[tokio::test]
    async fn a_local_library_filter_excludes_the_remote_half() {
        let pool = pool().await;
        seed(&pool).await;
        let order = library_album_order_clause(None, None);

        assert_eq!(
            albums(&pool, Some(1), None, order).await,
            vec![("local".to_string(), "Vespertine".to_string())]
        );
        // A library that holds nothing leaves nothing, rather than falling
        // back to the remote half.
        assert!(albums(&pool, Some(99), None, order).await.is_empty());
    }

    async fn tracks(
        pool: &SqlitePool,
        library_id: Option<i64>,
        source: Option<&str>,
        order: &str,
    ) -> Vec<(String, String, Option<i64>)> {
        sqlx::query(sqlx::AssertSqlSafe(library_tracks_sql(order)))
            .bind(library_id)
            .bind(library_id)
            .bind(library_id)
            .bind(source)
            .bind(source)
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.get("source"), row.get("title"), row.get("rating")))
            .collect()
    }

    #[tokio::test]
    async fn the_track_listing_sorts_both_halves_against_each_other() {
        let pool = pool().await;
        seed(&pool).await;

        let rows = tracks(&pool, None, None, library_track_order_clause(None, None)).await;
        // "aphex twin" before "bjork", from the normalised keys on both sides.
        assert_eq!(
            rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
            vec!["R1", "R2", "T1"]
        );
        // A server track has no rating, and that is not the same as unrated.
        assert_eq!(rows[0].2, None);
    }

    /// A track cached for a playlist is not part of the catalogue, and the
    /// library must not list it — it would appear with no album and no way to
    /// reach it.
    #[tokio::test]
    async fn the_track_listing_shows_only_the_mirrored_catalogue() {
        let pool = pool().await;
        seed(&pool).await;

        let titles: Vec<String> = tracks(&pool, None, None, library_track_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert!(!titles.iter().any(|title| title == "Not in the catalogue"));
    }

    /// A confirmed link proves the two rows are the same bytes, so the list
    /// must show one of them — the local one, which is the playable half.
    #[tokio::test]
    async fn a_linked_server_track_stops_being_a_second_row() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql(
            "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (1, 't-1', 'exact_full_hash',
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     'confirmed', 'local_first', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let titles: Vec<String> = tracks(&pool, None, None, library_track_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert_eq!(titles, vec!["R2".to_string(), "T1".to_string()]);
    }

    /// Two narrowings on that predicate, and dropping either one loses a
    /// track rather than a duplicate.
    #[tokio::test]
    async fn a_link_hides_nothing_it_cannot_prove() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql(
            "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (1, 't-1', 'exact_full_hash',
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     'stale', 'local_first', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // A stale link is a guess. Hiding on a guess loses the track.
        let stale: Vec<String> = tracks(&pool, None, None, library_track_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert!(stale.iter().any(|title| title == "R1"), "{stale:?}");

        // Confirmed, but the local file has gone: the local half already
        // filtered itself out, so hiding the remote half too would remove the
        // recording from the library altogether — and the server can play it.
        sqlx::raw_sql("UPDATE remote_track_link SET status = 'confirmed'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("UPDATE track SET is_available = 0 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();
        let gone: Vec<String> = tracks(&pool, None, None, library_track_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert_eq!(gone, vec!["R1".to_string(), "R2".to_string()]);
    }

    /// Mark a local track as examined — RFC-006's completeness frontier. Every
    /// pairing test needs this, because an unexamined track can never pair.
    async fn examine(pool: &SqlitePool, track_id: i64) {
        sqlx::query(
            "INSERT INTO local_full_hash (track_id, full_hash, file_size, file_modified, computed_at)
             SELECT t.id, ?, t.file_size, t.file_modified, 0 FROM track t WHERE t.id = ?",
        )
        .bind("a".repeat(64))
        .bind(track_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn link(pool: &SqlitePool, local: i64, remote: &str) {
        sqlx::query(
            "INSERT INTO remote_track_link
                 (local_track_id, remote_track_id, method, verified_full_hash,
                  status, playback_preference, confirmed_at, verified_at)
             VALUES (?, ?, 'exact_full_hash', ?, 'confirmed', 'local_first', 0, 0)",
        )
        .bind(local)
        .bind(remote)
        .bind("0".repeat(64))
        .execute(pool)
        .await
        .unwrap();
    }

    /// A local album and a server album whose track sets are in complete
    /// bijection are one release, and the listing shows it once.
    #[tokio::test]
    async fn an_album_pairs_only_on_a_complete_bijection() {
        let pool = pool().await;
        seed(&pool).await;
        // The fixture's local album holds one track; give the server album one
        // too, so a bijection is possible at all.
        sqlx::raw_sql("DELETE FROM remote_track WHERE remote_id = 't-2'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("UPDATE remote_album SET song_count = 1, mirrored_at = 1")
            .execute(&pool)
            .await
            .unwrap();

        // Linked but not yet examined: still two entries, because "no link" on
        // an unexamined track cannot be read as "different bytes".
        link(&pool, 1, "t-1").await;
        let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert_eq!(titles, vec!["Drukqs".to_string(), "Vespertine".to_string()]);

        examine(&pool, 1).await;
        let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert_eq!(
            titles,
            vec!["Vespertine".to_string()],
            "one release, one entry"
        );
    }

    /// The case that killed the RFC's first draft: two releases sharing
    /// recordings are not the same release, however unanimous the links.
    #[tokio::test]
    async fn sharing_recordings_is_not_being_the_same_album() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql("UPDATE remote_album SET mirrored_at = 1")
            .execute(&pool)
            .await
            .unwrap();
        // The server album keeps two tracks; the local one has a single track
        // linked to one of them. A "unanimity plus links" rule would pair them.
        link(&pool, 1, "t-1").await;
        examine(&pool, 1).await;

        let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert_eq!(
            titles,
            vec!["Drukqs".to_string(), "Vespertine".to_string()],
            "an unmatched server track must keep the albums apart"
        );
    }

    /// A bijection over two empty sets holds vacuously. It must not pair.
    #[tokio::test]
    async fn an_album_with_no_tracks_never_pairs() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql("DELETE FROM remote_track")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("UPDATE remote_album SET song_count = 0, mirrored_at = 1")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql("DELETE FROM track")
            .execute(&pool)
            .await
            .unwrap();

        let sources: Vec<String> =
            albums(&pool, None, None, library_album_order_clause(None, None))
                .await
                .into_iter()
                .map(|row| row.0)
                .collect();
        assert_eq!(
            sources,
            vec!["remote".to_string()],
            "the empty pair must not collapse"
        );
    }

    /// A server album still being walked is a prefix, not a set, so nothing
    /// pairs against it however complete the links look.
    #[tokio::test]
    async fn an_unwalked_album_never_pairs() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql("DELETE FROM remote_track WHERE remote_id = 't-2'")
            .execute(&pool)
            .await
            .unwrap();
        link(&pool, 1, "t-1").await;
        examine(&pool, 1).await;
        // `mirrored_at` left NULL: the walk has not finished.
        let titles: Vec<String> = albums(&pool, None, None, library_album_order_clause(None, None))
            .await
            .into_iter()
            .map(|row| row.1)
            .collect();
        assert_eq!(titles.len(), 2, "{titles:?}");
    }

    /// Artists pair on a sample, not on a set — but the sample has to be more
    /// than one link, and unanimous.
    #[tokio::test]
    async fn an_artist_pairs_on_two_unanimous_links() {
        let pool = pool().await;
        seed(&pool).await;
        // A second local track by the same artist, so two links are possible.
        sqlx::raw_sql(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 2, 0, 'T2', 1, 300000, 500, 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        examine(&pool, 1).await;
        examine(&pool, 2).await;

        link(&pool, 1, "t-1").await;
        let names: Vec<String> =
            artists(&pool, None, None, library_artist_order_clause(None, None))
                .await
                .into_iter()
                .map(|row| row.1)
                .collect();
        assert_eq!(names.len(), 2, "one link is a coincidence, not evidence");

        link(&pool, 2, "t-2").await;
        let names: Vec<String> =
            artists(&pool, None, None, library_artist_order_clause(None, None))
                .await
                .into_iter()
                .map(|row| row.1)
                .collect();
        assert_eq!(names, vec!["Björk".to_string()], "two unanimous links pair");
    }

    /// Unanimity is what disposes of Various Artists, with no `is_compilation`
    /// special case: a compilation's links point at several server artists, so
    /// the disagreeing one ends the question.
    #[tokio::test]
    async fn a_disagreeing_link_keeps_two_artists_apart() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (2, 1, '/m/2.flac', 'h2', 2, 0, 'T2', 1, 300000, 500, 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // A third server track credited to somebody else entirely.
        sqlx::raw_sql(
            "INSERT INTO remote_artist (remote_id, name, sort_key, mirrored_at)
             VALUES ('ar-2', 'Someone Else', 'someone else', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(
            "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at,
                                       in_catalogue)
             VALUES ('t-9', 'R9', 'ar-2', 0, 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        examine(&pool, 1).await;
        examine(&pool, 2).await;

        link(&pool, 1, "t-1").await;
        link(&pool, 2, "t-2").await;
        // Two unanimous links would pair — but this local artist also owns a
        // track linked to a different server artist.
        sqlx::raw_sql(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available,
                                primary_artist, hlc_wall, hlc_logical,
                                rating_hlc_wall, rating_hlc_logical)
             VALUES (3, 1, '/m/3.flac', 'h3', 3, 0, 'T3', 300000, 500, 1, 1, 0, 0, 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        examine(&pool, 3).await;
        link(&pool, 3, "t-9").await;

        let names: Vec<String> =
            artists(&pool, None, None, library_artist_order_clause(None, None))
                .await
                .into_iter()
                .map(|row| row.1)
                .collect();
        assert_eq!(
            names.len(),
            3,
            "a disagreeing link ends the question: {names:?}"
        );
    }

    /// Rating is local-only, so the remote half has none. Sorting by it must
    /// not read a missing rating as the worst one.
    #[tokio::test]
    async fn sorting_by_rating_puts_the_unratable_last_either_way() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql("UPDATE track SET rating = 200 WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();

        for direction in ["asc", "desc"] {
            let rows = tracks(
                &pool,
                None,
                None,
                library_track_order_clause(Some("rating"), Some(direction)),
            )
            .await;
            assert_eq!(rows[0].1, "T1", "{direction}: the rated track leads");
            assert!(rows[1..].iter().all(|row| row.2.is_none()));
        }
    }

    /// The sort dropdown and the persisted preference both carry the column
    /// name. Matching on anything else sends the sort to the fallback clause
    /// and does nothing visible — which is the quietest way for a sort to be
    /// broken.
    #[tokio::test]
    async fn sorting_by_duration_uses_the_key_the_dropdown_sends() {
        let pool = pool().await;
        seed(&pool).await;
        sqlx::raw_sql("UPDATE remote_track SET duration_ms = 999999 WHERE remote_id = 't-1'")
            .execute(&pool)
            .await
            .unwrap();

        let longest = tracks(
            &pool,
            None,
            None,
            library_track_order_clause(Some("duration_ms"), Some("desc")),
        )
        .await;
        assert_eq!(longest.first().map(|row| row.1.as_str()), Some("R1"));

        let shortest = tracks(
            &pool,
            None,
            None,
            library_track_order_clause(Some("duration_ms"), Some("asc")),
        )
        .await;
        assert_eq!(shortest.last().map(|row| row.1.as_str()), Some("R1"));
    }

    #[tokio::test]
    async fn the_track_filters_behave_like_the_album_ones() {
        let pool = pool().await;
        seed(&pool).await;
        let order = library_track_order_clause(None, None);

        let local = tracks(&pool, None, Some("local"), order).await;
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].0, "local");

        let remote = tracks(&pool, None, Some("remote"), order).await;
        assert_eq!(remote.len(), 2);
        assert!(remote.iter().all(|row| row.0 == "remote"));

        let scoped = tracks(&pool, Some(1), None, order).await;
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].0, "local");
    }

    #[tokio::test]
    async fn the_artist_listing_sorts_and_derives_its_counts() {
        let pool = pool().await;
        seed(&pool).await;

        let rows = artists(&pool, None, None, library_artist_order_clause(None, None)).await;
        assert_eq!(
            rows,
            vec![
                // Two mirrored tracks and one mirrored album, counted from the
                // rows the mirror holds rather than from a stored total.
                ("remote".to_string(), "Aphex Twin".to_string(), 2, 1),
                ("local".to_string(), "Björk".to_string(), 1, 1),
            ]
        );

        let by_tracks = artists(
            &pool,
            None,
            None,
            library_artist_order_clause(Some("tracks_count"), Some("desc")),
        )
        .await;
        assert_eq!(
            by_tracks.first().map(|row| row.1.as_str()),
            Some("Aphex Twin")
        );
    }

    #[tokio::test]
    async fn the_artist_filters_behave_like_the_album_ones() {
        let pool = pool().await;
        seed(&pool).await;
        let order = library_artist_order_clause(None, None);

        let local = artists(&pool, None, Some("local"), order).await;
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].0, "local");

        let remote = artists(&pool, None, Some("remote"), order).await;
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].0, "remote");

        // A local library filter drops the remote half here too.
        let scoped = artists(&pool, Some(1), None, order).await;
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].0, "local");
    }
}
