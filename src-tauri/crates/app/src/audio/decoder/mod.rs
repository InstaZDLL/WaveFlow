//! Symphonia-based decoder thread.
//!
//! Blocks on the `AudioCmd` channel when Idle. On `LoadAndPlay`, opens
//! the file with symphonia, decodes packets in a loop, converts the
//! sample format / channel layout / sample rate to match the cpal
//! device config, and pushes interleaved f32 samples into the SPSC ring
//! feeding the audio callback.
//!
//! Commands are polled between packets via `cmd_rx.try_recv()` so
//! pause / stop / seek feel responsive even during long tracks.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::Receiver;
use rtrb::Producer;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use tokio::sync::mpsc::UnboundedSender;

use super::analytics::AnalyticsMsg;
use super::engine::{AudioCmd, LoadIntent};
use super::events::{emit_radio_metadata, RadioMetadataPayload};
use super::state::{PlayerState, SharedPlayback};
use waveflow_core::playback::stream::ActiveStream;

mod control;
mod dop;
mod tone;
mod track;

use control::{accept_load, discard_unplayable, drain_ring_silent};
use dop::{maybe_match_source_rate, maybe_switch_dop_output, play_dop_track, restore_pcm_output};
use tone::{play_test_tone, ToneEnd};
use track::play_track;

/// Minimum interval between `player:position` events emitted during
/// playback. Keeps UI traffic bounded to ~4 Hz regardless of packet
/// cadence.
const POSITION_EMIT_INTERVAL: Duration = Duration::from_millis(250);

// Tauri event names (no scheme prefix — the convention is `domain:event`).
const EVENT_POSITION: &str = "player:position";
const EVENT_STATE: &str = "player:state";
const EVENT_TRACK_ENDED: &str = "player:track-ended";
const EVENT_ERROR: &str = "player:error";

#[derive(Serialize, Clone)]
struct PositionPayload {
    ms: u64,
}

#[derive(Serialize, Clone)]
struct StatePayload {
    state: &'static str,
    track_id: Option<i64>,
}

#[derive(Serialize, Clone)]
struct TrackEndedPayload {
    track_id: i64,
    completed: bool,
    listened_ms: u64,
}

#[derive(Serialize, Clone)]
struct ErrorPayload {
    message: String,
    /// Which failure this is, so the UI can say it in the user's own
    /// language instead of showing a backend string (#597). The message
    /// stays, and stays technical — it is what a bug report needs.
    kind: &'static str,
}

impl ErrorPayload {
    /// A track that will not play. The common case on this thread: the
    /// file would not open, the decode failed, no output could be built
    /// for it.
    fn track_failed(message: String) -> Self {
        Self {
            message,
            kind: "track-failed",
        }
    }

    /// Anything else, named explicitly.
    fn of(kind: &'static str, message: String) -> Self {
        Self { message, kind }
    }
}

