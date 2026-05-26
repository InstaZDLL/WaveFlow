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
use tauri::{AppHandle, Emitter, Manager};

use crate::{
    audio::{engine::AudioCmd, state::SharedPlayback, AudioEngine},
    error::{AppError, AppResult},
    paths::AppPaths,
    queue::{self, Direction, QueueTrack},
    state::AppState,
};

/// Look up the analyzed ReplayGain for a track. Returns `None` if the
/// track has never been analyzed or if the lookup fails — both cases
/// mean "leave the signal untouched", which is the safe default.
///
/// Called at every `LoadAndPlay` / `SetNextTrack` dispatch site so the
/// decoder thread never has to reach into SQLite from the audio path.
pub(crate) async fn fetch_replay_gain_db(pool: &sqlx::SqlitePool, track_id: i64) -> Option<f64> {
    sqlx::query_scalar::<_, Option<f64>>(
        "SELECT replay_gain_db FROM track_analysis WHERE track_id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
}

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
    /// Audio quality fields used by the PlayerBar footer / Hi-Res
    /// badge. Carried through here so the frontend doesn't have to
    /// issue a separate `list_tracks` lookup just to learn the
    /// codec of what's playing.
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    pub codec: Option<String>,
    pub file_size: i64,
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
            sample_rate: shared
                .sample_rate
                .load(std::sync::atomic::Ordering::Relaxed),
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
        bitrate: track.bitrate,
        sample_rate: track.sample_rate,
        channels: track.channels,
        bit_depth: track.bit_depth,
        codec: track.codec,
        file_size: track.file_size,
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
    let _ = app.emit("player:track-changed", payload.clone());
    if let Some(tray) = app.tray_by_id("waveflow") {
        let _ = tray.set_tooltip(Some(tooltip));
    }

    // Push the new track to the OS media overlay (SMTC / MPRIS /
    // MediaRemote). Optional — `init` returns None on platforms
    // where souvlaki failed to start.
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_metadata(
            payload.title,
            payload.artist_name,
            payload.album_title,
            payload.artwork_path,
            payload.duration_ms,
        );
        // Reset the OS progress bar to zero so the overlay's
        // scrubber matches the freshly loaded track. The decoder
        // will follow up with the Loading → Playing transition,
        // which keeps the OS state in sync after the actual
        // first samples reach the device.
        controls.update_playback(crate::audio::PlayerState::Playing, 0);
    }

    // Schedule a Last.fm `track.updateNowPlaying` ping after a short
    // settling delay. The delay filters out rapid skipping — if the
    // user blasts through five tracks looking for the right one, we
    // only announce the one they actually settle on. Best-effort: any
    // failure is logged but never surfaced to the UI.
    schedule_now_playing(app, track);

    // Push the new track to Discord Rich Presence (best-effort,
    // gated on the user-controlled opt-in flag). Done in a tokio
    // task because we need an async DB lookup for the public Deezer
    // cover URL — the local artwork path Discord can't reach.
    schedule_discord_presence(app, track);

    // Optional native OS toast (off by default). Same fire-and-
    // forget pattern as Discord RPC — gated on
    // `app_setting['notifications.track_change']`, no-op when off
    // or when the plugin failed to initialise.
    crate::notifications::schedule(app, track.title.clone(), track.artist_name.clone());
}

/// Spawn a tokio task that resolves the Deezer cover URL for the new
/// track and pushes the metadata to Discord. Held off the synchronous
/// path so the audio engine isn't waiting on SQLite.
fn schedule_discord_presence(app: &AppHandle, track: &QueueTrack) {
    let app = app.clone();
    let track_id = track.id;
    let title = track.title.clone();
    let artist = track.artist_name.clone();
    let album = track.album_title.clone();
    let duration_ms = track.duration_ms;

    tauri::async_runtime::spawn(async move {
        let Some(presence) = app.try_state::<crate::discord_presence::DiscordPresenceHandle>()
        else {
            return;
        };
        let state = app.state::<AppState>();
        let cover_url = match state.require_profile_pool().await {
            Ok(pool) => {
                crate::discord_presence::resolve_cover_url(
                    &pool,
                    &state.paths.metadata_artwork_dir,
                    track_id,
                )
                .await
            }
            Err(_) => None,
        };
        presence.update_metadata(title, artist, album, cover_url, duration_ms, 0);
    });
}

