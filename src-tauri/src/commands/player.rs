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
use tauri::{AppHandle, Emitter};

use crate::{
    audio::{engine::AudioCmd, state::SharedPlayback, AudioEngine},
    error::{AppError, AppResult},
    paths::AppPaths,
    queue::{self, Direction, QueueTrack},
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
#[derive(Debug, Clone, Serialize)]
pub struct QueueTrackPayload {
    pub id: i64,
    pub title: String,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub album_title: Option<String>,
    pub duration_ms: i64,
    pub file_path: String,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
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
    track: QueueTrack,
    profile_id: Option<i64>,
) -> QueueTrackPayload {
    queue_track_to_payload_with_paths(&state.paths, track, profile_id)
}

fn queue_track_to_payload_with_paths(
    paths: &AppPaths,
    track: QueueTrack,
    profile_id: Option<i64>,
) -> QueueTrackPayload {
    let (artwork_path, artwork_path_1x, artwork_path_2x) = match (
        track.artwork_hash.as_deref(),
        track.artwork_format.as_deref(),
        profile_id,
    ) {
        (Some(hash), Some(format), Some(pid)) => {
            let dir = paths.profile_artwork_dir(pid);
            let full = dir
                .join(format!("{hash}.{format}"))
                .to_string_lossy()
                .to_string();
            let (p1, p2) = crate::thumbnails::thumbnail_paths_for(&dir, hash);
            (Some(full), p1, p2)
        }
        _ => (None, None, None),
    };
    QueueTrackPayload {
        id: track.id,
        title: track.title,
        artist_id: track.artist_id,
        artist_name: track.artist_name,
        artist_ids: track.artist_ids,
        album_title: track.album_title,
        duration_ms: track.duration_ms,
        file_path: track.file_path,
        artwork_path,
        artwork_path_1x,
        artwork_path_2x,
    }
}

/// Emit `player:track-changed` with the full track payload so the
/// frontend can update the PlayerBar metadata (title, artist, album,
/// cover, duration) at the same moment the decoder starts decoding
/// the new track. Used by every command that kicks off a
/// `LoadAndPlay` plus the analytics task's auto-advance path.
///
/// Also refreshes the system-tray tooltip so right-clicking the tray
/// icon shows what's currently playing without opening the window.
pub(crate) fn emit_track_changed(
    app: &AppHandle,
    paths: &AppPaths,
    track: &QueueTrack,
    profile_id: Option<i64>,
) {
    let payload = queue_track_to_payload_with_paths(paths, track.clone(), profile_id);
    let tooltip = match payload.artist_name.as_deref() {
        Some(artist) if !artist.is_empty() => format!("{} — {}", payload.title, artist),
        _ => payload.title.clone(),
    };
    let _ = app.emit("player:track-changed", payload);
    if let Some(tray) = app.tray_by_id("waveflow") {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

/// Emit an empty `player:queue-changed` signal. The frontend uses
/// this as "refetch the queue" — payload is intentionally empty so
/// the event bus doesn't carry the full 100+ track list.
pub(crate) fn emit_queue_changed(app: &AppHandle) {
    let _ = app.emit("player:queue-changed", ());
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

            // Restore the persisted volume into the atomic shared
            // with the cpal callback. Without this, the volume knob
            // jumps back to 100 % on every app launch.
            if let Some(persisted) =
                queue::read_player_volume(&pool).await
            {
                engine.shared().set_volume(persisted);
            }
            // Restore audio settings (normalize, mono).
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.normalize'"
            ).fetch_optional(&pool).await {
                engine.shared().normalize_enabled.store(
                    v == "true",
                    std::sync::atomic::Ordering::Release,
                );
            }
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.mono'"
            ).fetch_optional(&pool).await {
                engine.shared().mono_enabled.store(
                    v == "true",
                    std::sync::atomic::Ordering::Release,
                );
            }
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.crossfade_ms'"
            ).fetch_optional(&pool).await {
                if let Ok(ms) = v.parse::<u32>() {
                    engine.shared().crossfade_ms.store(
                        ms,
                        std::sync::atomic::Ordering::Release,
                    );
                }
            }
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

/// Jump the queue cursor to an arbitrary position and start playing
/// the track there. Used by the QueuePanel when the user
/// double-clicks a row.
#[tauri::command]
pub async fn player_jump_to_index(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    position: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let Some(track) = queue::jump_to(&pool, position).await? else {
        return Err(AppError::Other("queue is empty".into()));
    };
    emit_track_changed(&app, &state.paths, &track, profile_id);
    emit_queue_changed(&app);
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
    })
}

