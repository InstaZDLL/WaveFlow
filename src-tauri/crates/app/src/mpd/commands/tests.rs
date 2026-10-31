use super::*;

#[test]
fn repeat_maps_onto_mpds_two_flags() {
    assert_eq!(repeat_flags(RepeatMode::Off), (false, false));
    assert_eq!(repeat_flags(RepeatMode::All), (true, false));
    // "Repeat one" is `repeat 1` + `single 1` in MPD.
    assert_eq!(repeat_flags(RepeatMode::One), (true, true));
}

#[test]
fn toggling_repeat_preserves_single() {
    // The client only touched the repeat bit, so a queue already in
    // "repeat one" must not silently downgrade to "repeat all".
    assert_eq!(repeat_from_mpd(RepeatMode::One, true), RepeatMode::One);
    assert_eq!(repeat_from_mpd(RepeatMode::Off, true), RepeatMode::All);
    assert_eq!(repeat_from_mpd(RepeatMode::One, false), RepeatMode::Off);
}

#[test]
fn toggling_single_preserves_repeat() {
    assert_eq!(single_from_mpd(RepeatMode::All, true), RepeatMode::One);
    // Turning single off from "repeat one" leaves repeat on, which
    // is what MPD's own flags do.
    assert_eq!(single_from_mpd(RepeatMode::One, false), RepeatMode::All);
    assert_eq!(single_from_mpd(RepeatMode::Off, false), RepeatMode::Off);
}

#[test]
fn single_oneshot_is_rejected_not_persisted_as_durable() {
    // `single 0` / `single 1` resolve to a durable repeat mode…
    assert_eq!(
        single_action(SingleMode::On, RepeatMode::All),
        Some(RepeatMode::One)
    );
    assert_eq!(
        single_action(SingleMode::Off, RepeatMode::One),
        Some(RepeatMode::All)
    );
    // …but `single oneshot` has no durable equivalent, so it is rejected
    // (None → the handler ACKs "unsupported") rather than silently stored
    // as a permanent `single 1`.
    assert_eq!(single_action(SingleMode::OneShot, RepeatMode::All), None);
    assert_eq!(single_action(SingleMode::OneShot, RepeatMode::Off), None);
}

#[test]
fn repeat_flags_round_trip_through_both_setters() {
    for mode in [RepeatMode::Off, RepeatMode::All, RepeatMode::One] {
        let (repeat, single) = repeat_flags(mode);
        let after_repeat = repeat_from_mpd(mode, repeat);
        let after_both = single_from_mpd(after_repeat, single);
        assert_eq!(after_both, mode, "{mode:?} did not survive a round trip");
    }
}

#[test]
fn seek_targets_keep_their_fractional_seconds() {
    // The bug this guards: `seconds as u64 * 1000` floors first, so
    // a half-second seek target collapsed to the whole second.
    assert_eq!(seconds_to_ms(12.5), 12_500);
    assert_eq!(seconds_to_ms(0.25), 250);
    assert_eq!(seconds_to_ms(0.0), 0);
    // MPD never sends a negative absolute target, but clamping
    // beats wrapping to u64::MAX if one arrives.
    assert_eq!(seconds_to_ms(-5.0), 0);
}

#[test]
fn advertised_commands_are_sorted_and_unique() {
    // `commands` output feeds client capability detection; a
    // duplicate would be harmless but signals a bad merge.
    let mut sorted = SUPPORTED_COMMANDS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.as_slice(), SUPPORTED_COMMANDS);
}
