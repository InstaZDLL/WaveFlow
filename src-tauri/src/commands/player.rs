//! Tauri command handlers for the audio player.
//!
//! These are thin wrappers: they validate the arguments, send an
//! [`AudioCmd`] on the engine's command channel, and optionally read
//! lock-free state from [`SharedPlayback`] for the "get state" style
//! queries. They never touch the decoder thread directly.
//!
//! Queue-dependent commands (`player_play_tracks`, `player_next`,
//! `player_previous`) fill the persistent `queue_item` table via the
//! [`crate::queue`] module and then self-send a `LoadAndPlay` to the
//! decoder thread.

use std::sync::Arc;

use serde::Serialize;

use crate::{
    audio::{engine::AudioCmd, state::SharedPlayback, AudioEngine},
    error::{AppError, AppResult},
    queue::{self, Direction},
    state::AppState,
};

/// Snapshot of the player state, returned to the frontend on demand.
///
/// `current_track` is populated from the persisted resume point at
/// startup (before the user has played anything), so the PlayerBar
/// can render the last-played metadata while the engine is still
/// Idle. Once playback starts it still reflects the currently
/// loaded track via `queue::restore_state`.
#[derive(Debug, Serialize)]
pub struct PlayerStateSnapshot {
    pub state: String,
    pub position_ms: u64,
    pub volume: f32,
    pub sample_rate: u32,
    pub channels: u16,
    pub shuffle: bool,
    pub repeat_mode: String,
    pub current_track: Option<QueueTrackPayload>,
}

/// Subset of [`crate::queue::QueueTrack`] flattened into the shape
/// the frontend expects for `Track` display (title / artist / album
/// / artwork path). We rebuild the artwork absolute path on the way
/// out so the UI can pass it straight to the asset protocol.
#[derive(Debug, Serialize)]
pub struct QueueTrackPayload {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub file_path: String,
    pub artwork_path: Option<String>,
}

impl PlayerStateSnapshot {
    fn from_shared(
        shared: &SharedPlayback,
        shuffle: bool,
        repeat_mode: queue::RepeatMode,
        current_track: Option<QueueTrackPayload>,
    ) -> Self {
        Self {
            state: shared.state().as_str().to_string(),
            position_ms: shared.current_position_ms(),
            volume: shared.volume(),
            sample_rate: shared.sample_rate.load(std::sync::atomic::Ordering::Relaxed),
            channels: shared.channels.load(std::sync::atomic::Ordering::Relaxed),
            shuffle,
            repeat_mode: repeat_mode.as_str().to_string(),
            current_track,
        }
    }
}

/// Build a [`QueueTrackPayload`] from a [`crate::queue::QueueTrack`],
/// resolving the artwork absolute path against the active profile's
/// data directory. Returns `None` if the profile paths aren't ready.
fn queue_track_to_payload(
    state: &AppState,
    track: crate::queue::QueueTrack,
    profile_id: Option<i64>,
) -> QueueTrackPayload {
    let artwork_path = match (track.artwork_hash.as_deref(), track.artwork_format.as_deref(), profile_id) {
        (Some(hash), Some(format), Some(pid)) => Some(
            state
                .paths
                .profile_artwork_dir(pid)
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string(),
        ),
        _ => None,
    };
    QueueTrackPayload {
        id: track.id,
        title: track.title,
        artist_name: track.artist_name,
        album_title: track.album_title,
        duration_ms: track.duration_ms,
        file_path: track.file_path,
        artwork_path,
    }
}

/// Return the current player snapshot. Also resolves the "resume
/// track" on the very first call after app launch by reading
/// `player.last_track_id` / `player.last_position_ms`, so the
/// PlayerBar can show the last-played track in paused-at-position
/// state without auto-playing.
#[tauri::command]
pub async fn player_get_state(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<PlayerStateSnapshot> {
    let pool_result = state.require_profile_pool().await;
    let (shuffle, repeat_mode, current_track, resumed_position) = match pool_result {
        Ok(pool) => {
            let shuffle = queue::read_shuffle(&pool).await;
            let repeat_mode = queue::read_repeat_mode(&pool).await;
            let profile_id = state.require_profile_id().await.ok();
            // Prefer the actively-playing track (non-zero track_id in
            // SharedPlayback), fall back to the persisted resume point
            // at startup when the engine is still Idle.
            let active_id = engine
                .shared()
                .current_track_id
                .load(std::sync::atomic::Ordering::Acquire);
            let (restored, position): (Option<crate::queue::QueueTrack>, u64) = if active_id > 0
            {
                // Use `restore_state` with a tweak — if there's an
                // active track_id, look it up directly instead of
                // relying on the persisted last-track.
                match queue::restore_state(&pool).await? {
                    Some((t, _)) if t.id == active_id => {
                        (Some(t), engine.shared().current_position_ms())
                    }
                    _ => (None, engine.shared().current_position_ms()),
                }
            } else {
                match queue::restore_state(&pool).await? {
                    Some((t, ms)) => (Some(t), ms),
                    None => (None, 0),
                }
            };
            let payload =
                restored.map(|t| queue_track_to_payload(&state, t, profile_id));
            (shuffle, repeat_mode, payload, position)
        }
        Err(_) => (false, queue::RepeatMode::Off, None, 0),
    };
    let mut snapshot = PlayerStateSnapshot::from_shared(
        engine.shared(),
        shuffle,
        repeat_mode,
        current_track,
    );
    // When the engine is Idle but we resolved a resume point, use the
    // persisted position instead of the (zero) live counter.
    if snapshot.state == "idle" && snapshot.position_ms == 0 {
        snapshot.position_ms = resumed_position;
    }
    Ok(snapshot)
}

/// Resume playback from the persisted last-track + position. Used by
/// the frontend when the user hits Play from the idle state without
/// having clicked a specific track yet.
#[tauri::command]
pub async fn player_resume_last(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let Some((track, position_ms)) = queue::restore_state(&pool).await? else {
        return Err(AppError::Other("no resume point available".into()));
    };
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: position_ms,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
    })
}