/// Spawn a tokio task that, after a 4 s settling window, posts
/// `track.updateNowPlaying` to Last.fm if the user is still on the
/// same track. The settling check is the cheap atomic read of
/// `current_track_id` — no DB hit before we know we'll actually
/// fire the request.
fn schedule_now_playing(app: &AppHandle, track: &QueueTrack) {
    let app = app.clone();
    let track_id = track.id;
    let title = track.title.clone();
    let artist = track.artist_name.clone();
    let album = track.album_title.clone();
    let duration_ms = track.duration_ms;

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;

        // Still on the same track? If the user skipped during the
        // settling window, current_track_id has moved on — don't
        // announce a track we've already left.
        let engine = app.state::<Arc<AudioEngine>>();
        let still_current = engine
            .shared()
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire)
            == track_id;
        if !still_current {
            return;
        }

        // Need a real artist for Last.fm — the API rejects calls
        // without one, and our scanner falls back to the file name
        // when tags are empty.
        let Some(artist) = artist.filter(|s| !s.trim().is_empty()) else {
            return;
        };
        // Honour offline mode — skip the now-playing ping; the
        // scrobble queue will also be drained later when offline is off.
        if crate::offline::is_offline() {
            return;
        }

        let state = app.state::<AppState>();
        let creds = match crate::commands::integration::read_lastfm_credentials(&state).await {
            Ok(Some(c)) => c,
            _ => return,
        };
        let (api_key, api_secret, session_key, _username) = creds;

        let client = crate::lastfm::LastfmClient::new();
        let duration_s = if duration_ms > 0 {
            Some(duration_ms / 1000)
        } else {
            None
        };
        match client
            .update_now_playing(
                &api_key,
                &api_secret,
                &session_key,
                &artist,
                &title,
                album.as_deref(),
                duration_s,
            )
            .await
        {
            Ok(()) => {
                tracing::debug!(track_id, "lastfm updateNowPlaying ok");
            }
            Err(crate::lastfm::LastfmError::Api { code: 9, .. }) => {
                // Same recovery as the scrobbler: wipe the dead
                // session + raise the re-auth banner. Pulling the
                // pool here is best-effort — failure to acquire it
                // means we just drop the event.
                if let Ok(pool) = state.require_profile_pool().await {
                    crate::scrobbler::handle_invalid_session(&app, &pool).await;
                }
            }
            Err(err) => {
                tracing::warn!(track_id, %err, "lastfm updateNowPlaying failed");
            }
        }
    });
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
            if let Some(persisted) = queue::read_player_volume(&pool).await {
                engine.shared().set_volume(persisted);
            }
            // Restore audio settings (normalize, mono).
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.normalize'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .normalize_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.mono'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .mono_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.crossfade_ms'",
            )
            .fetch_optional(&pool)
            .await
            {
                if let Ok(ms) = v.parse::<u32>() {
                    engine
                        .shared()
                        .crossfade_ms
                        .store(ms, std::sync::atomic::Ordering::Release);
                }
            }
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.replaygain'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .replaygain_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            // Gapless defaults to ON, so only override the boot-time
            // default when an explicit `false` row is found.
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.gapless'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .gapless_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            // Playback speed defaults to 1.0; only override when a
            // valid float row is persisted. Out-of-range values are
            // re-clamped on the way in so a hand-edited DB can't
            // resurrect the rubato instability range.
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.playback_speed'",
            )
            .fetch_optional(&pool)
            .await
            {
                if let Ok(speed) = v.parse::<f32>() {
                    // Use the raw atomic stores here instead of
                    // `set_playback_speed` — the latter would also
                    // rebase `samples_played` / `base_offset_ms`
                    // against a position we haven't loaded yet,
                    // moving the resume point off the persisted
                    // value. `speed_dirty` stays false because no
                    // stream is decoding yet; the first track's
                    // lazy resampler init picks up the speed via
                    // `stream.playback_speed = shared.playback_speed()`.
                    let clamped = speed.clamp(0.5, 2.0);
                    engine
                        .shared()
                        .playback_speed_bits
                        .store(clamped.to_bits(), std::sync::atomic::Ordering::Release);
                }
            }
            // Equalizer settings.
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.eq_enabled'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine.shared().eq.set_enabled(v == "true");
            }
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.eq_bands'",
            )
            .fetch_optional(&pool)
            .await
            {
                if let Ok(bands) = serde_json::from_str::<Vec<f32>>(&v) {
                    engine.shared().eq.set_all_bands_db(&bands);
                }
            }
            // Smart crossfade default OFF — only flip ON if the user
            // has explicitly enabled it.
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.smart_crossfade'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .smart_crossfade_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            // Dynamic (tempo-aware) crossfade — same opt-in pattern.
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'audio.dynamic_crossfade'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .dynamic_crossfade_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            // Visualizer toggle. Default OFF — the FFT cost is tiny
            // but the cpal-side telemetry isn't free, and most users
            // won't have the panel open.
            if let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
                "SELECT value FROM profile_setting WHERE key = 'ui.visualizer'",
            )
            .fetch_optional(&pool)
            .await
            {
                engine
                    .shared()
                    .visualizer_enabled
                    .store(v == "true", std::sync::atomic::Ordering::Release);
            }
            // Prefer the actively-playing track (non-zero track_id in
            // SharedPlayback), fall back to the persisted resume point
            // at startup when the engine is still Idle.
            let active_id = engine
                .shared()
                .current_track_id
                .load(std::sync::atomic::Ordering::Acquire);
            let (restored, position): (Option<crate::queue::QueueTrack>, u64) = if active_id > 0 {
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
            let payload = restored.map(|t| queue_track_to_payload(&state, t, profile_id));
            (shuffle, repeat_mode, payload, position)
        }
        Err(_) => (false, queue::RepeatMode::Off, None, 0),
    };
    let mut snapshot =
        PlayerStateSnapshot::from_shared(engine.shared(), shuffle, repeat_mode, current_track);
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
    let replay_gain_db = fetch_replay_gain_db(&pool, track.id).await;
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain_db,
    })
}