/// Common path for `LoadAndPlay` / `LoadUrlAndPlay` — fire the right
/// analytics message depending on how the stream finished. Live
/// radio (`finished.track_id < 0`, `finished.source_type == "radio"`)
/// skips the play_event credit entirely — there's no library row to
/// reference and the play wouldn't be eligible for scrobbling anyway.
fn handle_playback_outcome(
    outcome: Result<(PlaybackEnd, u64, FinishedTrack), String>,
    shared: &SharedPlayback,
    app: &AppHandle,
    analytics_tx: &UnboundedSender<AnalyticsMsg>,
    source_label: Option<String>,
) {
    // However the track ended — on its own, stopped, replaced, failed —
    // it asks for no next track and mixes nothing any more: a queue
    // change from here on is not for it.
    shared.next_wanted.store(false, Ordering::Release);
    shared.mixing.store(false, Ordering::Release);
    match outcome {
        Ok((PlaybackEnd::Natural, listened_ms, finished)) => {
            let completed = listened_ms + 2000 >= finished.duration_ms && finished.duration_ms > 0;
            tracing::info!(
                finished_track_id = finished.track_id,
                listened_ms,
                completed,
                "play_track ended naturally"
            );
            // The auto-advance starts here, where the track ended, so its
            // intent is claimed here too (#622). Claiming it on the
            // analytics side would put it after the channel hop and after
            // the queue work, and rank it above a Next the user pressed in
            // between. Claimed once for both branches: exactly one of them
            // sends, and an intent that is never sent orders nothing.
            let intent = LoadIntent::claim(shared);
            if finished.track_id > 0 {
                let _ = analytics_tx.send(AnalyticsMsg::TrackEnded {
                    track_id: finished.track_id,
                    completed,
                    listened_ms,
                    source_type: finished.source_type,
                    source_id: finished.source_id,
                    intent,
                });
            } else {
                // A finite non-library stream reached EOF. Radio is
                // infinite and never lands here on its own, so this is a
                // remote-queue track (or a dropped radio connection, which
                // the analytics side ignores when no remote queue is
                // active). Route it to the remote auto-advance.
                let _ = analytics_tx.send(AnalyticsMsg::RemoteTrackEnded {
                    track_id: finished.track_id,
                    intent,
                });
            }
        }
        Ok((PlaybackEnd::Interrupted, listened_ms, finished)) => {
            tracing::info!(
                finished_track_id = finished.track_id,
                listened_ms,
                will_credit = listened_ms >= 15_000,
                "play_track interrupted"
            );
            if finished.track_id > 0 && listened_ms >= 15_000 {
                let _ = analytics_tx.send(AnalyticsMsg::TrackListened {
                    track_id: finished.track_id,
                    listened_ms,
                    source_type: finished.source_type,
                    source_id: finished.source_id,
                });
            }
        }
        Err(err) => {
            tracing::warn!(
                ?err,
                source = source_label.as_deref().unwrap_or(""),
                "playback failed"
            );
            let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err.clone()));
            // Read the live `current_track_id` rather than the
            // id passed in from the original `LoadAndPlay` /
            // `LoadUrlAndPlay` command: `play_track`'s crossfade
            // swap (and the gapless EOF hand-off) reassigns the
            // active stream to a pre-fetched track and updates
            // the atomic accordingly. Using the request id would
            // surface the wrong track on the `player:state idle`
            // emit when a decode error lands during the crossfade
            // window (issue #229).
            let failing_track_id = shared.current_track_id.load(Ordering::Acquire);
            transition_state(shared, app, PlayerState::Idle, Some(failing_track_id));
        }
    }
}

/// Transition [`SharedPlayback`] state and emit a `player:state` event
/// in one place so the UI always sees transitions in order.
pub(super) fn transition_state(
    shared: &SharedPlayback,
    app: &AppHandle,
    state: PlayerState,
    track_id: Option<i64>,
) {
    shared.set_state(state);
    // Drop any cached radio metadata the moment playback goes idle —
    // covers every stop path (user Stop, decode error, stream end,
    // failed open) so `get_current_radio_metadata` never serves a
    // phantom station after the engine has gone quiet. Idempotent for
    // library playback (the cache is already cleared on LoadAndPlay).
    if matches!(state, PlayerState::Idle) {
        super::events::clear_radio_metadata();
    }
    let _ = app.emit(
        EVENT_STATE,
        StatePayload {
            state: state.as_str(),
            track_id,
        },
    );
    // Mirror the transition to the OS media overlay so the SMTC /
    // MPRIS play/pause icon flips at the same moment the in-app one
    // does. `Loading` is intentionally skipped — it's a brief
    // transient that would render as `Stopped` on the overlay and
    // flash the controls off for ~50 ms before Playing arrives.
    if !matches!(state, PlayerState::Loading) {
        // Not on `Loading` either: between two tracks it would release the
        // sleep inhibitor and take it again a moment later.
        crate::power::set_playing(state == PlayerState::Playing);
        if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
            controls.update_playback(state, shared.current_position_ms());
        }
        if let Some(presence) = app.try_state::<crate::discord_presence::DiscordPresenceHandle>() {
            presence.update_playback(state, shared.current_position_ms());
        }
    }
}

