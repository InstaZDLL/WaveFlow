//! Player actions shared by every *non-frontend* control surface.
//!
//! The tray menu ([`crate::lib`]), the OS media keys
//! ([`crate::media_controls`]) and the MPD server ([`crate::mpd`]) all
//! need the same "advance the queue, tell the UI, hand the track to the
//! decoder" sequence that `commands::player` performs for the frontend.
//!
//! Before this module existed the sequence was copy-pasted twice —
//! `media_controls::spawn_next` was literally commented "Mirror of
//! `lib.rs::spawn_next`", and the two had drifted only in their log
//! strings. Adding MPD would have made it three copies, each free to
//! forget an emit and desync the UI, so the logic lives here once.
//!
//! Everything is `async` and awaits rather than spawning: callers on a
//! sync callback thread (souvlaki, the tray) wrap these in
//! `tauri::async_runtime::spawn` themselves, and callers already inside
//! a task (MPD) just await, which lets them report success back to
//! their client.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::{
    audio::{engine::AudioCmd, AudioEngine},
    commands,
    queue::{self, Direction, QueueTrack},
    state::AppState,
};

/// Seek-to-start threshold for "previous", in milliseconds.
///
/// Past this point into a track, "previous" restarts the current track
/// instead of stepping back one — the rule Spotify, Apple Music and
/// every hardware transport share, so muscle memory carries over.
const PREVIOUS_RESTART_THRESHOLD_MS: u64 = 3000;

/// Outcome of a queue-moving action, so callers that must answer a
/// client (MPD) can tell "done" from "there was nothing to do".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moved {
    /// A new track was handed to the decoder.
    Track,
    /// The current track was restarted from 0 (the "previous" rule).
    Restarted,
    /// Nothing to move to — empty queue, or at an edge with repeat off.
    Nothing,
}

/// Hand a track to the decoder and tell every listener about it.
///
/// This is the tail every action shares. `emit_track_changed` feeds the
/// UI, the OS overlay and Discord; `emit_queue_changed` makes the queue
/// panel re-read. Skipping either leaves a surface showing the previous
/// track, which is precisely the class of bug that duplicating this
/// sequence kept producing.
async fn load_and_play(
    app: &AppHandle,
    pool: &sqlx::SqlitePool,
    track: QueueTrack,
    profile_id: Option<i64>,
) {
    let engine = app.state::<Arc<AudioEngine>>();
    commands::player::emit_track_changed(app, &app.state::<AppState>().paths, &track, profile_id);
    commands::player::emit_queue_changed(app);
    let replay_gain_db = commands::player::fetch_replay_gain_db(pool, track.id).await;
    let _ = engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain_db,
    });
}

/// Step the queue one entry in `direction` and play what lands.
///
/// `label` only tags the log lines so a failure can be traced back to
/// the surface that triggered it (`"tray"`, `"mpd"`, …).
pub async fn step(app: &AppHandle, direction: Direction, label: &str) -> Moved {
    let state = app.state::<AppState>();
    // One atomic snapshot: the pool and the profile id come from the same
    // read guard, so a profile switch can't slip a mismatched pair between
    // two separate `require_*` calls.
    let (pool, profile_id) = match state.require_profile_snapshot().await {
        Ok(pair) => pair,
        Err(err) => {
            tracing::warn!(%err, surface = label, "player action: no profile pool");
            return Moved::Nothing;
        }
    };
    let repeat = queue::read_repeat_mode(&pool).await;
    let track = match queue::advance(&pool, direction, repeat).await {
        Ok(Some(track)) => track,
        Ok(None) => return Moved::Nothing,
        Err(err) => {
            tracing::warn!(%err, surface = label, "player action: advance failed");
            return Moved::Nothing;
        }
    };
    load_and_play(app, &pool, track, Some(profile_id)).await;
    Moved::Track
}

/// Advance to the next queue entry.
pub async fn next(app: &AppHandle, label: &str) -> Moved {
    step(app, Direction::Next, label).await
}

/// Go back, applying the [`PREVIOUS_RESTART_THRESHOLD_MS`] rule.
pub async fn previous(app: &AppHandle, label: &str) -> Moved {
    let engine = app.state::<Arc<AudioEngine>>();
    if engine.shared().current_position_ms() > PREVIOUS_RESTART_THRESHOLD_MS {
        let _ = engine.send(AudioCmd::Seek(0));
        return Moved::Restarted;
    }
    step(app, Direction::Previous, label).await
}

/// Jump to an absolute queue position and play it.
///
/// Mirrors `commands::player::player_jump_to_index`, which the frontend
/// uses for a double-click in the queue panel.
/// Jump to an absolute queue position and play it, driving an
/// already-held profile snapshot.
///
/// Mirrors `commands::player::player_jump_to_index` (which the frontend uses
/// for a double-click in the queue panel). The MPD `play` / `playid` / `seek`
/// / `seekid` handlers validate a position against the queue and then call
/// this with the SAME snapshot, so no reacquire can slip a different profile
/// in between the check and the jump.
pub async fn play_at_index_with(
    app: &AppHandle,
    pool: &sqlx::SqlitePool,
    profile_id: i64,
    position: i64,
    label: &str,
) -> Moved {
    let track = match queue::jump_to(pool, position).await {
        Ok(Some(track)) => track,
        Ok(None) => return Moved::Nothing,
        Err(err) => {
            tracing::warn!(%err, surface = label, "player action: jump_to failed");
            return Moved::Nothing;
        }
    };
    load_and_play(app, pool, track, Some(profile_id)).await;
    Moved::Track
}
