//! Player actions shared by every *non-frontend* control surface.
//!
//! The tray menu ([`crate::lib`]), the OS media keys
//! ([`crate::media_controls`]), the taskbar thumbnail buttons on Windows
//! (`crate::taskbar_buttons`) and the MPD server ([`crate::mpd`]) all
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
//! their client. [`play`] and [`toggle_play_pause`] are the exceptions:
//! a menu item, a window message or an OS-overlay callback calls them,
//! so they are sync and spawn the one branch that needs the database.

use std::sync::Arc;

use tauri::{AppHandle, Manager};

use crate::{
    audio::{engine::AudioCmd, AudioEngine, PlayerState},
    commands,
    error::{AppError, AppResult},
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

/// Start playing: resume a paused track, or load the resume point when
/// nothing is open. Never pauses.
///
/// The counterpart of [`toggle_play_pause`] for surfaces that expose a
/// *separate* Play button — the OS media overlay, MPD's `play` and
/// `pause 0` — where a toggle would pause a playing track instead of
/// doing nothing.
///
/// Those surfaces used to send `AudioCmd::Resume` straight to the engine.
/// The decoder only handles it inside the pause loop, so with nothing
/// open it was dropped and Play did nothing at all: after a launch, and
/// at the end of the queue (#609).
pub fn play(app: &AppHandle, label: &str) {
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return;
    };
    match engine.shared().state() {
        // Already playing, or a track is already on its way: Play is a
        // no-op here, not a restart.
        PlayerState::Playing | PlayerState::Loading => {}
        PlayerState::Paused => {
            if let Err(err) = engine.send(AudioCmd::Resume) {
                tracing::warn!(%err, "{label} play: send failed");
            }
        }
        // No track open: `AudioCmd::Resume` would be dropped, so load the
        // persisted resume point instead — what the in-app Play button
        // does through `player_resume_last`.
        PlayerState::Idle | PlayerState::Ended => {
            let app = app.clone();
            let label = label.to_owned();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = resume_last(&app).await {
                    tracing::warn!(%err, "{label} play: resume failed");
                }
            });
        }
    }
}

/// Pause when playing, resume when paused, otherwise start the resume
/// point — the rule the in-app Play button follows.
///
/// Reads the engine's state (an atomic) at click time, so a surface can
/// offer one "play / pause" control instead of two stateful ones it would
/// have to keep in sync.
///
/// `AudioCmd::Resume` only reaches a paused track. From `Idle` or `Ended`
/// the decoder has no track open and drops it, which left this control
/// dead after launch and at the end of the queue; those states load the
/// persisted resume point through [`resume_last`] instead. `Loading` is
/// left alone: a track is already on its way.
pub fn toggle_play_pause(app: &AppHandle, label: &str) {
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return;
    };
    let cmd = match engine.shared().state() {
        PlayerState::Playing => AudioCmd::Pause,
        PlayerState::Paused => AudioCmd::Resume,
        PlayerState::Idle | PlayerState::Ended => {
            let app = app.clone();
            let label = label.to_owned();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = resume_last(&app).await {
                    tracing::warn!(%err, "{label} play_pause: resume failed");
                }
            });
            return;
        }
        PlayerState::Loading => return,
    };
    if let Err(err) = engine.send(cmd) {
        tracing::warn!(%err, "{label} play_pause: send failed");
    }
}

/// Load the persisted last track at its saved position and play it.
///
/// Behind both the in-app Play button from idle
/// (`commands::player::player_resume_last`) and [`toggle_play_pause`].
pub async fn resume_last(app: &AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let engine = app.state::<Arc<AudioEngine>>();
    // One resume at a time (#609). Two Play events landing together — a
    // double tap on the OS overlay, a client sending `play` twice — both
    // read `Idle` and both land here, and each awaits the database below
    // before sending its `LoadAndPlay`, so the second would restart the
    // track the first just started. Guarding here rather than in `play`
    // covers every caller: the tray, the taskbar buttons and the in-app
    // button through `player_resume_last`.
    let Some(_resume) = engine.begin_resume() else {
        tracing::debug!("resume already in flight; ignoring this play");
        return Ok(());
    };
    // One lock for both: two awaits could straddle a profile switch and
    // pair one profile's resume point with the other's id.
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let Some((track, position_ms)) = queue::restore_state(&pool).await? else {
        return Err(AppError::Other("no resume point available".into()));
    };
    commands::player::emit_track_changed(app, &state.paths, &track, Some(profile_id));
    let replay_gain = commands::player::fetch_replay_gain(&pool, track.id).await;
    engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: position_ms,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain,
    })
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
    let replay_gain = commands::player::fetch_replay_gain(pool, track.id).await;
    let _ = engine.send(AudioCmd::LoadAndPlay {
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain,
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
    #[cfg(feature = "sync_v2")]
    if let Some(moved) = try_remote_advance(app, Direction::Next, label).await {
        return moved;
    }
    step(app, Direction::Next, label).await
}

/// Go back, applying the [`PREVIOUS_RESTART_THRESHOLD_MS`] rule.
pub async fn previous(app: &AppHandle, label: &str) -> Moved {
    let engine = app.state::<Arc<AudioEngine>>();
    if engine.shared().current_position_ms() > PREVIOUS_RESTART_THRESHOLD_MS {
        let _ = engine.send(AudioCmd::Seek(0));
        return Moved::Restarted;
    }
    #[cfg(feature = "sync_v2")]
    if let Some(moved) = try_remote_advance(app, Direction::Previous, label).await {
        return moved;
    }
    step(app, Direction::Previous, label).await
}

/// When a remote play queue is active, advance it instead of the local
/// queue. Returns `Some(Moved)` when it handled the step, `None` to fall
/// through to the local `queue_item` path.
#[cfg(feature = "sync_v2")]
async fn try_remote_advance(app: &AppHandle, direction: Direction, label: &str) -> Option<Moved> {
    if !app.state::<AppState>().remote_playback.is_active() {
        return None;
    }
    match crate::remote::playback::advance(app, direction).await {
        // A track was actually loaded.
        Ok(true) => Some(Moved::Track),
        // End of queue (repeat off) — the session stopped, nothing new plays.
        Ok(false) => Some(Moved::Nothing),
        Err(err) => {
            tracing::warn!(%err, surface = label, "remote player action: advance failed");
            Some(Moved::Nothing)
        }
    }
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
