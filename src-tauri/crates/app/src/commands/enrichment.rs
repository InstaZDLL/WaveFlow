//! The library enrichment engine's commands: its switch, its passes, and
//! the review of what it was not sure about.
//!
//! The engine itself is [`waveflow_core::enrichment`] — matching, the
//! catalogue calls, the gap filling — and [`engine`] runs it over the
//! active profile. What is here is the IPC: read the decisions, accept,
//! reject, undo.
//!
//! Nothing here writes into an audio file. See
//! `docs/features/enrichment.md`.

mod engine;

use serde::Serialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use waveflow_core::enrichment::resolve;
use waveflow_core::enrichment::store::{
    self, AlbumDecision, ArtistDecision, Fields, Stats, Status, StoredCredit,
};
use waveflow_core::enrichment::ArtistFacts;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub use engine::{maybe_run, stop_for_profile_change};
// The tag workshop matches with the engine's clients and fills a missing
// cover the way the engine does.
pub(crate) use engine::{clients, fetch_cover, write_now, ProfileDb};

/// Emitted whenever a decision changes: the Settings card and the review
/// window re-read their counts and lists.
pub const CHANGED_EVENT: &str = "enrichment:changed";

const ENABLED_KEY: &str = "enrichment.enabled";
const GENRES_KEY: &str = "enrichment.fill_genres";
const DATES_KEY: &str = "enrichment.fill_dates";
const COVERS_KEY: &str = "enrichment.fill_covers";
const CREDITS_KEY: &str = "enrichment.fill_credits";

async fn read_bool(pool: &SqlitePool, key: &str, default: bool) -> bool {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    match raw.as_deref() {
        Some("true") | Some("1") => true,
        Some(_) => false,
        None => default,
    }
}

/// Off unless the user turned it on: a pass sends the library's titles
/// and artist names to a third party, one a second for as long as it
/// takes. The default is mirrored in `settingsRegistry.ts`.
pub(super) async fn read_enabled(pool: &SqlitePool) -> bool {
    read_bool(pool, ENABLED_KEY, false).await
}

/// What the engine may fill, all on by default (`settingsRegistry.ts`).
pub(super) async fn read_fields(pool: &SqlitePool) -> Fields {
    Fields {
        genres: read_bool(pool, GENRES_KEY, true).await,
        dates: read_bool(pool, DATES_KEY, true).await,
        covers: read_bool(pool, COVERS_KEY, true).await,
        credits: read_bool(pool, CREDITS_KEY, true).await,
    }
}

fn parse_status(status: &str) -> AppResult<Status> {
    match status {
        "applied" => Ok(Status::Applied),
        "review" => Ok(Status::Review),
        "rejected" => Ok(Status::Rejected),
        "no_match" => Ok(Status::NoMatch),
        other => Err(AppError::Other(format!(
            "unknown decision status {other:?}"
        ))),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// How many rows a review list returns, at most. The window virtualizes
/// them, so the cap is not a rendering one: it bounds one answer's size
/// (a few hundred bytes a row) for a library far past any the engine was
/// measured on. The Settings card counts every decision, capped or not.
const LIST_LIMIT: i64 = 5000;

#[derive(Debug, Serialize)]
pub struct EnrichmentStatus {
    pub running: bool,
    pub stats: Stats,
}

#[tauri::command]
pub async fn enrichment_status(state: tauri::State<'_, AppState>) -> AppResult<EnrichmentStatus> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    Ok(EnrichmentStatus {
        running: engine::is_running(),
        stats: store::stats(&mut conn).await?,
    })
}

/// Start a pass now (or right after the running one), if the engine is
/// on for the active profile.
#[tauri::command]
pub fn enrichment_run(app: AppHandle) {
    maybe_run(&app);
}

/// Stop the running pass between two albums. `false` when none ran.
#[tauri::command]
pub fn enrichment_cancel() -> bool {
    engine::request_stop()
}

#[tauri::command]
pub async fn enrichment_album_decisions(
    state: tauri::State<'_, AppState>,
    status: String,
) -> AppResult<Vec<AlbumDecision>> {
    let status = parse_status(&status)?;
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    Ok(store::album_decisions(&mut conn, status, LIST_LIMIT).await?)
}

/// Accept a proposed release: it is looked up again and applied as if
/// it had been confident, with the fields the user allows.
#[tauri::command]
pub async fn enrichment_accept_album(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<()> {
    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on — turn it off to apply a match".into(),
        ));
    }
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    let db = engine::ProfileDb::for_command(&app, profile_id);
    let mut conn = pool.acquire().await?;
    let release = store::proposed_release_mbid(&mut conn, album_id)
        .await?
        .ok_or_else(|| AppError::Other(format!("album {album_id} has no pending proposal")))?;
    let album = store::load_album(&mut conn, album_id)
        .await?
        .ok_or_else(|| AppError::Other(format!("album {album_id} not found")))?;
    drop(conn);
    let fields = read_fields(&pool).await;
    // No lease across the lookup: see `engine::ProfileDb`.
    drop(pool);
    let (mb, caa) = engine::clients();
    let resolution = resolve::resolve_against(&mb, &album, &release)
        .await
        .map_err(|e| AppError::Other(format!("musicbrainz: {e}")))?
        .ok_or_else(|| AppError::Other("the release is gone from MusicBrainz".into()))?;
    engine::apply_resolution(
        &db,
        &caa,
        &artwork_dir,
        album_id,
        &resolution,
        Status::Applied,
        fields,
    )
    .await?;
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("library:rescanned", ());
    Ok(())
}

