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
    queue::{self, Direction, QueueTrack},
    state::AppState,
};

/// One-way messages the decoder thread sends on a tokio unbounded
/// channel. Synchronous `send` is fine because `UnboundedSender::send`
/// is non-blocking and doesn't need a runtime handle.
#[derive(Debug, Clone)]
pub enum AnalyticsMsg {
    /// A track just finished decoding. `listened_ms` is the final
    /// position at EOF, `completed` is true when the play reached
    /// within 2s of the declared duration.
    TrackEnded {
        track_id: i64,
        completed: bool,
        listened_ms: u64,
        source_type: String,
        source_id: Option<i64>,
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

    match msg {
        AnalyticsMsg::TrackEnded {
            track_id,
            completed,
            listened_ms,
            source_type,
            source_id,
        } => {
            // 1. Persist the listen event. Best-effort: analytics is
            //    never allowed to abort playback.
            let now = chrono::Utc::now().timestamp_millis();
            if let Err(e) = sqlx::query(
                "INSERT INTO play_event
                    (track_id, played_at, listened_ms, completed, skipped,
                     source_type, source_id)
                 VALUES (?, ?, ?, ?, 0, ?, ?)",
            )
            .bind(track_id)
            .bind(now)
            .bind(*listened_ms as i64)
            .bind(if *completed { 1 } else { 0 })
            .bind(source_type)
            .bind(source_id)
            .execute(&pool)
            .await
            {
                tracing::warn!(?e, "failed to insert play_event");
            }

            // 2. Advance the queue cursor per the current repeat
            //    mode. Shuffle was already baked into the queue
            //    ordering by `queue::shuffle`, so advance just
            //    walks forward.
            let repeat = queue::read_repeat_mode(&pool).await;
            let next: Option<QueueTrack> = queue::advance(&pool, Direction::Next, repeat)
                .await
                .map_err(|e| format!("advance: {e}"))?;

            // 3. Self-send a LoadAndPlay if there's a next track,
            //    otherwise the decoder stays in Ended state until the
            //    user does something.
            if let Some(track) = next {
                let _ = cmd_tx.send(AudioCmd::LoadAndPlay {
                    path: track.as_path(),
                    start_ms: 0,
                    track_id: track.id,
                    duration_ms: track.duration_ms.max(0) as u64,
                    source_type: source_type.clone(),
                    source_id: *source_id,
                });
            }
        }
    }

    Ok(())
}
