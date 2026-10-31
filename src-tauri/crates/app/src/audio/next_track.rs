//! The track after the one playing, handed to the decoder for the gapless
//! or crossfade transition into it.
//!
//! The decoder asks for it near the end of a track (`PrefetchNext`), and
//! the answer is the queue's next entry with the transition's hints. The
//! queue can change after that — "Play next", a reorder, a shuffle, a
//! repeat change, an MPD client clearing it — and the transition prepared
//! for the old next would then play a track the queue no longer names,
//! while the cursor moved to the one it does. So while the decoder holds
//! a request ([`SharedPlayback::next_wanted`]), every queue change sends
//! the answer again ([`revise`]): the queue's next track now, or
//! `ClearNextTrack` when it has none. The decoder keeps the stream it has
//! when the track is the same, opens the new one when it is not, and
//! drops both once a fade has started ([`super::decoder`]'s `judge_next`).
//!
//! Both answers are made under the engine's publish lock, the lock every
//! producer that moves the cursor holds while it claims and sends its
//! load: an answer is worked out from a queue no producer is rewriting,
//! and reaches the decoder after any load sent before it. Each one names
//! the track it follows (`after`), and the decoder drops it once that
//! track is no longer the one playing, so it never sets up a transition
//! out of a track it was not prepared for.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

use super::engine::AudioCmd;
use super::state::SharedPlayback;
use super::AudioEngine;
use crate::error::AppResult;
use crate::queue::{self, QueueTrack, RepeatMode};
use crate::state::AppState;

/// How long before the earliest start of an AutoMix fade the next track
/// is asked for: time to open it and plan the transition, and the half
/// second the decoder enters the mix ahead of its start
/// ([`super::crossfade::AUTOMIX_ENTER_US`]).
pub const AUTOMIX_PREFETCH_LEAD_MS: u64 = 2_000;

/// Send the decoder the queue's next track after the one playing, with
/// the transition's hints, or `ClearNextTrack` when the queue has none.
///
/// Nothing is sent while the queue's cursor is not on the track playing:
/// a step is between moving the cursor and loading its track, and the
/// load it sends replaces the track this answer would be for.
///
/// The caller holds the engine's publish lock.
pub async fn send_next(
    pool: &SqlitePool,
    shared: &SharedPlayback,
    repeat: RepeatMode,
    send: impl Fn(AudioCmd),
) -> AppResult<()> {
    let current_id = shared.current_track_id.load(Ordering::Acquire);
    if current_id <= 0 {
        return Ok(());
    }
    let cursor = queue::current_track(pool).await?.map(|t| t.id);
    // An emptied queue (cleared from an MPD client) names no next track.
    if cursor.is_none() && queue::queue_length(pool).await? == 0 {
        send(AudioCmd::ClearNextTrack { after: current_id });
        return Ok(());
    }
    if cursor != Some(current_id) {
        tracing::debug!(
            current_id,
            "next track: the cursor is elsewhere, a step is in flight"
        );
        return Ok(());
    }
    let Some(track) = queue::peek_next(pool, repeat).await? else {
        send(AudioCmd::ClearNextTrack { after: current_id });
        return Ok(());
    };
    let replay_gain = crate::commands::player::fetch_replay_gain(pool, track.id).await;

    // Smart-crossfade hint: whether the next track shares an album with
    // the one playing. The decoder reads it at mix-decision time to
    // suppress the fade for same-album hand-offs (concept records / live
    // sets). Best-effort — any DB hiccup falls back to "different album"
    // (regular crossfade).
    let album = |id: i64| async move {
        sqlx::query_scalar::<_, Option<i64>>("SELECT album_id FROM track WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten()
    };
    let same_album = matches!(
        (album(current_id).await, album(track.id).await),
        (Some(a), Some(b)) if a == b
    );

    // Dynamic crossfade hint: scale the fade by the tempo gap so
    // similar-tempo transitions get the full window (clean blend) while
    // large gaps snap quickly (no time for the rhythms to clash). When
    // either tempo is unknown or not trusted, the override stays 0 so the
    // decoder falls back to the user-configured static crossfade.
    let base_ms = shared.crossfade_ms.load(Ordering::Relaxed);
    let mut crossfade_ms: u32 = 0;
    if shared.dynamic_crossfade_enabled.load(Ordering::Relaxed) && base_ms > 0 {
        let tempo = |id: i64| async move {
            sqlx::query_as::<_, (Option<f64>, Option<f64>)>(
                "SELECT bpm, bpm_confidence FROM track_analysis WHERE track_id = ?",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
        };
        crossfade_ms = super::analytics::dynamic_crossfade_ms(
            base_ms,
            tempo(current_id).await,
            tempo(track.id).await,
        );
    }

    // AutoMix plan: the fade placed on both tracks' beats. None — the
    // switch off, an album playing through, a fallback — and the decoder
    // makes the fade it makes without AutoMix.
    let mut automix_fade = None;
    if shared.automix.is_enabled() && !same_album && base_ms > 0 {
        automix_fade = automix_plan(pool, current_id, &track, u64::from(base_ms)).await;
    }

    send(AudioCmd::SetNextTrack {
        after: current_id,
        path: track.as_path(),
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        // Handed over as `manual`, whatever its queue row says — the
        // source autoplay's picks are loaded under from every surface,
        // the end of a track included.
        source_type: "manual".into(),
        source_id: None,
        replay_gain,
        same_album,
        crossfade_ms,
        automix_fade,
    });
    Ok(())
}

/// Send the decoder the queue's next track again if it has asked for one
/// ([`SharedPlayback::next_wanted`]): the queue has just changed. The
/// caller holds the engine's publish lock.
pub async fn revise_with(
    pool: &SqlitePool,
    shared: &SharedPlayback,
    send: impl Fn(AudioCmd),
) -> AppResult<()> {
    if !shared.next_wanted.load(Ordering::Acquire) {
        return Ok(());
    }
    let repeat = queue::read_repeat_mode(pool).await;
    send_next(pool, shared, repeat, send).await
}

/// [`revise_with`] for the active profile, under the publish lock, after
/// the queue changed. As the prefetch does, a queue that now ends is
/// first grown by autoplay when it is on.
pub async fn revise(app: &AppHandle) {
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return;
    };
    // Only a library track hands over to the library queue (the
    // prefetch's own rule): a file played without importing it, or a
    // remote track, must not grow it through autoplay below.
    let shared = engine.shared();
    if !shared.next_wanted.load(Ordering::Acquire)
        || shared.current_track_id.load(Ordering::Acquire) <= 0
    {
        return;
    }
    let Ok(pool) = app.state::<AppState>().require_profile_pool().await else {
        return;
    };
    let _publish = engine.lock_publish().await;
    let repeat = queue::read_repeat_mode(&pool).await;
    if crate::autoplay::top_up_or_log(&pool, repeat, "next-track revision")
        .await
        .is_some()
    {
        crate::commands::player::emit_queue_changed(app);
    }
    let result = revise_with(&pool, engine.shared(), |cmd| {
        if let Err(err) = engine.send(cmd) {
            tracing::warn!(%err, "next track: send failed");
        }
    })
    .await;
    if let Err(err) = result {
        tracing::warn!(%err, "next track: revision failed");
    }
}

/// Run `edit`, a change to the queue, under the engine's publish lock
/// (`publish`, its [`AudioEngine::lock_publish`]).
///
/// The prefetch and a revision read the queue's next track and send it
/// under that lock, and an autoplay top-up reads the queue's end, draws
/// its picks over several queries and appends them under it too. An edit
/// landing in between would leave the decoder a next track the queue no
/// longer names, or be followed by picks appended to a queue it has just
/// emptied. Taken after the profile pool, as those holders take it, and
/// released before the edit's [`revise_soon`] takes it again.
pub async fn edit_queue<G, T>(
    publish: impl std::future::Future<Output = G>,
    edit: impl std::future::Future<Output = T>,
) -> T {
    let _publish = publish.await;
    edit.await
}

/// [`revise`] in the background, for a command that has just changed the
/// queue and returns at once.
pub fn revise_soon(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { revise(&app).await });
}

