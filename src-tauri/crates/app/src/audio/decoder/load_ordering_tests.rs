use std::path::PathBuf;

use super::accept_load;
use crate::audio::engine::{AudioCmd, LoadIntent};
use crate::audio::replay_gain::TrackGain;
use crate::audio::state::SharedPlayback;

fn load(intent: u64, track_id: i64) -> AudioCmd {
    AudioCmd::LoadAndPlay {
        intent: LoadIntent::from_raw(intent),
        path: PathBuf::from("/dev/null"),
        start_ms: 0,
        track_id,
        duration_ms: 1_000,
        source_type: "test".into(),
        source_id: None,
        replay_gain: TrackGain::default(),
    }
}

#[test]
fn the_first_load_of_a_session_is_accepted() {
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(1, 42), &shared));
}

#[test]
fn a_preparation_that_finishes_late_is_dropped() {
    // Issue #622 verbatim. Play from a stopped player claims intent 1
    // and goes to the database; Next claims intent 2, prepares faster
    // and reaches the decoder first; Play's load arrives last and used
    // to win, so the track the user had left behind played.
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(2, 42), &shared));
    assert!(!accept_load(&load(1, 7), &shared));
}

#[test]
fn the_newest_load_still_wins_when_it_arrives_last() {
    // The ordinary case, which must keep working: the newer intent is
    // also the later arrival and takes over from the older one.
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(1, 7), &shared));
    assert!(accept_load(&load(2, 42), &shared));
}

#[test]
fn the_same_intent_passes_a_second_time() {
    // A load the drain stashed in `pending_cmd` is examined again at
    // the top of the decoder loop. Re-checking must not drop it.
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(3, 42), &shared));
    assert!(accept_load(&load(3, 42), &shared));
}

#[test]
fn an_intent_claimed_but_never_sent_blocks_nothing() {
    // Next on an exhausted queue claims intent 2 and returns without
    // sending anything. Intent 1, still preparing, is the only load
    // there will be — arbitrating against the allocation counter
    // instead of against what arrived would have silenced it.
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(1, 7), &shared));
}

#[test]
fn commands_that_are_not_loads_are_never_arbitrated() {
    // Pause, Seek and friends act on whatever is current, so they are
    // always meant for the state they find.
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(5, 42), &shared));
    assert!(accept_load(&AudioCmd::Pause, &shared));
    assert!(accept_load(&AudioCmd::Seek(1_000), &shared));
    assert!(accept_load(&AudioCmd::Stop, &shared));
    assert!(accept_load(&AudioCmd::SetVolume(0.5), &shared));
    // ...and none of them moved the high-water mark.
    assert!(!accept_load(&load(4, 7), &shared));
}

#[test]
fn every_older_intent_stays_dropped() {
    let shared = SharedPlayback::new();
    assert!(accept_load(&load(10, 42), &shared));
    for stale in 1..10 {
        assert!(
            !accept_load(&load(stale, 7), &shared),
            "intent {stale} must stay superseded by 10"
        );
    }
    assert!(accept_load(&load(11, 99), &shared));
}
