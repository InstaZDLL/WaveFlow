use super::*;

fn layouts(candidates: &[LayoutCandidate]) -> Vec<(usize, usize, &'static str)> {
    candidates
        .iter()
        .map(|c| (c.sample_rate, c.channels, c.origin))
        .collect()
}

/// The #409 report: Windows shows the endpoint as stereo 44.1 kHz,
/// but the mix format comes back as 8 channels because the audio
/// engine has a virtual-surround effect in front of it. Probing
/// the mix format first is what made every format fail.
#[test]
fn endpoint_layout_is_tried_before_an_inflated_mix_format() {
    let candidates = build_layout_candidates(Some((44_100, 2)), Some((48_000, 8)));
    assert_eq!(
        layouts(&candidates),
        vec![
            (44_100, 2, "endpoint"),
            (48_000, 8, "mix"),
            (48_000, 2, "stereo"),
        ],
        "the endpoint's own format must be candidate #1"
    );
}

/// #600: the track's rate goes first, on every layout the device
/// offered, and the device's own rates stay behind it. That tail is
/// the fallback that makes this a preference rather than a demand.
#[test]
fn the_track_s_rate_leads_and_the_device_s_own_rates_follow() {
    let base = build_layout_candidates(Some((48_000, 2)), Some((48_000, 8)));
    let with_rate = layouts_at_requested_rate(&base, 96_000);
    assert_eq!(
        layouts(&with_rate),
        vec![
            (96_000, 2, "source-rate"),
            (96_000, 8, "source-rate"),
            (48_000, 2, "endpoint"),
            (48_000, 8, "mix"),
        ]
    );
}

/// A track already at the device's rate must not produce the same
/// layout twice: the probe is a COM round trip per pair, and a
/// duplicate is a rejection asked for twice.
#[test]
fn a_track_already_at_the_device_s_rate_adds_nothing() {
    let base = build_layout_candidates(Some((44_100, 2)), None);
    let with_rate = layouts_at_requested_rate(&base, 44_100);
    assert_eq!(layouts(&with_rate), vec![(44_100, 2, "source-rate")]);
}

/// The common case: nothing is intercepting the stream, both
/// queries agree. One probe, not two identical ones.
#[test]
fn agreeing_endpoint_and_mix_collapse_to_one_candidate() {
    let candidates = build_layout_candidates(Some((48_000, 2)), Some((48_000, 2)));
    assert_eq!(layouts(&candidates), vec![(48_000, 2, "endpoint")]);
}

/// Both sources inflated: the stereo entry is the only way out.
#[test]
fn stereo_closes_the_list_when_every_reported_layout_is_multichannel() {
    let candidates = build_layout_candidates(Some((48_000, 6)), Some((48_000, 8)));
    assert_eq!(
        layouts(&candidates),
        vec![
            (48_000, 6, "endpoint"),
            (48_000, 8, "mix"),
            (48_000, 2, "stereo"),
        ]
    );
}

/// Endpoint and mix disagree on the rate as well — stereo is
/// offered at both rates, endpoint's first.
#[test]
fn stereo_fallback_covers_both_reported_rates() {
    let candidates = build_layout_candidates(Some((96_000, 8)), Some((48_000, 8)));
    assert_eq!(
        layouts(&candidates),
        vec![
            (96_000, 8, "endpoint"),
            (48_000, 8, "mix"),
            (48_000, 2, "stereo"),
            (96_000, 2, "stereo"),
        ]
    );
}

/// A driver that doesn't publish `PKEY_AudioEngine_DeviceFormat`
/// must still get the pre-#409 behaviour rather than no candidate.
#[test]
fn missing_endpoint_format_falls_back_to_the_mix_format() {
    let candidates = build_layout_candidates(None, Some((44_100, 2)));
    assert_eq!(layouts(&candidates), vec![(44_100, 2, "mix")]);
}

#[test]
fn no_usable_format_yields_no_candidates() {
    assert!(build_layout_candidates(None, None).is_empty());
}

/// A malformed format must not reach WASAPI as a 0-channel or
/// 0 Hz stream request. A usable rate carried alongside a bogus
/// channel count still earns a stereo probe — that is the whole
/// point of the channel-axis fallback.
#[test]
fn degenerate_formats_are_discarded() {
    let candidates = build_layout_candidates(Some((0, 2)), Some((48_000, 0)));
    assert_eq!(layouts(&candidates), vec![(48_000, 2, "stereo")]);
}
