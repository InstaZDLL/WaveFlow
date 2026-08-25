//! Mirroring the server's catalogue into the projection (RFC-005).
//!
//! [`projection`](super::projection) writes what the server's *user data*
//! references: the snapshot carries full song objects for playlists and the
//! queue, `sync/changes` carries bare identifiers, and a track the account
//! never touched is never mentioned by either. That is why the remote source
//! can only show playlists — nothing local knows the catalogue exists.
//!
//! Browsing both sources from one library needs that catalogue **in SQL**.
//! Merging a local table with a paginated HTTP endpoint cannot be sorted,
//! filtered or virtualised as a single list: the sort order of page 3 depends
//! on rows the server has not sent yet. So the catalogue is walked once and
//! mirrored, and every listing afterwards is a query over local tables.
//!
//! ## Why the walk goes album by album
//!
//! `GET /api/v2/libraries/{id}/tracks` enumerates everything, but answers with
//! `TrackRecord` — no `album_id`, no track or disc number, no year. Grouping
//! by album name and ordering a disc would both be guesswork. `GET
//! /api/v2/albums/{id}` answers with full `SongItem`s, the same shape the
//! snapshot uses, so walking albums reuses [`projection::cache_song`] verbatim
//! and yields rows indistinguishable from projected ones.
//!
//! The sweep still runs, for two things the album walk cannot see: a track
//! that belongs to no album, and a track the server has since deleted.
//!
//! ## What makes it incremental
//!
//! `AlbumItem::song_count` is the server's own count of the album's available
//! tracks. An album whose mirrored count already matches is skipped without
//! being fetched, so a second walk over an unchanged library costs one request
//! per 200 albums instead of one per album.
//!
//! ## A side effect worth naming
//!
//! `SongItem` carries `full_hash`, and [`projection::cache_song`] already
//! stores it. Mirroring the catalogue therefore lands the server's content
//! fingerprint for every track it has — which is exactly the input
//! [`reconciliation`](super::reconciliation) needs, obtained without asking
//! for it.

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU8, Ordering},
};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::{
    error::{AppError, AppResult},
    remote::{client::RemoteClient, dto::SongItem},
    state::AppState,
};

/// Albums per page of the listing. Well under the server's ceiling of 500:
/// an `AlbumItem` carries its credited artists, genres and disc titles, so a
/// full page is a large response for a listing we only read six fields from.
const ALBUM_PAGE: i64 = 200;
/// Tracks per page of the sweep. These rows are lean, so the ceiling is fine.
const TRACK_PAGE: i64 = 500;

const PHASE_IDLE: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_CANCELLED: u8 = 2;

static MIRROR_PHASE: AtomicU8 = AtomicU8::new(PHASE_IDLE);

/// Resets [`MIRROR_PHASE`] on every exit path — early return, `?`, panic — so
/// a failed walk cannot wedge the feature for the rest of the session.
struct PhaseGuard;

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        MIRROR_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
    }
}

/// Ask an in-flight walk to stop. Unlike the reconciliation scan, stopping
/// here is always safe and always honoured: every album committed so far is a
/// complete album, and the next walk resumes from what is missing. Idempotent.
pub fn request_cancel() -> bool {
    match MIRROR_PHASE.compare_exchange(
        PHASE_RUNNING,
        PHASE_CANCELLED,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => true,
        // Already cancelled (double click) — idempotently still "cancelled".
        Err(PHASE_CANCELLED) => true,
        // Nothing is running.
        Err(_) => false,
    }
}