/// A track's beat list as the analysis stored it, in seconds. `None` when
/// it has none (not analysed, analysed before beats were stored, or no
/// tempo) or the row cannot be read.
pub async fn stored_beats(pool: &SqlitePool, track_id: i64) -> Option<Vec<f64>> {
    let bytes: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT beats FROM track_analysis WHERE track_id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
            .flatten();
    waveflow_core::analysis::beats::decode(&bytes?)
}

/// The AutoMix fade from `current_id` into `next`, as the decoder takes
/// it: `(fade_start_us, fade_end_us)` in the outgoing track's time.
/// `None` when either track has no grid or the plan falls back, which is
/// logged with its reason.
async fn automix_plan(
    pool: &SqlitePool,
    current_id: i64,
    next: &QueueTrack,
    cap_ms: u64,
) -> Option<(u64, u64)> {
    use waveflow_core::automix::{plan, TrackBeats};
    let current_ms: Option<i64> = sqlx::query_scalar("SELECT duration_ms FROM track WHERE id = ?")
        .bind(current_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    let out_beats = stored_beats(pool, current_id).await.unwrap_or_default();
    let in_beats = stored_beats(pool, next.id).await.unwrap_or_default();
    let result = plan(
        TrackBeats {
            beats: &out_beats,
            duration_ms: current_ms.unwrap_or(0).max(0) as u64,
        },
        TrackBeats {
            beats: &in_beats,
            duration_ms: next.duration_ms.max(0) as u64,
        },
        cap_ms,
    );
    match result {
        Ok(p) => {
            tracing::info!(
                from = current_id,
                to = next.id,
                beats = p.beats,
                fold = p.fold,
                ratio = p.ratio,
                drift_ms = p.max_drift_ms,
                "automix: transition planned"
            );
            Some((p.fade_start_us, p.fade_end_us))
        }
        Err(reason) => {
            tracing::info!(
                from = current_id,
                to = next.id,
                ?reason,
                "automix: falling back"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
