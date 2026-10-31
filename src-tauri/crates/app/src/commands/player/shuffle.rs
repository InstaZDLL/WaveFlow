//! Shuffle and repeat: the commands, and the grouping and album ordering
//! published to the engine.

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::{
    audio::{next_track::edit_queue, AudioEngine},
    error::AppResult,
    queue::{self},
    state::AppState,
};

use super::*;

/// Put a shuffle mode in force: persist it, reorder the queue to
/// match, and mirror the grouping into the engine.
///
/// The single funnel on purpose. Three surfaces change shuffle — the
/// player controls, an MPD client's `random`, and the reshuffle
/// [`player_play_tracks`] does after filling a queue — and each of
/// them used to spell out its own persist-then-reorder pair. The
/// mirror is the part that most wants a single home: the decoder
/// thread reads it to decide whether a record is playing through
/// (#587), and a surface that forgot it would leave ReplayGain
/// applying track gain to an album-ordered queue with nothing to
/// show for it.
pub(crate) async fn set_shuffle_mode(
    pool: &sqlx::SqlitePool,
    engine: &AudioEngine,
    mode: queue::ShuffleMode,
) -> AppResult<()> {
    // Reorder first, and only then record it. The reorder is the part
    // that can fail; doing it first means a failure leaves the
    // persisted mode, the queue order and the engine's mirror all
    // still describing the state the user was already in, with nothing
    // to roll back. `apply_shuffle_mode` takes the mode as an argument
    // and reads nothing persisted, so there is no reason for the write
    // to come first.
    queue::apply_shuffle_mode(pool, mode).await?;
    queue::write_shuffle_mode(pool, mode).await?;
    publish_shuffle_grouping(engine, mode);
    Ok(())
}