fn cancelled() -> bool {
    MIRROR_PHASE.load(Ordering::SeqCst) == PHASE_CANCELLED
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Every projection table that can point at a track. A mirrored row is only
/// deletable once none of them does — otherwise dropping the mirror would
/// leave a playlist unable to render its own titles.
///
/// A macro rather than a `const` so the two statements below can be built
/// with `concat!` and stay `&'static str`: sqlx refuses a runtime-formatted
/// query outright, and the alternative — spelling the predicate out twice —
/// is how a table gets added to one copy and forgotten in the other.
macro_rules! unreferenced {
    () => {
        "remote_id NOT IN (SELECT track_remote_id FROM remote_playlist_track)
         AND remote_id NOT IN (SELECT track_remote_id FROM remote_queue_track)
         AND remote_id NOT IN (SELECT track_remote_id FROM remote_history)
         AND remote_id NOT IN (SELECT track_remote_id FROM remote_share_track)
         AND remote_id NOT IN (SELECT entity_id FROM remote_favorite WHERE entity_type = 'track')
         AND remote_id NOT IN (SELECT entity_id FROM remote_rating   WHERE entity_type = 'track')"
    };
}

/// Delete one vanished mirrored row, if nothing else points at it.
const DELETE_VANISHED: &str = concat!(
    "DELETE FROM remote_track WHERE remote_id = ? AND ",
    unreferenced!()
);

/// Delete the whole mirror, keeping every row something else points at.
const DELETE_MIRROR: &str = concat!(
    "DELETE FROM remote_track WHERE in_catalogue = 1 AND ",
    unreferenced!()
);

/// What one walk did. Every count is of work actually performed, so a report
/// of all zeros on a populated server means "nothing had changed", not
/// "nothing was found".
#[derive(Debug, Clone, Default, Serialize)]
pub struct MirrorReport {
    /// Albums the server listed.
    pub albums_seen: i64,
    /// Albums fetched, i.e. those whose track count had changed.
    pub albums_walked: i64,
    /// Tracks written, counting an album's tracks once per walk.
    pub tracks_mirrored: i64,
    /// Tracks that belong to no album and were fetched one by one.
    pub orphans_mirrored: i64,
    /// Mirrored rows dropped because the server no longer lists them.
    pub removed: i64,
    /// Libraries swept.
    pub libraries: i64,
    /// Stopped early on request. The mirror is consistent either way.
    pub cancelled: bool,
    /// Another walk already owned the slot; nothing here ran.
    pub already_running: bool,
}

/// Progress for the UI. `total` is 0 while a phase is still counting.
#[derive(Debug, Clone, Serialize)]
struct MirrorProgress {
    phase: &'static str,
    done: i64,
    total: i64,
}

fn emit(app: &AppHandle, phase: &'static str, done: i64, total: i64) {
    // Progress is decoration: a listener that has gone away must not turn a
    // successful walk into an error.
    let _ = app.emit("remote:mirror-progress", MirrorProgress { phase, done, total });
}

/// One library the account can see.
#[derive(Debug, Clone, Deserialize)]
struct LibraryAccessDto {
    id: String,
    name: String,
}

/// The six fields the mirror keeps out of a listed album. Everything else an
/// `AlbumItem` carries — credited artists, genres, release types, play counts
/// — is either derivable from the tracks or is user data the projection
/// already owns.
#[derive(Debug, Clone, Deserialize)]
struct AlbumListItem {
    id: String,
    #[serde(default)]
    library_id: Option<String>,
    title: String,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    artist_id: Option<String>,
    #[serde(default)]
    artwork_hash: Option<String>,
    #[serde(default)]
    year: Option<i64>,
    #[serde(default)]
    is_compilation: bool,
    #[serde(default)]
    sort_name: Option<String>,
    #[serde(default)]
    song_count: i64,
    #[serde(default)]
    duration_ms: i64,
    #[serde(default)]
    created_at: Option<i64>,
}

/// An album with its songs, as `GET /api/v2/albums/{id}` answers.
#[derive(Debug, Clone, Deserialize)]
struct AlbumDetailDto {
    #[serde(default)]
    songs: Vec<SongItem>,
}

/// The lean row the library sweep answers with. Only the identifier and the
/// availability flag matter: the rich metadata came from the album walk.
#[derive(Debug, Clone, Deserialize)]
struct TrackRecordDto {
    id: String,
    #[serde(default = "available_by_default")]
    available: bool,
}

/// A server that omits the flag is one that has no notion of unavailability,
/// and every track it lists is playable. Defaulting to `false` would empty the
/// mirror against such a server.
fn available_by_default() -> bool {
    true
}

/// Walk the server's catalogue into the projection, emitting
/// `remote:mirror-progress` and honouring [`request_cancel`].
pub async fn mirror_catalogue(state: &AppState, app: AppHandle) -> AppResult<MirrorReport> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    // Claim the slot atomically: a second walk would interleave its progress
    // and its cancel state with the first one's.
    if MIRROR_PHASE
        .compare_exchange(
            PHASE_IDLE,
            PHASE_RUNNING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Ok(MirrorReport {
            already_running: true,
            ..MirrorReport::default()
        });
    }
    let _guard = PhaseGuard;

    // Pin the session and the pool to one profile for the whole walk: a
    // profile switch mid-walk must not read one profile's credentials and
    // write into another's projection.
    let profile_id = state.require_profile_id().await?;
    let client = RemoteClient::try_build_for(state, profile_id)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;
    let pool = state.require_profile_pool_for(Some(profile_id)).await?;

    let mut report = MirrorReport::default();

    let libraries = fetch_libraries(&client).await?;
    report.libraries = libraries.len() as i64;
    store_libraries(&pool, &libraries).await?;

    walk_albums(&client, &pool, &app, &mut report).await?;
    if cancelled() {
        report.cancelled = true;
        return Ok(report);
    }

    // The sweep is what decides a track has vanished, and it decides it by
    // absence. With no library to sweep there is no absence to read — an empty
    // list would purge the catalogue that was just walked in. A server that
    // answers with none is one this account cannot browse, not one whose
    // catalogue is empty.
    if !libraries.is_empty() {
        sweep_libraries(&client, &pool, &app, &libraries, &mut report).await?;
    }
    report.cancelled = cancelled();
    Ok(report)
}

