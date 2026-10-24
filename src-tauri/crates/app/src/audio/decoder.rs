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

use std::fs::File;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use rtrb::{chunks::ChunkError, CopyToUninit, Producer};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use tokio::sync::mpsc::UnboundedSender;
use waveflow_core::audio_format::dsd::parser::{parse_dff, parse_dsf};

use super::analytics::AnalyticsMsg;
use super::crossfade::equal_power_gains;
use super::engine::{AudioCmd, LoadIntent};
use super::events::{emit_radio_metadata, RadioMetadataPayload};
use super::output::DopFormat;
use super::replay_gain::{effective_linear, TrackGain};
use super::state::{PlayerState, SharedPlayback};
use super::AudioEngine;
use waveflow_core::playback::stream::ActiveStream;

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
                // (no ICY, range-capable) so the scrubber works. Radio opens
                // with ICY so the source can re-emit `player:radio-metadata`
                // with the live `StreamTitle` while keeping the station's
                // cover + name; it stays forward-only.
                let opened = if finite {
                    // A cache target only ever accompanies a finite
                    // remote-queue track, so the seekable open is the only
                    // one that can honour it.
                    match cache {
                        Some(target) => {
                            super::http_source::HttpMediaSource::open_seekable_caching(&url, target)
                        }
                        None => super::http_source::HttpMediaSource::open_seekable(&url),
                    }
                } else {
                    let icy_ctx = super::http_source::IcyContext {
                        app: app.clone(),
                        track_id,
                        station_url: url.clone(),
                        station_name: title.clone(),
                        station_artist: artist.clone(),
                        artwork_url: artwork_url.clone(),
                    };
                    super::http_source::HttpMediaSource::open_with_icy(&url, icy_ctx)
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
                    Box::new(http_source),
                    ext_hint.as_deref(),
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

/// What the Settings card is told while the test tone plays.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
enum TestTonePayload {
    /// The format the tone is generated at, which is the stream's own.
    Started {
        sample_rate: u32,
        channels: u16,
        sample_format: super::state::OutputSampleFormat,
        /// Mono output folds left and right together, so the card says
        /// why both beeps sound alike.
        mono: bool,
    },
    /// A beep starts playing — counted at the output, not when pushed.
    Phase {
        phase: super::test_tone::TonePhase,
    },
    Finished,
    /// Something else took the decoder: a track, a stop, a shutdown.
    Interrupted,
    Failed {
        message: String,
    },
}

const EVENT_TEST_TONE: &str = "player:test-tone";

/// A tone asked for while a track holds the decoder.
///
/// The engine stops a session before asking for the tone, so this only
/// happens when a load was already on its way and the player did not
/// look busy yet. The track wins, and the card hears so at once rather
/// than waiting out its watchdog.
fn test_tone_yields(app: &AppHandle) {
    tracing::debug!("test tone asked for while a track plays; the track wins");
    let _ = app.emit(EVENT_TEST_TONE, TestTonePayload::Interrupted);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToneEnd {
    Done,
    Shutdown,
}

/// Play [`super::test_tone`] through the open output.
///
/// Its own loop rather than [`play_track`]: there is no file, no track to
/// credit, no position for the player bar, and nothing a rebuild should
/// replay. Commands are polled between chunks; a load or a `Stop` ends
/// the tone at once, and a load is handed back through `pending_cmd`.
fn play_test_tone(
    producer: &mut Producer<f32>,
    cmd_rx: &Receiver<AudioCmd>,
    shared: &SharedPlayback,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
) -> ToneEnd {
    use super::test_tone;

    // The engine parked the session and published `Idle` before its
    // `Stop` got here, and `play_track` returns from a `Stop` without
    // publishing anything. A load that was still opening then announced
    // `Playing` after the park, and a `Pause` queued ahead of the `Stop`
    // announced `Paused`: either would stay on screen over a decoder that
    // holds nothing, with a Play button the decoder ignores. Commands
    // arrive in order, so by now the `Stop` is done and nothing is loaded.
    if matches!(
        shared.state(),
        PlayerState::Playing | PlayerState::Paused | PlayerState::Loading
    ) {
        let track_id = shared.current_track_id.load(Ordering::Acquire);
        transition_state(shared, app, PlayerState::Idle, Some(track_id));
    }

    // A paused track's tail may still sit in the ring.
    unpause_and_drain_ring(producer, shared);
    // A DoP output reads every ring value as a DSD word: PCM pushed into
    // it comes out as full-scale noise.
    if let Err(err) = restore_pcm_output(app, producer) {
        tracing::warn!(%err, "no output for the test tone");
        let _ = app.emit(EVENT_TEST_TONE, TestTonePayload::Failed { message: err });
        return ToneEnd::Done;
    }
    let rate = shared.sample_rate.load(Ordering::Acquire);
    let channels = shared.channels.load(Ordering::Acquire);
    if rate == 0 || channels == 0 {
        let _ = app.emit(
            EVENT_TEST_TONE,
            TestTonePayload::Failed {
                message: "no output stream is open".into(),
            },
        );
        return ToneEnd::Done;
    }
    tracing::info!(
        sample_rate = rate,
        channels,
        sample_format = ?shared.sample_format(),
        "playing the output test tone"
    );
    let _ = app.emit(
        EVENT_TEST_TONE,
        TestTonePayload::Started {
            sample_rate: rate,
            channels,
            sample_format: shared.sample_format(),
            mono: shared.mono_enabled.load(Ordering::Acquire),
        },
    );

    // The output counts what it plays into the position clock; the tone
    // is nobody's position, so the clock is put back afterwards.
    let clock = shared.samples_played.load(Ordering::Acquire);
    let width = channels as usize;
    let total = test_tone::total_frames(rate);
    // 10 ms per push: fine enough for the phase to follow the output.
    let mut chunk = vec![0.0_f32; (rate as usize / 100).max(1) * width];
    let mut pushed: u64 = 0;
    let mut shown: Option<test_tone::TonePhase> = None;

    let end = loop {
        match cmd_rx.try_recv() {
            Ok(AudioCmd::Shutdown) | Err(TryRecvError::Disconnected) => break ToneEnd::Shutdown,
            Ok(AudioCmd::Stop) => break ToneEnd::Done,
            Ok(
                cmd @ (AudioCmd::LoadAndPlay { .. }
                | AudioCmd::LoadRemoteFileAndPlay { .. }
                | AudioCmd::LoadUrlAndPlay { .. }),
            ) => {
                *pending_cmd = Some(cmd);
                break ToneEnd::Done;
            }
            // The output it was written for is gone; the new one may
            // run at another rate.
            Ok(AudioCmd::SwapProducer(new_producer)) => {
                *producer = new_producer;
                break ToneEnd::Done;
            }
            // Every setting, not only the three this loop used to know:
            // the others were lost, and the track after the tone started
            // on the value before them.
            Ok(cmd) if apply_setting(&cmd, shared) => {}
            // Another press while this one plays, or a command that only
            // means something to a track.
            Ok(_) | Err(TryRecvError::Empty) => {}
        }

        // What the output has actually played: pushed, minus what still
        // waits in the ring.
        let queued = ((super::output::RING_CAPACITY - producer.slots()) / width) as u64;
        let played = pushed.saturating_sub(queued);
        let phase = test_tone::phase_at(played, rate);
        if phase.is_some() && phase != shown {
            shown = phase;
            if let Some(phase) = phase {
                let _ = app.emit(EVENT_TEST_TONE, TestTonePayload::Phase { phase });
            }
        }

        if pushed >= total {
            if queued == 0 || producer.is_abandoned() {
                let _ = app.emit(EVENT_TEST_TONE, TestTonePayload::Finished);
                shared.samples_played.store(clock, Ordering::Release);
                return ToneEnd::Done;
            }
        } else if producer.slots() >= chunk.len() {
            let frames = test_tone::fill(&mut chunk, pushed, rate, width);
            // Mono is folded on this thread now, not in the output; the
            // tone gets it the way a track does, so the card's warning
            // still describes what is heard.
            if shared.mono_enabled.load(Ordering::Relaxed) {
                super::stereo::mono_fold(&mut chunk[..frames * width], width);
            }
            if let Ok(slot) = producer.write_chunk_uninit(frames * width) {
                slot.fill_from_iter(chunk[..frames * width].iter().copied());
                pushed += frames as u64;
            }
            continue;
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Cut short: what is left of the tone must not play under whatever
    // comes next.
    unpause_and_drain_ring(producer, shared);
    shared.samples_played.store(clock, Ordering::Release);
    let _ = app.emit(EVENT_TEST_TONE, TestTonePayload::Interrupted);
    end
}

/// Result of [`push_samples`]. `Ok` means all samples were written;
/// any other variant signals that the outer loop should stop pushing
/// and react (propagate shutdown, end the track, apply a seek, or
/// hand off to a new load via `pending_cmd`).
enum PushOutcome {
    Ok,
    Stop,
    Shutdown,
    Seek(u64),
    LoadNext,
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

/// Decode tracks until either an interruption or a track ends naturally.
/// Honors crossfade: if `shared.crossfade_ms > 0` and there's a pending
/// next track (delivered by `AudioCmd::SetNextTrack`), this function
/// transparently mixes the two streams over the fade window, swaps the
/// secondary into primary on EOF, sends `AnalyticsMsg::CrossfadeStarted`
/// for the just-finished primary, and continues decoding the swapped
/// stream.
///
/// Returns `(PlaybackEnd, listened_ms_of_final_track, FinishedTrack)`.
///
/// `stream` is opened by the caller (file: [`ActiveStream::open`], live
/// HTTP: [`ActiveStream::open_from_source`] over an
/// [`super::http_source::HttpMediaSource`]). Hoisting the open out lets
/// the same playback loop drive both file-backed tracks and Web Radio
/// streams without re-deriving "is this a path or a URL" inside the
/// hot loop. A `stream` with `duration_ms == 0` is the canonical
/// live-stream marker — the prefetch + end-of-track guards branch on
/// it to suppress crossfade and auto-advance.
///
/// Yes, this takes a lot of parameters — but each one is load-bearing
/// (initial track context + the I/O + sync handles the decoder loop
/// needs) and folding them into a struct just to satisfy a lint would
/// obscure the call site without changing what the function does.
#[allow(clippy::too_many_arguments)]
/// Resolve the DoP output format a DSD file would need — `dsd_rate / 16`
/// at the container's channel count — by parsing just its header. Cheap
/// (a few reads, no decode). `None` for a non-DSD path or an unreadable
/// header (the caller then stays on the PCM path).
fn dop_format_for(path: &Path) -> Option<DopFormat> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())?
        .to_ascii_lowercase();
    let mut file = File::open(path).ok()?;
    let layout = match ext.as_str() {
        "dsf" => parse_dsf(&mut file).ok()?,
        "dff" => parse_dff(&mut file).ok()?,
        _ => return None,
    };
    Some(DopFormat {
        sample_rate: layout.sample_rate_hz / 16,
        channels: layout.channels.count() as u16,
    })
}

/// Reconcile the output format with the track about to play (#495) and
/// report whether DoP engaged.
///
/// - DSD file + `audio.dsd_dop` on → ask the engine to re-open the
///   exclusive output at the track's DoP rate. On success we swap in the
///   fresh ring producer and return `true`; if the DAC refused the DoP
///   format the engine transparently rebuilt a normal PCM output and we
///   return `false` (the caller opens the DSD → PCM path).
/// - Any other track → ask the engine to restore the normal PCM output
///   if the previous track left it in DoP mode; returns `false`.
///
/// The engine only rebuilds (and only then hands back a producer) when
/// the format actually changes, so an ordinary PCM-to-PCM track pays
/// nothing here.
/// Negotiate the output format for this track and report whether DoP
/// engaged.
///
/// `Ok(false)` is the ordinary fallback — the DAC refused the DoP format
/// and the engine opened a normal PCM output instead, so the caller plays
/// the track through DSD → PCM. `Err` is not that: by the time
/// [`AudioEngine::switch_output_for_track`] fails, it has already stopped
/// the previous output and couldn't open a replacement, so there is no
/// device on the other end of `producer` at all. Playing on would push
/// into an abandoned ring and show the UI a track that nobody can hear.
fn maybe_switch_dop_output(
    app: &AppHandle,
    shared: &SharedPlayback,
    path: &Path,
    producer: &mut Producer<f32>,
) -> Result<bool, String> {
    let want = if shared.dsd_dop_enabled.load(Ordering::Acquire) {
        dop_format_for(path)
    } else {
        None
    };
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        // No engine in managed state (shouldn't happen outside teardown)
        // — stay on the current output and the PCM path.
        return Ok(false);
    };
    match engine.switch_output_for_track(want.map(super::output::RequestedFormat::dop)) {
        Ok((new_producer, engaged)) => {
            if let Some(p) = new_producer {
                *producer = p;
            }
            Ok(engaged)
        }
        Err(err) => Err(format!("audio output switch failed: {err}")),
    }
}

/// Reopen the output at this track's own rate, so nothing resamples
/// (#600).
///
/// Four conditions, all of them cheap, and the order is the order they
/// rule things out:
///
/// - the user asked for it — this is a preference, because a reopen
///   costs an audible gap and every rate change becomes one;
/// - DoP did not already pin the format, which would be a demand
///   fighting a preference;
/// - the output really owns its device. In shared mode the system mixer
///   is in the path whatever rate we open at, so the gap would buy
///   nothing;
/// - the container declared a rate at all. AAC in MP4 does not until
///   decoding starts, and re-clocking a DAC to a guess is worse than
///   resampling.
///
/// Errors for the same reason as [`maybe_switch_dop_output`]: by the
/// time the engine fails, the previous output is gone and there is no
/// device on the other end of `producer`.
fn maybe_match_source_rate(
    app: &AppHandle,
    shared: &SharedPlayback,
    stream: &ActiveStream,
    dop_engaged: bool,
    producer: &mut Producer<f32>,
) -> Result<(), String> {
    if dop_engaged || !shared.match_source_rate.load(Ordering::Acquire) {
        return Ok(());
    }
    let Some(rate) = stream.declared_sample_rate.filter(|&rate| rate > 0) else {
        return Ok(());
    };
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return Ok(());
    };
    if !engine.exclusive_output() {
        return Ok(());
    }
    match engine.switch_output_for_track(Some(super::output::RequestedFormat::source_rate(rate))) {
        Ok((new_producer, _)) => {
            if let Some(p) = new_producer {
                *producer = p;
            }
            Ok(())
        }
        Err(err) => Err(format!("audio output switch failed: {err}")),
    }
}

/// Put the output back on the ordinary PCM path before a load that can't
/// carry DoP (remote files, HTTP streams).
///
/// Those branches never negotiate a format, so without this a DoP output
/// left over from the previous track stays installed — and that output
/// reads every ring value as a 24-bit DoP word (`to_bits`), stamps a
/// marker on it and ships it to the DAC. PCM samples pushed into it come
/// out as full-scale noise on a DAC that's still in DSD lock. Free when
/// there's nothing to do: the engine no-ops if the output is already PCM.
///
/// (DoP for a locally-reconciled remote track is possible — it opens a
/// real file — but it needs the same rebuild + `play_dop_track` dance as
/// `LoadAndPlay`, so it's left for its own change.)
/// Errors for the same reason as [`maybe_switch_dop_output`]: a failure
/// here means the engine has no output left, not that it kept the old one.
fn restore_pcm_output(app: &AppHandle, producer: &mut Producer<f32>) -> Result<(), String> {
    let Some(engine) = app.try_state::<Arc<AudioEngine>>() else {
        return Ok(());
    };
    match engine.switch_output_for_track(None) {
        Ok((Some(fresh), _)) => {
            *producer = fresh;
            Ok(())
        }
        Ok((None, _)) => Ok(()),
        Err(err) => Err(format!("audio output switch failed: {err}")),
    }
}

/// Holds the shared playback speed at 1.0 for the duration of a DoP
/// track, and puts the user's value back on the way out.
///
/// Speed is implemented by the resampler, which the DoP path doesn't
/// run: the words go to the DAC at its fixed carrier rate whatever the
/// setting says. But [`SharedPlayback::current_position_ms`] still
/// scales by it, so leaving a 1.5× in place would make the progress bar
/// and the scrobble clock run away from music that plays at 1×. Pinning
/// it keeps the reported position honest without discarding the
/// preference — the next PCM track picks the user's speed back up.
struct DopSpeedPin<'a> {
    shared: &'a SharedPlayback,
    restore: f32,
}