/// Spawn the decoder thread.
///
/// Takes ownership of:
/// - the command receiver,
/// - the rtrb producer feeding the cpal callback,
/// - a clone of [`SharedPlayback`] for state transitions and volume,
/// - a clone of the Tauri [`AppHandle`] so the thread can emit events.
///
/// The device's sample rate and channel count are read from `shared`
/// after [`super::output::spawn_output_thread`] has stamped them in.
pub fn spawn_decoder_thread(
    cmd_rx: Receiver<AudioCmd>,
    producer: Producer<f32>,
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    analytics_tx: UnboundedSender<AnalyticsMsg>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("waveflow-audio-decoder".into())
        .spawn(move || {
            // The decoder owns its `Producer<f32>`. It is never lent
            // out across threads; only swapped wholesale via
            // `AudioCmd::SwapProducer` when the engine rebuilds the
            // cpal output thread on a different device.
            let mut producer = producer;
            let mut panic_count = 0u32;
            const MAX_DECODER_PANICS: u32 = 3;
            loop {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    decoder_loop(
                        &cmd_rx,
                        &mut producer,
                        shared.clone(),
                        app.clone(),
                        &analytics_tx,
                    );
                }));
                match result {
                    Ok(()) => break,
                    Err(payload) => {
                        panic_count += 1;
                        let message = panic_payload_message(payload.as_ref());
                        tracing::error!(%message, panic_count, "audio decoder thread panicked");
                        let _ = app.emit(
                            EVENT_ERROR,
                            ErrorPayload::of(
                                "decoder-crashed",
                                format!("audio decoder crashed: {message}"),
                            ),
                        );
                        transition_state(&shared, &app, PlayerState::Idle, None);
                        if panic_count >= MAX_DECODER_PANICS {
                            tracing::error!(
                                panic_count,
                                "audio decoder thread panicked repeatedly, stopping recovery"
                            );
                            let _ = app.emit(
                                EVENT_ERROR,
                                ErrorPayload::of(
                                    "decoder-stopped",
                                    "audio decoder stopped after repeated crashes".to_string(),
                                ),
                            );
                            break;
                        }
                        let backoff_ms = 100_u64 * (1_u64 << (panic_count - 1));
                        std::thread::sleep(Duration::from_millis(backoff_ms));
                    }
                }
            }
        })
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "unknown panic payload".to_string()
}

