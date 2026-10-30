//! What the play loops share around the samples themselves: the commands
//! read between buffers, load ordering, the next-track slot, the tail
//! drain and the clock resets.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};
use rtrb::Producer;
use tauri::AppHandle;
use waveflow_core::playback::stream::ActiveStream;

use super::super::engine::AudioCmd;
use super::super::replay_gain::TrackGain;
use super::super::state::{PlayerState, SharedPlayback};
use super::tone::test_tone_yields;
use super::{transition_state, FinishedTrack, PlaybackEnd};

/// The outcome of a play cut short by `Shutdown`: credited like any other
/// interruption, with the `Shutdown` it consumed queued again so the outer
/// loop ends the thread once that credit is sent. The exit path waits for
/// the thread to finish, then for the analytics task to write what it was
/// sent; without the re-queue the thread went back to waiting for a command
/// and the exit could not tell when the credit had left.
pub(super) fn shutdown_outcome(
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

pub(super) fn finished_from(stream: &ActiveStream) -> FinishedTrack {
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
pub(super) fn drain_ring_silent(producer: &Producer<f32>, shared: &SharedPlayback) {
    if producer.slots() == super::super::output::RING_CAPACITY {
        return;
    }
    shared.drain_silent.store(true, Ordering::Release);
    let start = Instant::now();
    while producer.slots() < super::super::output::RING_CAPACITY
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
pub(super) fn reset_resampler_for_seek(
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
pub(super) fn reset_clock(shared: &SharedPlayback, ms: u64) {
    shared.restart_clock(ms, 0, false);
    shared.seek_generation.fetch_add(1, Ordering::Release);
}

/// Samples written to the ring that the output has not played yet.
pub(super) fn queued_samples(producer: &Producer<f32>) -> u64 {
    (producer.buffer().capacity() - producer.slots()) as u64
}

/// How often the end of a track is checked while the output plays it out.
pub(super) const TAIL_POLL: Duration = Duration::from_millis(10);

/// How long the end of a track may sit in the ring with the output taking
/// none of it, while not paused, before the track is declared over anyway:
/// a device that went away must not hold an end forever.
pub(super) const TAIL_STALL_LIMIT: Duration = Duration::from_millis(1500);

/// A gap between two checks longer than this means the loop was elsewhere
/// (blocked in `drain_commands` by a pause until Resume, or a slow command),
/// not watching an output that took nothing: the stall clock starts over.
const TAIL_MAX_POLL_GAP: Duration = Duration::from_millis(250);

/// The end of a track, decoded and still in the ring, being played out
/// before the end is declared (#932): what was queued at the last sign of
/// progress, when that was, and when the ring was last looked at.
#[derive(Debug)]
pub(super) struct TailDrain {
    queued: u64,
    progressed_at: Instant,
    polled_at: Instant,
}

impl TailDrain {
    /// Whether the end of the track is still being played: something is
    /// queued, and the output is taking it or is paused. False once the
    /// ring is empty, or when it has not moved for [`TAIL_STALL_LIMIT`]
    /// of watching it play.
    pub(super) fn still_playing(
        slot: &mut Option<Self>,
        queued: u64,
        paused: bool,
        now: Instant,
    ) -> bool {
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

pub(super) enum ControlFlow {
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
pub(super) fn accept_load(cmd: &AudioCmd, shared: &SharedPlayback) -> bool {
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
pub(super) fn drain_commands(
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
            Ok(cmd @ (AudioCmd::SetNextTrack { .. } | AudioCmd::ClearNextTrack { .. })) => {
                take_next(cmd, pending_next, shared, track_id);
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
                        Ok(
                            cmd @ (AudioCmd::SetNextTrack { .. } | AudioCmd::ClearNextTrack { .. }),
                        ) => take_next(cmd, pending_next, shared, track_id),
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

/// What the decoder does with a next track it is handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NextVerdict {
    /// Not for this transition: prepared for a track no longer playing,
    /// or arriving once the fade has started.
    Drop,
    /// The track already prepared: keep its stream, take the new hints.
    Keep,
    /// Open it in place of what is prepared, if anything.
    Replace,
}

/// The rule for a `SetNextTrack` prepared to follow `after`, offering
/// `offered`, while `playing` plays with `prepared` ready behind it.
///
/// A next is for the track it was prepared after: once that one has been
/// replaced (a load, a gapless or crossfade hand-off) it is dropped, so a
/// late answer never sets up a transition out of a track it was not
/// worked out for. And once a fade has started, the track fading in is
/// the next one: a later offer changes nothing.
pub(super) fn judge_next(
    after: i64,
    offered: i64,
    playing: i64,
    mixing: bool,
    prepared: Option<i64>,
) -> NextVerdict {
    if after != playing || mixing {
        NextVerdict::Drop
    } else if prepared == Some(offered) {
        NextVerdict::Keep
    } else {
        NextVerdict::Replace
    }
}

/// Act on a `SetNextTrack` or a `ClearNextTrack` for the track
/// `playing`: [`judge_next`], then store the stream and the hints that
/// came with it, so the two always describe the same pair.
fn take_next(
    cmd: AudioCmd,
    pending_next: &mut Option<ActiveStream>,
    shared: &SharedPlayback,
    playing: i64,
) {
    let mixing = shared.mixing.load(Ordering::Acquire);
    let prepared = pending_next.as_ref().map(|s| s.track_id);
    match cmd {
        AudioCmd::ClearNextTrack { after } => {
            if after == playing && !mixing {
                if prepared.is_some() {
                    tracing::info!(after, ?prepared, "next track dropped: the queue has none");
                    *pending_next = None;
                }
                clear_next_hints(shared);
            }
        }
        AudioCmd::SetNextTrack {
            after,
            path,
            track_id,
            duration_ms,
            source_type,
            source_id,
            replay_gain,
            same_album,
            crossfade_ms,
            automix_fade,
        } => {
            let verdict = judge_next(after, track_id, playing, mixing, prepared);
            match verdict {
                NextVerdict::Drop => {
                    tracing::debug!(after, track_id, playing, mixing, "next track dropped");
                    return;
                }
                NextVerdict::Keep => {}
                NextVerdict::Replace => {
                    if let Some(old) = prepared {
                        tracing::info!(old, track_id, "next track replaced: the queue changed");
                    }
                    // A replacement that fails to open must not leave the
                    // old one behind: the queue no longer names it.
                    *pending_next = None;
                    store_next(
                        pending_next,
                        path,
                        track_id,
                        duration_ms,
                        source_type,
                        source_id,
                        replay_gain,
                        shared.playback_speed(),
                        shared.dsd_taps.load(Ordering::Acquire) as usize,
                    );
                }
            }
            // Hints describe the stream prepared: none when the
            // replacement failed to open.
            if pending_next.is_none() {
                clear_next_hints(shared);
                return;
            }
            shared
                .pending_next_same_album
                .store(same_album, Ordering::Release);
            shared
                .pending_next_crossfade_ms
                .store(crossfade_ms, Ordering::Release);
            shared.automix.set_plan(after, track_id, automix_fade);
        }
        _ => {}
    }
}

/// No next track prepared: no hints either, so the dynamic length or the
/// album flag of one dropped never times or shapes the next transition.
fn clear_next_hints(shared: &SharedPlayback) {
    shared
        .pending_next_same_album
        .store(false, Ordering::Release);
    shared.pending_next_crossfade_ms.store(0, Ordering::Release);
    shared.automix.clear_plan();
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

/// Drop a reproducible copy that would not play.
///
/// Only ever called with `allowed` set for a stream-cache entry: the server
/// still holds those bytes, so a file that fails to open or decode is worth
/// losing to make the next play refetch it. A reconciled file from the user's
/// own library reaches the same failure path and must survive it — it is
/// theirs, and a decoder that deletes a listener's music because a codec
/// tripped would be a far worse bug than a cache miss.
pub(super) fn discard_unplayable(path: &std::path::Path, allowed: bool) {
    if !allowed {
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!(path = %path.display(), "dropped unplayable cached stream"),
        Err(err) => tracing::warn!(?err, path = %path.display(), "could not drop cached stream"),
    }
}