/// Flip shuffle on or off. Returns the new state. When turning on,
/// randomizes the existing queue in place (keeping the current track
/// in slot 0). When turning off, restores the pre-shuffle order.
#[tauri::command]
pub async fn player_toggle_shuffle(
    state: tauri::State<'_, AppState>,
) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;
    let current = queue::read_shuffle(&pool).await;
    let next = !current;
    queue::write_shuffle(&pool, next).await?;
    if next {
        queue::shuffle(&pool).await?;
    } else {
        queue::unshuffle(&pool).await?;
    }
    Ok(next)
}

/// Cycle `off → all → one → off` and return the new mode as a string.
#[tauri::command]
pub async fn player_cycle_repeat(
    state: tauri::State<'_, AppState>,
) -> AppResult<String> {
    let pool = state.require_profile_pool().await?;
    let current = queue::read_repeat_mode(&pool).await;
    let next = match current {
        queue::RepeatMode::Off => queue::RepeatMode::All,
        queue::RepeatMode::All => queue::RepeatMode::One,
        queue::RepeatMode::One => queue::RepeatMode::Off,
    };
    queue::write_repeat_mode(&pool, next).await?;
    Ok(next.as_str().to_string())
}

/// Pause the current track. No-op if nothing is playing.
#[tauri::command]
pub async fn player_pause(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<()> {
    engine.send(AudioCmd::Pause)
}

/// Resume a paused track. No-op if already playing.
#[tauri::command]
pub async fn player_resume(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<()> {
    engine.send(AudioCmd::Resume)
}

/// Stop playback entirely. The current track ends, the decoder goes
/// back to Idle, and the queue cursor is left untouched (advance/restore
/// is caller's responsibility in checkpoint 10).
#[tauri::command]
pub async fn player_stop(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<()> {
    engine.send(AudioCmd::Stop)
}

/// Seek the current track to an absolute position in milliseconds.
#[tauri::command]
pub async fn player_seek(
    engine: tauri::State<'_, Arc<AudioEngine>>,
    ms: u64,
) -> AppResult<()> {
    engine.send(AudioCmd::Seek(ms))
}

/// Set playback volume. `value` is clamped to `[0.0, 1.0]` on the
/// decoder side; values outside that range are saturated, not rejected.
/// Also persists to `profile_setting['player.volume']` (as an int 0-100
/// rounded from the f32) so the volume survives an app restart.
#[tauri::command]
pub async fn player_set_volume(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    value: f32,
) -> AppResult<()> {
    engine.send(AudioCmd::SetVolume(value))?;

    // Best-effort persist — not fatal if the profile pool is gone.
    if let Ok(pool) = state.require_profile_pool().await {
        let clamped = value.clamp(0.0, 1.0);
        let as_int = (clamped * 100.0).round() as i64;
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "UPDATE profile_setting
                SET value = ?, updated_at = ?
              WHERE key = 'player.volume'",
        )
        .bind(as_int.to_string())
        .bind(now)
        .execute(&pool)
        .await;
    }

    Ok(())
}

/// Replace the queue with the given track list and start playing at
/// `start_index`. `source_type` must match one of the enum values on
/// `queue_item.source_type` ('album'|'playlist'|'artist'|'library'|
/// 'liked'|'manual'|'radio').
#[tauri::command]
pub async fn player_play_tracks(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    source_type: String,
    source_id: Option<i64>,
    track_ids: Vec<i64>,
    start_index: usize,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    queue::fill_queue(&pool, &source_type, source_id, &track_ids, start_index).await?;

    let track = queue::current_track(&pool)
        .await?
        .ok_or_else(|| AppError::Other("queue empty after fill".into()))?;

    let pb = std::path::PathBuf::from(&track.file_path);
    if !pb.is_file() {
        return Err(AppError::Audio(format!(
            "file not found: {}",
            track.file_path
        )));
    }

    engine.send(AudioCmd::LoadAndPlay {
        path: pb,
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type,
        source_id,
    })
}

/// Advance to the next track in the queue, respecting the current
/// repeat mode. No-op when the queue is empty or the user is at the
/// end with repeat off.
#[tauri::command]
pub async fn player_next(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let repeat = queue::read_repeat_mode(&pool).await;
    let Some(track) = queue::advance(&pool, Direction::Next, repeat).await? else {
        return Ok(());
    };
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
    })
}

/// Go back to the previous track. Mirrors Spotify / Apple Music
/// behaviour: if the current position is past 3 s, seek to 0 instead
/// of jumping tracks.
#[tauri::command]
pub async fn player_previous(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    if engine.shared().current_position_ms() > 3000 {
        return engine.send(AudioCmd::Seek(0));
    }
    let pool = state.require_profile_pool().await?;
    let repeat = queue::read_repeat_mode(&pool).await;
    let Some(track) = queue::advance(&pool, Direction::Previous, repeat).await? else {
        return Ok(());
    };
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
    })
}