/// Top-level decoder thread loop. Never returns except on `Shutdown`.
fn decoder_loop(
    cmd_rx: &Receiver<AudioCmd>,
    producer: &mut Producer<f32>,
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    analytics_tx: &UnboundedSender<AnalyticsMsg>,
) {
    // When `play_track` returns due to a mid-decode LoadAndPlay, the
    // stashed command lands here so we process it before blocking on
    // `cmd_rx.recv()` again.
    let mut pending_cmd: Option<AudioCmd> = None;

    loop {
        // Block until there's something to do. Idle = cheap.
        let cmd = match pending_cmd.take() {
            Some(c) => c,
            None => match cmd_rx.recv() {
                Ok(c) => c,
                Err(_) => return, // sender dropped; engine shutting down
            },
        };

        // Loads reach this loop too — from an idle player, and from a
        // `pending_cmd` the drain just stashed. Same arbitration (#622); a
        // command that was already accepted carries the newest intent and
        // passes unchanged.
        if !accept_load(&cmd, &shared) {
            continue;
        }

        match cmd {
            AudioCmd::LoadAndPlay {
                // Arbitrated on receipt (#622); nothing below needs it.
                intent: _,
                path,
                start_ms,
                track_id,
                duration_ms,
                source_type,
                source_id,
                replay_gain,
            } => {
                // A-B loop is a single-track concern — clear it on
                // every fresh load so the new track doesn't inherit
                // the previous track's loop endpoints.
                shared.clear_ab_loop();

                // A library track ends any radio session, so drop the
                // stored radio metadata — a mini-player that mounts now
                // must hydrate the library track, not a stale station.
                super::events::clear_radio_metadata();

                transition_state(&shared, &app, PlayerState::Loading, Some(track_id));
                // Drain whatever's left of the previous track's
                // samples from the rtrb ring so the new track doesn't
                // start with the tail of the old one.
                unpause_and_drain_ring(producer, &shared);
                // Reset position counters so the UI clock starts from 0
                // (or from start_ms on a mid-track resume).
                shared.restart_clock(start_ms, 0, false);
                shared.current_track_id.store(track_id, Ordering::Release);

                // Native DSD via DoP (#495): before opening the stream,
                // ask the engine to re-open the output at this track's
                // DoP rate when the file is DSD and the opt-in is on. It
                // hands back a fresh ring producer when it actually
                // rebuilt (we swap ours) and reports whether DoP really
                // engaged — a DAC that refused the format falls back to
                // the ordinary DSD → PCM path, and a non-DSD track after
                // a DoP one rebuilds the output back to normal PCM.
                let dop_engaged = match maybe_switch_dop_output(&app, &shared, &path, producer) {
                    Ok(engaged) => engaged,
                    Err(err) => {
                        tracing::warn!(%err, path = %path.display(), "no output for this track");
                        let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
                        transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                        continue;
                    }
                };

                let stream = match ActiveStream::open(
                    &path,
                    track_id,
                    duration_ms,
                    source_type.clone(),
                    source_id,
                    replay_gain,
                    shared.dsd_taps.load(Ordering::Acquire) as usize,
                    dop_engaged,
                ) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(?err, path = %path.display(), "open failed");
                        let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
                        transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                        continue;
                    }
                };

                // #600: now that the container has told us the track's
                // rate, ask the device for it. After the open rather
                // than before, because that is when the rate is known —
                // and before the first packet, because the resampler the
                // decoder builds on it has to target the rate we end up
                // with.
                if let Err(err) =
                    maybe_match_source_rate(&app, &shared, &stream, dop_engaged, producer)
                {
                    tracing::warn!(%err, path = %path.display(), "no output for this track");
                    let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
                    transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                    continue;
                }

                if dop_engaged {
                    let outcome = play_dop_track(
                        stream,
                        start_ms,
                        producer,
                        &shared,
                        cmd_rx,
                        &app,
                        &mut pending_cmd,
                        analytics_tx,
                    );
                    handle_playback_outcome(
                        outcome,
                        &shared,
                        &app,
                        analytics_tx,
                        Some(path.display().to_string()),
                    );
                    continue;
                }

                let outcome = play_track(
                    stream,
                    start_ms,
                    producer,
                    &shared,
                    cmd_rx,
                    &app,
                    &mut pending_cmd,
                    analytics_tx,
                );
                handle_playback_outcome(
                    outcome,
                    &shared,
                    &app,
                    analytics_tx,
                    Some(path.display().to_string()),
                );
            }
            AudioCmd::LoadRemoteFileAndPlay {
                // Kept: the two repair paths below re-send under this same
                // intent rather than claiming a newer one (#622).
                intent,
                origin,
                path,
                start_ms,
                track_id,
                duration_ms,
                title,
                artist,
                artwork_url,
                fallback_url,
                discard_on_failure,
                replay_gain,
            } => {
                shared.clear_ab_loop();
                transition_state(&shared, &app, PlayerState::Loading, Some(track_id));

                unpause_and_drain_ring(producer, &shared);
                shared.restart_clock(start_ms, 0, false);
                shared.current_track_id.store(track_id, Ordering::Release);
                if let Err(err) = restore_pcm_output(&app, producer) {
                    tracing::warn!(%err, "no output for this remote track");
                    let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
                    transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                    continue;
                }

                let is_file = origin == super::engine::DetachedOrigin::File;
                // A file played without importing it has no server behind
                // it: the remote session has nothing to say about it.
                let remote_meta = if is_file {
                    crate::remote_playback::RemoteStreamMeta::default()
                } else {
                    let state = app.state::<crate::state::AppState>();
                    state.remote_playback.current_stream_meta()
                };
                emit_radio_metadata(
                    &app,
                    RadioMetadataPayload {
                        track_id,
                        title: title.clone(),
                        artist: artist.clone(),
                        artwork_url: artwork_url.clone(),
                        station_url: fallback_url.clone(),
                        station_name: title.clone(),
                        station_artist: artist.clone(),
                        station_artwork: artwork_url.clone(),
                        is_remote: !is_file,
                        duration_ms: remote_meta.duration_ms.or(Some(duration_ms as i64)),
                        artwork_hash: remote_meta.artwork_hash,
                        remote_track_id: remote_meta.remote_id,
                        is_file,
                        file_path: is_file.then(|| path.display().to_string()),
                    },
                );

                let stream = match ActiveStream::open(
                    &path,
                    track_id,
                    duration_ms,
                    if is_file { "file" } else { "remote-local" }.to_string(),
                    None,
                    replay_gain,
                    shared.dsd_taps.load(Ordering::Acquire) as usize,
                    // Local-first playback of a reconciled remote track never
                    // takes the DoP path: DoP requires re-opening the exclusive
                    // output at `dsd_rate / 16` via `maybe_switch_dop_output`,
                    // which this branch doesn't do. Handing DoP words to an
                    // output still clocked for PCM is white noise, so a DSD
                    // file reached this way plays through DSD → PCM.
                    false,
                ) {
                    Ok(stream) => stream,
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            path = %path.display(),
                            has_server_fallback = fallback_url.is_some(),
                            "remote local source open failed"
                        );
                        discard_unplayable(&path, discard_on_failure);
                        if let Some(url) = fallback_url.filter(|_| !crate::offline::is_offline()) {
                            pending_cmd = Some(AudioCmd::LoadUrlAndPlay {
                                // The repair path continues the load it was
                                // handed, so it keeps that load's intent
                                // rather than claiming a newer one (#622).
                                intent,
                                url,
                                ext_hint: None,
                                track_id,
                                title,
                                artist,
                                artwork_url,
                                // Streaming the same recording from the
                                // server instead of the local file must
                                // not change its level.
                                replay_gain,
                                // Not cached: this is the repair path for a
                                // file that would not open, and the target
                                // that named it is two commands upstream.
                                // Caching here would also risk re-writing the
                                // very entry that just failed to decode.
                                cache: None,
                                // Whatever failed was a finite file, and its
                                // replacement on the server is one too — this
                                // is the path a vanished library track takes.
                                // Its duration came with the command and is
                                // the only place that knows it, since no
                                // remote session is running.
                                duration_ms: Some(duration_ms),
                                seekable_file: true,
                            });
                            continue;
                        }
                        let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
                        transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                        continue;
                    }
                };

                let outcome = play_track(
                    stream,
                    start_ms,
                    producer,
                    &shared,
                    cmd_rx,
                    &app,
                    &mut pending_cmd,
                    analytics_tx,
                );
                if outcome.is_err() {
                    if let Some(url) = fallback_url.filter(|_| !crate::offline::is_offline()) {
                        tracing::warn!(
                            path = %path.display(),
                            "remote local decode failed; falling back to server"
                        );
                        discard_unplayable(&path, discard_on_failure);
                        pending_cmd = Some(AudioCmd::LoadUrlAndPlay {
                            // See above: the same load, continued.
                            intent,
                            url,
                            ext_hint: None,
                            replay_gain,
                            track_id,
                            title,
                            artist,
                            artwork_url,
                            // See above: a repair path does not repopulate.
                            cache: None,
                            duration_ms: Some(duration_ms),
                            seekable_file: true,
                        });
                        continue;
                    }
                }
                handle_playback_outcome(
                    outcome,
                    &shared,
                    &app,
                    analytics_tx,
                    Some(path.display().to_string()),
                );
            }
            AudioCmd::LoadUrlAndPlay {
                // Arbitrated on receipt (#622).
                intent: _,
                url,
                ext_hint,
                track_id,
                title,
                artist,
                artwork_url,
                replay_gain,
                cache,
                duration_ms: declared_duration_ms,
                seekable_file,
            } => {
                tracing::info!(
                    track_id,
                    url = %super::http_source::redact_url(&url),
                    "decoder picked up LoadUrlAndPlay"
                );
                shared.clear_ab_loop();
                transition_state(&shared, &app, PlayerState::Loading, Some(track_id));

                unpause_and_drain_ring(producer, &shared);
                shared.restart_clock(0, 0, false);
                shared.current_track_id.store(track_id, Ordering::Release);
                if let Err(err) = restore_pcm_output(&app, producer) {
                    tracing::warn!(%err, "no output for this stream");
                    let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err));
                    transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                    continue;
                }

                // Defence in depth: `player_play_url` already gates on
                // `offline::is_offline()` before dispatching the
                // command, but the decoder is the last hop before
                // reqwest::blocking opens a socket. Honouring the
                // toggle here guarantees the project-wide convention
                // ("every outbound HTTP path checks offline first")
                // holds even if some future code path enqueues a
                // LoadUrlAndPlay directly.
                if crate::offline::is_offline() {
                    let _ = app.emit(
                        EVENT_ERROR,
                        ErrorPayload::of("offline", "offline mode is enabled".to_string()),
                    );
                    transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                    continue;
                }

                // Emit the radio metadata up front so the OS overlay /
                // PlayerBar can paint the title + cover during the
                // network handshake (the symphonia probe blocks for a
                // few hundred ms on slow servers). The ICY de-interleaver
                // in `http_source` re-emits the same event with the live
                // song title once the stream's first metadata block lands.
                //
                // Placed AFTER the offline guard so a short-circuited
                // offline load never caches a phantom station; a later
                // open/probe failure transitions to Idle, which clears
                // the cache via `transition_state`.
                // A remote-queue track and a radio station both arrive via
                // `LoadUrlAndPlay` with a negative id; the live remote-session
                // flag is the only thing that tells them apart. Read the
                // active flag, duration and artwork hash in ONE lock (a plain
                // `std::sync::Mutex`, no await) so they stay consistent — three
                // separate reads could straddle a `clear()` and emit
                // `is_remote = true` with a `None` duration/artwork. It drives
                // the emit below AND whether the source is opened seekable.
                let remote_meta = {
                    let state = app.state::<crate::state::AppState>();
                    state.remote_playback.current_stream_meta()
                };
                let is_remote = remote_meta.is_remote;
                // Two different questions, and they used to share an answer.
                // `is_remote` says whether a remote *session* is running, which
                // is what the metadata emit below is about. Whether to open a
                // finite, range-capable body is a property of THIS stream, and
                // a caller outside the session — a library track falling back
                // to the server — knows it without one.
                let finite = is_remote || seekable_file;

                emit_radio_metadata(
                    &app,
                    RadioMetadataPayload {
                        track_id,
                        title: title.clone(),
                        artist: artist.clone(),
                        artwork_url: artwork_url.clone(),
                        // Station identity == the now-playing line on this
                        // first emit; the ICY de-interleaver overwrites
                        // title/artist with the live song but keeps these.
                        station_url: Some(url.clone()),
                        station_name: title.clone(),
                        station_artist: artist.clone(),
                        station_artwork: artwork_url.clone(),
                        is_remote,
                        // A remote-queue entry carries its length; radio
                        // does not. Drives a bounded seekbar on the bar.
                        // What the caller declared wins: it knows about streams
                        // no remote session covers, and a session that is not
                        // running has nothing to say about this one.
                        duration_ms: declared_duration_ms
                            .map(|ms| ms as i64)
                            .or(remote_meta.duration_ms),
                        // Cover hash for a remote track — the frontend turns
                        // it into a data URL for the PlayerBar.
                        artwork_hash: remote_meta.artwork_hash,
                        remote_track_id: remote_meta.remote_id,
                        is_file: false,
                        file_path: None,
                    },
                );

                // Snapshot the redacted URL once for both log sites
                // below + the outcome handler — `redact_url` is a
                // small string clone, cheap, and avoids per-log re-
                // parses while keeping any userinfo / query / stream
                // ticket out of the rolling log file.
                let redacted = super::http_source::redact_url(&url);

                // A remote-queue track is a finite file: open it seekable
                // (no ICY, range-capable) so the scrubber works. Radio goes
                // through the time-shift buffer, which re-emits
                // `player:radio-metadata` with the `StreamTitle` of what is
                // heard while keeping the station's cover + name; it stays
                // forward-only.
                let mut live_hint: Option<String> = None;
                let opened: Result<Box<dyn symphonia::core::io::MediaSource>, String> = if finite {
                    // A cache target only ever accompanies a finite
                    // remote-queue track, so the seekable open is the only
                    // one that can honour it.
                    match cache {
                        Some(target) => {
                            super::http_source::HttpMediaSource::open_seekable_caching(&url, target)
                        }
                        None => super::http_source::HttpMediaSource::open_seekable(&url),
                    }
                    .map(|source| Box::new(source) as Box<dyn symphonia::core::io::MediaSource>)
                } else {
                    // A live station: read ahead into the time-shift buffer
                    // by a thread of its own, so a pause does not stall the
                    // stream (`live`).
                    let station = super::live::Station {
                        app: app.clone(),
                        track_id,
                        url: url.clone(),
                        name: title.clone(),
                        artist: artist.clone(),
                        artwork_url: artwork_url.clone(),
                    };
                    super::live::open(&url, station).map(|(source, hint)| {
                        // An HLS stream says what it carries once its first
                        // segment is in: that, rather than the caller's guess.
                        if let Some(hint) = hint {
                            live_hint = Some(hint.to_string());
                        }
                        Box::new(source) as Box<dyn symphonia::core::io::MediaSource>
                    })
                };
                let http_source = match opened {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(?err, url = %redacted, "url stream open failed");
                        let _ = app.emit(
                            EVENT_ERROR,
                            ErrorPayload::of("stream-failed", format!("url stream open: {err}")),
                        );
                        transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                        continue;
                    }
                };

                let stream = match ActiveStream::open_from_source(
                    http_source,
                    live_hint.as_deref().or(ext_hint.as_deref()),
                    track_id,
                    // duration_ms = 0 is the live-stream sentinel —
                    // play_track interprets it to suppress crossfade
                    // prefetch + end-of-track guard.
                    0,
                    "radio".to_string(),
                    None,
                    // Empty for a live station; carries the track's own
                    // gain when this is a library track that fell back
                    // to streaming from the server.
                    replay_gain,
                ) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(?err, url = %redacted, "radio stream probe failed");
                        let _ = app.emit(
                            EVENT_ERROR,
                            ErrorPayload::of("stream-failed", format!("radio stream probe: {err}")),
                        );
                        transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                        continue;
                    }
                };

                let outcome = play_track(
                    stream,
                    0,
                    producer,
                    &shared,
                    cmd_rx,
                    &app,
                    &mut pending_cmd,
                    analytics_tx,
                );
                handle_playback_outcome(outcome, &shared, &app, analytics_tx, Some(redacted));
            }
            AudioCmd::SwapProducer(new_producer) => {
                // The output thread was rebuilt on a different cpal
                // device; the consumer half of the old ring went with
                // it, so any further `producer.push()` would silently
                // discard samples. Replace the local handle so the
                // next `LoadAndPlay` writes to the live ring.
                *producer = new_producer;
                tracing::info!("decoder picked up new ring producer");
            }
            AudioCmd::PlayTestTone => {
                if play_test_tone(producer, cmd_rx, &shared, &app, &mut pending_cmd)
                    == ToneEnd::Shutdown
                {
                    return;
                }
            }
            AudioCmd::Shutdown => return,
            // A setting still lands while nothing plays: it is what the
            // next track starts with. Dropping it here left the engine on
            // the old value until the next launch restored the row, while
            // the database and the switch on screen said otherwise.
            cmd if apply_setting(&cmd, &shared) => {}
            // All other commands are no-ops when no track is playing —
            // they're only meaningful inside `play_track`'s inner loop.
            _ => {}
        }
    }
}