#[tauri::command]
pub async fn enrichment_reject_album(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = now_ms();
    engine::write_now(&pool, |conn| {
        Box::pin(store::reject_album(conn, album_id, now))
    })
    .await?;
    let _ = app.emit(CHANGED_EVENT, ());
    Ok(())
}

/// Take back what an applied match filled; the album is not matched
/// again on its own.
#[tauri::command]
pub async fn enrichment_undo_album(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = now_ms();
    engine::write_now(&pool, |conn| {
        Box::pin(store::undo_album(conn, album_id, now))
    })
    .await?;
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("library:rescanned", ());
    Ok(())
}

/// Forget a decision so the next pass looks the album up again.
#[tauri::command]
pub async fn enrichment_forget_album(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    album_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = now_ms();
    engine::write_now(&pool, |conn| {
        Box::pin(store::forget_album(conn, album_id, now))
    })
    .await?;
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("library:rescanned", ());
    Ok(())
}

#[tauri::command]
pub async fn enrichment_artist_decisions(
    state: tauri::State<'_, AppState>,
    status: String,
) -> AppResult<Vec<ArtistDecision>> {
    let status = parse_status(&status)?;
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    Ok(store::artist_decisions(&mut conn, status, LIST_LIMIT).await?)
}

/// Accept proposed artists: each is looked up for its facts and marked
/// applied. Returns how many were. One that fails is left pending and
/// does not stop the others.
#[tauri::command]
pub async fn enrichment_accept_artists(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_ids: Vec<i64>,
) -> AppResult<u32> {
    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is on — turn it off to apply a match".into(),
        ));
    }
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    drop(pool);
    let db = engine::ProfileDb::for_command(&app, profile_id);
    let (mb, _) = engine::clients();
    let mut accepted = 0;
    for artist_id in artist_ids {
        // Asked again for every artist: a long batch must stop at the
        // moment the user goes offline, not after it.
        if crate::offline::is_offline() {
            break;
        }
        let lease = db.lease().await?;
        let mut conn = lease.acquire().await?;
        let mbid = store::proposed_artist_mbid(&mut conn, artist_id).await?;
        drop(conn);
        drop(lease);
        let Some(mbid) = mbid else {
            continue;
        };
        let facts = match mb.get_artist(&mbid).await {
            Ok(Some(found)) => resolve::artist_facts(&found),
            // The service no longer knows it: accept what the proposal
            // already says rather than nothing.
            Ok(None) => ArtistFacts {
                artist_mbid: Some(mbid.clone()),
                ..Default::default()
            },
            Err(err) => {
                tracing::warn!(artist_id, %err, "artist lookup failed");
                continue;
            }
        };
        let now = now_ms();
        let ok = db
            .write(|conn| {
                let facts = facts.clone();
                Box::pin(async move { store::accept_artist(conn, artist_id, &facts, now).await })
            })
            .await?;
        if ok {
            accepted += 1;
            let _ = app.emit("artist:updated", artist_id);
        }
        let _ = app.emit(CHANGED_EVENT, ());
    }
    Ok(accepted)
}

/// Refuse a proposal, or take back an accepted artist.
#[tauri::command]
pub async fn enrichment_reject_artist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let now = now_ms();
    engine::write_now(&pool, |conn| {
        Box::pin(store::reject_artist(conn, artist_id, now))
    })
    .await?;
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("artist:updated", artist_id);
    Ok(())
}

/// Forget an artist decision so the next pass proposes again.
#[tauri::command]
pub async fn enrichment_forget_artist(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    engine::write_now(&pool, |conn| {
        Box::pin(store::forget_artist(conn, artist_id))
    })
    .await?;
    let _ = app.emit(CHANGED_EVENT, ());
    let _ = app.emit("artist:updated", artist_id);
    Ok(())
}

/// The facts of an accepted artist, for its page.
#[tauri::command]
pub async fn get_artist_musicbrainz(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<Option<ArtistFacts>> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    Ok(store::artist_facts(&mut conn, artist_id).await?)
}

/// A track's credits, for its properties.
#[tauri::command]
pub async fn get_track_musicbrainz_credits(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Vec<StoredCredit>> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    Ok(store::track_credits(&mut conn, track_id).await?)
}
