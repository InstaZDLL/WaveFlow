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
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use rtrb::{chunks::ChunkError, CopyToUninit, Producer};
use serde::Serialize;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use tauri::{AppHandle, Emitter};

use tokio::sync::mpsc::UnboundedSender;

use super::analytics::AnalyticsMsg;
use super::engine::AudioCmd;
use super::resampler::Resampler;
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
                    producer,
                    &shared,
                    &cmd_rx,
                    &app,
                    &mut pending_cmd,
                );
                match outcome {
                    Ok((PlaybackEnd::Natural, listened_ms)) => {
                        // Only natural EOF triggers the auto-advance
                        // path. Analytics writes a play_event row
                        // and self-sends the next LoadAndPlay per
                        // queue + repeat.
                        let completed = listened_ms + 2000 >= duration_ms && duration_ms > 0;
                        tracing::info!(
                            track_id,
                            listened_ms,
                            completed,
                            "play_track ended naturally"
                        );
                        let _ = analytics_tx.send(AnalyticsMsg::TrackEnded {
                            track_id,
                            completed,
                            listened_ms,
                            source_type,
                            source_id,
                        });
                    }
                    Ok((PlaybackEnd::Interrupted, listened_ms)) => {
                        tracing::info!(
                            track_id,
                            listened_ms,
                            will_credit = listened_ms >= 15_000,
                            "play_track interrupted"
                        );
                        // User interrupted (Stop / LoadNext /
                        // Shutdown). Still credit the play if they
                        // heard ≥ 15 s — this is what makes the
                        // "Récemment joués" view populate when the
                        // user skips through tracks instead of
                        // letting them finish. No auto-advance: the
                        // new track is already queued via
                        // `pending_cmd`.
                        if listened_ms >= 15_000 {
                            let _ = analytics_tx.send(AnalyticsMsg::TrackListened {
                                track_id,
                                listened_ms,
                                source_type,
                                source_id,
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

/// Decode a single track start-to-finish, honoring any commands that
/// arrive on `cmd_rx` between packets. Emits `player:position` /
/// `player:state` / `player:track-ended` events via `app`.
///
/// Returns `(PlaybackEnd, listened_ms)`. The caller distinguishes
/// between a natural EOF (triggers analytics + auto-advance) and a
/// user-initiated interruption (just unwinds cleanly).
fn play_track(
    path: &Path,
    start_ms: u64,
    track_id: i64,
    duration_ms: u64,
    producer: &mut Producer<f32>,
    shared: &SharedPlayback,
    cmd_rx: &Receiver<AudioCmd>,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
) -> Result<(PlaybackEnd, u64), String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;

    let mut format = probed.format;
    let track_symphonia = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no decodable track found".to_string())?;
    let symphonia_track_id = track_symphonia.id;
    let codec_params = track_symphonia.codec_params.clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec init: {e}"))?;

    let dst_sample_rate = shared.sample_rate.load(Ordering::Relaxed);
    let dst_channels = shared.channels.load(Ordering::Relaxed) as usize;
    if dst_sample_rate == 0 || dst_channels == 0 {
        return Err("cpal output not initialized (sample_rate=0)".into());
    }

    tracing::info!(
        dst_sample_rate,
        dst_channels,
        path = %path.display(),
        "decoding start (src layout detected from first packet)"
    );

    // If the caller asked for a mid-track start (resume from persisted
    // position), apply an initial seek BEFORE entering the packet loop.
    if start_ms > 0 {
        apply_seek(&mut format, symphonia_track_id, start_ms);
    }

    // Source layout is discovered lazily from the first decoded
    // packet's `SignalSpec`, because:
    //   - AAC / M4A does not populate codec_params.channels (the info
    //     only lands in the decoded AudioBuffer);
    //   - AAC+SBR reports one sample_rate in codec_params and a
    //     different (doubled) rate at decode time;
    //   - some OGG/Vorbis streams similarly deliver channel counts
    //     after the first Setup packet.
    //
    // Keeping these `Option` until first decode lets every supported
    // codec go through the same path.
    let mut resampler: Option<Resampler> = None;
    let mut src_channels: usize = 0;

    let mut interleaved_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut resampled_scratch: Vec<f32> = Vec::with_capacity(8192);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut last_position_emit = Instant::now();

    transition_state(shared, app, PlayerState::Playing, Some(track_id));

    // Whether the loop exited for a "natural" EOF reason. Set to
    // true only after `format.next_packet()` returns EOF below.
    let mut ended_naturally = false;

    'pkt: loop {
        // Drain any pending commands between packets.
        match drain_commands(cmd_rx, shared, app, track_id, pending_cmd) {
            ControlFlow::Continue => {}
            ControlFlow::Break => break 'pkt,
            ControlFlow::Shutdown => {
                transition_state(shared, app, PlayerState::Idle, Some(track_id));
                return Ok((PlaybackEnd::Interrupted, shared.session_listened_ms()));
            }
            ControlFlow::LoadNext => {
                // The new load is in `*pending_cmd`. Bail out of this
                // track without emitting a TrackEnded — the outer
                // loop will start the new track from a clean state.
                return Ok((PlaybackEnd::Interrupted, shared.session_listened_ms()));
            }
            ControlFlow::Seek(ms) => {
                apply_seek(&mut format, symphonia_track_id, ms);
                reset_clock(shared, ms);
                if let Some(r) = resampler.as_mut() {
                    r.flush();
                }
                // Drop the decoder's internal state so the first decoded
                // packet after a seek doesn't carry pre-seek residue.
                decoder.reset();
                // Fire an immediate position event so the progress bar
                // snaps to the target without waiting for the next tick.
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                last_position_emit = Instant::now();
                continue;
            }
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                // End of stream — fall through to drain resampler.
                ended_naturally = true;
                break 'pkt;
            }
            Err(SymphoniaError::ResetRequired) => {
                ended_naturally = true;
                break 'pkt;
            }
            Err(e) => return Err(format!("next_packet: {e}")),
        };
        if packet.track_id() != symphonia_track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(e)) => {
                tracing::warn!(error = %e, "decode error, skipping packet");
                continue;
            }
            Err(e) => return Err(format!("decode fatal: {e}")),
        };

        // First packet: capture the real source layout from the
        // decoded buffer's SignalSpec, and lazily build the
        // SampleBuffer + Resampler now that we know the rate and
        // channel count.
        if sample_buf.is_none() {
            let spec = *decoded.spec();
            let capacity = decoded.capacity() as u64;
            sample_buf = Some(SampleBuffer::<f32>::new(capacity, spec));
            src_channels = spec.channels.count();
            let src_sample_rate = spec.rate;
            tracing::info!(
                src_sample_rate,
                src_channels,
                dst_sample_rate,
                dst_channels,
                path = %path.display(),
                "first packet decoded, resampler initialized"
            );
            match Resampler::new(src_sample_rate, dst_sample_rate, dst_channels) {
                Ok(r) => resampler = Some(r),
                Err(e) => return Err(format!("resampler init: {e}")),
            }
        }
        let sb = sample_buf.as_mut().unwrap();
        sb.copy_interleaved_ref(decoded);

        // Channel layout conversion: source -> destination channel count.
        interleaved_scratch.clear();
        convert_channels(sb.samples(), src_channels, dst_channels, &mut interleaved_scratch);

        // Resample if source and dest rates differ.
        resampled_scratch.clear();
        let resampler_ref = resampler
            .as_mut()
            .expect("resampler initialized on first packet");
        if let Err(e) = resampler_ref.process(&interleaved_scratch, &mut resampled_scratch) {
            return Err(format!("resample: {e}"));
        }

        // Push into the ring, blocking (with short sleeps) when full so
        // we never drop samples.
        match push_samples(
            &resampled_scratch,
            producer,
            cmd_rx,
            shared,
            app,
            track_id,
            pending_cmd,
        ) {
            PushOutcome::Ok => {}
            PushOutcome::Stop => break 'pkt,
            PushOutcome::Shutdown => {
                transition_state(shared, app, PlayerState::Idle, Some(track_id));
                return Ok((PlaybackEnd::Interrupted, shared.session_listened_ms()));
            }
            PushOutcome::LoadNext => {
                return Ok((PlaybackEnd::Interrupted, shared.session_listened_ms()));
            }
            PushOutcome::Seek(ms) => {
                apply_seek(&mut format, symphonia_track_id, ms);
                reset_clock(shared, ms);
                if let Some(r) = resampler.as_mut() {
                    r.flush();
                }
                decoder.reset();
                let _ = app.emit(EVENT_POSITION, PositionPayload { ms });
                last_position_emit = Instant::now();
                continue;
            }
        }

        // Throttled position event. 250ms cadence keeps the UI smooth
        // without flooding the event bus.
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

    // Flush any trailing resampler state so we don't tail-cut the track.
    if let Some(r) = resampler.as_mut() {
        r.flush();
    }
    // Session listened, not absolute track position: analytics
    // measures "how long did the user hear audio from this track
    // in this session", not "how far into the song did we reach".
    let listened_ms = shared.session_listened_ms();
    if ended_naturally {
        let completed = listened_ms + 2000 >= duration_ms && duration_ms > 0;
        let _ = app.emit(
            EVENT_TRACK_ENDED,
            TrackEndedPayload {
                track_id,
                completed,
                listened_ms,
            },
        );
        transition_state(shared, app, PlayerState::Ended, Some(track_id));
        Ok((PlaybackEnd::Natural, listened_ms))
    } else {
        // User-initiated Stop: leave state as-is for decoder_loop to
        // decide (most likely Idle). Do NOT emit TrackEnded — the
        // user didn't let the track finish.
        Ok((PlaybackEnd::Interrupted, listened_ms))
    }
}

/// Reset the position counters so the UI clock jumps to `ms`. Must be
/// called after `format.seek()` to keep `SharedPlayback::current_position_ms`
/// in sync. Note: the cpal callback may keep draining ~1 s of stale
/// samples still in the ring buffer before the new audio is audible —
/// acceptable gap for MVP.
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
) -> ControlFlow {
    loop {
        match cmd_rx.try_recv() {
            Ok(AudioCmd::Shutdown) => return ControlFlow::Shutdown,
            Ok(AudioCmd::Stop) => return ControlFlow::Break,
            Ok(AudioCmd::Seek(ms)) => return ControlFlow::Seek(ms),
            Ok(cmd @ AudioCmd::LoadAndPlay { .. }) => {
                // Stash the new load for `decoder_loop` to pick up on
                // its next iteration, and break out of the current
                // track. Any leftover paused-output flag is cleared
                // so the new track's samples aren't blocked.
                shared.paused_output.store(false, Ordering::Release);
                *pending_cmd = Some(cmd);
                return ControlFlow::LoadNext;
            }
            Ok(AudioCmd::Pause) => {
                transition_state(shared, app, PlayerState::Paused, Some(track_id));
                // Flip the callback's silencer flag. The cpal thread
                // will stop draining the ring within a few ms
                // (one callback period).
                shared.paused_output.store(true, Ordering::Release);
                let mut pending_seek: Option<u64> = None;
                // Block for the next command.
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
                        Ok(AudioCmd::Shutdown) => {
                            shared.paused_output.store(false, Ordering::Release);
                            return ControlFlow::Shutdown;
                        }
                        Ok(AudioCmd::Stop) => {
                            shared.paused_output.store(false, Ordering::Release);
                            return ControlFlow::Break;
                        }
                        Ok(AudioCmd::Seek(ms)) => pending_seek = Some(ms),
                        Ok(AudioCmd::SetVolume(v)) => shared.set_volume(v),
                        Ok(cmd @ AudioCmd::LoadAndPlay { .. }) => {
                            // User picked a new track while paused —
                            // stash for decoder_loop and exit pause.
                            shared.paused_output.store(false, Ordering::Release);
                            *pending_cmd = Some(cmd);
                            return ControlFlow::LoadNext;
                        }
                        Ok(AudioCmd::Pause) => {} // already paused, ignore
                        Err(_) => {
                            shared.paused_output.store(false, Ordering::Release);
                            return ControlFlow::Shutdown;
                        }
                    }
                }
                if let Some(ms) = pending_seek {
                    return ControlFlow::Seek(ms);
                }
            }
            Ok(AudioCmd::SetVolume(v)) => shared.set_volume(v),
            // Resume is a no-op when already playing.
            Ok(_) => {}
            Err(TryRecvError::Empty) => return ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => return ControlFlow::Shutdown,
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
                    match drain_commands(cmd_rx, shared, app, track_id, pending_cmd) {
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
fn convert_channels(
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
