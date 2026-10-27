//! Pause, resume, stop, seek, pause after the track, the A-B loop and the
//! output test tone.

use std::sync::Arc;

use tauri::{AppHandle, Emitter, Manager};

use crate::{
    audio::{engine::AudioCmd, AudioEngine},
    error::AppResult,
};

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

/// Play the output test tone from Settings: a beep on the left, on the
/// right, then on both, at the output's own format. Progress arrives as
/// `player:test-tone` events. A track in progress is set aside and picked
/// back up by Play.
#[tauri::command]
pub async fn player_play_test_tone(engine: tauri::State<'_, Arc<AudioEngine>>) -> AppResult<()> {
    engine.play_test_tone()
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
