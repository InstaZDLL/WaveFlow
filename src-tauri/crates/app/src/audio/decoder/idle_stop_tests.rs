use std::sync::atomic::Ordering;
use std::time::Duration;

use super::idle_stop_transition;
use crate::audio::engine::wait_for_idle;
use crate::audio::state::{PlayerState, SharedPlayback};

#[test]
fn a_stop_takes_an_ended_player_to_idle() {
    // Past a gapless hand-off the track that ran out is not the one that was
    // loaded, so the id is the live one.
    let shared = SharedPlayback::new();
    shared.set_state(PlayerState::Ended);
    shared.current_track_id.store(42, Ordering::Release);
    assert_eq!(idle_stop_transition(&shared), Some((PlayerState::Idle, 42)));
}

#[test]
fn a_stop_leaves_every_other_state_where_it_is() {
    // `Idle` has nothing to announce again. The others reach this loop only on
    // their way to something else, a rebuild or a resume, and what takes them
    // there reads them or settles them itself.
    let shared = SharedPlayback::new();
    for state in [
        PlayerState::Idle,
        PlayerState::Loading,
        PlayerState::Playing,
        PlayerState::Paused,
    ] {
        shared.set_state(state);
        assert_eq!(idle_stop_transition(&shared), None, "{state:?}");
    }
}

#[tokio::test]
async fn the_wait_of_a_reset_ends_once_the_stop_has_moved_the_state() {
    // A reset after the last track of a queue: nothing but this move gets the
    // wait out, and it used to run to its timeout before wiping the data.
    let shared = SharedPlayback::new();
    shared.set_state(PlayerState::Ended);
    assert!(wait_for_idle(&shared, Duration::from_millis(50))
        .await
        .is_err());

    let (state, _) = idle_stop_transition(&shared).expect("a stop answers an ended player");
    shared.set_state(state);
    assert!(wait_for_idle(&shared, Duration::from_millis(50))
        .await
        .is_ok());
}