impl<'a> DopSpeedPin<'a> {
    fn new(shared: &'a SharedPlayback) -> Self {
        let restore = shared.playback_speed();
        // `set_playback_speed` re-bases the position clock, so it's only
        // called when the value actually has to move.
        if (restore - 1.0).abs() > f32::EPSILON {
            shared.set_playback_speed(1.0);
        }
        // Swallow the `speed_dirty` flag our own write just raised, so
        // `resync` below can use it as a pure "the user asked for
        // something" signal.
        shared.speed_dirty.store(false, Ordering::Release);
        Self { shared, restore }
    }

    /// Re-pin after a `SetSpeed` reached `drain_commands` mid-track,
    /// remembering the requested value as the one to restore.
    ///
    /// The trigger is `speed_dirty`, not the value: reading the shared
    /// speed alone can't tell "the user just asked for 1.0×" apart from
    /// "we pinned it to 1.0× ourselves", and mistaking the first for the
    /// second would resurrect a speed the user had already cancelled
    /// once the next PCM track started. Nothing else consumes the flag
    /// on this path — the PCM loop does, and it isn't running.
    fn resync(&mut self) {
        if !self.shared.speed_dirty.swap(false, Ordering::AcqRel) {
            return;
        }
        self.restore = self.shared.playback_speed();
        if (self.restore - 1.0).abs() > f32::EPSILON {
            self.shared.set_playback_speed(1.0);
            self.shared.speed_dirty.store(false, Ordering::Release);
        }
    }
}

impl Drop for DopSpeedPin<'_> {
    fn drop(&mut self) {
        if (self.restore - 1.0).abs() > f32::EPSILON {
            self.shared.set_playback_speed(self.restore);
        }
    }
}

