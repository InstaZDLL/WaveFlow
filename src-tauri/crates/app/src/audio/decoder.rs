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
    // Mirror the transition to the OS media overlay so the SMTC /
    // MPRIS play/pause icon flips at the same moment the in-app one
    // does. `Loading` is intentionally skipped — it's a brief
    // transient that would render as `Stopped` on the overlay and
    // flash the controls off for ~50 ms before Playing arrives.
    if !matches!(state, PlayerState::Loading) {
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
                            ErrorPayload {
                                message: format!("audio decoder crashed: {message}"),
                            },
                        );
                        transition_state(&shared, &app, PlayerState::Idle, None);
                        if panic_count >= MAX_DECODER_PANICS {
                            tracing::error!(
                                panic_count,
                                "audio decoder thread panicked repeatedly, stopping recovery"
                            );
                            let _ = app.emit(
                                EVENT_ERROR,
                                ErrorPayload {
                                    message: "audio decoder stopped after repeated crashes"
                                        .to_string(),
                                },
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
                // A-B loop is a single-track concern — clear it on
                // every fresh load so the new track doesn't inherit
                // the previous track's loop endpoints.
                shared.clear_ab_loop();

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
                shared.current_track_id.store(track_id, Ordering::Release);

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
                    cmd_rx,
                    &app,
                    &mut pending_cmd,
                    analytics_tx,
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
            AudioCmd::SwapProducer(new_producer) => {
                // The output thread was rebuilt on a different cpal
                // device; the consumer half of the old ring went with
                // it, so any further `producer.push()` would silently
                // discard samples. Replace the local handle so the
                // next `LoadAndPlay` writes to the live ring.
                *producer = new_producer;
                tracing::info!("decoder picked up new ring producer");
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
///
/// Yes, this takes a lot of parameters — but each one is load-bearing
/// (initial track context + the I/O + sync handles the decoder loop
/// needs) and folding them into a struct just to satisfy a lint would
/// obscure the call site without changing what the function does.
#[allow(clippy::too_many_arguments)]
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
    // 6-band EQ processor — owns per-channel biquad state. The
    // shared atomics live on `shared.eq`; this struct just caches
    // coefficients and runs the per-sample filter chain. Cheap to
    // construct (no heap until the first packet) so creating it per
    // play_track is fine.
    let mut eq_processor = super::eq::EqProcessor::new();
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
                stream.seek_ms(ms);
                reset_clock(shared, ms);
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
                    stream.seek_ms(a_ms);
                    reset_clock(shared, a_ms);
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
                let pos = shared.current_position_ms();
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
                let pos = shared.current_position_ms();
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
            let mix_frames = if primary_at_eof {
                secondary_frames.min(remaining_window)
            } else {
                primary_frames.min(secondary_frames)
            };
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
                        let s = secondary_resampled[f * dst_channels + ch] * g_in;
                        mix_scratch.push(p + s);
                    }
                    mix_frames_written += 1;
                }
                // Drain only what each buffer actually contributed,
                // i.e. clamp to the per-buffer length — the primary
                // buffer may be empty in the EOF tail.
                let primary_consumed = mix_frames.min(primary_frames);
                primary_resampled.drain(..primary_consumed * dst_channels);
                secondary_resampled.drain(..mix_frames * dst_channels);

                eq_processor.process(
                    &mut mix_scratch,
                    dst_channels,
                    dst_sample_rate as f32,
                    &shared.eq,
                );
                spectrum_analyzer.feed(
                    &mix_scratch,
                    dst_channels,
                    dst_sample_rate as f32,
                    shared,
                    app,
                );
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
                        stream.seek_ms(ms);
                        reset_clock(shared, ms);
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
                shared
                    .current_track_id
                    .store(stream.track_id, Ordering::Release);
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

                    shared.samples_played.store(0, Ordering::Relaxed);
                    shared.base_offset_ms.store(0, Ordering::Relaxed);
                    shared
                        .current_track_id
                        .store(stream.track_id, Ordering::Release);
                    shared.seek_generation.fetch_add(1, Ordering::Release);

                    let _ = app.emit(EVENT_POSITION, PositionPayload { ms: 0 });
                    last_position_emit = Instant::now();
                    continue;
                }
                ended_naturally = true;
                break 'pkt;
            }
            eq_processor.process(
                &mut primary_resampled,
                dst_channels,
                dst_sample_rate as f32,
                &shared.eq,
            );
            spectrum_analyzer.feed(
                &primary_resampled,
                dst_channels,
                dst_sample_rate as f32,
                shared,
                app,
            );
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
                    stream.seek_ms(ms);
                    reset_clock(shared, ms);
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
/// Also briefly forces `paused_output = true` so the cpal callback
/// writes silence for the duration of the spin-wait (#173). Without
/// that, a callback whose `drain_silent` atomic load was hoisted
/// above the store below — or whose buffer was already mid-fill
/// when the seek landed — can leak ~5-20 ms of pre-seek samples
/// past the drain. The previous `paused_output` value is restored
/// at the end so a user who paused before seeking stays paused.
fn drain_ring_silent(producer: &Producer<f32>, shared: &SharedPlayback) {
    if producer.slots() == super::output::RING_CAPACITY {
        return;
    }
    let was_paused = shared.paused_output.swap(true, Ordering::AcqRel);
    shared.drain_silent.store(true, Ordering::Release);
    let start = Instant::now();
    while producer.slots() < super::output::RING_CAPACITY
        && start.elapsed() < Duration::from_millis(500)
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    shared.drain_silent.store(false, Ordering::Release);
    // Restore the pre-call paused_output value so a deliberate
    // user-driven pause isn't lifted by a downstream seek/A-B wrap.
    shared.paused_output.store(was_paused, Ordering::Release);
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
    stream: &mut super::crossfade::ActiveStream,
    shared: &SharedPlayback,
    dst_sample_rate: u32,
    dst_channels: usize,
) {
    let speed = shared.playback_speed();
    if let Err(err) = stream.rebuild_resampler(speed, dst_sample_rate, dst_channels) {
        tracing::warn!(?err, "resampler rebuild on seek failed; falling back to flush");
        stream.resampler.flush();
    }
}

