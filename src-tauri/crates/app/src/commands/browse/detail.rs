//! The album, artist and genre detail pages.

use serde::Serialize;
use sqlx::FromRow;

use crate::{error::AppResult, state::AppState};

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
    /// The tracks and discs the album is missing, by
    /// [`waveflow_core::album_completeness`]: empty when it is complete
    /// or cannot be judged.
    pub completeness: waveflow_core::album_completeness::AlbumCompleteness,
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
    let (pool, profile_id) = state.require_profile_snapshot().await?;
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
         ORDER BY g.name COLLATE FOLD
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
    let tracks_raw = sqlx::query_as::<_, AlbumTrackRaw>(concat!(
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
               t.year, t.bitrate, t.channels, "#,
        waveflow_core::effective_musical_key_sql!(),
        r#" AS musical_key,
               t.file_size, t.added_at, t.rating,
               aw.hash AS artwork_hash, aw.format AS artwork_format
          FROM ranked r
          JOIN track t ON t.id = r.id
          LEFT JOIN album al ON al.id = t.album_id
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE r.rn = 1
         ORDER BY t.disc_number, t.track_number
        "#,
    ))
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
    let completeness = crate::commands::inventory::album_completeness_of(&pool, album_id).await?;

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
        completeness,
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
    /// True when `background_path` is a backdrop the user chose rather
    /// than TheAudioDB's — what tells the picker to offer "remove" and
    /// the hero not to treat it as a strip (#693).
    pub has_custom_background: bool,
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
    /// A backdrop the user chose (#693) — profile artwork dir, and it
    /// wins over the cached `background_hash` above.
    custom_background_hash: Option<String>,
    custom_background_format: Option<String>,
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
    let (pool, profile_id) = state.require_profile_snapshot().await?;
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
               bg.hash   AS custom_background_hash,
               bg.format AS custom_background_format,
               COUNT(DISTINCT t.id) AS track_count,
               COUNT(DISTINCT t.album_id) AS album_count
          FROM artist ar
          LEFT JOIN artwork aw ON aw.id = ar.artwork_id
          LEFT JOIN artwork bg ON bg.id = ar.background_artwork_id
          LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
          -- Credited on the track, or album artist of its album: an
          -- artist who is only ever an album artist ("Various Artists")
          -- has a page too, instead of "artist not found".
          JOIN track t ON t.is_available = 1
                      AND (t.id IN (SELECT track_id FROM track_artist WHERE artist_id = ?1)
                           OR t.album_id IN (SELECT id FROM album WHERE artist_id = ?1))
         WHERE ar.id = ?1
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
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE al.artist_id = ?1
            OR t.id IN (SELECT track_id FROM track_artist WHERE artist_id = ?1)
         GROUP BY al.id
         ORDER BY al.year DESC, al.canonical_title COLLATE NAME
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
    // A chosen backdrop wins over the cached one, and lives in the
    // profile artwork dir rather than the shared metadata cache (#693).
    let custom_background_path = crate::metadata_artwork::existing_profile_path(
        &artwork_dir,
        header.custom_background_hash.as_deref(),
        header.custom_background_format.as_deref(),
    );
    let has_custom_background = custom_background_path.is_some();
    let background_path = custom_background_path.or_else(|| {
        header
            .background_hash
            .as_deref()
            .and_then(|h| crate::metadata_artwork::existing_path(metadata_dir, h))
    });

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
        // A chosen backdrop has no remote twin: the URL would point at
        // whatever TheAudioDB last suggested, which is exactly what the
        // user replaced.
        background_url: (!has_custom_background)
            .then_some(header.background_url)
            .flatten(),
        background_path,
        has_custom_background,
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

/// One track row of a detail page that lists tracks across albums (a
/// genre, a year), shaped by [`detail_tracks_sql`].
#[derive(FromRow)]
pub(crate) struct DetailTrackRaw {
    pub(crate) id: i64,
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
    let (pool, profile_id) = state.require_profile_snapshot().await?;
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

    let rows = sqlx::query_as::<_, DetailTrackRaw>(detail_tracks_sql(
        "JOIN track_genre tg ON tg.track_id = t.id",
        "tg.genre_id = ?",
    ))
    .bind(genre_id)
    .fetch_all(&*pool)
    .await?;

    let tracks: Vec<crate::commands::track::Track> = rows
        .into_iter()
        .map(|row| detail_track(row, &artwork_dir))
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

/// The track query of a detail page: every available track that `join`
/// and `filter` keep, ordered by artist → album → disc → track number to
/// match `list_tracks`'s default layout. `filter` binds what the caller
/// binds. Both fragments are literals of this crate, never user input,
/// which is what makes the assertion of safety true.
pub(crate) fn detail_tracks_sql(join: &str, filter: &str) -> sqlx::AssertSqlSafe<String> {
    let musical_key = waveflow_core::effective_musical_key_sql!();
    sqlx::AssertSqlSafe(format!(
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
               t.bit_depth, t.codec, {musical_key} AS musical_key,
               t.file_path, t.file_size, t.added_at,
               aw.hash   AS artwork_hash,
               aw.format AS artwork_format,
               t.rating  AS rating
          FROM track t
          {join}
          LEFT JOIN album   al ON al.id = t.album_id
          LEFT JOIN artist  ar ON ar.id = t.primary_artist
          LEFT JOIN artwork aw ON aw.id = al.artwork_id
         WHERE {filter} AND t.is_available = 1
         ORDER BY ar.canonical_name COLLATE NAME,
                  al.canonical_title COLLATE NAME,
                  t.disc_number,
                  t.track_number,
                  t.title COLLATE FOLD
        "#
    ))
}

/// A [`DetailTrackRaw`] as the `Track` the frontend plays, its cover
/// resolved under `artwork_dir`.
pub(crate) fn detail_track(
    row: DetailTrackRaw,
    artwork_dir: &std::path::Path,
) -> crate::commands::track::Track {
    let (artwork_path, artwork_path_1x, artwork_path_2x) =
        match (row.artwork_hash.as_deref(), row.artwork_format.as_deref()) {
            (Some(hash), Some(format)) => {
                let full = artwork_dir
                    .join(format!("{}.{}", hash, format))
                    .to_string_lossy()
                    .to_string();
                let (p1, p2) = crate::thumbnails::thumbnail_paths_for(artwork_dir, hash);
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
}
