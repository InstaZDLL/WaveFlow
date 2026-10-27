use super::{notice::output_mode_of, OutputMode};

#[test]
fn nothing_asked_for_is_shared() {
    assert_eq!(output_mode_of(false, false, false), OutputMode::Shared);
}

#[test]
fn asked_and_granted_is_exclusive() {
    assert_eq!(output_mode_of(true, true, false), OutputMode::Exclusive);
}

/// Across a negotiation, the cause that tells the user what to do wins:
/// a device in use refused as busy on one layout and on format on the
/// next is a busy device.
#[test]
fn the_most_telling_refusal_wins() {
    use super::ExclusiveRefusal::{self, *};
    assert!(Busy.outranks(Format));
    assert!(Busy.outranks(NotAllowed));
    assert!(NotAllowed.outranks(Format));
    assert!(Format.outranks(Unknown));
    assert!(!Format.outranks(Busy));
    assert!(!Busy.outranks(Busy));
    for reason in [Busy, NotAllowed, Format, Unknown] {
        assert_eq!(ExclusiveRefusal::from_u8(reason.as_u8()), Some(reason));
    }
    assert_eq!(ExclusiveRefusal::from_u8(0), None);
}

#[test]
fn asked_and_refused_is_its_own_state() {
    // The one #597 exists for: playback is fine, in shared mode, and
    // until now the only trace of the user's choice not being honoured
    // was a `tracing::warn!` line.
    assert_eq!(
        output_mode_of(true, false, false),
        OutputMode::ExclusiveRefused
    );
}

#[test]
fn dop_outranks_the_rest() {
    // On Linux and macOS the DoP toggle engages the exclusive path by
    // itself, so the exclusive preference can be off while the DAC is
    // being handed native DSD. Reporting "shared" there would be a lie
    // about the most demanding mode we have.
    assert_eq!(output_mode_of(false, true, true), OutputMode::Dop);
    assert_eq!(output_mode_of(true, true, true), OutputMode::Dop);
}
