use super::SharedPlayback;

#[test]
fn the_first_claim_of_a_session_succeeds() {
    let shared = SharedPlayback::new();
    assert!(shared.try_claim_load(1));
}

#[test]
fn an_older_intent_cannot_claim_after_a_newer_one() {
    // #632: the older producer is about to write the queue cursor and
    // relabel the player bar for a load the decoder would drop. It has
    // to find out here, before any of that.
    let shared = SharedPlayback::new();
    assert!(shared.try_claim_load(7));
    assert!(!shared.try_claim_load(5));
}

#[test]
fn the_same_intent_can_claim_twice() {
    // `player_play_tracks` claims before filling the queue and again
    // before publishing, because reads separate the two.
    let shared = SharedPlayback::new();
    assert!(shared.try_claim_load(4));
    assert!(shared.try_claim_load(4));
}

#[test]
fn a_failed_claim_leaves_the_mark_alone() {
    let shared = SharedPlayback::new();
    assert!(shared.try_claim_load(9));
    assert!(!shared.try_claim_load(2));
    // Still 9, so the load that did claim is still the newest.
    assert!(shared.try_claim_load(9));
}

#[test]
fn a_newer_intent_takes_the_claim_over() {
    let shared = SharedPlayback::new();
    assert!(shared.try_claim_load(3));
    assert!(shared.try_claim_load(4));
    assert!(!shared.try_claim_load(3));
}
