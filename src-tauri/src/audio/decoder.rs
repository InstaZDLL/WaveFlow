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

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use rtrb::{chunks::ChunkError, CopyToUninit, Producer};
use serde::Serialize;
use symphonia::core::formats::{SeekMode, SeekTo};
use symphonia::core::units::Time;
use tauri::{AppHandle, Emitter};

use tokio::sync::mpsc::UnboundedSender;

use super::analytics::AnalyticsMsg;
use super::crossfade::{equal_power_gains, ActiveStream};
use super::engine::AudioCmd;
use super::state::{PlayerState, SharedPlayback};

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
}

/// Transition [`SharedPlayback`] state and emit a `player:state` event
/// in one place so the UI always sees transitions in order.
fn transition_state(
    shared: &SharedPlayback,
    app: &AppHandle,
    state: PlayerState,
    track_id: Option<i64>,
) {
    shared.set_state(state);
    let _ = app.emit(
        EVENT_STATE,
        StatePayload {
            state: state.as_str(),
            track_id,
        },
    );
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
    mut producer: Producer<f32>,
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    analytics_tx: UnboundedSender<AnalyticsMsg>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("waveflow-audio-decoder".into())
        .spawn(move || {
            decoder_loop(cmd_rx, &mut producer, shared, app, analytics_tx);
        })
}