/// Apply a command that only changes a setting the decoder reads — the
/// same stores the arms of [`drain_commands`] make while a track plays.
/// Returns whether `cmd` was one.
///
/// For the loops that have no track to apply them to (the idle loop, the
/// test tone): there they used to fall into the catch-all and be lost.
///
/// The speed is the one store that differs. With no stream decoding there
/// is no clock to rebase and no resampler to rebuild, so it is written the
/// way the boot-time restore writes it, without `set_playback_speed`'s
/// rebase, seek generation and dirty flag; the next track's stream takes
/// the speed when it opens.
fn apply_setting(cmd: &AudioCmd, shared: &SharedPlayback) -> bool {
    match *cmd {
        AudioCmd::SetVolume(v) => shared.set_volume(v),
        AudioCmd::SetNormalize(on) => shared.normalize_enabled.store(on, Ordering::Release),
        AudioCmd::SetMono(on) => shared.mono_enabled.store(on, Ordering::Release),
        AudioCmd::SetCrossfade(ms) => shared.crossfade_ms.store(ms, Ordering::Release),
        AudioCmd::SetReplayGain(on) => shared.replaygain_enabled.store(on, Ordering::Release),
        AudioCmd::SetGapless(on) => shared.gapless_enabled.store(on, Ordering::Release),
        AudioCmd::SetBalance(pct) => shared.stereo.set_balance(pct),
        AudioCmd::SetStereoWidth(pct) => shared.stereo.set_width(pct),
        AudioCmd::SetSpeed(v) => {
            if v.is_finite() {
                shared
                    .playback_speed_bits
                    .store(v.clamp(0.5, 2.0).to_bits(), Ordering::Release);
            }
        }
        _ => return false,
    }
    true
}

