//! The resume point the next launch offers back (#624).

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::audio::AudioEngine;
use crate::queue;
use crate::state::AppState;

/// The track and position a resume point would name right now, or `None`
/// when there is nothing to come back to.
///
/// Radio and the remote queue use negative sentinels and nothing loaded is
/// `0`; neither has a `track` row for [`queue::restore_state`] to find.
pub(super) fn current_resume_point(app: &AppHandle) -> Option<(i64, u64)> {
    let engine = app.try_state::<Arc<AudioEngine>>()?;
    let track_id = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if track_id <= 0 {
        return None;
    }
    Some((track_id, engine.shared().current_position_ms()))
}

/// Persist where playback is, so the next launch can offer it back.
/// Returns whether the point reached the database.
///
/// Idempotent, so every caller can write without coordinating with the
/// others: the exit event, and the ten-second ticker spawned in `setup`.
pub(super) async fn write_resume_point(app: &AppHandle) -> bool {
    let Some((track_id, position_ms)) = current_resume_point(app) else {
        return false;
    };
    let state = app.state::<AppState>();
    // Pin the write to the profile that is playing, captured before the
    // await and without waiting for the lock: one we cannot take means a
    // switch is under way, and the profile being switched to must not
    // receive this track's resume point (#485).
    let profile_id = state
        .profile
        .try_read()
        .ok()
        .and_then(|active| active.as_ref().map(|p| p.profile_id));
    let Some(profile_id) = profile_id else {
        return false;
    };
    let Ok(pool) = state.require_profile_pool_for(Some(profile_id)).await else {
        return false;
    };
    if let Err(err) = queue::persist_resume_point(&pool, track_id, position_ms).await {
        tracing::warn!(%err, "resume point not persisted");
        return false;
    }
    true
}
