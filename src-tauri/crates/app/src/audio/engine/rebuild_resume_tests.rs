use super::super::replay_gain::TrackGain;
use super::super::state::PlayerState;
use super::rebuild::{endpoint_moved, rebuild_resume, RebuildResume};
use super::{pause_pending_after, AudioCmd, LoadIntent};

#[test]
fn a_session_that_was_playing_picks_back_up() {
    // By the time the rebuild runs, `notify_device_lost` has already
    // parked a playing session as `Paused` without raising
    // `paused_output`.
    assert_eq!(
        rebuild_resume(PlayerState::Paused, false),
        RebuildResume::Play
    );
    assert_eq!(
        rebuild_resume(PlayerState::Playing, false),
        RebuildResume::Play
    );
}

#[test]
fn a_session_the_user_paused_stays_paused() {
    // #611: resuming this one started the music on whatever device the
    // system fell back to.
    assert_eq!(
        rebuild_resume(PlayerState::Paused, true),
        RebuildResume::StayPaused
    );
}

#[test]
fn a_paused_radio_session_stays_paused_too() {
    // It did not, until #617: parking it left Play to the persisted
    // resume point, which is always a library track's, so the station
    // came back as the last local track. The park now holds the load
    // itself, so what the user paused is what Play picks up — and a
    // device flap no longer restarts a stream they had stopped.
    assert_eq!(
        rebuild_resume(PlayerState::Playing, true),
        RebuildResume::StayPaused
    );
}

#[test]
fn nothing_loaded_means_nothing_to_resume() {
    // `Loading` is deliberately not here — see below.
    for state in [PlayerState::Idle, PlayerState::Ended] {
        assert_eq!(rebuild_resume(state, false), RebuildResume::Nothing);
    }
}

#[test]
fn a_track_still_loading_is_still_a_session() {
    // The decoder is inside `play_track` from the moment it accepts
    // a load, so a track picked between the rebuild's `Stop` and its
    // swap is one this answer decides for. The swap now ends that
    // track instead of being dropped (#639), which leaves the
    // session loaded and stopped — `Nothing` would be the one answer
    // that never starts it again.
    assert_eq!(
        rebuild_resume(PlayerState::Loading, false),
        RebuildResume::Play
    );
    assert_eq!(
        rebuild_resume(PlayerState::Loading, true),
        RebuildResume::StayPaused
    );
}

#[test]
fn a_pause_the_decoder_has_not_reached_yet_still_counts() {
    // `paused_output` is the decoder's answer and it lags: a rebuild
    // reading it in the gap decided `Play` for a session the user had
    // just paused, and its resume then cleared the flag and started
    // the music. The engine reads both, and this is the half it owns.
    assert!(pause_pending_after(&AudioCmd::Pause, false));
    assert!(pause_pending_after(&AudioCmd::Pause, true));
}

#[test]
fn anything_that_asks_for_playback_clears_it() {
    assert!(!pause_pending_after(&AudioCmd::Resume, true));
    // A load is a request to play — the same moment the decoder
    // clears its own `paused_output`.
    let load = AudioCmd::LoadAndPlay {
        intent: LoadIntent::from_raw(1),
        path: std::path::PathBuf::from("/dev/null"),
        start_ms: 0,
        track_id: 42,
        duration_ms: 1000,
        source_type: "album".into(),
        source_id: None,
        replay_gain: TrackGain::default(),
    };
    assert!(!pause_pending_after(&load, true));
}

#[test]
fn a_stop_decides_nothing_about_the_pause() {
    // The rebuilds send one, and it says nothing about whether the
    // user wants to hear anything afterwards. Clearing on it would
    // undo the pause this flag exists to carry.
    assert!(pause_pending_after(&AudioCmd::Stop, true));
    assert!(!pause_pending_after(&AudioCmd::Stop, false));
    assert!(pause_pending_after(&AudioCmd::Seek(1_000), true));
    assert!(pause_pending_after(&AudioCmd::SetVolume(0.5), true));
}

#[test]
fn coming_back_on_the_same_endpoint_is_not_a_move() {
    // A device that flaps and comes back: the automatic recovery #175
    // exists for, and nothing #617 should pause.
    assert!(!endpoint_moved(Some("USB DAC"), Some("USB DAC")));
}

#[test]
fn landing_on_another_endpoint_is_a_move() {
    // The reported case: headphones off, Windows falls back to the
    // built-in speakers.
    assert!(endpoint_moved(Some("Headphones"), Some("Speakers")));
}

#[test]
fn an_endpoint_nobody_can_name_is_never_a_move() {
    // Hog mode can name nothing at all, and the two sides do not
    // always come from the same backend accessor. A wrong "moved"
    // stops the music for a user who asked for none of this; a wrong
    // "not moved" only costs the pause, leaving the behaviour that
    // shipped before #617.
    assert!(!endpoint_moved(None, Some("Speakers")));
    assert!(!endpoint_moved(Some("Headphones"), None));
    assert!(!endpoint_moved(None, None));
}
