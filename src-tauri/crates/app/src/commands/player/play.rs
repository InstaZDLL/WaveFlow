//! Starting playback from a list of tracks, editing the queue and moving
//! through it.

use std::sync::Arc;

use tauri::AppHandle;

use crate::{
    audio::{engine::AudioCmd, next_track::edit_queue, AudioEngine},
    error::{AppError, AppResult},
    queue::{self, Direction},
    state::AppState,
};

use super::*;

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
    // Before the first await (#622): the queue read and the ReplayGain
    // lookup below are exactly where a newer click overtakes this one.
    let intent = engine.next_load_intent();
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    // The lock covers the whole queue half of the dispatch, not just its
    // tail (#632): the index below is read from the queue as it stands
    // now, and committing it after another producer replaced that queue
    // would put the cursor on a track this load never resolved.
    let _publish = engine.lock_publish().await;
    // Peeked, not applied: the cursor moves below, once this load is
    // known to be the one that will play (#632).
    let Some((index, track)) = queue::peek_jump(&pool, position).await? else {
        return Err(AppError::Other("queue is empty".into()));
    };
    let replay_gain = fetch_replay_gain(&pool, track.id).await;
    // Nothing has been written yet, so this is the last moment at which
    // giving up costs nothing (#632). Past it the gain is in hand and the
    // lock is still held, so the cursor write, the emits and the send are
    // one step: either this load is the one that plays and all four
    // happen, or none of them does.
    if !engine.claim_dispatch(intent) {
        tracing::debug!("jump superseded by a newer selection; leaving the queue alone");
        return Ok(());
    }
    queue::commit_index(&pool, index).await?;
    emit_track_changed(&app, &state.paths, &track, Some(profile_id));
    emit_queue_changed(&app);
    engine.send(AudioCmd::LoadAndPlay {
        intent,
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain,
    })
}

/// Replace the queue with the given track list and start playing at
/// `start_index`. `source_type` must match one of the enum values on
/// `queue_item.source_type` ('album'|'playlist'|'artist'|'library'|
/// 'liked'|'manual'|'radio').
///
/// `album_ordered` is for the one caller that knows something the
/// source type cannot express: a Mood Radio answered in whole records.
/// Omitted everywhere else, and then the answer is worked out here —
/// see [`queue_is_album_ordered`] (#647).
#[tauri::command]
pub async fn player_play_tracks(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    source_type: String,
    source_id: Option<i64>,
    track_ids: Vec<i64>,
    start_index: usize,
    album_ordered: Option<bool>,
) -> AppResult<()> {
    // Claimed before the queue is even filled (#622): this command does
    // the most preparation of any load path, so it is the one most likely
    // to finish after a Next the user pressed while waiting for it.
    let intent = engine.next_load_intent();
    play_tracks_with_intent(
        app,
        state,
        engine,
        source_type,
        source_id,
        track_ids,
        start_index,
        album_ordered,
        intent,
    )
    .await
}