/// Top-level decoder thread loop. Never returns except on `Shutdown`.
fn decoder_loop(
    cmd_rx: Receiver<AudioCmd>,
    producer: &mut Producer<f32>,
    shared: Arc<SharedPlayback>,
    app: AppHandle,
    analytics_tx: UnboundedSender<AnalyticsMsg>,
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

        match cmd {
            AudioCmd::LoadAndPlay {
                path,
                start_ms,
                track_id,
                duration_ms,
                source_type,
                source_id,
                replay_gain_db,
            } => {
                transition_state(&shared, &app, PlayerState::Loading, Some(track_id));
                // Drain whatever's left of the previous track's
                // samples from the rtrb ring so the new track doesn't
                // start with the tail of the old one.
                //
                // Strategy: engage `drain_silent` mode on the cpal
                // callback — it pops the ring AND writes zeros, so
                // the tail of the old track never reaches the
                // device. Clear any lingering `paused_output` flag
                // so the callback actually runs (otherwise we'd
                // deadlock). Spin-wait on `producer.slots()` until
                // the ring is empty (bounded at 500 ms), then drop
                // the drain flag before we start pushing new samples.
                if producer.slots() != super::output::RING_CAPACITY {
                    shared.paused_output.store(false, Ordering::Release);
                    shared.drain_silent.store(true, Ordering::Release);
                    let start = std::time::Instant::now();
                    while producer.slots() < super::output::RING_CAPACITY
                        && start.elapsed() < Duration::from_millis(500)
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    shared.drain_silent.store(false, Ordering::Release);
                } else {
                    // Ring already empty — just make sure we're not
                    // stuck paused from a previous state.
                    shared.paused_output.store(false, Ordering::Release);
                }
                // Reset position counters so the UI clock starts from 0
                // (or from start_ms on a mid-track resume).
                shared.samples_played.store(0, Ordering::Relaxed);
                shared.base_offset_ms.store(start_ms, Ordering::Relaxed);
                shared
                    .current_track_id
                    .store(track_id, Ordering::Release);

                let outcome = play_track(
                    &path,
                    start_ms,
                    track_id,
                    duration_ms,
                    source_type.clone(),
                    source_id,
                    replay_gain_db,
                    producer,
                    &shared,
                    &cmd_rx,
                    &app,
                    &mut pending_cmd,
                    &analytics_tx,
                );
                match outcome {
                    Ok((PlaybackEnd::Natural, listened_ms, finished)) => {
                        let completed =
                            listened_ms + 2000 >= finished.duration_ms && finished.duration_ms > 0;
                        tracing::info!(
                            finished_track_id = finished.track_id,
                            listened_ms,
                            completed,
                            "play_track ended naturally"
                        );
                        let _ = analytics_tx.send(AnalyticsMsg::TrackEnded {
                            track_id: finished.track_id,
                            completed,
                            listened_ms,
                            source_type: finished.source_type,
                            source_id: finished.source_id,
                        });
                    }
                    Ok((PlaybackEnd::Interrupted, listened_ms, finished)) => {
                        tracing::info!(
                            finished_track_id = finished.track_id,
                            listened_ms,
                            will_credit = listened_ms >= 15_000,
                            "play_track interrupted"
                        );
                        if listened_ms >= 15_000 {
                            let _ = analytics_tx.send(AnalyticsMsg::TrackListened {
                                track_id: finished.track_id,
                                listened_ms,
                                source_type: finished.source_type,
                                source_id: finished.source_id,
                            });
                        }
                    }
                    Err(err) => {
                        tracing::warn!(?err, path = %path.display(), "playback failed");
                        let _ = app.emit(
                            EVENT_ERROR,
                            ErrorPayload {
                                message: err.clone(),
                            },
                        );
                        transition_state(&shared, &app, PlayerState::Idle, Some(track_id));
                    }
                }
            }
            AudioCmd::Shutdown => return,
            // All other commands are no-ops when no track is playing —
            // they're only meaningful inside `play_track`'s inner loop.
            _ => {}
        }
    }
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
fn play_track(
    initial_path: &Path,
    initial_start_ms: u64,
    initial_track_id: i64,
    initial_duration_ms: u64,
    initial_source_type: String,
    initial_source_id: Option<i64>,
    initial_replay_gain_db: Option<f64>,
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

    let mut stream = ActiveStream::open(
        initial_path,
        initial_track_id,
        initial_duration_ms,
        initial_source_type,
        initial_source_id,
        initial_replay_gain_db,
    )?;
    if initial_start_ms > 0 {
        apply_seek(&mut stream.format, stream.symphonia_track_id, initial_start_ms);
    }

    tracing::info!(
        dst_sample_rate,
        dst_channels,
        path = %initial_path.display(),
        "decoding start"
    );

    // Crossfade locals — reset on every track swap.
    let mut pending_next: Option<ActiveStream> = None;
    let mut next_requested = false;
    let mut mix_active = false;
    let mut mix_frames_written: u64 = 0;
    let mut mix_frames_total: u64 = 0;
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
    let mut last_position_emit = Instant::now();
    // Minimum frames each side should hold before mixing — keeps
    // both buffers topped up so a slow decoder doesn't starve the mix.
    const CROSSFADE_MIN_FRAMES: usize = 1024;

    transition_state(shared, app, PlayerState::Playing, Some(stream.track_id));

    let mut ended_naturally = false;

    'pkt: loop {
        match drain_commands(
            cmd_rx,
            shared,
            app,
            stream.track_id,
            pending_cmd,
            &mut pending_next,
        ) {
            ControlFlow::Continue => {}
            ControlFlow::Break => break 'pkt,
            ControlFlow::Shutdown => {
                transition_state(shared, app, PlayerState::Idle, Some(stream.track_id));
                return Ok((
                    PlaybackEnd::Interrupted,
                    shared.session_listened_ms(),
                    finished_from(&stream),
                ));
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
                apply_seek(&mut stream.format, stream.symphonia_track_id, ms);
                reset_clock(shared, ms);
                stream.resampler.flush();
                stream.decoder.reset();
                drain_ring_silent(producer, shared);
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                last_position_emit = Instant::now();
                continue;
            }
        }

        // Crossfade trigger evaluation, only when not already mixing.
        if !mix_active && stream.duration_ms > 0 {
            let cf_ms = shared.crossfade_ms.load(Ordering::Relaxed) as u64;
            if cf_ms > 0 {
                // Effective fade window — never longer than half the
                // track, so a 30 s clip with crossfade=12 s doesn't
                // start mixing at the 18 s mark.
                let effective_ms = cf_ms.min(stream.duration_ms / 2);
                let pos = shared.current_position_ms();
                let remaining = stream.duration_ms.saturating_sub(pos);

                // Pre-fetch ~500 ms before the window starts so the
                // file open + first packet decode have time to complete.
                if !next_requested
                    && pending_next.is_none()
                    && remaining <= effective_ms + 500
                {
                    let _ = analytics_tx.send(AnalyticsMsg::PrefetchNext);
                    next_requested = true;
                }

                if pending_next.is_some() && remaining <= effective_ms {
                    mix_active = true;
                    mix_frames_written = 0;
                    mix_frames_total =
                        (effective_ms * dst_sample_rate as u64) / 1000;
                }
            }
        }

        if mix_active {
            // Top up the primary buffer (one packet per iteration is
            // enough — the resampler may yield fewer frames than the
            // packet held, so we keep going until we hit the minimum
            // or EOF).
            while !primary_at_eof
                && primary_resampled.len() / dst_channels < CROSSFADE_MIN_FRAMES
            {
                let prev_len = primary_resampled.len();
                primary_at_eof = stream.decode_next(
                    &mut primary_resampled,
                    &mut interleaved_scratch,
                    dst_sample_rate,
                    dst_channels,
                )?;
                apply_replay_gain(
                    &mut primary_resampled[prev_len..],
                    shared,
                    stream.replay_gain_linear,
                );
            }
            let secondary = pending_next
                .as_mut()
                .expect("mix_active requires pending_next");
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
                apply_replay_gain(
                    &mut secondary_resampled[prev_len..],
                    shared,
                    secondary.replay_gain_linear,
                );
            }

            let primary_frames = primary_resampled.len() / dst_channels;
            let secondary_frames = secondary_resampled.len() / dst_channels;
            let mix_frames = primary_frames.min(secondary_frames);
            let denom = mix_frames_total.max(1) as f32;

            if mix_frames > 0 {
                mix_scratch.clear();
                mix_scratch.reserve(mix_frames * dst_channels);
                for f in 0..mix_frames {
                    let t = (mix_frames_written as f32 / denom).min(1.0);
                    let (g_out, g_in) = equal_power_gains(t);
                    for ch in 0..dst_channels {
                        let p = primary_resampled[f * dst_channels + ch] * g_out;
                        let s = secondary_resampled[f * dst_channels + ch] * g_in;
                        mix_scratch.push(p + s);
                    }
                    mix_frames_written += 1;
                }
                // Drop only what we mixed; surplus stays for next iter.
                primary_resampled.drain(..mix_frames * dst_channels);
                secondary_resampled.drain(..mix_frames * dst_channels);

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
                        transition_state(shared, app, PlayerState::Idle, Some(stream.track_id));
                        return Ok((
                            PlaybackEnd::Interrupted,
                            shared.session_listened_ms(),
                            finished_from(&stream),
                        ));
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
                        apply_seek(&mut stream.format, stream.symphonia_track_id, ms);
                        reset_clock(shared, ms);
                        stream.resampler.flush();
                        stream.decoder.reset();
                        drain_ring_silent(producer, shared);
                        let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                        last_position_emit = Instant::now();
                        continue;
                    }
                }
            }

            // Swap when the primary is fully drained or the fade
            // window has elapsed. We also swap if both sides EOF'd
            // simultaneously and there's nothing left to mix —
            // otherwise we'd loop forever with mix_frames == 0.
            let primary_drained = primary_at_eof && primary_resampled.is_empty();
            let window_done = mix_frames_written >= mix_frames_total;
            let dead_loop = mix_frames == 0 && primary_at_eof && secondary_at_eof;
            if primary_drained || window_done || dead_loop {
                let listened = shared.session_listened_ms();
                let _ = analytics_tx.send(AnalyticsMsg::CrossfadeStarted {
                    finished_track_id: stream.track_id,
                    finished_listened_ms: listened,
                    finished_source_type: stream.source_type.clone(),
                    finished_source_id: stream.source_id,
                });

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

                shared.samples_played.store(0, Ordering::Relaxed);
                shared.base_offset_ms.store(0, Ordering::Relaxed);
                shared.current_track_id.store(stream.track_id, Ordering::Release);
                shared.seek_generation.fetch_add(1, Ordering::Release);

                let _ = app.emit(EVENT_POSITION, PositionPayload { ms: 0 });
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
                apply_replay_gain(
                    &mut primary_resampled[prev_len..],
                    shared,
                    stream.replay_gain_linear,
                );
            }
            if primary_resampled.is_empty() && primary_at_eof {
                ended_naturally = true;
                break 'pkt;
            }
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
                    transition_state(shared, app, PlayerState::Idle, Some(stream.track_id));
                    return Ok((
                        PlaybackEnd::Interrupted,
                        shared.session_listened_ms(),
                        finished_from(&stream),
                    ));
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
                    apply_seek(&mut stream.format, stream.symphonia_track_id, ms);
                    reset_clock(shared, ms);
                    stream.resampler.flush();
                    stream.decoder.reset();
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
fn drain_ring_silent(producer: &Producer<f32>, shared: &SharedPlayback) {
    if producer.slots() == super::output::RING_CAPACITY {
        return;
    }
    shared.drain_silent.store(true, Ordering::Release);
    let start = Instant::now();
    while producer.slots() < super::output::RING_CAPACITY
        && start.elapsed() < Duration::from_millis(500)
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    shared.drain_silent.store(false, Ordering::Release);
}