/// Mirror just the mode, without touching the queue or the database —
/// for the boot-time and profile-switch restore, where the persisted
/// state is already the truth.
pub(crate) fn publish_shuffle_grouping(engine: &AudioEngine, mode: queue::ShuffleMode) {
    engine
        .shared()
        .shuffle_mode_bits
        .store(mode.as_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The same mirror for the other half of the question: whether the queue
/// as it stands is whole records in their own order (#647).
///
/// Separate from the shuffle mirror because the two answer different
/// questions and change at different moments — shuffle when the user
/// presses the button, this when the queue is replaced.
pub(crate) fn publish_album_ordering(engine: &AudioEngine, album_ordered: bool) {
    engine
        .shared()
        .album_ordered_queue
        .store(album_ordered, std::sync::atomic::Ordering::Relaxed);
}

/// Whether replacing the queue from this source means playing whole
/// records — the signal `queue_item.source_type` cannot carry, because
/// each of its values already means something to `play_event` and an
/// album-mode radio may not claim to be an `'album'` (#647).
///
/// Two halves, because the two generators know it at different times.
/// Mood Radio knows as it answers, so its command hands the flag back
/// and the caller declares it here. A Daily Mix does not: it is a stored
/// playlist, and playing it a week later goes through the ordinary
/// playlist path with nothing to distinguish it — so the answer is read
/// back out of the rule the generator wrote.
///
/// Anything else is `false`, which is also what an unreadable or absent
/// rule reads as: album gain is the answer that changes what the
/// listener hears, so it is the one that has to be established.
pub(super) async fn queue_is_album_ordered(
    pool: &sqlx::SqlitePool,
    source_type: &str,
    source_id: Option<i64>,
    declared: Option<bool>,
) -> bool {
    if let Some(declared) = declared {
        return declared;
    }
    if source_type != "playlist" {
        return false;
    }
    let Some(playlist_id) = source_id else {
        return false;
    };
    let rules: Option<String> = sqlx::query_scalar(
        "SELECT smart_rules FROM playlist WHERE id = ? AND is_smart = 1 AND smart_rules IS NOT NULL",
    )
    .bind(playlist_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(rules) = rules else {
        return false;
    };
    matches!(
        serde_json::from_str::<waveflow_core::smart_playlists::SmartPlaylistRules>(&rules),
        Ok(
            waveflow_core::smart_playlists::SmartPlaylistRules::DailyMix {
                album_mode: true,
                ..
            }
        )
    )
}

/// Emit an empty `player:queue-changed` signal. The frontend uses
/// this as "refetch the queue" — payload is intentionally empty so
/// the event bus doesn't carry the full 100+ track list.
pub(crate) fn emit_queue_changed(app: &AppHandle) {
    let _ = app.emit("player:queue-changed", ());
}

/// Flip shuffle on or off. Returns the mode now in force — not a
/// boolean, because turning shuffle *on* restores whichever grouping
/// the listener last picked, and only the backend knows which that
/// was. When turning on,
/// randomizes the existing queue in place (keeping the current track
/// in slot 0). When turning off, restores the pre-shuffle order.
#[tauri::command]
pub async fn player_toggle_shuffle(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<String> {
    let pool = state.require_profile_pool().await?;
    // The mode is read under the lock too: two toggles at once (two
    // windows) would otherwise both flip from the same state, and the
    // second would be lost.
    let next = edit_queue(engine.lock_publish(), async {
        let next = if queue::read_shuffle(&pool).await {
            queue::ShuffleMode::Off
        } else {
            // On, using whichever grouping the listener last picked — a
            // preference for whole records should not be forgotten every
            // time shuffle is switched off and back on.
            queue::read_shuffle_grouping_preference(&pool).await
        };
        set_shuffle_mode(&pool, &engine, next).await.map(|()| next)
    })
    .await?;
    // Queue content changed (reordered in place) — tell the panel, and
    // the decoder if it has a next track prepared.
    emit_queue_changed(&app);
    crate::audio::next_track::revise_soon(&app);
    // …and the other windows. Each one runs its own `PlayerContext` with
    // its own optimistic toggle state, so without this the mini-player
    // and the main window disagree about shuffle while sharing one queue
    // (#523). Cheap: two setting reads, only on an explicit toggle.
    emit_options_changed(&app, &pool).await;
    Ok(next.as_str().to_string())
}

/// Choose how shuffle groups the queue: `"off"`, `"tracks"` or
/// `"albums"`. Returns the mode in force.
///
/// Distinct from [`player_toggle_shuffle`], which is the on/off any
/// "Shuffle" button on an album or a playlist means. This one is the
/// player control that cycles through the three, and it is also the
/// only way to switch grouping without passing through off — which
/// would otherwise re-shuffle the queue twice.
///
/// An unrecognised value reads as `"off"` rather than failing: the
/// worst outcome of a frontend/backend disagreement here should be an
/// unshuffled queue, not a dead button.
#[tauri::command]
pub async fn player_set_shuffle_mode(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    mode: String,
) -> AppResult<String> {
    let pool = state.require_profile_pool().await?;
    let mode = queue::ShuffleMode::from_str(&mode);
    edit_queue(
        engine.lock_publish(),
        set_shuffle_mode(&pool, &engine, mode),
    )
    .await?;
    emit_queue_changed(&app);
    crate::audio::next_track::revise_soon(&app);
    emit_options_changed(&app, &pool).await;
    Ok(mode.as_str().to_string())
}

/// Cycle `off → all → one → off` and return the new mode as a string.
#[tauri::command]
pub async fn player_cycle_repeat(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
) -> AppResult<String> {
    let pool = state.require_profile_pool().await?;
    // Read and written under the publish lock, which a prefetch holds
    // while it reads the mode: it never sends the next track by the mode
    // this replaces, and two cycles at once (two windows) both count.
    let next = edit_queue(engine.lock_publish(), async {
        let next = match queue::read_repeat_mode(&pool).await {
            queue::RepeatMode::Off => queue::RepeatMode::All,
            queue::RepeatMode::All => queue::RepeatMode::One,
            queue::RepeatMode::One => queue::RepeatMode::Off,
        };
        queue::write_repeat_mode(&pool, next).await.map(|()| next)
    })
    .await?;
    // Repeat decides what follows the last track.
    crate::audio::next_track::revise_soon(&app);
    // Same reason as `player_toggle_shuffle` (#523): the caller updates
    // its own state from the return value, the *other* windows only
    // learn about it here.
    emit_options_changed(&app, &pool).await;
    Ok(next.as_str().to_string())
}