/// [`player_play_tracks`] under an intent the caller claimed: a caller
/// whose preparation started earlier (files opened from the system, which
/// first walk and look them up) is ordered from that moment, not from
/// this call (#622).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn play_tracks_with_intent(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    source_type: String,
    source_id: Option<i64>,
    track_ids: Vec<i64>,
    start_index: usize,
    album_ordered: Option<bool>,
    intent: crate::audio::engine::LoadIntent,
) -> AppResult<()> {
    let (pool, profile_id) = state.require_profile_snapshot().await?;

    // This path cannot peek: replacing the queue is the point. So it
    // claims before the first write instead, which is the last moment at
    // which giving up is free (#632) — and it holds the publish lock
    // across the replacement, because that is precisely what must not
    // happen underneath another producer: a Next that peeked the old
    // queue and committed its index onto the new one would leave the
    // cursor on a track it never resolved. A Next pressed during the fill
    // therefore waits, and then steps through the album it was given
    // rather than the one it replaced.
    let publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("play_tracks superseded before filling the queue; leaving it alone");
        return Ok(());
    }
    let album_ordered = queue_is_album_ordered(&pool, &source_type, source_id, album_ordered).await;
    queue::fill_queue(
        &pool,
        &source_type,
        source_id,
        &track_ids,
        start_index,
        album_ordered,
    )
    .await?;
    // Mirrored after the write that persists it, the way the shuffle
    // mode is: the database is what a restart reads back, and the
    // engine's copy is what the decoder reads now. Unconditional —
    // this is the only place the queue is replaced, so it is the only
    // place a flag from the previous session can be cleared.
    publish_album_ordering(&engine, album_ordered);

    // Spotify-style: if the user has shuffle enabled, randomize the
    // queue in place immediately after filling it, keeping the track
    // they clicked in position 0. Without this, enabling shuffle
    // before clicking a track would leave the queue sequential and
    // Next would advance alphabetically — visibly "not random".
    // The flag above is deliberately not cleared when this reorders the
    // queue. Shuffle is read first by `listening_to`, so a shuffled
    // session already answers by its mode whatever the flag says; and
    // turning shuffle off restores `queue.preshuffle`, which is the
    // album-ordered list this flag describes — clearing it would leave
    // the records back in order and the gain wrong (#647).
    let mode = queue::read_shuffle_mode(&pool).await;
    let shuffled = mode.is_on();
    if shuffled {
        // The queue was just replaced, so only the ordering is at
        // stake here — the mode is already persisted and already
        // mirrored into the engine.
        queue::apply_shuffle_mode(&pool, mode).await?;
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

    // The queue is replaced and the panel has been told; what is left to
    // publish is the label and the load. The lock goes back here, because
    // what comes next can be slow in a way no other surface should wait
    // on: a ReplayGain read, and — when the file is gone — an HTTP
    // round-trip to mint a streaming ticket. Both branches below re-take
    // it and re-claim immediately before they publish.
    drop(publish);

    let pb = std::path::PathBuf::from(&track.file_path);
    if !pb.is_file() {
        // The file is gone, but the recording may not be: if this track was
        // proved to be one the bound server holds, play it from there instead
        // of refusing. Announced rather than silent — the row keeps saying the
        // file is missing, because it is.
        #[cfg(feature = "sync_v2")]
        if let Some(url) = server_stand_in(&state, &pool, profile_id, track.id).await {
            tracing::info!(
                track_id = track.id,
                path = %track.file_path,
                "local file missing; playing the linked server copy"
            );
            let replay_gain = fetch_replay_gain(&pool, track.id).await;
            // Both slow reads are done, so the lock comes back for the
            // publish itself.
            let _publish = engine.lock_publish().await;
            if !engine.claim_dispatch(intent) {
                tracing::debug!("play_tracks superseded before the server stand-in");
                return Ok(());
            }
            emit_track_changed(&app, &state.paths, &track, Some(profile_id));
            return engine.send(AudioCmd::LoadRemoteFileAndPlay {
                intent,
                origin: crate::audio::engine::DetachedOrigin::Remote,
                // The missing path on purpose: the decoder's repair path
                // is exactly this situation, and reusing it keeps one
                // implementation of "try the file, then the server".
                path: pb,
                start_ms: 0,
                track_id: track.id,
                duration_ms: track.duration_ms.max(0) as u64,
                title: Some(track.title.clone()),
                artist: track.artist_name.clone(),
                artwork_url: None,
                fallback_url: Some(url),
                // The user's own library row. Never ours to delete.
                discard_on_failure: false,
                replay_gain,
            });
        }
        return Err(AppError::Audio(format!(
            "file not found: {}",
            track.file_path
        )));
    }

    let replay_gain = fetch_replay_gain(&pool, track.id).await;
    // Claimed again, because the reads above are awaits: the queue is
    // already replaced and there is no undoing that, but the label and
    // the load are still ours to withhold if a newer selection has taken
    // over since (#632). The same intent re-claims successfully as long as
    // nothing newer did. The lock comes back for the publish, now that
    // the gain is in hand.
    let _publish = engine.lock_publish().await;
    if !engine.claim_dispatch(intent) {
        tracing::debug!("play_tracks superseded while resolving its first track");
        return Ok(());
    }
    // Tell the frontend about the new track BEFORE dispatching the
    // decoder command so the PlayerBar updates without waiting on
    // the first position/state event.
    emit_track_changed(&app, &state.paths, &track, Some(profile_id));
    engine.send(AudioCmd::LoadAndPlay {
        intent,
        path: pb,
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type,
        source_id,
        replay_gain,
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
    engine: tauri::State<'_, Arc<AudioEngine>>,
    from_position: i64,
    to_position: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    edit_queue(
        engine.lock_publish(),
        queue::reorder(&pool, from_position, to_position),
    )
    .await?;
    emit_queue_changed(&app);
    // A prepared next track follows the queue (see `audio::next_track`).
    crate::audio::next_track::revise_soon(&app);
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
    engine: tauri::State<'_, Arc<AudioEngine>>,
    track_ids: Vec<i64>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    // An empty queue is built rather than added to, so it decides
    // afresh what it is made of — and picks stacked by hand are not a
    // session of records. The mirror has to follow the row that branch
    // just wrote, or the next track plays with the album gain of a
    // session that ended (#647).
    let built = edit_queue(
        engine.lock_publish(),
        queue::append_to_user_queue(&pool, &track_ids, None),
    )
    .await?;
    if built {
        publish_album_ordering(&engine, false);
    }
    emit_queue_changed(&app);
    crate::audio::next_track::revise_soon(&app);
    Ok(())
}

/// Insert a list of tracks immediately after the currently-playing
/// position. Used by "Play next" — does not interrupt playback.
#[tauri::command]
pub async fn player_play_next(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    track_ids: Vec<i64>,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    // Same as "Add to queue": only the empty-queue branch replaces the
    // queue, and only then does what it is made of change (#647).
    let built = edit_queue(
        engine.lock_publish(),
        queue::insert_after_current(&pool, &track_ids, "manual", None),
    )
    .await?;
    if built {
        publish_album_ordering(&engine, false);
    }
    emit_queue_changed(&app);
    crate::audio::next_track::revise_soon(&app);
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
    // Claimed with the invocation, before either branch (#622) — the
    // remote queue needs it too, and it mints none of its own.
    let intent = engine.next_load_intent();
    // A remote play queue takes over next/previous while it is active: its
    // tracks stream by URL and have no row in the local `queue_item` table.
    // Files played without importing them, the same way.
    if state.external_playback.is_active() {
        crate::external_playback::advance(&app, Direction::Next, intent).await?;
        return Ok(());
    }
    #[cfg(feature = "sync_v2")]
    if state.remote_playback.is_active() {
        crate::remote::playback::advance(&app, Direction::Next, intent).await?;
        return Ok(());
    }
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let repeat = queue::read_repeat_mode(&pool).await;
    // The lock covers the whole queue half of the dispatch, not just its
    // tail (#632): the index below is read from the queue as it stands
    // now, and committing it after another producer replaced that queue
    // would put the cursor on a track this load never resolved.
    let _publish = engine.lock_publish().await;
    // At the end of the queue with autoplay on, the queue grows first and
    // this step lands on what was added — the same call every other
    // producer makes, so every surface lands on the same track. Taken
    // back out below if the step loses its claim; announced by the
    // queue change this step emits once it has claimed.
    let topped_up = crate::autoplay::top_up_or_log(&pool, repeat, "next").await;
    // Any return below that gives up takes the batch back out.
    let read = async {
        let queue_len = queue::queue_length(&pool).await?;
        let next_opt = queue::peek_step(&pool, Direction::Next, repeat).await?;
        AppResult::Ok((queue_len, next_opt))
    };
    let (queue_len, next_opt) = match read.await {
        Ok(pair) => pair,
        Err(err) => {
            crate::autoplay::undo_if_added(topped_up, &pool).await;
            return Err(err);
        }
    };
    match &next_opt {
        Some((_, track)) => tracing::info!(
            next_track_id = track.id,
            next_title = %track.title,
            queue_len,
            ?repeat,
            "player_next advanced"
        ),
        None => tracing::info!(queue_len, ?repeat, "player_next: queue exhausted, no-op"),
    }
    let Some((index, track)) = next_opt else {
        crate::autoplay::undo_if_added(topped_up, &pool).await;
        return Ok(());
    };
    let replay_gain = fetch_replay_gain(&pool, track.id).await;
    // Nothing has been written yet, so this is the last moment at which
    // giving up costs nothing (#632). Past it the gain is in hand and the
    // lock is still held, so the cursor write, the emits and the send are
    // one step: either this load is the one that plays and all four
    // happen, or none of them does.
    if !engine.claim_dispatch(intent) {
        tracing::debug!("next superseded by a newer selection; leaving the queue alone");
        crate::autoplay::undo_if_added(topped_up, &pool).await;
        return Ok(());
    }
    if let Err(err) = queue::commit_index(&pool, index).await {
        crate::autoplay::undo_if_added(topped_up, &pool).await;
        return Err(err);
    }
    emit_track_changed(&app, &state.paths, &track, Some(profile_id));
    emit_queue_changed(&app);
    engine.send(AudioCmd::LoadAndPlay {
        intent,
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain,
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
    // As in `player_next`: claimed with the invocation, before every
    // branch (#622). The restart-in-place branch below sends no load, and
    // an intent that is never sent orders nothing.
    let intent = engine.next_load_intent();
    // Same restart-vs-step rule as the local queue applies to a remote
    // one: past 3 s, "previous" restarts the current track.
    if engine.shared().current_position_ms() > 3000 {
        return engine.send(AudioCmd::Seek(0));
    }
    if state.external_playback.is_active() {
        crate::external_playback::advance(&app, Direction::Previous, intent).await?;
        return Ok(());
    }
    #[cfg(feature = "sync_v2")]
    if state.remote_playback.is_active() {
        crate::remote::playback::advance(&app, Direction::Previous, intent).await?;
        return Ok(());
    }
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let repeat = queue::read_repeat_mode(&pool).await;
    // The lock covers the whole queue half of the dispatch, not just its
    // tail (#632): the index below is read from the queue as it stands
    // now, and committing it after another producer replaced that queue
    // would put the cursor on a track this load never resolved.
    let _publish = engine.lock_publish().await;
    let Some((index, track)) = queue::peek_step(&pool, Direction::Previous, repeat).await? else {
        return Ok(());
    };
    let replay_gain = fetch_replay_gain(&pool, track.id).await;
    // Nothing has been written yet, so this is the last moment at which
    // giving up costs nothing (#632). Past it the gain is in hand and the
    // lock is still held, so the cursor write, the emits and the send are
    // one step: either this load is the one that plays and all four
    // happen, or none of them does.
    if !engine.claim_dispatch(intent) {
        tracing::debug!("previous superseded by a newer selection; leaving the queue alone");
        return Ok(());
    }
    queue::commit_index(&pool, index).await?;
    emit_track_changed(&app, &state.paths, &track, Some(profile_id));
    emit_queue_changed(&app);
    engine.send(AudioCmd::LoadAndPlay {
        intent,
        path: track.as_path(),
        start_ms: 0,
        track_id: track.id,
        duration_ms: track.duration_ms.max(0) as u64,
        source_type: "manual".into(),
        source_id: None,
        replay_gain,
    })
}

/// A stream URL for the server copy of a local track whose file is gone.
///
/// `None` for every ordinary reason — no server bound, no confirmed link,
/// offline, the mint refused — and each of them means the caller reports the
/// missing file, which is the honest answer when there is nothing to play
/// instead.
#[cfg(feature = "sync_v2")]
async fn server_stand_in(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    profile_id: i64,
    local_track_id: i64,
) -> Option<String> {
    if crate::offline::is_offline() {
        return None;
    }
    let remote_id = crate::remote::reconciliation::linked_remote_track(pool, local_track_id)
        .await
        .ok()
        .flatten()?;
    crate::remote::stream::ticket_url(state, profile_id, &remote_id)
        .await
        .ok()
}
