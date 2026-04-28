//! Analytics + auto-advance task.
//!
//! Runs inside the tokio runtime (spawned from [`AudioEngine::new`])
//! and consumes messages sent by the decoder thread whenever a track
//! ends. Responsibilities:
//!
//! 1. Write a `play_event` row for the completed track (for the
//!    upcoming "Récemment joués" / stats views).
//! 2. Advance the persistent queue cursor per the current repeat /
//!    shuffle settings.
//! 3. Self-send an `AudioCmd::LoadAndPlay` back to the decoder thread
//!    so playback continues without a frontend round-trip.
//!
//! Keeping this logic in a tokio task (rather than the decoder thread
//! itself) means the real-time audio path never blocks on SQLite.

use crossbeam_channel::Sender as CrossbeamSender;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    audio::engine::AudioCmd,
    commands::player::{emit_queue_changed, emit_track_changed},
    queue::{self, Direction, QueueTrack},
    state::AppState,
};

/// One-way messages the decoder thread sends on a tokio unbounded
/// channel. Synchronous `send` is fine because `UnboundedSender::send`
/// is non-blocking and doesn't need a runtime handle.
#[derive(Debug, Clone)]
pub enum AnalyticsMsg {
    /// A track just finished decoding naturally (EOF reached).
    /// Writes a `play_event` row AND triggers auto-advance to the
    /// next track in the queue.
    TrackEnded {
        track_id: i64,
        completed: bool,
        listened_ms: u64,
        source_type: String,
        source_id: Option<i64>,
    },
    /// A track was interrupted by the user (Next, jump, new load)
    /// BUT had been listened to long enough to count as a "real"
    /// play (≥ 15 s). Writes a `play_event` row, does NOT trigger
    /// auto-advance — that path is reserved for natural ends.
    TrackListened {
        track_id: i64,
        listened_ms: u64,
        source_type: String,
        source_id: Option<i64>,
    },
    /// Sent by the decoder when it's approaching the end of the
    /// current track and crossfade is enabled. Triggers a
    /// `peek_next` and a `SetNextTrack` reply so the decoder can
    /// open the second decoder before the fade window starts.
    PrefetchNext,
    /// Sent by the decoder right after the crossfade swap has
    /// happened and the second decoder is now the primary. Writes a
    /// `play_event` for the just-faded-out track AND advances the
    /// queue cursor (without firing a new LoadAndPlay — the new
    /// track is already playing).
    CrossfadeStarted {
        finished_track_id: i64,
        finished_listened_ms: u64,
        finished_source_type: String,
        finished_source_id: Option<i64>,
    },
}

/// Top-level task body. Loops forever, exiting only when the sender
/// side is dropped (engine teardown).
pub async fn analytics_task(
    mut rx: UnboundedReceiver<AnalyticsMsg>,
    cmd_tx: CrossbeamSender<AudioCmd>,
    app: AppHandle,
) {
    while let Some(msg) = rx.recv().await {
        if let Err(err) = handle_message(&msg, &cmd_tx, &app).await {
            tracing::warn!(?err, ?msg, "analytics task error");
        }
    }
    tracing::debug!("analytics task exiting");
}