/// DoP playback loop (#495). The bit-perfect twin of [`play_track`]:
/// no crossfade, no gapless, no EQ / ReplayGain / speed — every one of
/// those would mutate the 24-bit DoP words and corrupt the embedded DSD.
/// It just decodes DoP blocks and pushes them straight to the ring,
/// reusing [`drain_commands`] (pause / stop / seek / next) and
/// [`push_samples`] (backpressure) so transport control stays identical.
///
/// Seeking is the one transport action that survives the constraint —
/// it moves the read head and resets the encoder without touching a
/// single word — so A-B repeat is honoured here too, unlike the DSP
/// features above. Speed is not: see [`DopSpeedPin`].
fn play_dop_track(
    mut stream: ActiveStream,
    initial_start_ms: u64,
    producer: &mut Producer<f32>,
    shared: &SharedPlayback,
    cmd_rx: &Receiver<AudioCmd>,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
    _analytics_tx: &UnboundedSender<AnalyticsMsg>,
) -> Result<(PlaybackEnd, u64, FinishedTrack), String> {
    // Both halves of the output format are needed: the ring is drained in
    // whole frames, so a zero channel count would make the backend read
    // frames of nothing and the position clock divide by a placeholder.
    if shared.sample_rate.load(Ordering::Relaxed) == 0
        || shared.channels.load(Ordering::Relaxed) == 0
    {
        return Err("dop output not initialized (sample_rate / channels unset)".into());
    }
    // Pinned for the whole track, restored on every exit path.
    let mut speed_pin = DopSpeedPin::new(shared);
    if initial_start_ms > 0 {
        stream.seek_ms(initial_start_ms);
        stream.reset_decoder();
    }

    tracing::info!(
        rate = shared.sample_rate.load(Ordering::Relaxed),
        channels = shared.channels.load(Ordering::Relaxed),
        track_id = stream.track_id,
        "dop decoding start"
    );

    // DoP never crossfades, so no prefetch is ever requested — this stays
    // `None`. It only exists to satisfy the shared helpers' signatures;
    // anything they store is dropped when this function returns.
    let mut no_prefetch: Option<ActiveStream> = None;
    let mut dop_words: Vec<u32> = Vec::with_capacity(16 * 1024);
    let mut ring_scratch: Vec<f32> = Vec::with_capacity(16 * 1024);
    let mut last_position_emit = Instant::now();
    let mut ended_naturally = false;
    // The file is read to its end and its last words are being played out
    // of the ring (#932), as in [`play_track`].
    let mut dop_at_eof = false;
    let mut tail_drain: Option<TailDrain> = None;

    transition_state(shared, app, PlayerState::Playing, Some(stream.track_id));

    'pkt: loop {
        match drain_commands(
            cmd_rx,
            producer,
            shared,
            app,
            stream.track_id,
            pending_cmd,
            &mut no_prefetch,
        ) {
            ControlFlow::Continue => {}
            ControlFlow::Break => break 'pkt,
            ControlFlow::Shutdown => {
                return shutdown_outcome(&stream, shared, app, pending_cmd);
            }
            ControlFlow::LoadNext => {
                return Ok((
                    PlaybackEnd::Interrupted,
                    shared.session_listened_ms(),
                    finished_from(&stream),
                ));
            }
            ControlFlow::Seek(ms) => {
                stream.seek_ms(ms);
                stream.reset_decoder();
                reset_clock(shared, ms);
                drain_ring_silent(producer, shared);
                dop_at_eof = false;
                tail_drain = None;
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                last_position_emit = Instant::now();
                continue;
            }
        }
        no_prefetch = None;
        speed_pin.resync();

        // A-B repeat: same rule as [`play_track`] — when the loop is
        // armed and we've passed B, seek back to A. Nothing here touches
        // the DoP words; `seek_ms` resets the encoder and the DAC
        // re-locks on the next marker.
        if let Some((a_ms, b_ms)) = shared.ab_loop_armed() {
            if shared.current_position_ms() >= b_ms {
                stream.seek_ms(a_ms);
                stream.reset_decoder();
                reset_clock(shared, a_ms);
                drain_ring_silent(producer, shared);
                dop_at_eof = false;
                tail_drain = None;
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms: a_ms });
                last_position_emit = Instant::now();
                continue;
            }
        }

        // The end reached: the track ends once the output has played what
        // the ring still holds of it, not as soon as the file is read.
        if dop_at_eof {
            if TailDrain::still_playing(
                &mut tail_drain,
                queued_samples(producer),
                shared.paused_output.load(Ordering::Relaxed),
                Instant::now(),
            ) {
                if last_position_emit.elapsed() >= POSITION_EMIT_INTERVAL
                    && shared.state() == PlayerState::Playing
                {
                    let _ = app.emit(
                        EVENT_POSITION,
                        PositionPayload {
                            ms: shared.current_position_ms(),
                        },
                    );
                    last_position_emit = Instant::now();
                }
                std::thread::sleep(TAIL_POLL);
                continue;
            }
            ended_naturally = true;
            break 'pkt;
        }

        dop_words.clear();
        match stream.decode_dop_block(&mut dop_words) {
            Ok(true) => {
                dop_at_eof = true;
                continue;
            }
            Ok(false) => {}
            Err(err) => {
                let _ = app.emit(EVENT_ERROR, ErrorPayload::track_failed(err.clone()));
                return Err(err);
            }
        }
        if dop_words.is_empty() {
            continue;
        }

        // Reinterpret each 24-bit DoP word as an f32 bit pattern. The ring
        // is a pure byte pipe — `from_bits` here and `to_bits` in the DoP
        // event loop round-trip exactly, with no arithmetic in between.
        ring_scratch.clear();
        ring_scratch.extend(dop_words.iter().map(|&w| f32::from_bits(w)));

        match push_samples(
            &ring_scratch,
            producer,
            cmd_rx,
            shared,
            app,
            stream.track_id,
            pending_cmd,
            &mut no_prefetch,
        ) {
            PushOutcome::Ok => {}
            PushOutcome::Stop => break 'pkt,
            PushOutcome::Shutdown => {
                return shutdown_outcome(&stream, shared, app, pending_cmd);
            }
            PushOutcome::LoadNext => {
                return Ok((
                    PlaybackEnd::Interrupted,
                    shared.session_listened_ms(),
                    finished_from(&stream),
                ));
            }
            PushOutcome::Seek(ms) => {
                stream.seek_ms(ms);
                stream.reset_decoder();
                reset_clock(shared, ms);
                drain_ring_silent(producer, shared);
                dop_at_eof = false;
                tail_drain = None;
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                last_position_emit = Instant::now();
                continue;
            }
        }
        no_prefetch = None;
        speed_pin.resync();

        if last_position_emit.elapsed() >= POSITION_EMIT_INTERVAL
            && shared.state() == PlayerState::Playing
        {
            let _ = app.emit(
                EVENT_POSITION,
                PositionPayload {
                    ms: shared.current_position_ms(),
                },
            );
            last_position_emit = Instant::now();
        }
    }

    let listened_ms = shared.session_listened_ms();
    let finished = finished_from(&stream);
    if ended_naturally {
        let completed = listened_ms + 2000 >= stream.duration_ms && stream.duration_ms > 0;
        let _ = app.emit(
            EVENT_TRACK_ENDED,
            TrackEndedPayload {
                track_id: stream.track_id,
                completed,
                listened_ms,
            },
        );
        transition_state(shared, app, PlayerState::Ended, Some(stream.track_id));
        Ok((PlaybackEnd::Natural, listened_ms, finished))
    } else {
        Ok((PlaybackEnd::Interrupted, listened_ms, finished))
    }
}