/// [`drain_ring_silent`] from the idle loop, where the output may still
/// be paused: a lingering `paused_output` is cleared first, or the output
/// would never pop the ring and the drain would only end at its timeout.
fn unpause_and_drain_ring(producer: &Producer<f32>, shared: &SharedPlayback) {
    shared.paused_output.store(false, Ordering::Release);
    drain_ring_silent(producer, shared);
}

/// Why [`play_track`] returned. Only `Natural` means "the track ran
/// to EOF on its own", which is the trigger for writing a
/// `play_event` row and auto-advancing the queue. `Interrupted`
/// covers every user-initiated break (Stop / LoadNext / Shutdown)
/// — in those cases the analytics task must NOT fire, otherwise
/// auto-advance cascades into a new LoadAndPlay which itself gets
/// interrupted by whatever command is still in flight, producing an
/// infinite loop.
#[derive(Debug, Clone, Copy)]
pub enum PlaybackEnd {
    Natural,
    Interrupted,
}

/// Track-level metadata returned from [`play_track`] so the caller
/// can credit the right track in analytics — distinct from the
/// initial parameters because crossfade may swap the active stream
/// mid-flight, and the analytics row must reflect whichever track
/// actually finished.
pub struct FinishedTrack {
    pub track_id: i64,
    pub duration_ms: u64,
    pub source_type: String,
    pub source_id: Option<i64>,
}

#[cfg(test)]
mod crossfade_mix_tests;

#[cfg(test)]
mod tail_drain_tests;

#[cfg(test)]
mod apply_setting_tests;

#[cfg(test)]
mod load_ordering_tests;

#[cfg(test)]
mod next_track_tests;
