//! Decoding one PCM track into the ring: the play loop with its crossfade
//! and gapless hand-off, and the per-buffer gain, clamp and push.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use rtrb::chunks::ChunkError;
use rtrb::{CopyToUninit, Producer};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedSender;
use waveflow_core::playback::stream::ActiveStream;

use super::super::analytics::AnalyticsMsg;
use super::super::crossfade::{MixWindow, AUTOMIX_ENTER_US};
use super::super::engine::AudioCmd;
use super::super::replay_gain::effective_linear;
use super::super::state::{PlayerState, SharedPlayback};
use super::control::{
    drain_commands, drain_ring_silent, finished_from, queued_samples, reset_clock,
    reset_resampler_for_seek, shutdown_outcome, ControlFlow, TailDrain, TAIL_POLL,
};
use super::{
    transition_state, FinishedTrack, PlaybackEnd, PositionPayload, TrackEndedPayload,
    EVENT_POSITION, EVENT_TRACK_ENDED, POSITION_EMIT_INTERVAL,
};

/// Result of [`push_samples`]. `Ok` means all samples were written;
/// any other variant signals that the outer loop should stop pushing
/// and react (propagate shutdown, end the track, apply a seek, or
/// hand off to a new load via `pending_cmd`).
pub(super) enum PushOutcome {
    Ok,
    Stop,
    Shutdown,
    Seek(u64),
    LoadNext,
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
/// [`super::super::http_source::HttpMediaSource`]). Hoisting the open out lets
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
pub(super) fn play_track(
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
    // How far before the asked position the primary's last seek landed:
    // the clock counts from the position asked for, the audio from the
    // packet that holds it. AutoMix places its fade on the track's own
    // time and takes this off the clock.
    let mut primary_slip_ms: u64 = 0;
    if initial_start_ms > 0 {
        primary_slip_ms = initial_start_ms.saturating_sub(stream.seek_ms(initial_start_ms));
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
    let mut mix_window = MixWindow::fixed(0);
    // Whether the incoming track's resampler delay has been taken off its
    // start (AutoMix): known once its first packet is decoded.
    let mut incoming_delay_taken = false;
    // Whether the analytics task has been asked for this primary's AutoMix
    // cue.
    let mut automix_cue_sent = false;
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
    let mut eq_processor = super::super::eq::EqProcessor::new();
    // Balance and stereo width, after the EQ. Keeps the gains the last
    // buffer ended on so a moved slider ramps instead of stepping.
    let mut stereo_processor = super::super::stereo::StereoProcessor::new();
    // Spectrum analyzer (FFT for the visualizer). Lives across both
    // the single-stream and crossfade-mix push paths; we feed it the
    // post-EQ buffer that's about to land in the ring. Cheap when the
    // visualizer toggle is off — feed() short-circuits on the atomic.
    let mut spectrum_analyzer = super::super::spectrum::SpectrumAnalyzer::new();
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
                primary_slip_ms = ms.saturating_sub(stream.seek_ms(ms));
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
                    primary_slip_ms = a_ms.saturating_sub(stream.seek_ms(a_ms));
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
                // AutoMix plans within the crossfade length, so it only
                // acts while one is set. A library track only: the plan
                // is made from its analysis row.
                let automix_on = shared.automix.is_enabled() && stream.track_id > 0;
                if automix_on && !automix_cue_sent {
                    let _ = analytics_tx.send(AnalyticsMsg::AutoMixCue {
                        track_id: stream.track_id,
                        duration_ms: stream.duration_ms,
                    });
                    automix_cue_sent = true;
                }
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
                // AutoMix asks at its cue instead when that comes first:
                // its fade can start before the usual window does.
                let automix_cue_reached = automix_on
                    && shared
                        .automix
                        .prefetch_at_ms(stream.track_id)
                        .is_some_and(|at| pos >= at);
                if !next_requested
                    && pending_next.is_none()
                    && (remaining <= effective_ms + 500 || automix_cue_reached)
                {
                    let _ = analytics_tx.send(AnalyticsMsg::PrefetchNext);
                    next_requested = true;
                }

                // Smart crossfade: when the prefetched track is from
                // the same album as the current one, suppress the
                // mix and fall through to the gapless EOF swap below.
                // The hint comes from analytics — set right before
                // SetNextTrack lands — so we only consult it once
                // pending_next is populated. AutoMix keeps the same
                // rule: an album plays through without a fade.
                let smart_skip = pending_next.is_some()
                    && (shared.smart_crossfade_enabled.load(Ordering::Relaxed) || automix_on)
                    && shared.pending_next_same_album.load(Ordering::Relaxed);

                // AutoMix: a plan for this pair places the fade on the
                // beats, and the plain trigger below waits for it.
                let plan = if automix_on && !smart_skip {
                    pending_next
                        .as_ref()
                        .and_then(|next| shared.automix.plan_for(stream.track_id, next.track_id))
                } else {
                    None
                };
                if let Some((fade_start_us, fade_end_us)) = plan {
                    let now_us = pos.saturating_sub(primary_slip_ms) * 1000;
                    if now_us + AUTOMIX_ENTER_US >= fade_start_us {
                        match MixWindow::automix(
                            fade_start_us,
                            fade_end_us,
                            now_us,
                            dst_sample_rate,
                            shared.playback_speed(),
                            stream.resampler.output_delay_frames(),
                        ) {
                            Some(window) => {
                                tracing::info!(
                                    track_id = stream.track_id,
                                    fade_start_us,
                                    fade_end_us,
                                    now_us,
                                    "automix: mixing on the beats"
                                );
                                mix_active = true;
                                mix_window = window;
                                incoming_delay_taken = false;
                                faded_track_ms = 0.0;
                                shared.pending_next_crossfade_ms.store(0, Ordering::Release);
                            }
                            None => {
                                // Already past the planned start (a seek
                                // over it): the plain fade takes this one.
                                tracing::info!(
                                    track_id = stream.track_id,
                                    fade_start_us,
                                    now_us,
                                    "automix: plan missed, plain crossfade"
                                );
                                shared.automix.clear_plan();
                            }
                        }
                    }
                } else if !smart_skip && pending_next.is_some() && remaining <= effective_ms {
                    mix_active = true;
                    faded_track_ms = 0.0;
                    mix_window = MixWindow::fixed((effective_ms * dst_sample_rate as u64) / 1000);
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
                // Its resampler exists from its first packet on, and puts
                // its delay ahead of the track's first sample: start it
                // that much earlier so the sample lands where planned.
                // Nothing to move for the plain fade, which starts the
                // incoming track on its first frame.
                if !incoming_delay_taken {
                    mix_window.start_incoming_earlier(secondary.resampler.output_delay_frames());
                    incoming_delay_taken = true;
                }
            }

            // Once the primary stream EOFs we must keep mixing — but
            // with a primary contribution of zero — for the remainder
            // of the fade window. Otherwise the swap-on-EOF path
            // collapses g_in from < 1 straight to "100% secondary at
            // full amplitude" mid-fade and produces an audible step.
            // Common when the outgoing track's true sample count
            // doesn't quite match the duration the fade was timed
            // against (DSD files in particular: the data chunk is
            // block-aligned, not duration-aligned).
            let (primary_consumed, secondary_consumed) = mix_window.mix(
                &primary_resampled,
                &secondary_resampled,
                primary_at_eof,
                secondary_at_eof,
                dst_channels,
                &mut mix_scratch,
            );
            let mix_frames = mix_scratch.len() / dst_channels;

            if mix_frames > 0 {
                // Drain only what each buffer actually contributed: the
                // primary buffer may be empty in the EOF tail, and the
                // incoming one gives nothing before its first frame.
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
                        primary_slip_ms = ms.saturating_sub(stream.seek_ms(ms));
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
            let window_done = mix_window.done();
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
                // The new primary plays from its first sample, and has not
                // been asked about yet.
                primary_slip_ms = 0;
                automix_cue_sent = false;

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
                    primary_slip_ms = 0;
                    automix_cue_sent = false;

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
                // A command already waiting — another track, a stop — is
                // served first, at the top of the loop. A live station
                // released for it (`live::release_starved_reader`) reads as
                // ended, and declaring that end would put a track-ended and
                // an advance ahead of what the user just asked for.
                if !cmd_rx.is_empty() {
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
                    primary_slip_ms = ms.saturating_sub(stream.seek_ms(ms));
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

/// Push all samples into the producer, sleeping briefly when the ring
/// is full. Commands are polled between retries so pause/stop/seek
/// still respond promptly even when the buffer is saturated.
///
/// rtrb 0.3's `write_chunk_uninit` either gives us the full `n` slots
/// we asked for or returns `TooFewSlots(available)`; in the latter case
/// we submit what fits, sleep, and loop. `CopyToUninit` handles the
/// actual `MaybeUninit<f32>` → `f32` copy without `unsafe` code.
pub(super) fn push_samples(
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