fn play_track(
    initial_stream: ActiveStream,
    initial_start_ms: u64,
    producer: &mut Producer<f32>,
    shared: &SharedPlayback,
    cmd_rx: &Receiver<AudioCmd>,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
    analytics_tx: &UnboundedSender<AnalyticsMsg>,
) -> Result<(PlaybackEnd, u64, FinishedTrack), String> {
    let dst_sample_rate = shared.sample_rate.load(Ordering::Relaxed);
    let dst_channels = shared.channels.load(Ordering::Relaxed) as usize;
    if dst_sample_rate == 0 || dst_channels == 0 {
        return Err("cpal output not initialized (sample_rate=0)".into());
    }

    let mut stream = initial_stream;
    // Inherit the active playback speed so the lazy resampler init
    // inside decode_next builds against the correct effective input
    // rate from the very first packet. Without this, a track that
    // starts while speed != 1.0 would briefly resample at 1.0× and
    // then trigger a rebuild on the next speed_dirty cycle.
    stream.playback_speed = shared.playback_speed();
    if initial_start_ms > 0 {
        stream.seek_ms(initial_start_ms);
    }

    tracing::info!(
        dst_sample_rate,
        dst_channels,
        track_id = stream.track_id,
        is_live = stream.duration_ms == 0,
        "decoding start"
    );

    // Crossfade locals — reset on every track swap.
    let mut pending_next: Option<ActiveStream> = None;
    let mut next_requested = false;
    let mut mix_active = false;
    let mut mix_frames_written: u64 = 0;
    let mut mix_frames_total: u64 = 0;
    // Track time of the incoming stream the fade has played so far,
    // summed chunk by chunk at the speed each chunk was mixed at — a
    // speed change during the fade would otherwise re-scale all of it.
    let mut faded_track_ms: f64 = 0.0;
    // EOF flags persist across iterations so we don't keep poking
    // an exhausted stream — flipped back to false on a track swap.
    let mut primary_at_eof = false;
    let mut secondary_at_eof = false;

    let mut interleaved_scratch: Vec<f32> = Vec::with_capacity(8192);
    // Persistent decoded-sample buffers in mix mode: each iteration
    // tops them up via `decode_next`, mixes the min of both, then
    // drains the consumed prefix. Without this, two streams with
    // different source rates produce different frame counts per
    // packet, the surplus gets discarded, and the shorter stream
    // appears to play fast (~9 % at 48k vs 44.1k boundaries).
    let mut primary_resampled: Vec<f32> = Vec::with_capacity(8192);
    let mut secondary_resampled: Vec<f32> = Vec::with_capacity(8192);
    let mut mix_scratch: Vec<f32> = Vec::with_capacity(8192);
    // 6-band EQ processor — owns per-channel biquad state. The
    // shared atomics live on `shared.eq`; this struct just caches
    // coefficients and runs the per-sample filter chain. Cheap to
    // construct (no heap until the first packet) so creating it per
    // play_track is fine.
    let mut eq_processor = super::eq::EqProcessor::new();
    // Balance and stereo width, after the EQ. Keeps the gains the last
    // buffer ended on so a moved slider ramps instead of stepping.
    let mut stereo_processor = super::stereo::StereoProcessor::new();
    // Spectrum analyzer (FFT for the visualizer). Lives across both
    // the single-stream and crossfade-mix push paths; we feed it the
    // post-EQ buffer that's about to land in the ring. Cheap when the
    // visualizer toggle is off — feed() short-circuits on the atomic.
    let mut spectrum_analyzer = super::spectrum::SpectrumAnalyzer::new();
    let mut last_position_emit = Instant::now();
    // Minimum frames each side should hold before mixing — keeps
    // both buffers topped up so a slow decoder doesn't starve the mix.
    const CROSSFADE_MIN_FRAMES: usize = 1024;

    transition_state(shared, app, PlayerState::Playing, Some(stream.track_id));

    let mut ended_naturally = false;
    // The end of the track being played out of the ring (#932).
    let mut tail_drain: Option<TailDrain> = None;

    'pkt: loop {
        match drain_commands(
            cmd_rx,
            producer,
            shared,
            app,
            stream.track_id,
            pending_cmd,
            &mut pending_next,
        ) {
            ControlFlow::Continue => {}
            ControlFlow::Break => break 'pkt,
            ControlFlow::Shutdown => {
                return shutdown_outcome(&stream, shared, app, pending_cmd);
            }
            ControlFlow::LoadNext => {
                return Ok((
                    PlaybackEnd::Interrupted,
                    shared.session_listened_ms(),
                    finished_from(&stream),
                ));
            }
            ControlFlow::Seek(ms) => {
                if mix_active {
                    // Cancel the in-progress crossfade — the user
                    // jumping inside the current track means we should
                    // restart from the new position with no fade.
                    pending_next = None;
                    mix_active = false;
                    next_requested = false;
                }
                // Clear every already-resampled buffer + EOF flag so
                // a stale block from before the seek can't be pushed
                // into the ring after we re-fill it. PushOutcome::Seek
                // (deeper in the loop) does the same — keep the two
                // branches symmetric so a seek behaves identically
                // whether the user triggered it via Tauri command or
                // an A-B wrap.
                primary_resampled.clear();
                secondary_resampled.clear();
                primary_at_eof = false;
                secondary_at_eof = false;
                stream.seek_ms(ms);
                reset_clock(shared, ms);
                spectrum_analyzer.discard_pending();
                reset_resampler_for_seek(&mut stream, shared, dst_sample_rate, dst_channels);
                stream.reset_decoder();
                drain_ring_silent(producer, shared);
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                last_position_emit = Instant::now();
                continue;
            }
        }

        // Speed change pending? Rebuild every active stream's
        // resampler at the new effective input rate, drain the ring
        // so we don't bleed old-speed audio for a second, and flush
        // local already-resampled buffers. `set_playback_speed`
        // already rebased the position counters so the progress bar
        // stays continuous; this branch only handles the DSP side.
        if shared.speed_dirty.swap(false, Ordering::AcqRel) {
            let speed = shared.playback_speed();
            if let Err(err) = stream.rebuild_resampler(speed, dst_sample_rate, dst_channels) {
                tracing::warn!(?err, "resampler rebuild on speed change failed");
            }
            if let Some(next) = pending_next.as_mut() {
                if let Err(err) = next.rebuild_resampler(speed, dst_sample_rate, dst_channels) {
                    tracing::warn!(?err, "secondary resampler rebuild on speed change failed");
                }
            }
            primary_resampled.clear();
            secondary_resampled.clear();
            drain_ring_silent(producer, shared);
        }

        // A-B repeat: when an A-B loop is armed and we've reached B,
        // seek back to A. Skipped during a crossfade — the loop is a
        // single-track concern and would fight the cross-track mix.
        if !mix_active {
            if let Some((a_ms, b_ms)) = shared.ab_loop_armed() {
                let pos = shared.current_position_ms();
                if pos >= b_ms {
                    // Cleared like the two seek arms: B can be reached
                    // while the end of the track plays out, with the file
                    // already at EOF, and keeping that flag ended the
                    // track instead of looping it (#932).
                    primary_resampled.clear();
                    primary_at_eof = false;
                    tail_drain = None;
                    stream.seek_ms(a_ms);
                    reset_clock(shared, a_ms);
                    spectrum_analyzer.discard_pending();
                    reset_resampler_for_seek(&mut stream, shared, dst_sample_rate, dst_channels);
                    stream.reset_decoder();
                    drain_ring_silent(producer, shared);
                    let _ = app.emit(EVENT_POSITION, PositionPayload { ms: a_ms });
                    last_position_emit = Instant::now();
                    continue;
                }
            }
        }

        // Crossfade / gapless trigger evaluation, only when not
        // already mixing. Crossfade wins when both are enabled
        // (the fade implicitly subsumes the gap).
        //
        // Sleep-timer "end of current track" mode (`pause_after_current_track`)
        // is handled here by suppressing the prefetch + the swap entirely
        // when armed: we want the primary stream to play to its natural
        // EOF and break out of `play_track` so the analytics worker hits
        // the `TrackEnded` branch (which checks the same flag and skips
        // auto-advance). Without this guard, gapless / crossfade would
        // start playing the next track BEFORE the analytics worker ever
        // sees `TrackEnded` — which is exactly the user-reported bug
        // ("sleep timer EOT still plays the next song").
        let sleep_armed = shared.pause_after_current_track.load(Ordering::Relaxed);
        if !mix_active && !sleep_armed && stream.duration_ms > 0 {
            // Dynamic crossfade override (set by analytics PrefetchNext
            // when both this and the next track have a known BPM).
            // Falls back to the static `crossfade_ms` when 0.
            let override_ms = shared.pending_next_crossfade_ms.load(Ordering::Relaxed) as u64;
            let cf_ms = if override_ms > 0 {
                override_ms
            } else {
                shared.crossfade_ms.load(Ordering::Relaxed) as u64
            };
            if cf_ms > 0 {
                // Effective fade window — never longer than half the
                // track, so a 30 s clip with crossfade=12 s doesn't
                // start mixing at the 18 s mark.
                let effective_ms = cf_ms.min(stream.duration_ms / 2);
                // Timed on what the decoder has reached, not on what was
                // heard (#932): the mix written now is heard a ring later,
                // so on the heard clock the fade started that much late
                // and a fade shorter than the ring never started at all.
                let pos = shared.decoded_position_ms(queued_samples(producer));
                let remaining = stream.duration_ms.saturating_sub(pos);

                // Pre-fetch ~500 ms before the window starts so the
                // file open + first packet decode have time to complete.
                if !next_requested && pending_next.is_none() && remaining <= effective_ms + 500 {
                    let _ = analytics_tx.send(AnalyticsMsg::PrefetchNext);
                    next_requested = true;
                }

                // Smart crossfade: when the prefetched track is from
                // the same album as the current one, suppress the
                // mix and fall through to the gapless EOF swap below.
                // The hint comes from analytics — set right before
                // SetNextTrack lands — so we only consult it once
                // pending_next is populated.
                let smart_skip = pending_next.is_some()
                    && shared.smart_crossfade_enabled.load(Ordering::Relaxed)
                    && shared.pending_next_same_album.load(Ordering::Relaxed);

                if !smart_skip && pending_next.is_some() && remaining <= effective_ms {
                    mix_active = true;
                    mix_frames_written = 0;
                    faded_track_ms = 0.0;
                    mix_frames_total = (effective_ms * dst_sample_rate as u64) / 1000;
                    // One-shot — consume the dynamic override so the
                    // next prefetch starts from a clean slate.
                    shared.pending_next_crossfade_ms.store(0, Ordering::Release);
                }
            } else if shared.gapless_enabled.load(Ordering::Relaxed) {
                // Gapless mode: prefetch the next track ~500 ms before
                // EOF so its decoder is warm and ready. The actual swap
                // happens in the EOF branch below — no fade, no overlap,
                // just an immediate baton hand-off the moment the
                // primary buffer drains.
                //
                // On the decoder's clock (#932). On the heard one, a ring
                // behind, 500 ms before the end was reached only after the
                // decoder had hit EOF: the hand-off never ran, and the
                // advance that replaced it cut the last second.
                let pos = shared.decoded_position_ms(queued_samples(producer));
                let remaining = stream.duration_ms.saturating_sub(pos);
                if !next_requested && pending_next.is_none() && remaining <= 500 {
                    let _ = analytics_tx.send(AnalyticsMsg::PrefetchNext);
                    next_requested = true;
                }
            }
        }

        if mix_active {
            // Top up the primary buffer (one packet per iteration is
            // enough — the resampler may yield fewer frames than the
            // packet held, so we keep going until we hit the minimum
            // or EOF).
            while !primary_at_eof && primary_resampled.len() / dst_channels < CROSSFADE_MIN_FRAMES {
                let prev_len = primary_resampled.len();
                primary_at_eof = stream.decode_next(
                    &mut primary_resampled,
                    &mut interleaved_scratch,
                    dst_sample_rate,
                    dst_channels,
                )?;
                apply_replay_gain(&mut primary_resampled[prev_len..], shared, &stream);
            }
            let secondary = pending_next
                .as_mut()
                .expect("mix_active requires pending_next");
            // Only a mix of two mono tracks has no side to widen.
            let mix_mono_source = stream.src_channels == 1 && secondary.src_channels == 1;
            while !secondary_at_eof
                && secondary_resampled.len() / dst_channels < CROSSFADE_MIN_FRAMES
            {
                let prev_len = secondary_resampled.len();
                secondary_at_eof = secondary.decode_next(
                    &mut secondary_resampled,
                    &mut interleaved_scratch,
                    dst_sample_rate,
                    dst_channels,
                )?;
                apply_replay_gain(&mut secondary_resampled[prev_len..], shared, secondary);
            }

            let primary_frames = primary_resampled.len() / dst_channels;
            let secondary_frames = secondary_resampled.len() / dst_channels;
            // Once the primary stream EOFs we must keep mixing — but
            // with a primary contribution of zero — for the remainder
            // of the fade window. Otherwise the swap-on-EOF path
            // collapses g_in from < 1 straight to "100% secondary at
            // full amplitude" mid-fade and produces an audible step.
            // Common when the outgoing track's true sample count
            // doesn't quite match the duration the fade was timed
            // against (DSD files in particular: the data chunk is
            // block-aligned, not duration-aligned).
            let remaining_window = mix_frames_total.saturating_sub(mix_frames_written) as usize;
            let mix_frames = crossfade_mix_frames(
                primary_frames,
                secondary_frames,
                primary_at_eof,
                secondary_at_eof,
                remaining_window,
            );
            let denom = mix_frames_total.max(1) as f32;

            if mix_frames > 0 {
                mix_scratch.clear();
                mix_scratch.reserve(mix_frames * dst_channels);
                for f in 0..mix_frames {
                    let t = (mix_frames_written as f32 / denom).min(1.0);
                    let (g_out, g_in) = equal_power_gains(t);
                    for ch in 0..dst_channels {
                        // Use 0 for primary frames past the buffer —
                        // happens only when primary EOF'd and we're
                        // running out the rest of the fade.
                        let p = if f < primary_frames {
                            primary_resampled[f * dst_channels + ch] * g_out
                        } else {
                            0.0
                        };
                        // And 0 for the incoming track once it has run
                        // out, when it is shorter than the window.
                        let s = if f < secondary_frames {
                            secondary_resampled[f * dst_channels + ch] * g_in
                        } else {
                            0.0
                        };
                        mix_scratch.push(p + s);
                    }
                    mix_frames_written += 1;
                }
                // Drain only what each buffer actually contributed,
                // i.e. clamp to the per-buffer length — the primary
                // buffer may be empty in the EOF tail.
                let primary_consumed = mix_frames.min(primary_frames);
                let secondary_consumed = mix_frames.min(secondary_frames);
                primary_resampled.drain(..primary_consumed * dst_channels);
                secondary_resampled.drain(..secondary_consumed * dst_channels);
                // How far into the incoming track the fade has got: what
                // was taken from it, not the silence after its end.
                faded_track_ms += secondary_consumed as f64 * 1000.0
                    / f64::from(dst_sample_rate.max(1))
                    * f64::from(shared.playback_speed());

                eq_processor.process(
                    &mut mix_scratch,
                    dst_channels,
                    dst_sample_rate as f32,
                    &shared.eq,
                );

                stereo_processor.process(
                    &mut mix_scratch,
                    dst_channels,
                    &shared.stereo,
                    shared.mono_enabled.load(Ordering::Relaxed),
                    mix_mono_source,
                );
                spectrum_analyzer.feed(
                    &mix_scratch,
                    dst_channels,
                    dst_sample_rate as f32,
                    queued_samples(producer),
                    shared,
                    app,
                );
                clamp_to_unity(&mut mix_scratch);
                match push_samples(
                    &mix_scratch,
                    producer,
                    cmd_rx,
                    shared,
                    app,
                    stream.track_id,
                    pending_cmd,
                    &mut pending_next,
                ) {
                    PushOutcome::Ok => {}
                    PushOutcome::Stop => break 'pkt,
                    PushOutcome::Shutdown => {
                        return shutdown_outcome(&stream, shared, app, pending_cmd);
                    }
                    PushOutcome::LoadNext => {
                        return Ok((
                            PlaybackEnd::Interrupted,
                            shared.session_listened_ms(),
                            finished_from(&stream),
                        ));
                    }
                    PushOutcome::Seek(ms) => {
                        pending_next = None;
                        mix_active = false;
                        next_requested = false;
                        primary_at_eof = false;
                        secondary_at_eof = false;
                        primary_resampled.clear();
                        secondary_resampled.clear();
                        stream.seek_ms(ms);
                        reset_clock(shared, ms);
                        spectrum_analyzer.discard_pending();
                        reset_resampler_for_seek(
                            &mut stream,
                            shared,
                            dst_sample_rate,
                            dst_channels,
                        );
                        stream.reset_decoder();
                        drain_ring_silent(producer, shared);
                        let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                        last_position_emit = Instant::now();
                        continue;
                    }
                }
            }

            // Swap once the fade window has fully elapsed. Don't
            // swap on primary-drained alone — that path used to
            // collapse the mix to "100% secondary at full amplitude"
            // mid-fade, audible as a 0.5 s pop on DSD→anything
            // transitions where the outgoing data ended a few hundred
            // ms before the announced duration. Now the EOF tail is
            // mixed with primary=0 instead, so g_in keeps ramping
            // smoothly to 1 before the swap.
            //
            // dead_loop guards against an infinite spin when both
            // sides are EOF'd and there's nothing left to write.
            let window_done = mix_frames_written >= mix_frames_total;
            let dead_loop = mix_frames == 0 && primary_at_eof && secondary_at_eof;
            if window_done || dead_loop {
                let listened = shared.session_listened_ms();
                let _ = analytics_tx.send(AnalyticsMsg::CrossfadeStarted {
                    finished_track_id: stream.track_id,
                    finished_listened_ms: listened,
                    finished_source_type: stream.source_type.clone(),
                    finished_source_id: stream.source_id,
                });

                // The new track has been playing under the fade for
                // `faded_track_ms`; taken before the reset below.
                let faded_ms = faded_track_ms as u64;
                faded_track_ms = 0.0;
                stream = pending_next.take().expect("pending_next set during mix");
                // Move the secondary's leftover decoded samples into
                // the primary buffer so they aren't re-decoded /
                // dropped — without this we'd hear a tiny gap right
                // after the swap.
                primary_resampled = std::mem::take(&mut secondary_resampled);
                primary_at_eof = secondary_at_eof;
                secondary_at_eof = false;
                next_requested = false;
                mix_active = false;
                mix_frames_written = 0;
                mix_frames_total = 0;

                // The fade is already `faded_ms` into the new track, but
                // the end of it is still in the ring: the clock starts
                // there and reaches `faded_ms` once the output has played
                // what is queued.
                shared.restart_clock(faded_ms, queued_samples(producer), false);
                shared
                    .current_track_id
                    .store(stream.track_id, Ordering::Release);
                shared.seek_generation.fetch_add(1, Ordering::Release);

                let _ = app.emit(
                    EVENT_POSITION,
                    PositionPayload {
                        ms: shared.current_position_ms(),
                    },
                );
                last_position_emit = Instant::now();
                continue;
            }
        } else {
            // Refill only when buffer empty — `primary_resampled`
            // may already hold leftover frames moved over from the
            // secondary stream during a recent crossfade swap.
            if primary_resampled.is_empty() && !primary_at_eof {
                let prev_len = primary_resampled.len();
                primary_at_eof = stream.decode_next(
                    &mut primary_resampled,
                    &mut interleaved_scratch,
                    dst_sample_rate,
                    dst_channels,
                )?;
                apply_replay_gain(&mut primary_resampled[prev_len..], shared, &stream);
            }
            if primary_resampled.is_empty() && primary_at_eof {
                // Gapless hand-off: if a next track was pre-fetched
                // while crossfade was disabled, swap to it in-place
                // instead of returning to the analytics → LoadAndPlay
                // path (which adds a few hundred ms of decoder
                // spin-up gap). Mirrors the crossfade swap block but
                // skips the fade math entirely. Keeps the same
                // `CrossfadeStarted` analytics message so the queue
                // cursor advances and play_event gets credited
                // exactly the same way.
                //
                // Skip the swap when the sleep-timer EOT mode is
                // armed — even though we suppressed the prefetch in
                // the trigger block above, a fast user can arm the
                // timer between prefetch and EOF, so we re-check
                // here as a safety net. Falling through to the
                // natural EOF path lets the analytics worker hit
                // the `TrackEnded` branch that pauses cleanly.
                let sleep_armed_eof = shared.pause_after_current_track.load(Ordering::Relaxed);
                if pending_next.is_some() && !sleep_armed_eof {
                    let listened = shared.session_listened_ms();
                    let _ = analytics_tx.send(AnalyticsMsg::CrossfadeStarted {
                        finished_track_id: stream.track_id,
                        finished_listened_ms: listened,
                        finished_source_type: stream.source_type.clone(),
                        finished_source_id: stream.source_id,
                    });

                    stream = pending_next.take().expect("pending_next set");
                    primary_resampled.clear();
                    primary_at_eof = false;
                    secondary_at_eof = false;
                    next_requested = false;
                    mix_active = false;
                    mix_frames_written = 0;
                    mix_frames_total = 0;

                    // What is still in the ring is the end of the track
                    // that just finished: the new one starts at 0 only
                    // once the output has played it.
                    shared.restart_clock(0, queued_samples(producer), true);
                    shared
                        .current_track_id
                        .store(stream.track_id, Ordering::Release);
                    shared.seek_generation.fetch_add(1, Ordering::Release);

                    let _ = app.emit(EVENT_POSITION, PositionPayload { ms: 0 });
                    last_position_emit = Instant::now();
                    continue;
                }
                // A natural end, and the ring still holds up to a second
                // of the track (#932). Declared now, the end let the
                // advance that follows drain it unheard: every track lost
                // its last second. Wait for the output to play it. The top
                // of the loop keeps serving commands meanwhile (a pause, a
                // seek, another track) and the gapless trigger, whose next
                // track can still arrive and take over in the swap above.
                if TailDrain::still_playing(
                    &mut tail_drain,
                    queued_samples(producer),
                    shared.paused_output.load(Ordering::Relaxed),
                    Instant::now(),
                ) {
                    if last_position_emit.elapsed() >= POSITION_EMIT_INTERVAL
                        && shared.state() == PlayerState::Playing
                    {
                        let _ = app.emit(
                            EVENT_POSITION,
                            PositionPayload {
                                ms: shared.current_position_ms(),
                            },
                        );
                        last_position_emit = Instant::now();
                    }
                    std::thread::sleep(TAIL_POLL);
                    continue;
                }
                ended_naturally = true;
                break 'pkt;
            }
            // Producing audio again: whatever end was being waited for
            // (before a seek) is no longer the one in the ring.
            tail_drain = None;
            eq_processor.process(
                &mut primary_resampled,
                dst_channels,
                dst_sample_rate as f32,
                &shared.eq,
            );
            stereo_processor.process(
                &mut primary_resampled,
                dst_channels,
                &shared.stereo,
                shared.mono_enabled.load(Ordering::Relaxed),
                stream.src_channels == 1,
            );
            spectrum_analyzer.feed(
                &primary_resampled,
                dst_channels,
                dst_sample_rate as f32,
                queued_samples(producer),
                shared,
                app,
            );
            clamp_to_unity(&mut primary_resampled);
            match push_samples(
                &primary_resampled,
                producer,
                cmd_rx,
                shared,
                app,
                stream.track_id,
                pending_cmd,
                &mut pending_next,
            ) {
                PushOutcome::Ok => primary_resampled.clear(),
                PushOutcome::Stop => break 'pkt,
                PushOutcome::Shutdown => {
                    return shutdown_outcome(&stream, shared, app, pending_cmd);
                }
                PushOutcome::LoadNext => {
                    return Ok((
                        PlaybackEnd::Interrupted,
                        shared.session_listened_ms(),
                        finished_from(&stream),
                    ));
                }
                PushOutcome::Seek(ms) => {
                    primary_resampled.clear();
                    primary_at_eof = false;
                    stream.seek_ms(ms);
                    reset_clock(shared, ms);
                    spectrum_analyzer.discard_pending();
                    reset_resampler_for_seek(&mut stream, shared, dst_sample_rate, dst_channels);
                    stream.reset_decoder();
                    drain_ring_silent(producer, shared);
                    let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                    last_position_emit = Instant::now();
                    continue;
                }
            }
        }

        if last_position_emit.elapsed() >= POSITION_EMIT_INTERVAL
            && shared.state() == PlayerState::Playing
        {
            let _ = app.emit(
                EVENT_POSITION,
                PositionPayload {
                    ms: shared.current_position_ms(),
                },
            );
            last_position_emit = Instant::now();
        }
    }

    stream.resampler.flush();
    let listened_ms = shared.session_listened_ms();
    let finished = finished_from(&stream);
    if ended_naturally {
        let completed = listened_ms + 2000 >= stream.duration_ms && stream.duration_ms > 0;
        let _ = app.emit(
            EVENT_TRACK_ENDED,
            TrackEndedPayload {
                track_id: stream.track_id,
                completed,
                listened_ms,
            },
        );
        transition_state(shared, app, PlayerState::Ended, Some(stream.track_id));
        Ok((PlaybackEnd::Natural, listened_ms, finished))
    } else {
        Ok((PlaybackEnd::Interrupted, listened_ms, finished))
    }
}

