//! The factor a music video multiplies the volume slider by
//! ([`SharedPlayback::clip_gain`]): what the engine puts on the track's
//! samples besides the slider.

use std::path::PathBuf;

use super::super::engine::{AudioCmd, DetachedOrigin, LoadIntent};
use super::super::replay_gain::{GainMode, TrackGain};
use super::*;
use crate::queue::ShuffleMode;

fn db(gain_db: f64) -> f32 {
    10f64.powf(gain_db / 20.0) as f32
}

fn close(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 1e-5
}

/// ReplayGain on, in `mode`, with no pre-amp, no fallback and clipping
/// prevention off, so each test reads one number.
fn replay_gain_on(mode: GainMode) -> SharedPlayback {
    let shared = SharedPlayback::new();
    shared.replaygain_enabled.store(true, Ordering::Relaxed);
    shared
        .replaygain_prevent_clipping
        .store(false, Ordering::Relaxed);
    shared
        .replaygain_mode_bits
        .store(mode.as_bits(), Ordering::Relaxed);
    shared
        .shuffle_mode_bits
        .store(ShuffleMode::Off.as_bits(), Ordering::Relaxed);
    shared
}

/// A track with a gain of its own and a quieter one for its record.
fn track_and_album() -> TrackGain {
    TrackGain {
        gain_db: Some(-6.0),
        album_gain_db: Some(-10.0),
        ..Default::default()
    }
}

fn album_load(track_id: i64, replay_gain: TrackGain) -> AudioCmd {
    AudioCmd::LoadAndPlay {
        intent: LoadIntent::from_raw(1),
        path: PathBuf::from("/dev/null"),
        start_ms: 0,
        track_id,
        duration_ms: 1000,
        source_type: "album".into(),
        source_id: Some(7),
        replay_gain,
    }
}

/// The playing track's clip takes the gain its stream is given: the
/// load's own gain under the load's own listening context (a record
/// playing through, so the album gain in auto mode) — not what the
/// library row alone would say.
#[test]
fn the_playing_tracks_clip_takes_the_gain_of_its_stream() {
    let shared = replay_gain_on(GainMode::Auto);
    shared.record_accepted_load(&album_load(42, track_and_album()));

    let gain = shared.clip_gain(42, Some(track_and_album()));
    assert!(close(gain, db(-10.0)), "album gain expected, got {gain}");
    assert!(close(
        gain,
        shared.replay_gain_linear(track_and_album(), "album")
    ));
}

/// A clip opened from another track's menu takes that track's gain, as one
/// track on its own: its track gain, even while shuffle plays records.
#[test]
fn another_tracks_clip_is_one_track_on_its_own() {
    let shared = replay_gain_on(GainMode::Auto);
    shared
        .shuffle_mode_bits
        .store(ShuffleMode::Albums.as_bits(), Ordering::Relaxed);
    shared.record_accepted_load(&album_load(42, TrackGain::default()));

    let gain = shared.clip_gain(7, Some(track_and_album()));
    assert!(close(gain, db(-6.0)), "track gain expected, got {gain}");
}

/// A server row the engine is not on is played with no measured gain:
/// the fallback and the pre-amp, as the engine would apply them.
#[test]
fn a_server_row_takes_the_fallback_and_the_pre_amp() {
    let shared = replay_gain_on(GainMode::Track);
    shared
        .replaygain_fallback_db_bits
        .store((-4.0f32).to_bits(), Ordering::Relaxed);
    shared
        .replaygain_preamp_db_bits
        .store(2.0f32.to_bits(), Ordering::Relaxed);

    assert!(close(shared.clip_gain(-1, None), db(-2.0)));
}

/// A server track the engine is playing takes the gain its load carries,
/// read as the decoder opens that stream.
#[test]
fn a_playing_server_track_takes_its_loads_gain() {
    let shared = replay_gain_on(GainMode::Track);
    shared.record_accepted_load(&AudioCmd::LoadRemoteFileAndPlay {
        intent: LoadIntent::from_raw(1),
        origin: DetachedOrigin::Remote,
        path: PathBuf::from("/dev/null"),
        start_ms: 0,
        track_id: -5,
        duration_ms: 1000,
        title: None,
        artist: None,
        artwork_url: None,
        fallback_url: None,
        discard_on_failure: false,
        replay_gain: TrackGain {
            gain_db: Some(-4.0),
            ..Default::default()
        },
    });

    assert!(close(shared.clip_gain(-5, None), db(-4.0)));
}

/// Normalization multiplies in, ReplayGain on or off.
#[test]
fn normalization_multiplies_in() {
    let shared = SharedPlayback::new();
    shared.normalize_enabled.store(true, Ordering::Relaxed);
    assert!(close(shared.clip_gain(1, None), NORMALIZE_GAIN));

    let shared = replay_gain_on(GainMode::Track);
    shared.normalize_enabled.store(true, Ordering::Relaxed);
    assert!(close(
        shared.clip_gain(1, Some(track_and_album())),
        db(-6.0) * NORMALIZE_GAIN
    ));
}

/// Nothing on: the slider alone.
#[test]
fn with_nothing_on_the_clip_follows_the_slider_alone() {
    let shared = SharedPlayback::new();
    shared.record_accepted_load(&album_load(42, track_and_album()));
    assert_eq!(shared.clip_gain(42, Some(track_and_album())), 1.0);
}

/// A boost comes back as one: capping it against the slider is the
/// element's business, since half the slider and +6 dB is full volume.
#[test]
fn a_boost_comes_back_above_unity() {
    let shared = replay_gain_on(GainMode::Track);
    shared
        .replaygain_preamp_db_bits
        .store(6.0f32.to_bits(), Ordering::Relaxed);
    let gain = shared.clip_gain(
        1,
        Some(TrackGain {
            gain_db: Some(0.0),
            ..Default::default()
        }),
    );
    assert!(close(gain, db(6.0)), "got {gain}");
}
