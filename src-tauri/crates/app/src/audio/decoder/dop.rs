//! Native DSD over DoP (#495): choosing the DoP output for a DSD file,
//! switching the device in and out of it, and playing the packed stream.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::Receiver;
use rtrb::Producer;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedSender;
use waveflow_core::audio_format::dsd::parser::{parse_dff, parse_dsf};
use waveflow_core::playback::stream::ActiveStream;

use super::super::analytics::AnalyticsMsg;
use super::super::engine::AudioCmd;
use super::super::output::DopFormat;
use super::super::state::{PlayerState, SharedPlayback};
use super::super::AudioEngine;
use super::control::{
    drain_commands, drain_ring_silent, finished_from, queued_samples, reset_clock,
    shutdown_outcome, ControlFlow, TailDrain, TAIL_POLL,
};
use super::track::{push_samples, PushOutcome};
use super::{
    transition_state, ErrorPayload, FinishedTrack, PlaybackEnd, PositionPayload, TrackEndedPayload,
    EVENT_ERROR, EVENT_POSITION, EVENT_TRACK_ENDED, POSITION_EMIT_INTERVAL,
};

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
pub(super) fn maybe_switch_dop_output(
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
    match engine.switch_output_for_track(want.map(super::super::output::RequestedFormat::dop)) {
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
pub(super) fn maybe_match_source_rate(
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
    match engine.switch_output_for_track(Some(super::super::output::RequestedFormat::source_rate(
        rate,
    ))) {
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
pub(super) fn restore_pcm_output(
    app: &AppHandle,
    producer: &mut Producer<f32>,
) -> Result<(), String> {
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
pub(super) fn play_dop_track(
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
            // A command already waiting goes first — see the PCM path.
            if !cmd_rx.is_empty() {
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