/// The outcome of a play cut short by `Shutdown`: credited like any other
/// interruption, with the `Shutdown` it consumed queued again so the outer
/// loop ends the thread once that credit is sent. The exit path waits for
/// the thread to finish, then for the analytics task to write what it was
/// sent; without the re-queue the thread went back to waiting for a command
/// and the exit could not tell when the credit had left.
fn shutdown_outcome(
    stream: &ActiveStream,
    shared: &SharedPlayback,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
) -> Result<(PlaybackEnd, u64, FinishedTrack), String> {
    transition_state(shared, app, PlayerState::Idle, Some(stream.track_id));
    *pending_cmd = Some(AudioCmd::Shutdown);
    Ok((
        PlaybackEnd::Interrupted,
        shared.session_listened_ms(),
        finished_from(stream),
    ))
}

fn finished_from(stream: &ActiveStream) -> FinishedTrack {
    FinishedTrack {
        track_id: stream.track_id,
        duration_ms: stream.duration_ms,
        source_type: stream.source_type.clone(),
        source_id: stream.source_id,
    }
}

/// Engage `drain_silent` mode on the cpal callback so the contents of
/// the SPSC ring are popped + replaced with silence, then wait (bounded)
/// for the ring to empty. Used after a seek so pre-seek samples don't
/// reach the device — particularly important in the Resume-then-Seek
/// sequence where `paused_output` gets cleared before the seek lands.
///
/// **Do not pre-flip `paused_output = true` here**: the cpal callback
/// (and the WASAPI exclusive event loop) checks `paused_output`
/// BEFORE `drain_silent` and short-circuits on silence without
/// draining. The earlier round-1 fix did the swap first and locked
/// the ring at full slots for the entire 500 ms timeout, defeating
/// its own purpose. The `drain_silent` branch already writes silence
/// AND pops the ring, so it's sufficient to cover the user-facing
/// "no pre-seek leak" guarantee on its own.
///
/// A ring whose consumer is gone never drains — the output thread drops
/// a stuck stream and waits for the rebuild's `SwapProducer` — so this
/// wait ends there rather than at the timeout.
fn drain_ring_silent(producer: &Producer<f32>, shared: &SharedPlayback) {
    if producer.slots() == super::output::RING_CAPACITY {
        return;
    }
    shared.drain_silent.store(true, Ordering::Release);
    let start = Instant::now();
    while producer.slots() < super::output::RING_CAPACITY
        && !producer.is_abandoned()
        && start.elapsed() < Duration::from_millis(500)
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    shared.drain_silent.store(false, Ordering::Release);
}

