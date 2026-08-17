//! Driving the engine from a remote play queue (RFC-005).
//!
//! The queue itself — its entries and cursor — lives in
//! [`crate::remote_playback`], which is plain in-memory data compiled into
//! every build. This module is the `sync_v2`-gated half: it fills that
//! queue from the projection, selects a confirmed local reconciliation link
//! when requested, and otherwise mints a stream ticket for the current entry.
//!
//! Advancing is safe to do here because it runs entirely in the async
//! layer — both the manual next/previous seams and the auto-advance on
//! end query the SQLite pool already, so minting a ticket (one HTTP
//! round-trip) at advance time adds no work to the real-time callback.

use std::sync::Arc;

use sqlx::Row;
use tauri::{AppHandle, Manager};

use crate::{
    audio::{engine::AudioCmd, AudioEngine},
    error::AppResult,
    queue::Direction,
    remote_playback::{RemoteEntry, RemoteQueue},
    state::AppState,
};

/// Install `entries` as the remote queue and start playing at `start_index`.
/// The shared tail of every "start a remote session" path.
pub async fn play_entries(
    app: &AppHandle,
    entries: Vec<RemoteEntry>,
    start_index: usize,
) -> AppResult<()> {
    if entries.is_empty() {
        return Err(crate::error::AppError::Other("no tracks to play".into()));
    }
    let index = start_index.min(entries.len() - 1);
    app.state::<AppState>()
        .remote_playback
        .set(RemoteQueue { entries, index });
    play_current(app).await
}

/// Start playing a projected remote playlist from `start_index`, filling
/// the remote queue from the projection so subsequent tracks auto-advance.
pub async fn start(app: &AppHandle, playlist_id: &str, start_index: usize) -> AppResult<()> {
    let state = app.state::<AppState>();
    let entries = {
        let pool = state.require_profile_pool().await?;
        let mut conn = pool.acquire().await?;
        crate::remote::read::playlist_tracks(&mut conn, playlist_id)
            .await?
            .into_iter()
            .map(|t| RemoteEntry {
                id: t.id,
                title: t.title,
                artist: t.artist,
                artist_id: t.artist_id,
                artwork_hash: t.artwork_hash,
                duration_ms: t.duration_ms,
            })
            .collect::<Vec<_>>()
    };
    play_entries(app, entries, start_index).await
}

/// Start a remote session from an explicit list of track ids, reading each
/// one's display metadata from the `remote_track` cache. Backs "play this
/// album": its tracks were cached when the album was fetched, so titles and
/// covers are already on hand; an uncached id still plays by ticket, just
/// without a label until a later fetch.
pub async fn play_track_ids(
    app: &AppHandle,
    track_ids: &[String],
    start_index: usize,
) -> AppResult<()> {
    let state = app.state::<AppState>();
    let entries = {
        let pool = state.require_profile_pool().await?;
        let mut conn = pool.acquire().await?;
        let mut entries = Vec::with_capacity(track_ids.len());
        for id in track_ids {
            let row = sqlx::query(
                "SELECT title, artist, artist_id, artwork_hash, duration_ms
                   FROM remote_track WHERE remote_id = ?",
            )
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
            let (title, artist, artist_id, artwork_hash, duration_ms) = match row {
                Some(r) => (
                    // The cache stores an empty title for a bare id; treat
                    // that as "unknown" rather than a blank label.
                    r.try_get::<String, _>("title")
                        .ok()
                        .filter(|s| !s.is_empty()),
                    r.try_get("artist").ok().flatten(),
                    r.try_get("artist_id").ok().flatten(),
                    r.try_get("artwork_hash").ok().flatten(),
                    r.try_get("duration_ms").ok(),
                ),
                None => (None, None, None, None, None),
            };
            entries.push(RemoteEntry {
                id: id.clone(),
                title,
                artist,
                artist_id,
                artwork_hash,
                duration_ms,
            });
        }
        entries
    };
    play_entries(app, entries, start_index).await
}

/// Move the cursor to an absolute position and play it. Backs the queue
/// panel's click-to-jump on a remote session.
pub async fn jump_to(app: &AppHandle, index: usize) -> AppResult<()> {
    let state = app.state::<AppState>();
    match state.remote_playback.seek_to(index) {
        Some(_) => play_current(app).await,
        None => Ok(()),
    }
}

/// Step the remote cursor and play what it lands on. Returns `Ok(true)`
/// when a track was loaded, `Ok(false)` when the queue ran off the end with
/// repeat off (the session is already cleared, so the engine is stopped).
///
/// A failure to mint the ticket for the stepped-to entry must not strand
/// the session on a track it never played: the engine is stopped and the
/// session cleared before the error propagates, so the next action starts
/// clean rather than acting on a phantom cursor.
pub async fn advance(app: &AppHandle, direction: Direction) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let repeat = {
        let pool = state.require_profile_pool().await?;
        crate::queue::read_repeat_mode(&pool).await
    };
    match state.remote_playback.step(direction, repeat) {
        Some(_) => {
            if let Err(err) = play_current(app).await {
                let engine = app.state::<Arc<AudioEngine>>();
                let _ = engine.send(AudioCmd::Stop);
                state.remote_playback.clear();
                return Err(err);
            }
            Ok(true)
        }
        None => {
            let engine = app.state::<Arc<AudioEngine>>();
            let _ = engine.send(AudioCmd::Stop);
            Ok(false)
        }
    }
}

/// Prefer a confirmed local reconciliation link for the entry under the
/// cursor, otherwise mint a server ticket. Local playback keeps the negative
/// remote sentinel and metadata path, so queue controls and auto-advance stay
/// remote. When online, a best-effort ticket accompanies the local command as
/// a decoder-level fallback if the file disappears or fails to decode.
async fn play_current(app: &AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let Some(entry) = state.remote_playback.current() else {
        return Ok(());
    };

    // Reuse the negative sentinel id space radio uses: no library row, and
    // a fresh id per load so overlays can tell back-to-back tracks apart.
    let track_id = crate::commands::player::next_radio_track_id();

    let engine = app.state::<Arc<AudioEngine>>();
    let pool = state.require_profile_pool().await?;
    if let Some(local) =
        crate::remote::reconciliation::preferred_local_playback(&pool, &entry.id).await?
    {
        let replay_gain_db =
            crate::commands::player::fetch_replay_gain_db(&pool, local.track_id).await;
        let fallback_url = if crate::offline::is_offline() {
            None
        } else {
            match crate::remote::stream::ticket_url(&state, &entry.id).await {
                Ok(url) => Some(url),
                Err(_) => {
                    tracing::warn!("remote local playback has no server fallback ticket");
                    None
                }
            }
        };
        engine.send(AudioCmd::LoadRemoteFileAndPlay {
            path: local.path,
            start_ms: 0,
            track_id,
            duration_ms: local.duration_ms,
            title: entry.title.clone(),
            artist: entry.artist.clone(),
            artwork_url: None,
            fallback_url,
            replay_gain_db,
        })?;
        return Ok(());
    }

    let url = crate::remote::stream::ticket_url(&state, &entry.id).await?;
    engine.send(AudioCmd::LoadUrlAndPlay {
        url,
        ext_hint: None,
        track_id,
        title: entry.title.clone(),
        artist: entry.artist.clone(),
        // Cover art is Bearer-only (fetched as a data URL for the view);
        // the PlayerBar overlay path expects a plain URL, so leave it off
        // here and let the bar fall back to its placeholder.
        artwork_url: None,
    })?;
    Ok(())
}