/// Reset the position counters so the UI clock jumps to `ms`. Must be
/// called after `format.seek()` to keep `SharedPlayback::current_position_ms`
/// in sync.
fn reset_clock(shared: &SharedPlayback, ms: u64) {
    shared.samples_played.store(0, Ordering::Relaxed);
    shared.base_offset_ms.store(ms, Ordering::Release);
    shared.seek_generation.fetch_add(1, Ordering::Release);
}

/// Apply a seek to the format reader. Errors are logged and ignored —
/// seeking past EOF on a VBR MP3, for instance, is not fatal.
fn apply_seek(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    symphonia_track_id: u32,
    ms: u64,
) {
    let time = Time::from(Duration::from_millis(ms));
    if let Err(err) = format.seek(
        SeekMode::Accurate,
        SeekTo::Time {
            time,
            track_id: Some(symphonia_track_id),
        },
    ) {
        tracing::warn!(?err, ms, "format seek failed");
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
            Ok(cmd @ AudioCmd::LoadAndPlay { .. }) => {
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
                replay_gain_db,
            }) => {
                store_next(
                    pending_next,
                    path,
                    next_id,
                    duration_ms,
                    source_type,
                    source_id,
                    replay_gain_db,
                );
            }
            Ok(AudioCmd::SetReplayGain(on)) => {
                shared.replaygain_enabled.store(on, Ordering::Release)
            }
            Ok(AudioCmd::Pause) => {
                transition_state(shared, app, PlayerState::Paused, Some(track_id));
                shared.paused_output.store(true, Ordering::Release);
                let mut pending_seek: Option<u64> = None;
                loop {
                    match cmd_rx.recv() {
                        Ok(AudioCmd::Resume) => {
                            shared.paused_output.store(false, Ordering::Release);
                            transition_state(
                                shared,
                                app,
                                PlayerState::Playing,
                                Some(track_id),
                            );
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
                            replay_gain_db,
                        }) => store_next(
                            pending_next,
                            path,
                            next_id,
                            duration_ms,
                            source_type,
                            source_id,
                            replay_gain_db,
                        ),
                        Ok(AudioCmd::SetReplayGain(on)) => {
                            shared.replaygain_enabled.store(on, Ordering::Release)
                        }
                        Ok(cmd @ AudioCmd::LoadAndPlay { .. }) => {
                            shared.paused_output.store(false, Ordering::Release);
                            *pending_cmd = Some(cmd);
                            return ControlFlow::LoadNext;
                        }
                        Ok(AudioCmd::Pause) => {}
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
            Ok(_) => {}
            Err(TryRecvError::Empty) => return ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => return ControlFlow::Shutdown,
        }
    }
}