/// Full resampler reset for a seek (#173). `Resampler::flush()`
/// only clears the pending input queue — the underlying `rubato`
/// FFT resampler keeps an overlap-save window that holds 10-30 ms
/// of interpolation history from BEFORE the seek. Without a
/// rebuild that history bleeds across the boundary and adds an
/// audible discontinuity to whatever the SPSC drain didn't catch.
///
/// Rebuilding the resampler at the same `(speed, dst_rate, dst_channels)`
/// triple produces a brand-new state machine with empty history,
/// so the first post-seek block is interpolated against zeros
/// rather than the pre-seek samples. Falls back to the cheap
/// `flush()` if rubato refuses to re-init (logged warn — the user
/// will hear the old behaviour but won't lose the seek).
fn reset_resampler_for_seek(
    stream: &mut ActiveStream,
    shared: &SharedPlayback,
    dst_sample_rate: u32,
    dst_channels: usize,
) {
    let speed = shared.playback_speed();
    if let Err(err) = stream.rebuild_resampler(speed, dst_sample_rate, dst_channels) {
        tracing::warn!(
            ?err,
            "resampler rebuild on seek failed; falling back to flush"
        );
        stream.resampler.flush();
    }
}

/// Reset the position counters so the UI clock jumps to `ms`. Must be
/// called after `format.seek()` to keep `SharedPlayback::current_position_ms`
/// in sync.
fn reset_clock(shared: &SharedPlayback, ms: u64) {
    shared.restart_clock(ms, 0, false);
    shared.seek_generation.fetch_add(1, Ordering::Release);
}

/// Samples written to the ring that the output has not played yet.
fn queued_samples(producer: &Producer<f32>) -> u64 {
    (producer.buffer().capacity() - producer.slots()) as u64
}

/// How often the end of a track is checked while the output plays it out.
const TAIL_POLL: Duration = Duration::from_millis(10);

/// How long the end of a track may sit in the ring with the output taking
/// none of it, while not paused, before the track is declared over anyway:
/// a device that went away must not hold an end forever.
const TAIL_STALL_LIMIT: Duration = Duration::from_millis(1500);

/// A gap between two checks longer than this means the loop was elsewhere
/// (blocked in `drain_commands` by a pause until Resume, or a slow command),
/// not watching an output that took nothing: the stall clock starts over.
const TAIL_MAX_POLL_GAP: Duration = Duration::from_millis(250);

/// The end of a track, decoded and still in the ring, being played out
/// before the end is declared (#932): what was queued at the last sign of
/// progress, when that was, and when the ring was last looked at.
#[derive(Debug)]
struct TailDrain {
    queued: u64,
    progressed_at: Instant,
    polled_at: Instant,
}

impl TailDrain {
    /// Whether the end of the track is still being played: something is
    /// queued, and the output is taking it or is paused. False once the
    /// ring is empty, or when it has not moved for [`TAIL_STALL_LIMIT`]
    /// of watching it play.
    fn still_playing(slot: &mut Option<Self>, queued: u64, paused: bool, now: Instant) -> bool {
        if queued == 0 {
            *slot = None;
            return false;
        }
        match slot {
            Some(drain)
                if queued >= drain.queued
                    && !paused
                    && now.duration_since(drain.polled_at) <= TAIL_MAX_POLL_GAP =>
            {
                drain.polled_at = now;
                now.duration_since(drain.progressed_at) < TAIL_STALL_LIMIT
            }
            _ => {
                *slot = Some(Self {
                    queued,
                    progressed_at: now,
                    polled_at: now,
                });
                true
            }
        }
    }
}

enum ControlFlow {
    Continue,
    Break,
    Shutdown,
    Seek(u64),
    /// A new `LoadAndPlay` was received while the current track was
    /// still decoding. The command has been stashed in
    /// `pending_cmd`; the caller should break out of `play_track`
    /// so `decoder_loop` can pick it up on its next iteration.
    LoadNext,
}

/// Arbitrate one incoming command against the newest load the decoder
/// has already been handed (#622).
///
/// Returns `false` only for a load whose [`LoadIntent`] is older than one
/// already delivered — an asynchronous preparation that finished late.
/// Every other command is accepted, and so is a re-check of the same
/// intent: a load stashed in `pending_cmd` passes again when the decoder
/// loop looks at it.
///
/// The comparison is against what has actually *arrived*, never against
/// the engine's allocation counter. An intent can be claimed and never
/// sent — Next on an exhausted queue, a track row that no longer exists —
/// and that must not silence a load still on its way.
///
/// This has to happen at the moment of *receipt*, not where the command is
/// executed: the drain returns [`ControlFlow::LoadNext`], which stops the
/// current track before anything looks at the payload. Dropping a stale
/// load later would leave the decoder with nothing playing at all.
fn accept_load(cmd: &AudioCmd, shared: &SharedPlayback) -> bool {
    let Some(intent) = cmd.load_intent() else {
        return true;
    };
    let newest = shared.newest_load_intent.load(Ordering::Acquire);
    if intent.get() < newest {
        tracing::debug!(
            intent = intent.get(),
            newest,
            ?cmd,
            "dropping a load a newer playback intent has already superseded"
        );
        return false;
    }
    shared
        .newest_load_intent
        .store(intent.get(), Ordering::Release);
    // Accepted, so this is now what the decoder plays — and what an output
    // rebuild has to put back after its `Stop` (#634). Recorded here, at
    // the single point every load passes through, rather than at send
    // time: a load superseded on the way in must not become the thing a
    // rebuild resumes.
    shared.record_accepted_load(cmd);
    true
}

/// Install a ring producer the engine handed over while a track was
/// playing, and end that track (#639).
///
/// Dropping the swap instead — which both drains used to do — leaves the
/// decoder writing into a ring whose consumer went away with the old
/// output thread: every `push` succeeds, nothing is ever read, and the
/// session is silent until something else rebuilds the output. The
/// engine sends a `Stop` before the swap precisely so this never
/// happens, but on the pre-release branch the two are separated by a
/// full exclusive device open, and a track the user picks inside that
/// window puts the decoder back inside `play_track` before the producer
/// lands.
///
/// **Ending the track is not incidental.** The resampler is built
/// against the output's sample rate, read once when the track started;
/// a producer swapped in underneath a running track would play it at
/// the wrong speed. `Break` ends it exactly as the `Stop` the rebuild
/// already sends does: the queue does not advance, and a listen is
/// credited under the same 15-second rule as any other interruption —
/// which a track the user picked inside a device open cannot have
/// reached. The rebuild's own resume then re-dispatches `last_load`,
/// which by then is what they picked, under its own intent.
fn install_swapped_producer(
    producer: &mut Producer<f32>,
    new_producer: Producer<f32>,
) -> ControlFlow {
    *producer = new_producer;
    tracing::info!("decoder picked up a new ring producer mid-track; ending it for the rebuild");
    ControlFlow::Break
}

/// Drain pending commands without blocking. Returns:
/// - `Continue` to keep decoding
/// - `Break` to stop the current track but keep the decoder alive
/// - `Shutdown` to exit the decoder loop entirely
/// - `Seek(ms)` to ask the caller to apply a seek on the format reader
///
/// On `Pause`, this function loops on `recv()` (blocking) until a
/// Resume / Stop / Shutdown arrives, so the decoder is cheap while
/// paused. Every state transition (Paused / Playing / Idle) is
/// routed through [`transition_state`] so the UI receives matching
/// `player:state` events — without that, the frontend sees the
/// engine stuck in "playing" after a pause and rejects the next
/// click as a no-op.
///
/// While paused, Seek commands are buffered in a `pending_seek`
/// local and applied immediately after Resume.
fn drain_commands(
    cmd_rx: &Receiver<AudioCmd>,
    producer: &mut Producer<f32>,
    shared: &SharedPlayback,
    app: &AppHandle,
    track_id: i64,
    pending_cmd: &mut Option<AudioCmd>,
    pending_next: &mut Option<ActiveStream>,
) -> ControlFlow {
    loop {
        match cmd_rx.try_recv() {
            Ok(AudioCmd::Shutdown) => return ControlFlow::Shutdown,
            Ok(AudioCmd::Stop) => return ControlFlow::Break,
            Ok(AudioCmd::Seek(ms)) => return ControlFlow::Seek(ms),
            // One arm for the three loads: whichever arrives takes over
            // from the track being decoded. A load older than one already
            // delivered is skipped and decoding continues — acting on it
            // would stop the current track to install a selection the user
            // had already replaced (#622).
            Ok(
                cmd @ (AudioCmd::LoadAndPlay { .. }
                | AudioCmd::LoadRemoteFileAndPlay { .. }
                | AudioCmd::LoadUrlAndPlay { .. }),
            ) => {
                if !accept_load(&cmd, shared) {
                    continue;
                }
                shared.paused_output.store(false, Ordering::Release);
                *pending_cmd = Some(cmd);
                return ControlFlow::LoadNext;
            }
            Ok(AudioCmd::SetCrossfade(ms)) => {
                shared.crossfade_ms.store(ms, Ordering::Release);
            }
            Ok(AudioCmd::SetNextTrack {
                path,
                track_id: next_id,
                duration_ms,
                source_type,
                source_id,
                replay_gain,
            }) => {
                store_next(
                    pending_next,
                    path,
                    next_id,
                    duration_ms,
                    source_type,
                    source_id,
                    replay_gain,
                    shared.playback_speed(),
                    shared.dsd_taps.load(Ordering::Acquire) as usize,
                );
            }
            Ok(AudioCmd::SetReplayGain(on)) => {
                shared.replaygain_enabled.store(on, Ordering::Release)
            }
            Ok(AudioCmd::SetGapless(on)) => shared.gapless_enabled.store(on, Ordering::Release),
            Ok(AudioCmd::SetBalance(pct)) => shared.stereo.set_balance(pct),
            Ok(AudioCmd::SetStereoWidth(pct)) => shared.stereo.set_width(pct),
            Ok(AudioCmd::SetSpeed(v)) => shared.set_playback_speed(v),
            Ok(AudioCmd::Pause) => {
                transition_state(shared, app, PlayerState::Paused, Some(track_id));
                shared.paused_output.store(true, Ordering::Release);
                let mut pending_seek: Option<u64> = None;
                loop {
                    match cmd_rx.recv() {
                        Ok(AudioCmd::Resume) => {
                            shared.paused_output.store(false, Ordering::Release);
                            transition_state(shared, app, PlayerState::Playing, Some(track_id));
                            break;
                        }
                        Ok(AudioCmd::Shutdown) => return ControlFlow::Shutdown,
                        Ok(AudioCmd::Stop) => {
                            shared.paused_output.store(false, Ordering::Release);
                            return ControlFlow::Break;
                        }
                        Ok(AudioCmd::Seek(ms)) => pending_seek = Some(ms),
                        Ok(AudioCmd::SetVolume(v)) => shared.set_volume(v),
                        Ok(AudioCmd::SetNormalize(on)) => {
                            shared.normalize_enabled.store(on, Ordering::Release)
                        }
                        Ok(AudioCmd::SetMono(on)) => {
                            shared.mono_enabled.store(on, Ordering::Release)
                        }
                        Ok(AudioCmd::SetCrossfade(ms)) => {
                            shared.crossfade_ms.store(ms, Ordering::Release)
                        }
                        Ok(AudioCmd::SetNextTrack {
                            path,
                            track_id: next_id,
                            duration_ms,
                            source_type,
                            source_id,
                            replay_gain,
                        }) => store_next(
                            pending_next,
                            path,
                            next_id,
                            duration_ms,
                            source_type,
                            source_id,
                            replay_gain,
                            shared.playback_speed(),
                            shared.dsd_taps.load(Ordering::Acquire) as usize,
                        ),
                        Ok(AudioCmd::SetReplayGain(on)) => {
                            shared.replaygain_enabled.store(on, Ordering::Release)
                        }
                        Ok(AudioCmd::SetGapless(on)) => {
                            shared.gapless_enabled.store(on, Ordering::Release)
                        }
                        Ok(AudioCmd::SetBalance(pct)) => shared.stereo.set_balance(pct),
                        Ok(AudioCmd::SetStereoWidth(pct)) => shared.stereo.set_width(pct),
                        Ok(AudioCmd::SetSpeed(v)) => shared.set_playback_speed(v),
                        // Same three-in-one arm as above. A stale load
                        // found while paused is skipped without clearing
                        // `paused_output`: the player stays paused rather
                        // than resuming on a superseded selection (#622).
                        Ok(
                            cmd @ (AudioCmd::LoadAndPlay { .. }
                            | AudioCmd::LoadRemoteFileAndPlay { .. }
                            | AudioCmd::LoadUrlAndPlay { .. }),
                        ) => {
                            if !accept_load(&cmd, shared) {
                                continue;
                            }
                            shared.paused_output.store(false, Ordering::Release);
                            *pending_cmd = Some(cmd);
                            return ControlFlow::LoadNext;
                        }
                        Ok(AudioCmd::Pause) => {}
                        Ok(AudioCmd::PlayTestTone) => test_tone_yields(app),
                        // The `Stop` the engine sends first should
                        // take us out of here before the swap arrives —
                        // a rebuild treats a paused session as one to
                        // park, so it does send one. This arm is what
                        // holds if it ever does not, and it holds by
                        // installing rather than dropping.
                        //
                        // `paused_output` is deliberately left raised:
                        // ending the track here hands the rebuild a
                        // session it will park, which is what the user
                        // asked for by pausing it (#611).
                        Ok(AudioCmd::SwapProducer(new_producer)) => {
                            return install_swapped_producer(producer, new_producer)
                        }
                        Err(_) => return ControlFlow::Shutdown,
                    }
                }
                if let Some(ms) = pending_seek {
                    return ControlFlow::Seek(ms);
                }
            }
            Ok(AudioCmd::SetVolume(v)) => shared.set_volume(v),
            Ok(AudioCmd::SetNormalize(on)) => shared.normalize_enabled.store(on, Ordering::Release),
            Ok(AudioCmd::SetMono(on)) => shared.mono_enabled.store(on, Ordering::Release),
            Ok(AudioCmd::PlayTestTone) => test_tone_yields(app),
            // Named before the catch-all, which used to swallow it: a
            // swap that reaches a decoding track is the one command
            // this arm must not treat as "not meaningful here".
            Ok(AudioCmd::SwapProducer(new_producer)) => {
                return install_swapped_producer(producer, new_producer)
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) => return ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => return ControlFlow::Shutdown,
        }
    }
}

