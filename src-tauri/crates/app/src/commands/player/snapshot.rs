//! The player and queue snapshots the frontend reads, and resuming the
//! last session.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::{
    audio::AudioEngine,
    error::AppResult,
    queue::{self},
    state::AppState,
};

use super::*;

/// Return the current player snapshot. Also resolves the "resume
/// track" on the very first call after app launch by reading
/// `player.last_track_id` / `player.last_position_ms`, so the
/// PlayerBar can show the last-played track in paused-at-position
/// state without auto-playing.
#[tauri::command]
pub async fn player_get_state(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<PlayerStateSnapshot> {
    let pool_result = state.require_profile_pool().await;
    let (shuffle, repeat_mode, current_track, resumed_position) = match pool_result {
        Ok(pool) => {
            let shuffle = queue::read_shuffle_mode(&pool).await;
            let repeat_mode = queue::read_repeat_mode(&pool).await;
            let profile_id = state.require_profile_id().await.ok();

            // Restores the persisted volume into the atomic shared with
            // the cpal callback (without which the knob jumps back to
            // 100 % on every launch), and every other audio setting.
            restore_profile_audio_settings(&pool, &engine).await;
            // Album grouping is persisted per profile like everything
            // above, and the decoder reads it to tell a record playing
            // through from a shuffled playlist (#587). Mirrored here
            // rather than left to the first toggle, which may never
            // come: a profile that starts up with album shuffle on
            // would otherwise get track gain until something touched
            // the setting.
            // The value this call already resolved for the snapshot,
            // not a second read of the same row: the mirror and what
            // the UI is told must describe one state, and two reads
            // could straddle a change.
            publish_shuffle_grouping(&engine, shuffle);
            // And the queue's own shape, for the same reason: a session
            // of whole records that survives a restart has to keep its
            // album gain across it, and the decoder cannot read the
            // setting itself (#647).
            publish_album_ordering(&engine, queue::read_album_ordered(&pool).await);
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
            } else if active_id < 0 {
                // A live Web Radio session owns the engine (negative
                // sentinel id, no library row). Don't surface the
                // persisted last-track here: a webview that mounts
                // mid-stream (the mini-player) would otherwise show that
                // stale library track instead of the radio. Returning
                // `None` lets the frontend take the radio-hydration path
                // via `get_current_radio_metadata`.
                (None, engine.shared().current_position_ms())
            } else {
                match queue::restore_state(&pool).await? {
                    Some((t, ms)) => (Some(t), ms),
                    None => (None, 0),
                }
            };
            let payload = restored.map(|t| queue_track_to_payload(&state, t, profile_id));
            (shuffle, repeat_mode, payload, position)
        }
        Err(_) => (queue::ShuffleMode::Off, queue::RepeatMode::Off, None, 0),
    };
    let mut snapshot = PlayerStateSnapshot::from_shared(
        engine.shared(),
        shuffle,
        repeat_mode,
        current_track,
        engine.current_output_is_dop(),
        engine.exclusive_output(),
        engine.output_mode(),
    );
    // When the engine is Idle but we resolved a resume point, use the
    // persisted position instead of the (zero) live counter.
    if snapshot.state == "idle" && snapshot.position_ms == 0 {
        snapshot.position_ms = resumed_position;
    }

    // Hand that restored track to the OS media overlay (#609). Until now
    // the overlay showed no WaveFlow session at all before something
    // played, so its Play button — which starts the resume point since
    // this release — had nothing to appear on after a launch.
    //
    // Only while the engine holds nothing: once a track is loaded the
    // decoder's own transitions own the overlay, and this command also
    // runs on profile switch and re-hydration. Paused, at the persisted
    // position, and nothing is started — the session describes exactly
    // what Play would resume.
    if snapshot.state == "idle" {
        if let (Some(track), Some(controls)) = (
            snapshot.current_track.as_ref(),
            app.try_state::<crate::media_controls::MediaControlsHandle>(),
        ) {
            // Re-read the engine immediately before publishing. This
            // command has just spent its time in the database, and a
            // surface outside the window — a media key, MPD — can have
            // started playback since the snapshot was taken. Laying a
            // stale "restored track, paused" over a session that is
            // already playing is worse than publishing nothing at all.
            if engine.shared().state() == crate::audio::PlayerState::Idle {
                controls.update_metadata(
                    track.title.clone(),
                    track.artist_name.clone(),
                    track.album_title.clone(),
                    track.artwork_path.clone(),
                    track.duration_ms,
                );
                controls.update_playback(crate::audio::PlayerState::Paused, snapshot.position_ms);
            }
        }
    }

    Ok(snapshot)
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
    // Bounded to the queue: the stored cursor can outlive a shrink, and a
    // raw one would mark no row and number the track past the end.
    let current_index = queue::current_index(&pool).await;

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
            .fetch_optional(&*pool)
            .await?;

    Ok(PlayerQueueSnapshot {
        current_index,
        items: payload_items,
        source_type,
    })
}

/// Resume playback from the persisted last-track + position. Used by
/// the frontend when the user hits Play from the idle state without
/// having clicked a specific track yet. The tray and the taskbar's
/// play/pause share the same sequence through
/// [`crate::player_actions::resume_last`].
#[tauri::command]
pub async fn player_resume_last(app: AppHandle) -> AppResult<()> {
    crate::player_actions::resume_last(&app).await
}