/// Handle one message. Errors are returned to the caller so they can
/// be logged but never crash the task.
async fn handle_message(
    msg: &AnalyticsMsg,
    cmd_tx: &CrossbeamSender<AudioCmd>,
    app: &AppHandle,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let pool = state
        .require_profile_pool()
        .await
        .map_err(|e| format!("profile pool: {e}"))?;

    // Both variants write a play_event row. Only `TrackEnded` also
    // triggers auto-advance — `TrackListened` is the user-skipped
    // case where we still credit the listen but don't touch the
    // queue cursor.
    match msg {
        AnalyticsMsg::TrackEnded {
            track_id,
            completed,
            listened_ms,
            source_type,
            source_id,
        } => {
            insert_play_event(
                &pool,
                *track_id,
                *listened_ms,
                *completed,
                source_type,
                *source_id,
            )
            .await;

            // Auto-advance.
            let repeat = queue::read_repeat_mode(&pool).await;
            let next: Option<QueueTrack> = queue::advance(&pool, Direction::Next, repeat)
                .await
                .map_err(|e| format!("advance: {e}"))?;
            if let Some(track) = next {
                let profile_id = state.require_profile_id().await.ok();
                emit_track_changed(app, &state.paths, &track, profile_id);
                emit_queue_changed(app);
                let replay_gain_db =
                    crate::commands::player::fetch_replay_gain_db(&pool, track.id).await;
                let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                    path: track.as_path(),
                    start_ms: 0,
                    track_id: track.id,
                    duration_ms: track.duration_ms.max(0) as u64,
                    source_type: source_type.clone(),
                    source_id: *source_id,
                    replay_gain_db,
                });
            }
        }
        AnalyticsMsg::TrackListened {
            track_id,
            listened_ms,
            source_type,
            source_id,
        } => {
            // User skipped but listened long enough — credit the
            // play without advancing the queue (the user already
            // picked what's next by clicking).
            insert_play_event(
                &pool,
                *track_id,
                *listened_ms,
                false,
                source_type,
                *source_id,
            )
            .await;
        }
        AnalyticsMsg::PrefetchNext => {
            // Look up what would be played next without bumping the
            // cursor (the cursor is bumped only when the crossfade
            // actually starts, via CrossfadeStarted).
            let repeat = queue::read_repeat_mode(&pool).await;
            let next: Option<QueueTrack> = queue::peek_next(&pool, repeat)
                .await
                .map_err(|e| format!("peek_next: {e}"))?;
            if let Some(track) = next {
                let replay_gain_db =
                    crate::commands::player::fetch_replay_gain_db(&pool, track.id).await;
                let _ = cmd_tx.send(AudioCmd::SetNextTrack {
                    path: track.as_path(),
                    track_id: track.id,
                    duration_ms: track.duration_ms.max(0) as u64,
                    // The next track inherits the same source as the
                    // current one for analytics — auto-advance never
                    // crosses a source boundary in this app.
                    source_type: "manual".into(),
                    source_id: None,
                    replay_gain_db,
                });
            }
        }
        AnalyticsMsg::CrossfadeStarted {
            finished_track_id,
            finished_listened_ms,
            finished_source_type,
            finished_source_id,
        } => {
            // Credit the just-finished track (treated as completed
            // since the crossfade window only starts at the tail).
            insert_play_event(
                &pool,
                *finished_track_id,
                *finished_listened_ms,
                true,
                finished_source_type,
                *finished_source_id,
            )
            .await;

            // Bump the cursor so the QueuePanel reflects the new
            // current track. The decoder is already playing it — do
            // NOT send LoadAndPlay.
            let repeat = queue::read_repeat_mode(&pool).await;
            let advanced: Option<QueueTrack> = queue::advance(&pool, Direction::Next, repeat)
                .await
                .map_err(|e| format!("advance after crossfade: {e}"))?;
            if let Some(track) = advanced {
                let profile_id = state.require_profile_id().await.ok();
                emit_track_changed(app, &state.paths, &track, profile_id);
                emit_queue_changed(app);
            }
        }
    }

    Ok(())
}

/// Insert a row into `play_event`. Best-effort: errors are logged
/// and swallowed so analytics never blocks playback.
async fn insert_play_event(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    listened_ms: u64,
    completed: bool,
    source_type: &str,
    source_id: Option<i64>,
) {
    let now = chrono::Utc::now().timestamp_millis();
    tracing::info!(
        track_id,
        listened_ms,
        completed,
        source_type,
        source_id,
        "insert play_event"
    );
    if let Err(e) = sqlx::query(
        "INSERT INTO play_event
            (track_id, played_at, listened_ms, completed, skipped,
             source_type, source_id)
         VALUES (?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(track_id)
    .bind(now)
    .bind(listened_ms as i64)
    .bind(if completed { 1 } else { 0 })
    .bind(source_type)
    .bind(source_id)
    .execute(pool)
    .await
    {
        tracing::warn!(?e, "failed to insert play_event");
        return;
    }

    // Last.fm scrobble enqueue. We check eligibility *before* hitting
    // the queue so a session of 12 s previews never floods the
    // `scrobble_queue` table with rows the worker would then have to
    // discard. The worker itself is responsible for actually POSTing
    // to Last.fm — keeping the analytics task off the network.
    let duration_ms: Option<i64> = match sqlx::query_scalar(
        "SELECT duration_ms FROM track WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(?e, "scrobble eligibility lookup failed");
            return;
        }
    };
    let Some(duration_ms) = duration_ms else { return };
    if crate::scrobbler::is_eligible(duration_ms, listened_ms as i64) {
        if let Err(e) =
            crate::scrobbler::enqueue(pool, track_id, now, listened_ms as i64).await
        {
            tracing::warn!(?e, track_id, "failed to enqueue scrobble");
        }
    }
}