/// Multiply the trailing `len_before..` slice of a freshly-decoded
/// buffer by the gain this track works out to, gated on the live
/// `replaygain_enabled` toggle. Done here (rather than in the cpal
/// callback) so each stream gets its own gain even during the
/// crossfade dual-decoder mix, where two tracks with different gains
/// are summed before reaching the ring.
///
/// The scalar is derived per buffer rather than cached on the stream:
/// it depends on the pre-amp and clipping settings as well as the
/// track, so baking it in at load time would leave the pre-amp slider
/// doing nothing until the next track. One `powf` per decoded buffer
/// is nothing next to the decode that produced it.
///
/// Takes the whole stream rather than just its [`TrackGain`] so the
/// gain and the listening context can only ever come from the *same*
/// track: during a crossfade two streams are being gained in the same
/// loop, and a pair of arguments would let one track's record context
/// be applied to the other one's gain.
#[inline]
fn apply_replay_gain(buf: &mut [f32], shared: &SharedPlayback, stream: &ActiveStream) {
    let gain = effective_linear(
        stream.replay_gain,
        shared.replay_gain_settings(),
        shared.listening_to(&stream.source_type),
    );
    if gain == 1.0 {
        return;
    }
    for s in buf.iter_mut() {
        *s *= gain;
    }
}

/// Final safety clamp before samples reach the SPSC ring (and the cpal
/// callback / WASAPI-exclusive thread that drain it). Decoded PCM is
/// already within `[-1.0, 1.0]`, so for an untouched stream this is the
/// identity and bit-perfect output is preserved — it only bites when a
/// gain stage (ReplayGain, normalize, EQ peaking, or a speed-resample
/// overshoot) pushes a peak past unity, where the un-clamped value would
/// hard-clip and distort the DAC. Runs on the decoder thread, never in
/// the hot callback.
#[inline]
fn clamp_to_unity(buf: &mut [f32]) {
    for s in buf.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
}

/// Open the supplied next-track file into an [`ActiveStream`] and
/// stash it for the crossfade pipeline. Failures are logged but
/// non-fatal — playback continues without crossfade.
#[allow(clippy::too_many_arguments)]
fn store_next(
    pending_next: &mut Option<ActiveStream>,
    path: std::path::PathBuf,
    track_id: i64,
    duration_ms: u64,
    source_type: String,
    source_id: Option<i64>,
    replay_gain: TrackGain,
    speed: f32,
    dsd_taps: usize,
) {
    match ActiveStream::open(
        &path,
        track_id,
        duration_ms,
        source_type,
        source_id,
        replay_gain,
        dsd_taps,
        // Prefetch is a crossfade / gapless concern; DoP never fades, so
        // a prefetched stream is always opened on the PCM path. A DSD
        // track that should play as DoP reaches that mode through the
        // cold LoadAndPlay in `decoder_loop`, not here.
        false,
    ) {
        Ok(mut s) => {
            // Prefetched stream needs the active speed too so its
            // first packet decode builds the resampler at the right
            // effective rate (avoids a rebuild + tiny gap when the
            // crossfade mix starts).
            s.playback_speed = speed;
            *pending_next = Some(s);
        }
        Err(err) => {
            tracing::warn!(
                ?err,
                path = %path.display(),
                "failed to open next track for crossfade — skipping fade"
            );
        }
    }
}

