use super::*;

#[test]
fn a_device_we_could_not_ask_is_not_remembered_as_such() {
    // The one caching rule that matters: a device held by another
    // client answers nothing *now*. Remembering that would make the
    // sheet say "unavailable" for the rest of the session, long after
    // the other client let go.
    let busy = DeviceCapabilities::unavailable(Some("hw:CARD=D50s".into()), "busy".into());
    assert!(!busy.is_answer());
}

#[test]
fn a_real_answer_is_remembered() {
    let answered = DeviceCapabilities {
        device_id: None,
        source: CapabilitySource::WasapiExclusive,
        formats: vec![DeviceFormat {
            label: "S24_4LE".into(),
            bits: 24,
            float: false,
            sample_rates: vec![44_100, 48_000],
        }],
        sample_rates: vec![44_100, 48_000],
        max_channels: 2,
        min_period_frames: Some(480),
        unavailable_reason: None,
    };
    assert!(answered.is_answer());
}

#[test]
fn the_default_device_has_a_key_of_its_own() {
    assert_ne!(memo_key(None), memo_key(Some("hw:CARD=PCH,DEV=0")));
}