async fn fetch_libraries(client: &RemoteClient<'_>) -> AppResult<Vec<LibraryAccessDto>> {
    client
        .send_json::<Vec<LibraryAccessDto>>(client.get("/api/v2/libraries"))
        .await
        // Keep the whole failure: a 401 here means the session lapsed, which
        // the caller must tell apart from "this server has no libraries".
        .map_err(|err| AppError::Other(format!("library list failed: {err}")))
}

async fn store_libraries(pool: &SqlitePool, libraries: &[LibraryAccessDto]) -> AppResult<()> {
    // Same reading as the sweep guard in `mirror_catalogue`: an empty answer
    // means this account cannot browse anything, not that the server holds
    // nothing. Deleting on it would throw away every sweep date and leave a
    // mirror that can no longer see its own libraries.
    if libraries.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for library in libraries {
        // `mirrored_at` is deliberately absent from the update: a rename must
        // not read as a library that was never swept.
        sqlx::query(
            "INSERT INTO remote_library (remote_id, name) VALUES (?, ?)
             ON CONFLICT(remote_id) DO UPDATE SET name = excluded.name",
        )
        .bind(&library.id)
        .bind(&library.name)
        .execute(&mut *tx)
        .await?;
    }
    // A library the account lost access to would otherwise keep its row and its
    // sweep date forever, making a mirror that can no longer see it look
    // complete. Its tracks are dropped by the sweep, which stops listing them.
    let keep: HashSet<&str> = libraries.iter().map(|l| l.id.as_str()).collect();
    let known: Vec<String> = sqlx::query_scalar("SELECT remote_id FROM remote_library")
        .fetch_all(&mut *tx)
        .await?;
    for id in known.iter().filter(|id| !keep.contains(id.as_str())) {
        sqlx::query("DELETE FROM remote_library WHERE remote_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// What is already mirrored for one album, as far as freshness is concerned.
#[derive(Debug, Clone, Copy)]
struct MirroredAlbum {
    song_count: i64,
    /// `false` while `mirrored_at` is NULL — listed, but its tracks never
    /// fetched. That is the state an interrupted walk leaves behind.
    walked: bool,
}

/// `true` when the album needs no fetch: already walked, and the server still
/// reports the same number of available tracks.
fn is_fresh(known: Option<&MirroredAlbum>, listed: &AlbumListItem) -> bool {
    matches!(known, Some(state) if state.walked && state.song_count == listed.song_count)
}

/// Everything the mirror knows about albums, read once for the whole walk.
///
/// Deciding freshness per album would cost a query per album, which on a
/// second walk over an unchanged library *is* the whole cost — every other
/// step is skipped. One read up front makes the repeat walk a listing and
/// nothing else.
async fn known_albums(pool: &SqlitePool) -> AppResult<HashMap<String, MirroredAlbum>> {
    let rows: Vec<(String, i64, Option<i64>)> =
        sqlx::query_as("SELECT remote_id, song_count, mirrored_at FROM remote_album")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(id, song_count, mirrored_at)| {
            (
                id,
                MirroredAlbum {
                    song_count,
                    walked: mirrored_at.is_some(),
                },
            )
        })
        .collect())
}

/// Freshness straight from the database. Production reads [`known_albums`]
/// once and answers from memory; this exists so a test asserts against the
/// stored rows rather than against the cache built from them.
#[cfg(test)]
async fn album_is_fresh(pool: &SqlitePool, album: &AlbumListItem) -> AppResult<bool> {
    Ok(is_fresh(known_albums(pool).await?.get(&album.id), album))
}

/// Page the album listing, fetching only the albums whose track count no
/// longer matches what is mirrored.
async fn walk_albums(
    client: &RemoteClient<'_>,
    pool: &SqlitePool,
    app: &AppHandle,
    report: &mut MirrorReport,
) -> AppResult<()> {
    let known = known_albums(pool).await?;
    let mut offset = 0i64;
    let mut processed = 0i64;
    loop {
        if cancelled() {
            return Ok(());
        }
        let page: Vec<AlbumListItem> = client
            .send_json(client.request(reqwest::Method::GET, "/api/v2/albums").query(&[
                ("offset", offset.to_string()),
                ("limit", ALBUM_PAGE.to_string()),
            ]))
            .await
            .map_err(|err| AppError::Other(format!("album listing failed: {err}")))?;
        if page.is_empty() {
            return Ok(());
        }
        report.albums_seen += page.len() as i64;

        // Freshness is read from `known`, which predates the upsert below —
        // the upsert overwrites `song_count` with the server's new value, and
        // asking afterwards would compare the answer with itself.
        let stale: Vec<&AlbumListItem> = page
            .iter()
            .filter(|album| !is_fresh(known.get(&album.id), album))
            .collect();

        // One transaction for the page rather than one per album: on a repeat
        // walk these upserts are the only writes, and paying an fsync each
        // would make the cheap path the slow one.
        let mut tx = pool.begin().await?;
        for album in &page {
            upsert_album(&mut tx, album).await?;
        }
        tx.commit().await?;

        for album in stale {
            if cancelled() {
                return Ok(());
            }
            let walked = walk_one_album(client, pool, &album.id).await?;
            report.albums_walked += 1;
            report.tracks_mirrored += walked;
        }
        // Advance by the page, once the page is done: the count the card shows
        // is albums accounted for, and every album of the page now is.
        processed += page.len() as i64;
        emit(app, "albums", processed, 0);

        // A short page is the last page. Asking for one more would cost a
        // round-trip to be told the same thing.
        if (page.len() as i64) < ALBUM_PAGE {
            return Ok(());
        }
        offset += ALBUM_PAGE;
    }
}

async fn upsert_album(conn: &mut sqlx::SqliteConnection, album: &AlbumListItem) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO remote_album
            (remote_id, library_id, title, artist, artist_id, artwork_hash, year,
             is_compilation, sort_name, song_count, duration_ms, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(remote_id) DO UPDATE SET
            library_id     = excluded.library_id,
            title          = excluded.title,
            artist         = excluded.artist,
            artist_id      = excluded.artist_id,
            artwork_hash   = excluded.artwork_hash,
            year           = excluded.year,
            is_compilation = excluded.is_compilation,
            sort_name      = excluded.sort_name,
            song_count     = excluded.song_count,
            duration_ms    = excluded.duration_ms,
            created_at     = excluded.created_at,
            -- A changed count invalidates the walk, and must do so in the same
            -- statement that records the new count. Writing the count while
            -- keeping the old stamp is how an album interrupted between this
            -- upsert and its fetch would read as fresh forever, and never
            -- receive the tracks it just gained.
            mirrored_at    = CASE
                                WHEN excluded.song_count = remote_album.song_count
                                THEN remote_album.mirrored_at
                                ELSE NULL
                             END",
    )
    .bind(&album.id)
    .bind(album.library_id.as_deref())
    .bind(&album.title)
    .bind(album.artist.as_deref())
    .bind(album.artist_id.as_deref())
    .bind(album.artwork_hash.as_deref())
    .bind(album.year)
    .bind(i64::from(album.is_compilation))
    .bind(album.sort_name.as_deref())
    .bind(album.song_count.max(0))
    .bind(album.duration_ms.max(0))
    .bind(album.created_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Fetch one album and cache its songs. The `mirrored_at` stamp is set in the
/// same transaction as the tracks: an album marked walked whose tracks were
/// not committed would be skipped by every later walk.
async fn walk_one_album(
    client: &RemoteClient<'_>,
    pool: &SqlitePool,
    album_id: &str,
) -> AppResult<i64> {
    let detail: AlbumDetailDto = client
        .send_json(client.get(&format!("/api/v2/albums/{album_id}")))
        .await
        .map_err(|err| AppError::Other(format!("album fetch failed: {err}")))?;

    let mut tx = pool.begin().await?;
    for song in &detail.songs {
        crate::remote::projection::cache_song(&mut tx, song).await?;
        mark_in_catalogue(&mut tx, &song.id).await?;
    }
    sqlx::query("UPDATE remote_album SET mirrored_at = ? WHERE remote_id = ?")
        .bind(now_ms())
        .bind(album_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(detail.songs.len() as i64)
}

async fn mark_in_catalogue(conn: &mut sqlx::SqliteConnection, remote_id: &str) -> AppResult<()> {
    sqlx::query("UPDATE remote_track SET in_catalogue = 1 WHERE remote_id = ?")
        .bind(remote_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Sweep every library for the two things the album walk cannot see: tracks
/// that belong to no album, and mirrored rows the server has dropped.
async fn sweep_libraries(
    client: &RemoteClient<'_>,
    pool: &SqlitePool,
    app: &AppHandle,
    libraries: &[LibraryAccessDto],
    report: &mut MirrorReport,
) -> AppResult<()> {
    let mut seen: HashSet<String> = HashSet::new();
    for library in libraries {
        let mut offset = 0i64;
        loop {
            if cancelled() {
                return Ok(());
            }
            let page: Vec<TrackRecordDto> = client
                .send_json(
                    client
                        .request(
                            reqwest::Method::GET,
                            &format!("/api/v2/libraries/{}/tracks", library.id),
                        )
                        .query(&[
                            ("offset", offset.to_string()),
                            ("limit", TRACK_PAGE.to_string()),
                        ]),
                )
                .await
                .map_err(|err| AppError::Other(format!("library sweep failed: {err}")))?;
            if page.is_empty() {
                break;
            }
            // An unavailable track is one the server can list but not serve.
            // Mirroring it would put a row in the library that cannot play.
            for record in page.iter().filter(|record| record.available) {
                seen.insert(record.id.clone());
            }
            emit(app, "sweep", seen.len() as i64, 0);
            if (page.len() as i64) < TRACK_PAGE {
                break;
            }
            offset += TRACK_PAGE;
        }
        sqlx::query("UPDATE remote_library SET mirrored_at = ? WHERE remote_id = ?")
            .bind(now_ms())
            .bind(&library.id)
            .execute(pool)
            .await?;
    }
    if cancelled() {
        return Ok(());
    }

    report.orphans_mirrored += fetch_missing(client, pool, &seen).await?;
    report.removed += drop_vanished(pool, &seen).await?;
    Ok(())
}

/// Fetch, one by one, the tracks the sweep listed and the album walk never
/// wrote. On a tidy library this is empty; it exists for the singles that
/// belong to no album.
async fn fetch_missing(
    client: &RemoteClient<'_>,
    pool: &SqlitePool,
    seen: &HashSet<String>,
) -> AppResult<i64> {
    // Every row we hold, whatever its origin. A track the projection already
    // cached for a playlist needs its flag set, not a round-trip: it is the
    // same row, and the metadata it carries came from the same server.
    let held: HashSet<String> = sqlx::query_scalar("SELECT remote_id FROM remote_track")
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect();

    let mut fetched = 0i64;
    let mut tx = pool.begin().await?;
    for id in seen.intersection(&held) {
        mark_in_catalogue(&mut tx, id).await?;
    }
    tx.commit().await?;

    for id in seen.difference(&held) {
        if cancelled() {
            break;
        }
        let song: SongItem = client
            .send_json(client.get(&format!("/api/v2/tracks/{id}")))
            .await
            .map_err(|err| AppError::Other(format!("track fetch failed: {err}")))?;
        let mut tx = pool.begin().await?;
        crate::remote::projection::cache_song(&mut tx, &song).await?;
        mark_in_catalogue(&mut tx, &song.id).await?;
        tx.commit().await?;
        fetched += 1;
    }
    Ok(fetched)
}

/// Drop mirrored rows the sweep did not list. Only rows the mirror owns are
/// touched, and only those nothing else points at: a track a playlist still
/// references keeps its row, it merely stops being part of the catalogue.
async fn drop_vanished(pool: &SqlitePool, seen: &HashSet<String>) -> AppResult<i64> {
    let mirrored: Vec<String> =
        sqlx::query_scalar("SELECT remote_id FROM remote_track WHERE in_catalogue = 1")
            .fetch_all(pool)
            .await?;

    let mut removed = 0i64;
    let mut tx = pool.begin().await?;
    for id in mirrored.iter().filter(|id| !seen.contains(*id)) {
        // Clear the flag first: it is what makes the row stop counting as
        // catalogue, whether or not the delete below can fire.
        sqlx::query("UPDATE remote_track SET in_catalogue = 0 WHERE remote_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // Count the deletions, not the visits: a track a playlist still
        // references leaves the catalogue but stays in the table, and
        // reporting it as removed would overstate what the purge did.
        removed += sqlx::query(DELETE_VANISHED)
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected() as i64;
    }
    tx.commit().await?;
    Ok(removed)
}

/// What the mirror currently holds, for the settings card.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CatalogueStats {
    pub albums: i64,
    /// Albums whose tracks have been walked.
    pub albums_mirrored: i64,
    pub tracks: i64,
    pub artists: i64,
    pub libraries: i64,
    /// When the least recently swept library was swept. `None` until every
    /// library has been swept at least once — a partial mirror must not
    /// report a date that suggests it is complete.
    pub mirrored_at: Option<i64>,
}

pub async fn stats(pool: &SqlitePool) -> AppResult<CatalogueStats> {
    let (albums, albums_mirrored): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(mirrored_at IS NOT NULL), 0) FROM remote_album",
    )
    .fetch_one(pool)
    .await?;
    let (tracks, artists): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(DISTINCT artist_id) FROM remote_track WHERE in_catalogue = 1",
    )
    .fetch_one(pool)
    .await?;
    let (libraries, pending): (i64, i64) = sqlx::query_as(
        "SELECT count(*), coalesce(sum(mirrored_at IS NULL), 0) FROM remote_library",
    )
    .fetch_one(pool)
    .await?;
    let mirrored_at: Option<i64> = if libraries > 0 && pending == 0 {
        sqlx::query_scalar("SELECT min(mirrored_at) FROM remote_library")
            .fetch_one(pool)
            .await?
    } else {
        None
    };

    Ok(CatalogueStats {
        albums,
        albums_mirrored,
        tracks,
        artists,
        libraries,
        mirrored_at,
    })
}

/// Drop the mirror, keeping every row the user data still needs. Recovery for
/// a mirror that went wrong, and the honest way to free the space.
pub async fn clear(pool: &SqlitePool) -> AppResult<()> {
    // Take the same slot a walk takes. A walk writes rows and stamps albums
    // while this deletes them, so overlapping the two leaves a catalogue whose
    // albums are gone and whose tracks are still flagged as belonging to them.
    // Refusing is honest and the collision is a stray click, not a workflow.
    if MIRROR_PHASE
        .compare_exchange(
            PHASE_IDLE,
            PHASE_RUNNING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Err(AppError::Other(
            "a catalogue copy is running; stop it first".into(),
        ));
    }
    let _guard = PhaseGuard;

    let mut tx = pool.begin().await?;
    // Same order as `drop_vanished`: delete what nothing else points at, then
    // unflag whatever survived because something does.
    sqlx::query(DELETE_MIRROR).execute(&mut *tx).await?;
    sqlx::query("UPDATE remote_track SET in_catalogue = 0 WHERE in_catalogue = 1")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM remote_album").execute(&mut *tx).await?;
    sqlx::query("UPDATE remote_library SET mirrored_at = NULL")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    /// `foreign_keys` ON, like the real profile database: a fixture that
    /// leaves it off would let a broken delete pass. See the projection
    /// tests for the same migration list.
    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::raw_sql("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        for migration in [
            include_str!(
                "../../../../migrations/profile/20260810120000_remote_source_projection.sql"
            ),
            include_str!("../../../../migrations/profile/20260810140000_remote_track_cache.sql"),
            include_str!(
                "../../../../migrations/profile/20260813090000_remote_track_full_hash.sql"
            ),
            include_str!(
                "../../../../migrations/profile/20260816120000_remote_track_artist_id.sql"
            ),
            include_str!(
                "../../../../migrations/profile/20260824210000_remote_catalogue_mirror.sql"
            ),
        ] {
            sqlx::raw_sql(migration).execute(&pool).await.unwrap();
        }
        pool
    }

    fn album(id: &str, song_count: i64) -> AlbumListItem {
        AlbumListItem {
            id: id.into(),
            library_id: Some("lib-1".into()),
            title: format!("Album {id}"),
            artist: Some("Artist".into()),
            artist_id: Some("art-1".into()),
            artwork_hash: None,
            year: Some(1999),
            is_compilation: false,
            sort_name: None,
            song_count,
            duration_ms: 1_000,
            created_at: Some(10),
        }
    }

    /// `upsert_album` writes through the page transaction in production; the
    /// tests upsert one album at a time.
    async fn upsert(pool: &SqlitePool, listed: &AlbumListItem) {
        let mut tx = pool.begin().await.unwrap();
        upsert_album(&mut tx, listed).await.unwrap();
        tx.commit().await.unwrap();
    }

    async fn mirrored_track(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO remote_track (remote_id, title, artist_id, duration_ms, cached_at, in_catalogue)
             VALUES (?, ?, 'art-1', 0, 1, 1)",
        )
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_album_is_fresh_only_once_walked_with_the_same_count() {
        let pool = pool().await;
        let listed = album("al-1", 12);

        // Never seen: not fresh.
        assert!(!album_is_fresh(&pool, &listed).await.unwrap());

        // Listed but not yet walked: still not fresh — `mirrored_at` is NULL,
        // which is exactly the state an interrupted walk leaves behind.
        upsert(&pool, &listed).await;
        assert!(!album_is_fresh(&pool, &listed).await.unwrap());

        sqlx::query("UPDATE remote_album SET mirrored_at = 1 WHERE remote_id = 'al-1'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(album_is_fresh(&pool, &listed).await.unwrap());

        // The server gained a track: the count no longer matches, so it is
        // fetched again.
        assert!(!album_is_fresh(&pool, &album("al-1", 13)).await.unwrap());
    }

    /// The listing is the source of truth for an album's own fields; a second
    /// pass must overwrite them rather than accumulate stale ones.
    #[tokio::test]
    async fn re_listing_an_album_updates_it_in_place() {
        let pool = pool().await;
        upsert(&pool, &album("al-1", 3)).await;
        let mut renamed = album("al-1", 4);
        renamed.title = "Renamed".into();
        renamed.is_compilation = true;
        upsert(&pool, &renamed).await;

        let (count, title, compilation): (i64, String, i64) = sqlx::query_as(
            "SELECT song_count, title, is_compilation FROM remote_album WHERE remote_id = 'al-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((count, title.as_str(), compilation), (4, "Renamed", 1));
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// The whole point of `in_catalogue`: dropping the mirror must not take a
    /// playlist's rows with it, or the playlist loses its titles.
    #[tokio::test]
    async fn dropping_the_mirror_spares_rows_the_user_data_needs() {
        let pool = pool().await;
        mirrored_track(&pool, "t-keep").await;
        mirrored_track(&pool, "t-drop").await;
        sqlx::query("INSERT INTO remote_playlist (remote_id, name, updated_at) VALUES ('p1','P',1)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO remote_playlist_track (playlist_remote_id, position, track_remote_id)
             VALUES ('p1', 0, 't-keep')",
        )
        .execute(&pool)
        .await
        .unwrap();

        clear(&pool).await.unwrap();

        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT remote_id, in_catalogue FROM remote_track")
                .fetch_all(&pool)
                .await
                .unwrap();
        // Kept, but no longer part of the catalogue.
        assert_eq!(rows, vec![("t-keep".to_string(), 0)]);
    }

    /// Same rule on the incremental path: a track the server dropped stops
    /// counting as catalogue, and only disappears if nothing points at it.
    #[tokio::test]
    async fn a_vanished_track_is_unflagged_before_it_is_deleted() {
        let pool = pool().await;
        mirrored_track(&pool, "t-gone").await;
        mirrored_track(&pool, "t-liked").await;
        sqlx::query(
            "INSERT INTO remote_favorite (entity_type, entity_id, starred_at)
             VALUES ('track', 't-liked', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Both left the catalogue, but only the unreferenced one was deleted —
        // `removed` counts deletions, not visits.
        let removed = drop_vanished(&pool, &HashSet::new()).await.unwrap();
        assert_eq!(removed, 1);

        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT remote_id, in_catalogue FROM remote_track")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![("t-liked".to_string(), 0)]);
    }

    #[tokio::test]
    async fn a_still_listed_track_survives_the_sweep() {
        let pool = pool().await;
        mirrored_track(&pool, "t-1").await;
        let seen: HashSet<String> = ["t-1".to_string()].into_iter().collect();

        assert_eq!(drop_vanished(&pool, &seen).await.unwrap(), 0);
        let flag: i64 =
            sqlx::query_scalar("SELECT in_catalogue FROM remote_track WHERE remote_id = 't-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(flag, 1);
    }

    /// A date on a half-swept mirror would read as "the catalogue is up to
    /// date", which is the one thing it is not.
    #[tokio::test]
    async fn the_mirror_reports_no_date_until_every_library_is_swept() {
        let pool = pool().await;
        store_libraries(
            &pool,
            &[
                LibraryAccessDto {
                    id: "lib-1".into(),
                    name: "One".into(),
                },
                LibraryAccessDto {
                    id: "lib-2".into(),
                    name: "Two".into(),
                },
            ],
        )
        .await
        .unwrap();
        mirrored_track(&pool, "t-1").await;
        upsert(&pool, &album("al-1", 1)).await;

        let held = stats(&pool).await.unwrap();
        assert_eq!((held.libraries, held.tracks, held.albums), (2, 1, 1));
        // Listed but never walked.
        assert_eq!(held.albums_mirrored, 0);
        assert_eq!(held.mirrored_at, None);

        sqlx::query("UPDATE remote_library SET mirrored_at = 500 WHERE remote_id = 'lib-1'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(stats(&pool).await.unwrap().mirrored_at, None);

        sqlx::query("UPDATE remote_library SET mirrored_at = 900 WHERE remote_id = 'lib-2'")
            .execute(&pool)
            .await
            .unwrap();
        // The oldest sweep, not the newest: that is the age of the mirror.
        assert_eq!(stats(&pool).await.unwrap().mirrored_at, Some(500));
    }

    #[tokio::test]
    async fn re_listing_a_library_keeps_its_sweep_date() {
        let pool = pool().await;
        let libraries = [LibraryAccessDto {
            id: "lib-1".into(),
            name: "One".into(),
        }];
        store_libraries(&pool, &libraries).await.unwrap();
        sqlx::query("UPDATE remote_library SET mirrored_at = 42")
            .execute(&pool)
            .await
            .unwrap();

        // A rename must not look like a library that was never swept.
        store_libraries(
            &pool,
            &[LibraryAccessDto {
                id: "lib-1".into(),
                name: "Renamed".into(),
            }],
        )
        .await
        .unwrap();

        let (name, mirrored): (String, Option<i64>) =
            sqlx::query_as("SELECT name, mirrored_at FROM remote_library")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((name.as_str(), mirrored), ("Renamed", Some(42)));
    }

    /// A library the account lost access to must not keep its sweep date: a
    /// mirror that can no longer see it would still read as complete.
    #[tokio::test]
    async fn a_library_that_disappears_is_dropped() {
        let pool = pool().await;
        store_libraries(
            &pool,
            &[
                LibraryAccessDto {
                    id: "lib-1".into(),
                    name: "One".into(),
                },
                LibraryAccessDto {
                    id: "lib-2".into(),
                    name: "Two".into(),
                },
            ],
        )
        .await
        .unwrap();

        store_libraries(
            &pool,
            &[LibraryAccessDto {
                id: "lib-1".into(),
                name: "One".into(),
            }],
        )
        .await
        .unwrap();

        let rows: Vec<String> = sqlx::query_scalar("SELECT remote_id FROM remote_library")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(rows, vec!["lib-1".to_string()]);
    }

    /// A server with no notion of unavailability omits the flag. Reading that
    /// as "unavailable" would sweep the whole catalogue away on the first run.
    #[test]
    fn a_track_record_without_the_flag_counts_as_available() {
        let record: TrackRecordDto = serde_json::from_str(r#"{"id":"t-1"}"#).unwrap();
        assert!(record.available);

        let hidden: TrackRecordDto =
            serde_json::from_str(r#"{"id":"t-2","available":false}"#).unwrap();
        assert!(!hidden.available);
    }

    /// The listing carries far more than the mirror keeps; unknown fields must
    /// not break the walk, and the ones we do read must land.
    #[test]
    fn an_album_listing_parses_past_the_fields_we_ignore() {
        let listed: AlbumListItem = serde_json::from_str(
            r#"{"id":"al-1","library_id":"lib-1","title":"Kind of Blue",
                "artist":"Miles Davis","artist_id":"art-1","artwork_hash":"abc",
                "year":1959,"is_compilation":false,"sort_name":"Kind of Blue",
                "song_count":5,"duration_ms":2643000,"created_at":17,
                "genres":["Jazz"],"release_types":["album"],"play_count":3}"#,
        )
        .unwrap();
        assert_eq!(listed.song_count, 5);
        assert_eq!(listed.year, Some(1959));
        assert_eq!(listed.artist_id.as_deref(), Some("art-1"));
    }

    /// The walk slot is process-global, so this exercises the whole machine in
    /// one test rather than several: two tests sharing it would race each
    /// other's assertions, not the code under test.
    #[tokio::test]
    async fn the_walk_slot_is_exclusive() {
        // Idle: nothing to cancel, and clearing is allowed.
        assert!(!request_cancel());
        assert!(!cancelled());
        let pool = pool().await;
        clear(&pool).await.unwrap();

        // A walk owns the slot: clearing is refused rather than interleaved.
        MIRROR_PHASE.store(PHASE_RUNNING, Ordering::SeqCst);
        assert!(clear(&pool).await.is_err());
        assert!(request_cancel());
        assert!(cancelled());

        // And the guard hands the slot back, so the next clear goes through.
        MIRROR_PHASE.store(PHASE_IDLE, Ordering::SeqCst);
        clear(&pool).await.unwrap();
        assert_eq!(MIRROR_PHASE.load(Ordering::SeqCst), PHASE_IDLE);
    }

    /// The defect the `mirrored_at` CASE exists for: an album that gains a
    /// track and is interrupted before its fetch must not read as fresh.
    #[tokio::test]
    async fn a_changed_count_invalidates_the_walk_even_without_a_fetch() {
        let pool = pool().await;
        upsert(&pool, &album("al-1", 3)).await;
        sqlx::query("UPDATE remote_album SET mirrored_at = 1 WHERE remote_id = 'al-1'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(album_is_fresh(&pool, &album("al-1", 3)).await.unwrap());

        // The listing now says four, and nothing else happens — no fetch, no
        // stamp. The album must come back stale on the next walk.
        upsert(&pool, &album("al-1", 4)).await;
        assert!(!album_is_fresh(&pool, &album("al-1", 4)).await.unwrap());

        // Re-listing at the same count must not clear a stamp that is valid.
        sqlx::query("UPDATE remote_album SET mirrored_at = 2 WHERE remote_id = 'al-1'")
            .execute(&pool)
            .await
            .unwrap();
        upsert(&pool, &album("al-1", 4)).await;
        assert!(album_is_fresh(&pool, &album("al-1", 4)).await.unwrap());
    }

    /// An empty answer means "this account can browse nothing", not "the
    /// server has nothing" — deleting on it discards every sweep date.
    #[tokio::test]
    async fn an_empty_library_answer_deletes_nothing() {
        let pool = pool().await;
        store_libraries(
            &pool,
            &[LibraryAccessDto {
                id: "lib-1".into(),
                name: "One".into(),
            }],
        )
        .await
        .unwrap();
        sqlx::query("UPDATE remote_library SET mirrored_at = 7")
            .execute(&pool)
            .await
            .unwrap();

        store_libraries(&pool, &[]).await.unwrap();

        let rows: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT remote_id, mirrored_at FROM remote_library")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows, vec![("lib-1".to_string(), Some(7))]);
    }
}