/// Multiply the trailing `len_before..` slice of a freshly-decoded
/// buffer by the active stream's stored ReplayGain factor, gated on
/// the live `replaygain_enabled` toggle. Done here (rather than in the
/// cpal callback) so each stream gets its own gain even during the
/// crossfade dual-decoder mix, where two tracks with different gains
/// are summed before reaching the ring.
#[inline]
fn apply_replay_gain(buf: &mut [f32], shared: &SharedPlayback, gain: f32) {
    if gain == 1.0 || !shared.replaygain_enabled.load(Ordering::Relaxed) {
        return;
    }
    for s in buf.iter_mut() {
        *s *= gain;
    }
}

/// Open the supplied next-track file into an [`ActiveStream`] and
/// stash it for the crossfade pipeline. Failures are logged but
/// non-fatal — playback continues without crossfade.
fn store_next(
    pending_next: &mut Option<ActiveStream>,
    path: std::path::PathBuf,
    track_id: i64,
    duration_ms: u64,
    source_type: String,
    source_id: Option<i64>,
    replay_gain_db: Option<f64>,
) {
    match ActiveStream::open(
        &path,
        track_id,
        duration_ms,
        source_type,
        source_id,
        replay_gain_db,
    ) {
        Ok(s) => {
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

/// Convert interleaved `src_chans`-wide samples to interleaved
/// `dst_chans`-wide samples in `out`. Simple fixed rules:
/// - Equal counts: copy verbatim
/// - mono (1) → stereo (2): duplicate
/// - stereo (2) → mono (1): average
/// - ≥3 → 2: Lo/Ro downmix on the first 6 channels (ITU BS.775)
/// - anything else: take the first `min(src, dst)` channels, pad zeros
pub(super) fn convert_channels(
    input: &[f32],
    src_chans: usize,
    dst_chans: usize,
    out: &mut Vec<f32>,
) {
    if src_chans == 0 || dst_chans == 0 {
        return;
    }
    if src_chans == dst_chans {
        out.extend_from_slice(input);
        return;
    }
    let frames = input.len() / src_chans;
    match (src_chans, dst_chans) {
        (1, 2) => {
            out.reserve(frames * 2);
            for f in 0..frames {
                let s = input[f];
                out.push(s);
                out.push(s);
            }
        }
        (2, 1) => {
            out.reserve(frames);
            for f in 0..frames {
                out.push(0.5 * (input[f * 2] + input[f * 2 + 1]));
            }
        }
        // 5.1 → stereo Lo/Ro (ITU-R BS.775): L' = L + 0.707*C + 0.707*Ls
        (s, 2) if s >= 6 => {
            const K: f32 = 0.707;
            out.reserve(frames * 2);
            for f in 0..frames {
                let base = f * s;
                let l = input[base];
                let r = input[base + 1];
                let c = input[base + 2];
                // LFE (base+3) skipped; Ls/Rs at base+4/5
                let ls = input[base + 4];
                let rs = input[base + 5];
                out.push(l + K * c + K * ls);
                out.push(r + K * c + K * rs);
            }
        }
        _ => {
            // Fallback: truncate or zero-pad.
            out.reserve(frames * dst_chans);
            for f in 0..frames {
                for ch in 0..dst_chans {
                    let v = if ch < src_chans {
                        input[f * src_chans + ch]
                    } else {
                        0.0
                    };
                    out.push(v);
                }
            }
        }
    }
}
