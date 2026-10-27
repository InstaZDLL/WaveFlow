use super::*;
use lofty::tag::{ItemValue, TagItem, TagType};

fn tag_with(items: &[(ItemKey, &str)]) -> Tag {
    let mut tag = Tag::new(TagType::VorbisComments);
    for (key, value) in items {
        tag.insert(TagItem::new(*key, ItemValue::Text((*value).to_string())));
    }
    tag
}

#[test]
fn a_textual_gain_is_read_with_or_without_its_unit() {
    for (raw, want) in [
        ("-7.89 dB", -7.89),
        ("-7.89dB", -7.89),
        ("-7.89", -7.89),
        ("+3.20 dB", 3.20),
        ("  0.00 DB  ", 0.0),
    ] {
        let tags = extract_replay_gain(&tag_with(&[(ItemKey::ReplayGainTrackGain, raw)]));
        let got = tags
            .track_gain_db
            .unwrap_or_else(|| panic!("{raw} parsed to nothing"));
        assert!((got - want).abs() < 1e-9, "{raw} parsed to {got}");
    }
}

#[test]
fn a_peak_is_linear_and_a_zero_peak_is_refused() {
    let tags = extract_replay_gain(&tag_with(&[(ItemKey::ReplayGainTrackPeak, "0.987654")]));
    assert_eq!(tags.track_peak, Some(0.987654));

    // Clipped masters really do exceed unity.
    let over = extract_replay_gain(&tag_with(&[(ItemKey::ReplayGainTrackPeak, "1.0234")]));
    assert_eq!(over.track_peak, Some(1.0234));

    for bogus in ["0", "0.0", "-0.5", "not a number", "120.0"] {
        let tags = extract_replay_gain(&tag_with(&[(ItemKey::ReplayGainTrackPeak, bogus)]));
        assert_eq!(tags.track_peak, None, "{bogus} should not parse");
    }
}

/// The conversion that keeps Opus files on the same scale as
/// everything else: Q7.8 LU against −23 LUFS, out as dB against
/// −18 LUFS.
#[test]
fn an_r128_gain_lands_on_the_replay_gain_scale() {
    // 0 LU below −23 LUFS is −18 + 5, i.e. a +5 dB boost here.
    let zero = extract_replay_gain(&tag_with(&[(ItemKey::R128TrackGain, "0")]));
    assert_eq!(zero.track_gain_db, Some(5.0));

    // −1536/256 = −6 LU → −6 + 5 = −1 dB.
    let quiet = extract_replay_gain(&tag_with(&[(ItemKey::R128TrackGain, "-1536")]));
    assert_eq!(quiet.track_gain_db, Some(-1.0));
}

/// A file carrying both pairs is tagged for two kinds of player;
/// the textual one is already on our scale, so it wins and no
/// conversion is involved.
#[test]
fn the_textual_gain_wins_over_r128() {
    let tags = extract_replay_gain(&tag_with(&[
        (ItemKey::ReplayGainTrackGain, "-4.00 dB"),
        (ItemKey::R128TrackGain, "0"),
    ]));
    assert_eq!(tags.track_gain_db, Some(-4.0));
}

#[test]
fn album_values_are_read_independently_of_track_values() {
    let tags = extract_replay_gain(&tag_with(&[
        (ItemKey::ReplayGainAlbumGain, "-6.50 dB"),
        (ItemKey::ReplayGainAlbumPeak, "0.995"),
    ]));
    assert_eq!(tags.album_gain_db, Some(-6.5));
    assert_eq!(tags.album_peak, Some(0.995));
    assert_eq!(tags.track_gain_db, None);
    assert!(!tags.is_empty());
}

#[test]
fn a_file_without_replay_gain_tags_reads_empty() {
    assert!(extract_replay_gain(&tag_with(&[])).is_empty());
}

/// A gain far outside the plausible range is a broken tag, and
/// handing it to the mixer would be a way to blow a pair of
/// speakers.
#[test]
fn an_implausible_gain_is_dropped() {
    for bogus in ["999 dB", "-500", "inf", "NaN", ""] {
        let tags = extract_replay_gain(&tag_with(&[(ItemKey::ReplayGainTrackGain, bogus)]));
        assert_eq!(tags.track_gain_db, None, "{bogus} should not parse");
    }
}
