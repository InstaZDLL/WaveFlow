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
/// The `current_track` field stays `None` until checkpoint 10 lands the
/// queue module — for now commands like `player_debug_load` play tracks
/// directly without touching the queue, so we have no "current track"
/// to report.
#[derive(Debug, Serialize)]
pub struct PlayerStateSnapshot {
    pub state: String,
    pub position_ms: u64,
    pub volume: f32,
    pub sample_rate: u32,
    pub channels: u16,
}

impl PlayerStateSnapshot {
    fn from_shared(shared: &SharedPlayback) -> Self {
        Self {
            state: shared.state().as_str().to_string(),
            position_ms: shared.current_position_ms(),
            volume: shared.volume(),
            sample_rate: shared.sample_rate.load(std::sync::atomic::Ordering::Relaxed),
            channels: shared.channels.load(std::sync::atomic::Ordering::Relaxed),
        }
    }
}

/// Return the current player snapshot. Cheap — pure atomic reads.
#[tauri::command]
pub async fn player_get_state(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<PlayerStateSnapshot> {
    Ok(PlayerStateSnapshot::from_shared(engine.shared()))
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