/// Response shape for `player_get_queue`: the full list of tracks
/// currently in `queue_item`, plus the active cursor position.
#[derive(Debug, Serialize)]
pub struct PlayerQueueSnapshot {
    pub current_index: i64,
    pub items: Vec<QueueTrackPayload>,
    /// `queue_item.source_type` of the queue's first item, or `None`
    /// when the queue is empty. Single value because every row in a
    /// freshly-filled queue shares the same source — a mixed-source
    /// queue would only happen via append (`add_to_queue`) and the
    /// frontend uses this field for the queue-wide "Radio based on
    /// X" banner, which is only meaningful for fill-queue sources.
    pub source_type: Option<String>,
}

/// Return the live playback queue (joined with track metadata and
/// artwork paths) plus the current cursor. Used by the QueuePanel
/// frontend component; re-called every time a `player:queue-changed`
/// event fires.
#[tauri::command]
pub async fn player_get_queue(state: tauri::State<'_, AppState>) -> AppResult<PlayerQueueSnapshot> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await.ok();
    let items = queue::list_queue(&pool).await?;
    let current_index: i64 = sqlx::query_scalar::<_, Option<String>>(
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

    // Single extra query for the queue-wide source type. Reading
    // position 0 is fine because fill_queue always rewrites the
    // whole queue with one source — append paths use 'manual' which
    // the frontend treats the same as no-source.
    let source_type: Option<String> =
        sqlx::query_scalar("SELECT source_type FROM queue_item WHERE position = 0 LIMIT 1")
            .fetch_optional(&pool)
            .await?;

    Ok(PlayerQueueSnapshot {
        current_index,
        items: payload_items,
        source_type,
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
    let replay_gain_db = fetch_replay_gain_db(&pool, track.id).await;
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: position_ms,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain_db,
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
pub async fn player_cycle_repeat(state: tauri::State<'_, AppState>) -> AppResult<String> {
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

/// Arm or disarm the "pause when the current track ends" flag. Used
/// by the sleep-timer's "end of current track" mode to suppress the
/// auto-advance step so the queue cursor stays put. The flag is
/// one-shot — consumed by the analytics worker the next time a track
/// ends naturally.
#[tauri::command]
pub async fn player_set_pause_after_track(
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine
        .shared
        .pause_after_current_track
        .store(enabled, std::sync::atomic::Ordering::Release);
    Ok(())
}

/// A-B loop snapshot returned to the frontend so the UI can render the
/// current loop state on mount (and after a track change clears it).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AbLoopSnapshot {
    pub a_ms: Option<u64>,
    pub b_ms: Option<u64>,
}

/// Configure the A-B loop. Pass `None` for either endpoint to clear
/// only that side; passing both as `None` disarms the loop entirely.
/// The decoder ignores the loop unless `a_ms < b_ms` and both are set,
/// so a partially-configured loop just sits as a tentative bookmark
/// until the user sets the second point.
///
/// Emits `player:ab-loop` with the new snapshot so every open view
/// (player bar, fullscreen player, lyrics overlay) can re-render.
#[tauri::command]
pub async fn player_set_ab_loop(
    app: AppHandle,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    a_ms: Option<u64>,
    b_ms: Option<u64>,
) -> AppResult<AbLoopSnapshot> {
    use std::sync::atomic::Ordering;
    let shared = engine.shared();
    if let Some(a) = a_ms {
        shared.loop_a_ms.store(a, Ordering::Release);
    }
    if let Some(b) = b_ms {
        shared.loop_b_ms.store(b, Ordering::Release);
    }
    if a_ms.is_none() && b_ms.is_none() {
        shared.clear_ab_loop();
    }
    let snap = current_ab_loop(shared);
    let _ = app.emit("player:ab-loop", snap.clone());
    Ok(snap)
}

/// Drop both A-B loop endpoints. Same effect as calling
/// `player_set_ab_loop(None, None)` — exposed as a separate command
/// so the UI's "Clear" button has a self-documenting call site.
#[tauri::command]
pub async fn player_clear_ab_loop(
    app: AppHandle,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<AbLoopSnapshot> {
    engine.shared().clear_ab_loop();
    let snap = AbLoopSnapshot {
        a_ms: None,
        b_ms: None,
    };
    let _ = app.emit("player:ab-loop", snap.clone());
    Ok(snap)
}

#[tauri::command]
pub async fn player_get_ab_loop(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<AbLoopSnapshot> {
    Ok(current_ab_loop(engine.shared()))
}

fn current_ab_loop(shared: &crate::audio::state::SharedPlayback) -> AbLoopSnapshot {
    use std::sync::atomic::Ordering;
    let a = shared.loop_a_ms.load(Ordering::Acquire);
    let b = shared.loop_b_ms.load(Ordering::Acquire);
    AbLoopSnapshot {
        a_ms: if a > 0 { Some(a) } else { None },
        b_ms: if b > 0 { Some(b) } else { None },
    }
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
    app: AppHandle,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    ms: u64,
) -> AppResult<()> {
    engine.send(AudioCmd::Seek(ms))?;
    // Resync the OS overlay's progress bar. The decoder doesn't
    // transition state on a seek (Playing stays Playing), so the
    // automatic state-change hook wouldn't fire here.
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        let state = engine.shared().state();
        controls.update_playback(state, ms);
    }
    if let Some(presence) = app.try_state::<crate::discord_presence::DiscordPresenceHandle>() {
        presence.update_playback(engine.shared().state(), ms);
    }
    Ok(())
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

/// Toggle ReplayGain — multiply each track by its analyzed gain to
/// even out perceived loudness across the library.
/// Persisted in `profile_setting['audio.replaygain']`.
#[tauri::command]
pub async fn player_set_replaygain(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine.send(AudioCmd::SetReplayGain(enabled))?;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.replaygain', ?, 'bool', ?)
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

/// Toggle smart crossfade. When ON, same-album transitions skip
/// the fade and fall through to the gapless hand-off so concept
/// albums / live records aren't smeared by an equal-power mix.
/// Default OFF — it's an opinionated behaviour change so users
/// opt in. Persisted in `profile_setting['audio.smart_crossfade']`.
#[tauri::command]
pub async fn player_set_smart_crossfade(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine
        .shared()
        .smart_crossfade_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.smart_crossfade', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn player_get_smart_crossfade(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<bool> {
    Ok(engine
        .shared()
        .smart_crossfade_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Toggle dynamic (tempo-aware) crossfade. When ON, the analytics
/// worker scales each upcoming fade by the BPM gap between the
/// current and next tracks: similar tempos keep the full window
/// (clean blend), large gaps shrink it so the rhythms don't clash.
/// Falls through to the user's static `crossfade_ms` when either
/// track has no stored BPM. Default OFF — opt-in via Settings.
/// Persisted in `profile_setting['audio.dynamic_crossfade']`.
#[tauri::command]
pub async fn player_set_dynamic_crossfade(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine
        .shared()
        .dynamic_crossfade_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    if !enabled {
        // Drop any in-flight override so the next transition snaps
        // back to the static crossfade immediately.
        engine
            .shared()
            .pending_next_crossfade_ms
            .store(0, std::sync::atomic::Ordering::Release);
    }
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.dynamic_crossfade', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn player_get_dynamic_crossfade(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<bool> {
    Ok(engine
        .shared()
        .dynamic_crossfade_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Toggle the spectrum visualizer. Flips the atomic the decoder
/// thread checks before running the FFT, then persists the new
/// value to `profile_setting['ui.visualizer']` so the choice
/// survives a restart.
#[tauri::command]
pub async fn player_set_visualizer(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine
        .shared()
        .visualizer_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('ui.visualizer', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn player_get_visualizer(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<bool> {
    Ok(engine
        .shared()
        .visualizer_enabled
        .load(std::sync::atomic::Ordering::Relaxed))
}

/// Update the playback speed multiplier. Pushes the new value live
/// to the audio engine so the active stream's resampler is rebuilt
/// against the new effective input rate, then persists to
/// `profile_setting['audio.playback_speed']`. Clamped to `[0.5, 2.0]`
/// on the engine side — values outside that range are saturated, not
/// rejected.
///
/// Pitch is NOT preserved (1.5× → ~ +7 semitones). Proper pitch-locked
/// time-stretching would need a phase vocoder; this is intentionally
/// the simple resampler-shift approach used by VLC's default playback
/// rate, since it costs ~zero CPU and works on every codec.
#[tauri::command]
pub async fn player_set_speed(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    value: f32,
) -> AppResult<()> {
    engine.send(AudioCmd::SetSpeed(value))?;
    if let Ok(pool) = state.require_profile_pool().await {
        let clamped = value.clamp(0.5, 2.0);
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.playback_speed', ?, 'float', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(clamped.to_string())
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn player_get_speed(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<f32> {
    Ok(engine.shared().playback_speed())
}

/// Toggle gapless playback. Pushes the new value live to the audio
/// engine and persists to `profile_setting['audio.gapless']`.
#[tauri::command]
pub async fn player_set_gapless(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine.send(AudioCmd::SetGapless(enabled))?;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.gapless', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(if enabled { "true" } else { "false" })
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

// ─── Equalizer ──────────────────────────────────────────────────────

/// Snapshot returned by `player_get_eq` so the Settings view can
/// hydrate the curve + bypass + preset dropdown without three
/// round-trips.
#[derive(Debug, serde::Serialize)]
pub struct EqSnapshot {
    pub enabled: bool,
    pub bands_db: Vec<f32>,
    pub band_freqs: Vec<f32>,
    pub max_gain_db: f32,
    pub presets: Vec<EqPresetEntry>,
}

#[derive(Debug, serde::Serialize)]
pub struct EqPresetEntry {
    pub key: String,
    pub gains: Vec<f32>,
}

#[tauri::command]
pub async fn player_get_eq(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<EqSnapshot> {
    let eq = &engine.shared().eq;
    let bands = eq.read_bands_db().to_vec();
    let presets = crate::audio::eq::PRESETS
        .iter()
        .map(|(k, g)| EqPresetEntry {
            key: (*k).to_string(),
            gains: g.to_vec(),
        })
        .collect();
    Ok(EqSnapshot {
        enabled: eq.is_enabled(),
        bands_db: bands,
        band_freqs: crate::audio::eq::BAND_FREQS.to_vec(),
        max_gain_db: crate::audio::eq::MAX_GAIN_DB,
        presets,
    })
}

#[tauri::command]
pub async fn player_set_eq_enabled(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    engine.shared().eq.set_enabled(enabled);
    persist_eq(&state, engine.shared()).await;
    Ok(())
}

#[tauri::command]
pub async fn player_set_eq_band(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    index: usize,
    gain_db: f32,
) -> AppResult<()> {
    engine.shared().eq.set_band_db(index, gain_db);
    persist_eq(&state, engine.shared()).await;
    Ok(())
}

#[tauri::command]
pub async fn player_set_eq_preset(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    preset_key: String,
) -> AppResult<()> {
    if let Some(gains) = crate::audio::eq::preset_gains(&preset_key) {
        engine.shared().eq.set_all_bands_db(&gains);
        persist_eq(&state, engine.shared()).await;
    }
    Ok(())
}

/// Persist the EQ snapshot under two keys: `audio.eq_enabled` (bool)
/// and `audio.eq_bands` (JSON array of f32). Splitting the bypass
/// from the bands lets the user toggle the EQ off without losing
/// their custom curve.
async fn persist_eq(state: &AppState, shared: &crate::audio::state::SharedPlayback) {
    let Ok(pool) = state.require_profile_pool().await else {
        return;
    };
    let now = chrono::Utc::now().timestamp_millis();
    let enabled = shared.eq.is_enabled();
    let bands = shared.eq.read_bands_db();
    let bands_json = serde_json::to_string(&bands.to_vec()).unwrap_or_else(|_| "[]".into());
    let _ = sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES ('audio.eq_enabled', ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(if enabled { "true" } else { "false" })
    .bind(now)
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "INSERT INTO profile_setting (key, value, value_type, updated_at)
         VALUES ('audio.eq_bands', ?, 'json', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(&bands_json)
    .bind(now)
    .execute(&pool)
    .await;
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
    let replaygain = shared
        .replaygain_enabled
        .load(std::sync::atomic::Ordering::Relaxed);
    let gapless = shared
        .gapless_enabled
        .load(std::sync::atomic::Ordering::Relaxed);

    let mut crossfade_ms: i64 = 0;
    if let Ok(pool) = state.require_profile_pool().await {
        if let Ok(Some(val)) = sqlx::query_scalar::<_, String>(
            "SELECT value FROM profile_setting WHERE key = 'audio.crossfade_ms'",
        )
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
        replaygain,
        gapless,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct AudioSettingsSnapshot {
    pub normalize: bool,
    pub mono: bool,
    pub crossfade_ms: i64,
    pub replaygain: bool,
    pub gapless: bool,
}

/// One row in the output-device picker that powers the PlayerBar
/// device menu. `id` and `name` are both the cpal device name (cpal
/// gives us no stable ID, and the UI matches selections by name on
/// the way back). `is_active` is true for the device the engine is
/// currently driving — `None` in `current_output_device` means
/// "tracking the OS default", which is selected if no row matches.
#[derive(Debug, serde::Serialize)]
pub struct OutputDeviceRow {
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub is_active: bool,
}

/// Enumerate every output device and flag which one the engine is
/// currently driving. The frontend re-fetches this whenever the
/// device menu opens so freshly-attached USB DACs / Bluetooth sinks
/// show up without an app restart.
///
/// On Linux ALSA, `output_devices()` probes every card and prints
/// scary `pcm_dmix` / `pcm_route` warnings for cards that are
/// enumerable but not openable (HDMI sinks with no monitor attached,
/// Bluetooth profiles in the wrong state, …). The errors don't
/// indicate a real problem on our side — they're cpal internals
/// leaking through ALSA's stderr. We still return whatever names
/// cpal hands us; clicking a broken device is handled by the engine
/// keeping the previous device alive.
#[tauri::command]
pub async fn player_list_output_devices(
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<Vec<OutputDeviceRow>> {
    let active = engine.current_output_device();
    // cpal enumeration walks the OS audio stack and can block for
    // ~100 ms+ on Linux. Push it onto the blocking pool so the tokio
    // runtime stays responsive — without this the WebView freezes
    // while the menu is opening.
    let devices = tokio::task::spawn_blocking(crate::audio::list_output_devices)
        .await
        .map_err(|e| AppError::Audio(format!("device enumeration task: {e}")))??;
    Ok(devices
        .into_iter()
        .map(|d| {
            // When the engine isn't pinned to a specific device
            // (`active = None`), it's tracking the OS default — so
            // show the OS-default row as active. Without this the
            // header reads "Active output" with nothing highlighted
            // on first open, which looks broken even though playback
            // works fine.
            let is_active = match active.as_deref() {
                Some(name) => d.id == name,
                None => d.is_default,
            };
            OutputDeviceRow {
                is_active,
                id: d.id,
                name: d.name,
                is_default: d.is_default,
            }
        })
        .collect())
}

/// Switch playback to a different cpal output device. `device_id =
/// None` follows the OS default (the engine's startup state). The
/// engine validates the new device, releases the old one, then
/// resumes the same track at the same position. If the new device
/// can't be opened (broken HDMI sink, busy exclusive output, …) the
/// engine keeps the previous device active and the error is
/// surfaced to the frontend.
///
/// Persisted in `profile_setting['audio.output_device']` so the
/// choice survives an app restart.
#[tauri::command]
pub async fn player_set_output_device(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    device_id: Option<String>,
) -> AppResult<()> {
    let engine_clone: Arc<AudioEngine> = engine.inner().clone();
    let device_for_engine = device_id.clone();
    // Same rationale as `player_list_output_devices`: opening /
    // tearing down a cpal stream blocks. Don't wedge the runtime.
    tokio::task::spawn_blocking(move || engine_clone.set_output_device(device_for_engine))
        .await
        .map_err(|e| AppError::Audio(format!("set output device task: {e}")))??;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        // Empty string represents "default" so the column stays
        // NOT NULL — the load path translates back to None.
        let stored = device_id.unwrap_or_default();
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.output_device', ?, 'string', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(stored)
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

/// Toggle WASAPI Exclusive Mode (Windows-only audiophile path).
///
/// Re-opens the active output stream in exclusive event-driven mode
/// at the device's mix-format sample rate. Bypasses the Windows audio
/// engine so no other app can mix in / DSP / resample our audio.
/// Falls back silently to cpal shared mode if init fails (device
/// busy, unsupported format, no exclusive support on the driver) —
/// see `audio/wasapi_exclusive.rs` for the contract.
///
/// No-op on Linux / macOS; the persisted setting is still written so
/// the value follows the user across platforms.
///
/// Persisted in `profile_setting['audio.wasapi_exclusive']`.
#[tauri::command]
pub async fn player_set_wasapi_exclusive(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    enabled: bool,
) -> AppResult<()> {
    let engine_clone: Arc<AudioEngine> = engine.inner().clone();
    // Same rationale as `player_set_output_device`: opening / tearing
    // down a WASAPI stream blocks for a few hundred ms.
    tokio::task::spawn_blocking(move || engine_clone.set_wasapi_exclusive(enabled))
        .await
        .map_err(|e| AppError::Audio(format!("set wasapi exclusive task: {e}")))??;
    if let Ok(pool) = state.require_profile_pool().await {
        let now = chrono::Utc::now().timestamp_millis();
        let stored = if enabled { "1" } else { "0" };
        let _ = sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES ('audio.wasapi_exclusive', ?, 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(stored)
        .bind(now)
        .execute(&pool)
        .await;
    }
    Ok(())
}

/// Read the current WASAPI Exclusive Mode state from the audio engine.
/// Always `false` on Linux / macOS. Used by the Settings card to
/// reflect whether the engine actually engaged exclusive mode (the
/// init could have silently fallen back to shared).
#[tauri::command]
pub fn player_get_wasapi_exclusive(engine: tauri::State<'_, Arc<AudioEngine>>) -> bool {
    engine.inner().wasapi_exclusive()
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

    let replay_gain_db = fetch_replay_gain_db(&pool, track.id).await;
    engine.send(AudioCmd::LoadAndPlay {
        path: pb,
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type,
        source_id,
        replay_gain_db,
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

/// Append a list of tracks to the **user queue** (the contiguous
/// 'manual' block sitting between the current track and the context
/// tail), without disturbing the current cursor. Mirrors Spotify's
/// "Add to queue" — manual picks fire after Now Playing and before
/// the album / playlist tail resumes, rather than being banished to
/// the very end of the list.
#[tauri::command]
pub async fn player_add_to_queue(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    track_ids: Vec<i64>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    queue::append_to_user_queue(&pool, &track_ids, None).await?;
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
        None => tracing::info!(queue_len, ?repeat, "player_next: queue exhausted, no-op"),
    }
    let Some(track) = next_opt else {
        return Ok(());
    };
    emit_track_changed(&app, &state.paths, &track, profile_id);
    emit_queue_changed(&app);
    let replay_gain_db = fetch_replay_gain_db(&pool, track.id).await;
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain_db,
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
    let replay_gain_db = fetch_replay_gain_db(&pool, track.id).await;
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain_db,
    })
}
