use super::control::{judge_next, NextVerdict};

#[test]
fn a_different_track_replaces_the_one_prepared() {
    assert_eq!(judge_next(1, 5, 1, false, Some(2)), NextVerdict::Replace);
    // Nothing prepared yet: the first answer, or one after a clear.
    assert_eq!(judge_next(1, 5, 1, false, None), NextVerdict::Replace);
}

#[test]
fn the_same_track_keeps_its_stream() {
    assert_eq!(judge_next(1, 2, 1, false, Some(2)), NextVerdict::Keep);
}

/// Prepared for a track that has since been replaced (a load, or the
/// hand-off itself): never set up as the next of the track playing now.
#[test]
fn a_next_prepared_for_another_track_is_dropped() {
    assert_eq!(judge_next(1, 5, 2, false, None), NextVerdict::Drop);
    assert_eq!(judge_next(1, 5, 2, false, Some(3)), NextVerdict::Drop);
}

/// The fade has started: the track fading in is the next one.
#[test]
fn once_the_fade_has_started_nothing_changes() {
    assert_eq!(judge_next(1, 5, 1, true, Some(2)), NextVerdict::Drop);
    assert_eq!(judge_next(1, 2, 1, true, Some(2)), NextVerdict::Drop);
}
