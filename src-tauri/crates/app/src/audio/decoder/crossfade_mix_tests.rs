use super::track::crossfade_mix_frames;

#[test]
fn both_playing_mixes_what_both_hold() {
    assert_eq!(crossfade_mix_frames(1_000, 400, false, false, 5_000), 400);
}

#[test]
fn an_outgoing_track_that_ended_is_silence_for_the_rest_of_the_window() {
    assert_eq!(crossfade_mix_frames(0, 800, true, false, 500), 500);
    // The incoming track holds less than the window has left: mix
    // that, and decode more of it next pass.
    assert_eq!(crossfade_mix_frames(0, 200, true, false, 500), 200);
}

/// The case that stalled playback: the incoming track ran out first.
#[test]
fn an_incoming_track_that_ran_out_lets_the_outgoing_one_finish_its_fade() {
    assert_eq!(crossfade_mix_frames(1_000, 0, false, true, 5_000), 1_000);
    assert_eq!(crossfade_mix_frames(1_000, 0, false, true, 300), 300);
}

#[test]
fn both_ended_runs_out_what_is_left_then_nothing() {
    assert_eq!(crossfade_mix_frames(300, 100, true, true, 5_000), 300);
    assert_eq!(crossfade_mix_frames(0, 0, true, true, 5_000), 0);
}