/// Response shape for `player_get_queue`: the full list of tracks
/// currently in `queue_item`, plus the active cursor position.
#[derive(Debug, Serialize)]
pub struct PlayerQueueSnapshot {
    pub current_index: i64,
    pub items: Vec<QueueTrackPayload>,
}

/// Return the live playback queue (joined with track metadata and
/// artwork paths) plus the current cursor. Used by the QueuePanel
/// frontend component; re-called every time a `player:queue-changed`
/// event fires.
#[tauri::command]
pub async fn player_get_queue(
    state: tauri::State<'_, AppState>,
) -> AppResult<PlayerQueueSnapshot> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let items = queue::list_queue(&pool).await?;
    let current_index: i64 =
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT value FROM profile_setting WHERE key = 'queue.current_index'",
        )
        .fetch_optional(&pool)
        .await?
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    let payload_items = items
        .into_iter()
        .map(|t| queue_track_to_payload_with_paths(&state.paths, t, profile_id))
        .collect();

    Ok(PlayerQueueSnapshot {
        current_index,
        items: payload_items,
    })
}

/// Resume playback from the persisted last-track + position. Used by
/// the frontend when the user hits Play from the idle state without
/// having clicked a specific track yet.
#[tauri::command]
pub async fn player_resume_last(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let Some((track, position_ms)) = queue::restore_state(&pool).await? else {
        return Err(AppError::Other("no resume point available".into()));
    };
    emit_track_changed(&app, &state.paths, &track, profile_id);
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
    app: AppHandle,
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
    // Queue content changed (reordered in place) — tell the panel.
    emit_queue_changed(&app);
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

/// Toggle volume normalization (−3 dB gain reduction on loud tracks).
/// Persisted in `profile_setting['audio.normalize']`.
#[tauri::command]
pub async fn player_set_normalize(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine.send(AudioCmd::SetNormalize(enabled))?;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.normalize', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

/// Toggle mono downmix (average L+R into both channels).
/// Persisted in `profile_setting['audio.mono']`.
#[tauri::command]
pub async fn player_set_mono(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine.send(AudioCmd::SetMono(enabled))?;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.mono', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

/// Update the crossfade duration. Pushes the new value live to the
/// audio engine so the next track transition uses it, and persists
/// to `profile_setting['audio.crossfade_ms']` so it survives a restart.
#[tauri::command]
pub async fn player_set_crossfade(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    seconds: f64,
) -> AppResult<()> {
    let ms = (seconds.max(0.0) * 1000.0).round() as i64;
    engine.send(AudioCmd::SetCrossfade(ms.max(0) as u32))?;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.crossfade_ms', ?, 'int', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(ms.to_string())
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

/// Return current audio settings so the Settings view can hydrate.
#[tauri::command]
pub async fn player_get_audio_settings(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<AudioSettingsSnapshot> {
    let shared = engine.shared();
    let normalize = shared
        .normalize_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let mono = shared
        .mono_enabled
        .load(std::sync::atomic::Ordering::Relaxed);

    let mut crossfade_ms: i64 = 0;
    if let Ok(pool) = state.require_profile_pool().await {
        if let Ok(Some(val)) =
            sqlx::query_scalar::<_, String>("SELECT value FROM profile_setting WHERE key = 'audio.crossfade_ms'")
                .fetch_optional(&pool)
                .await
        {
            crossfade_ms = val.parse().unwrap_or(0);
        }
    }

    Ok(AudioSettingsSnapshot {
        normalize,
        mono,
        crossfade_ms,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct AudioSettingsSnapshot {
    pub normalize: bool,
    pub mono: bool,
    pub crossfade_ms: i64,
}

/// Replace the queue with the given track list and start playing at
/// `start_index`. `source_type` must match one of the enum values on
/// `queue_item.source_type` ('album'|'playlist'|'artist'|'library'|
/// 'liked'|'manual'|'radio').
#[tauri::command]
pub async fn player_play_tracks(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    source_type: String,
    source_id: Option<i64>,
    track_ids: Vec<i64>,
    start_index: usize,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();

    queue::fill_queue(&pool, &source_type, source_id, &track_ids, start_index).await?;

    // Spotify-style: if the user has shuffle enabled, randomize the
    // queue in place immediately after filling it, keeping the track
    // they clicked in position 0. Without this, enabling shuffle
    // before clicking a track would leave the queue sequential and
    // Next would advance alphabetically — visibly "not random".
    let shuffled = queue::read_shuffle(&pool).await;
    if shuffled {
        queue::shuffle(&pool).await?;
    }

    let track = queue::current_track(&pool)
        .await?
        .ok_or_else(|| AppError::Other("queue empty after fill".into()))?;

    tracing::info!(
        source_type = %source_type,
        source_id = ?source_id,
        shuffled,
        queue_len = track_ids.len(),
        start_index,
        current_track_id = track.id,
        current_title = %track.title,
        "player_play_tracks"
    );

    // Tell the QueuePanel to refetch — the queue content and cursor
    // both just changed.
    emit_queue_changed(&app);

    let pb = std::path::PathBuf::from(&track.file_path);
    if !pb.is_file() {
        return Err(AppError::Audio(format!(
            "file not found: {}",
            track.file_path
        )));
    }

    // Tell the frontend about the new track BEFORE dispatching the
    // decoder command so the PlayerBar updates without waiting on
    // the first position/state event.
    emit_track_changed(&app, &state.paths, &track, profile_id);

    engine.send(AudioCmd::LoadAndPlay {
        path: pb,
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type,
        source_id,
    })
}

/// Move the queue item at `from_position` to `to_position`, used by
/// the queue panel's drag-and-drop. The backend re-numbers the
/// surrounding items so positions stay dense and adjusts
/// `queue.current_index` so the playing track keeps playing.
#[tauri::command]
pub async fn player_reorder_queue(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    from_position: i64,
    to_position: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    queue::reorder(&pool, from_position, to_position).await?;
    emit_queue_changed(&app);
    Ok(())
}

/// Append a list of tracks to the end of the playback queue without
/// disturbing the current cursor. Used by the context menu's
/// "Add to queue" action.
#[tauri::command]
pub async fn player_add_to_queue(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<i64>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    queue::append(&pool, &track_ids, "manual", None).await?;
    emit_queue_changed(&app);
    Ok(())
}

/// Insert a list of tracks immediately after the currently-playing
/// position. Used by "Play next" — does not interrupt playback.
#[tauri::command]
pub async fn player_play_next(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<i64>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    queue::insert_after_current(&pool, &track_ids, "manual", None).await?;
    emit_queue_changed(&app);
    Ok(())
}

/// Advance to the next track in the queue, respecting the current
/// repeat mode. No-op when the queue is empty or the user is at the
/// end with repeat off.
#[tauri::command]
pub async fn player_next(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let repeat = queue::read_repeat_mode(&pool).await;
    let queue_len = queue::queue_length(&pool).await?;
    let next_opt = queue::advance(&pool, Direction::Next, repeat).await?;
    match &next_opt {
        Some(track) => tracing::info!(
            next_track_id = track.id,
            next_title = %track.title,
            queue_len,
            ?repeat,
            "player_next advanced"
        ),
        None => tracing::info!(
            queue_len,
            ?repeat,
            "player_next: queue exhausted, no-op"
        ),
    }
    let Some(track) = next_opt else {
        return Ok(());
    };
    emit_track_changed(&app, &state.paths, &track, profile_id);
    emit_queue_changed(&app);
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
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<()> {
    if engine.shared().current_position_ms() > 3000 {
        return engine.send(AudioCmd::Seek(0));
    }
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let repeat = queue::read_repeat_mode(&pool).await;
    let Some(track) = queue::advance(&pool, Direction::Previous, repeat).await? else {
        return Ok(());
    };
    emit_track_changed(&app, &state.paths, &track, profile_id);
    emit_queue_changed(&app);
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
    })
}
