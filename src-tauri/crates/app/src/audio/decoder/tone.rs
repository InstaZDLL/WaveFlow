//! Playing the output test tone through the open output, and what the
//! Settings card is told while it plays.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::{Receiver, TryRecvError};
use rtrb::Producer;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use super::super::engine::AudioCmd;
use super::super::state::{PlayerState, SharedPlayback};
use super::dop::restore_pcm_output;
use super::{apply_setting, transition_state, unpause_and_drain_ring};

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
        sample_format: super::super::state::OutputSampleFormat,
        /// Mono output folds left and right together, so the card says
        /// why both beeps sound alike.
        mono: bool,
    },
    /// A beep starts playing — counted at the output, not when pushed.
    Phase {
        phase: super::super::test_tone::TonePhase,
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
pub(super) fn test_tone_yields(app: &AppHandle) {
    tracing::debug!("test tone asked for while a track plays; the track wins");
    let _ = app.emit(EVENT_TEST_TONE, TestTonePayload::Interrupted);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToneEnd {
    Done,
    Shutdown,
}

/// Play [`super::super::test_tone`] through the open output.
///
/// Its own loop rather than [`play_track`]: there is no file, no track to
/// credit, no position for the player bar, and nothing a rebuild should
/// replay. Commands are polled between chunks; a load or a `Stop` ends
/// the tone at once, and a load is handed back through `pending_cmd`.
pub(super) fn play_test_tone(
    producer: &mut Producer<f32>,
    cmd_rx: &Receiver<AudioCmd>,
    shared: &SharedPlayback,
    app: &AppHandle,
    pending_cmd: &mut Option<AudioCmd>,
) -> ToneEnd {
    use super::super::test_tone;

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
        let queued = ((super::super::output::RING_CAPACITY - producer.slots()) / width) as u64;
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
                super::super::stereo::mono_fold(&mut chunk[..frames * width], width);
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
