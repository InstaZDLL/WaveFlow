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