/// Reset the position counters so the UI clock jumps to `ms`. Must be
/// called after `format.seek()` to keep `SharedPlayback::current_position_ms`
/// in sync.
fn reset_clock(shared: &SharedPlayback, ms: u64) {
    shared.samples_played.store(0, Ordering::Relaxed);
    shared.base_offset_ms.store(ms, Ordering::Release);
    shared.seek_generation.fetch_add(1, Ordering::Release);
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
                    shared.playback_speed(),
                );
            }
            Ok(AudioCmd::SetReplayGain(on)) => {
                shared.replaygain_enabled.store(on, Ordering::Release)
            }
            Ok(AudioCmd::SetGapless(on)) => shared.gapless_enabled.store(on, Ordering::Release),
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
                            replay_gain_db,
                        }) => store_next(
                            pending_next,
                            path,
                            next_id,
                            duration_ms,
                            source_type,
                            source_id,
                            replay_gain_db,
                            shared.playback_speed(),
                        ),
                        Ok(AudioCmd::SetReplayGain(on)) => {
                            shared.replaygain_enabled.store(on, Ordering::Release)
                        }
                        Ok(AudioCmd::SetGapless(on)) => {
                            shared.gapless_enabled.store(on, Ordering::Release)
                        }
                        Ok(AudioCmd::SetSpeed(v)) => shared.set_playback_speed(v),
                        Ok(cmd @ AudioCmd::LoadAndPlay { .. }) => {
                            shared.paused_output.store(false, Ordering::Release);
                            *pending_cmd = Some(cmd);
                            return ControlFlow::LoadNext;
                        }
                        Ok(AudioCmd::Pause) => {}
                        // SwapProducer can't reach this loop in
                        // practice — `set_output_device` always sends
                        // a `Stop` first, which breaks us out before
                        // the new producer arrives — but the match
                        // has to be exhaustive. Drop it on the floor;
                        // the producer goes out of scope and tears
                        // the orphaned ring down.
                        Ok(AudioCmd::SwapProducer(_)) => {}
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
    speed: f32,
) {
    match ActiveStream::open(
        &path,
        track_id,
        duration_ms,
        source_type,
        source_id,
        replay_gain_db,
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
                    match drain_commands(cmd_rx, shared, app, track_id, pending_cmd, pending_next) {
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
            for &s in input.iter().take(frames) {
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