/// Push all samples into the producer, sleeping briefly when the ring
/// is full. Commands are polled between retries so pause/stop/seek
/// still respond promptly even when the buffer is saturated.
///
/// rtrb 0.3's `write_chunk_uninit` either gives us the full `n` slots
/// we asked for or returns `TooFewSlots(available)`; in the latter case
/// we submit what fits, sleep, and loop. `CopyToUninit` handles the
/// actual `MaybeUninit<f32>` → `f32` copy without `unsafe` code.
fn push_samples(
    samples: &[f32],
    producer: &mut Producer<f32>,
    cmd_rx: &Receiver<AudioCmd>,
    shared: &SharedPlayback,
    app: &AppHandle,
    track_id: i64,
    pending_cmd: &mut Option<AudioCmd>,
    pending_next: &mut Option<ActiveStream>,
) -> PushOutcome {
    let mut written = 0;
    while written < samples.len() {
        let remaining = &samples[written..];
        let requested = remaining.len();
        let result = producer.write_chunk_uninit(requested);
        let (slots, chunk) = match result {
            Ok(chunk) => (requested, chunk),
            Err(ChunkError::TooFewSlots(available)) => {
                if available == 0 {
                    // Ring full. Yield briefly and poll commands so
                    // pause/stop/seek aren't blocked by a saturated
                    // buffer.
                    match drain_commands(
                        cmd_rx,
                        producer,
                        shared,
                        app,
                        track_id,
                        pending_cmd,
                        pending_next,
                    ) {
                        ControlFlow::Shutdown => return PushOutcome::Shutdown,
                        ControlFlow::Break => return PushOutcome::Stop,
                        ControlFlow::Seek(ms) => return PushOutcome::Seek(ms),
                        ControlFlow::LoadNext => return PushOutcome::LoadNext,
                        ControlFlow::Continue => {}
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                match producer.write_chunk_uninit(available) {
                    Ok(chunk) => (available, chunk),
                    Err(_) => unreachable!("available slots vanished under us"),
                }
            }
        };

        let slice = &remaining[..slots];
        // rtrb's WriteChunkUninit exposes two contiguous regions
        // (the ring may wrap between them). We copy into each region
        // separately, then commit the whole chunk.
        let mut chunk = chunk;
        {
            let (first, second) = chunk.as_mut_slices();
            let split = first.len();
            slice[..split].copy_to_uninit(first);
            if split < slice.len() {
                slice[split..].copy_to_uninit(second);
            }
        }
        // SAFETY: we just wrote exactly `slots` f32 values via
        // `copy_to_uninit` into the full chunk. Every `MaybeUninit<f32>`
        // slot is now initialized, satisfying `commit_all`'s contract.
        unsafe { chunk.commit_all() };
        written += slots;
    }
    PushOutcome::Ok
}

/// Drop a reproducible copy that would not play.
///
/// Only ever called with `allowed` set for a stream-cache entry: the server
/// still holds those bytes, so a file that fails to open or decode is worth
/// losing to make the next play refetch it. A reconciled file from the user's
/// own library reaches the same failure path and must survive it — it is
/// theirs, and a decoder that deletes a listener's music because a codec
/// tripped would be a far worse bug than a cache miss.
fn discard_unplayable(path: &std::path::Path, allowed: bool) {
    if !allowed {
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!(path = %path.display(), "dropped unplayable cached stream"),
        Err(err) => tracing::warn!(?err, path = %path.display(), "could not drop cached stream"),
    }
}

/// How many frames a crossfade mixes in one pass.
///
/// A side that has reached its end counts as silence for the rest of the
/// window, either way round. The outgoing track can end a little before
/// the duration the fade was timed against (DSD in particular); the
/// incoming one can be shorter than the window altogether, since the
/// window is timed against the outgoing track (a jingle under a long
/// fade). Before the second case was handled, an incoming track that had
/// run out left nothing to mix and the loop waited for frames that never
/// came, with the outgoing track still playing: playback stalled. Now the
/// outgoing track finishes its fade, then the queue moves on.
///
/// While neither side has ended, only what both hold is mixed: the next
/// pass decodes more of the shorter one.
fn crossfade_mix_frames(
    primary_frames: usize,
    secondary_frames: usize,
    primary_at_eof: bool,
    secondary_at_eof: bool,
    remaining_window: usize,
) -> usize {
    match (primary_at_eof, secondary_at_eof) {
        (false, false) => primary_frames.min(secondary_frames),
        (true, false) => secondary_frames.min(remaining_window),
        (false, true) => primary_frames.min(remaining_window),
        (true, true) => primary_frames.max(secondary_frames).min(remaining_window),
    }
}

#[cfg(test)]
mod crossfade_mix_tests {
    use super::crossfade_mix_frames;

    #[test]
    fn both_playing_mixes_what_both_hold() {
        assert_eq!(crossfade_mix_frames(1_000, 400, false, false, 5_000), 400);
    }

    #[test]
    fn an_outgoing_track_that_ended_is_silence_for_the_rest_of_the_window() {
        assert_eq!(crossfade_mix_frames(0, 800, true, false, 500), 500);
        // The incoming track holds less than the window has left: mix
        // that, and decode more of it next pass.
        assert_eq!(crossfade_mix_frames(0, 200, true, false, 500), 200);
    }

    /// The case that stalled playback: the incoming track ran out first.
    #[test]
    fn an_incoming_track_that_ran_out_lets_the_outgoing_one_finish_its_fade() {
        assert_eq!(crossfade_mix_frames(1_000, 0, false, true, 5_000), 1_000);
        assert_eq!(crossfade_mix_frames(1_000, 0, false, true, 300), 300);
    }

    #[test]
    fn both_ended_runs_out_what_is_left_then_nothing() {
        assert_eq!(crossfade_mix_frames(300, 100, true, true, 5_000), 300);
        assert_eq!(crossfade_mix_frames(0, 0, true, true, 5_000), 0);
    }
}

#[cfg(test)]
mod tail_drain_tests {
    use std::time::{Duration, Instant};

    use super::{TailDrain, TAIL_POLL, TAIL_STALL_LIMIT};

    /// Poll every [`TAIL_POLL`] with nothing taken from the ring, from
    /// `from` for `watched`; the answer at the last poll.
    fn watch_stalled(slot: &mut Option<TailDrain>, from: Instant, watched: Duration) -> bool {
        let mut t = from;
        let mut playing = true;
        while t.duration_since(from) < watched {
            t += TAIL_POLL;
            playing = TailDrain::still_playing(slot, 50_000, false, t);
        }
        playing
    }

    #[test]
    fn an_empty_ring_ends_the_track() {
        let mut slot = None;
        assert!(!TailDrain::still_playing(
            &mut slot,
            0,
            false,
            Instant::now()
        ));
        assert!(slot.is_none());
    }

    #[test]
    fn the_end_plays_out_while_the_output_takes_it() {
        let start = Instant::now();
        let mut slot = None;
        assert!(TailDrain::still_playing(&mut slot, 96_000, false, start));
        // Progress, however long it takes in all: still the end playing.
        let later = start + TAIL_STALL_LIMIT * 2;
        assert!(TailDrain::still_playing(&mut slot, 40_000, false, later));
        assert!(TailDrain::still_playing(
            &mut slot,
            1_000,
            false,
            later + TAIL_STALL_LIMIT / 2
        ));
        assert!(!TailDrain::still_playing(
            &mut slot,
            0,
            false,
            later + TAIL_STALL_LIMIT
        ));
    }

    #[test]
    fn a_stall_watched_for_the_limit_gives_the_end_up() {
        // Playing, and the output takes nothing: a lost device.
        let start = Instant::now();
        let mut slot = None;
        assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
        assert!(watch_stalled(&mut slot, start, TAIL_STALL_LIMIT / 2));
        let mut slot = None;
        assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
        assert!(!watch_stalled(&mut slot, start, TAIL_STALL_LIMIT));
    }

    #[test]
    fn a_pause_holds_the_end() {
        let start = Instant::now();
        let mut slot = None;
        assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
        // Polled while paused: the end waits for the user, however long.
        let mut t = start;
        for _ in 0..1_000 {
            t += TAIL_POLL * 10;
            assert!(TailDrain::still_playing(&mut slot, 50_000, true, t));
        }
    }

    #[test]
    fn time_away_from_the_loop_is_not_a_stall() {
        // A pause blocks the decoder in `drain_commands` until Resume: the
        // ring is not looked at meanwhile, and that minute is not the
        // output failing to take samples. Resumed, the end plays on.
        let start = Instant::now();
        let mut slot = None;
        assert!(TailDrain::still_playing(&mut slot, 50_000, false, start));
        let resumed = start + Duration::from_secs(60);
        assert!(TailDrain::still_playing(&mut slot, 50_000, false, resumed));
        assert!(TailDrain::still_playing(
            &mut slot,
            40_000,
            false,
            resumed + TAIL_POLL
        ));
        // And a real stall after it is still caught.
        assert!(!watch_stalled(
            &mut slot,
            resumed + TAIL_POLL,
            TAIL_STALL_LIMIT
        ));
    }
}

#[cfg(test)]
mod apply_setting_tests {
    use std::sync::atomic::Ordering;

    use super::apply_setting;
    use crate::audio::engine::AudioCmd;
    use crate::audio::state::SharedPlayback;

    #[test]
    fn every_setting_lands_without_a_track() {
        // The idle loop and the test tone take these through
        // `apply_setting`; each used to be dropped there.
        let shared = SharedPlayback::new();
        for cmd in [
            AudioCmd::SetVolume(0.25),
            AudioCmd::SetNormalize(true),
            AudioCmd::SetMono(true),
            AudioCmd::SetCrossfade(4_000),
            AudioCmd::SetReplayGain(true),
            AudioCmd::SetGapless(false),
            AudioCmd::SetSpeed(1.5),
            AudioCmd::SetBalance(-40),
            AudioCmd::SetStereoWidth(150),
        ] {
            assert!(apply_setting(&cmd, &shared), "{cmd:?} not applied");
        }
        assert_eq!(shared.stereo.balance(), -40);
        assert_eq!(shared.stereo.width(), 150);
        assert!((shared.volume() - 0.25).abs() < f32::EPSILON);
        assert!(shared.normalize_enabled.load(Ordering::Acquire));
        assert!(shared.mono_enabled.load(Ordering::Acquire));
        assert_eq!(shared.crossfade_ms.load(Ordering::Acquire), 4_000);
        assert!(shared.replaygain_enabled.load(Ordering::Acquire));
        assert!(!shared.gapless_enabled.load(Ordering::Acquire));
        assert!((shared.playback_speed() - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn a_setting_switched_back_follows_the_last_command() {
        // A reset sends the default after the user's own value; the
        // engine must end on the second, as the row does.
        let shared = SharedPlayback::new();
        assert!(apply_setting(&AudioCmd::SetMono(true), &shared));
        assert!(apply_setting(&AudioCmd::SetMono(false), &shared));
        assert!(!shared.mono_enabled.load(Ordering::Acquire));
    }

    #[test]
    fn an_idle_speed_change_leaves_the_clock_alone() {
        // Nothing decodes: no rebase, no rebuild requested, and the speed
        // still clamped to what the resampler supports.
        let shared = SharedPlayback::new();
        let generation = shared.seek_generation.load(Ordering::Acquire);
        assert!(apply_setting(&AudioCmd::SetSpeed(3.0), &shared));
        assert!((shared.playback_speed() - 2.0).abs() < f32::EPSILON);
        assert!(!shared.speed_dirty.load(Ordering::Acquire));
        assert_eq!(shared.seek_generation.load(Ordering::Acquire), generation);
        assert!(apply_setting(&AudioCmd::SetSpeed(f32::NAN), &shared));
        assert!((shared.playback_speed() - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_command_that_is_not_a_setting_is_left_to_the_caller() {
        let shared = SharedPlayback::new();
        assert!(!apply_setting(&AudioCmd::Pause, &shared));
        assert!(!apply_setting(&AudioCmd::Stop, &shared));
        assert!(!apply_setting(&AudioCmd::Seek(1_000), &shared));
    }
}

#[cfg(test)]
mod load_ordering_tests {
    use std::path::PathBuf;

    use super::accept_load;
    use crate::audio::engine::{AudioCmd, LoadIntent};
    use crate::audio::replay_gain::TrackGain;
    use crate::audio::state::SharedPlayback;

    fn load(intent: u64, track_id: i64) -> AudioCmd {
        AudioCmd::LoadAndPlay {
            intent: LoadIntent::from_raw(intent),
            path: PathBuf::from("/dev/null"),
            start_ms: 0,
            track_id,
            duration_ms: 1_000,
            source_type: "test".into(),
            source_id: None,
            replay_gain: TrackGain::default(),
        }
    }

    #[test]
    fn the_first_load_of_a_session_is_accepted() {
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(1, 42), &shared));
    }

    #[test]
    fn a_preparation_that_finishes_late_is_dropped() {
        // Issue #622 verbatim. Play from a stopped player claims intent 1
        // and goes to the database; Next claims intent 2, prepares faster
        // and reaches the decoder first; Play's load arrives last and used
        // to win, so the track the user had left behind played.
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(2, 42), &shared));
        assert!(!accept_load(&load(1, 7), &shared));
    }

    #[test]
    fn the_newest_load_still_wins_when_it_arrives_last() {
        // The ordinary case, which must keep working: the newer intent is
        // also the later arrival and takes over from the older one.
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(1, 7), &shared));
        assert!(accept_load(&load(2, 42), &shared));
    }

    #[test]
    fn the_same_intent_passes_a_second_time() {
        // A load the drain stashed in `pending_cmd` is examined again at
        // the top of the decoder loop. Re-checking must not drop it.
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(3, 42), &shared));
        assert!(accept_load(&load(3, 42), &shared));
    }

    #[test]
    fn an_intent_claimed_but_never_sent_blocks_nothing() {
        // Next on an exhausted queue claims intent 2 and returns without
        // sending anything. Intent 1, still preparing, is the only load
        // there will be — arbitrating against the allocation counter
        // instead of against what arrived would have silenced it.
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(1, 7), &shared));
    }

    #[test]
    fn commands_that_are_not_loads_are_never_arbitrated() {
        // Pause, Seek and friends act on whatever is current, so they are
        // always meant for the state they find.
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(5, 42), &shared));
        assert!(accept_load(&AudioCmd::Pause, &shared));
        assert!(accept_load(&AudioCmd::Seek(1_000), &shared));
        assert!(accept_load(&AudioCmd::Stop, &shared));
        assert!(accept_load(&AudioCmd::SetVolume(0.5), &shared));
        // ...and none of them moved the high-water mark.
        assert!(!accept_load(&load(4, 7), &shared));
    }

    #[test]
    fn every_older_intent_stays_dropped() {
        let shared = SharedPlayback::new();
        assert!(accept_load(&load(10, 42), &shared));
        for stale in 1..10 {
            assert!(
                !accept_load(&load(stale, 7), &shared),
                "intent {stale} must stay superseded by 10"
            );
        }
        assert!(accept_load(&load(11, 99), &shared));
    }
}
