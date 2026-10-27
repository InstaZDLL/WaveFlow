use super::*;

fn line(start: Option<i64>, value: &str) -> LyricsLineDto {
    LyricsLineDto {
        start,
        value: value.to_string(),
    }
}
fn track(synced: bool, lines: Vec<LyricsLineDto>) -> StructuredLyricsDto {
    StructuredLyricsDto { synced, lines }
}

#[test]
fn lrc_stamp_covers_boundaries() {
    assert_eq!(lrc_stamp(0), "[00:00.00]");
    assert_eq!(lrc_stamp(999), "[00:00.99]");
    assert_eq!(lrc_stamp(1000), "[00:01.00]");
    assert_eq!(lrc_stamp(61_230), "[01:01.23]");
    // Negatives are clamped to zero rather than producing a bogus stamp.
    assert_eq!(lrc_stamp(-5), "[00:00.00]");
}

#[test]
fn fully_stamped_track_renders_synced_lrc() {
    let got = structured_to_lyrics(&[track(
        true,
        vec![line(Some(0), "one"), line(Some(1500), "two")],
    )])
    .unwrap();
    assert!(got.synced);
    assert_eq!(got.content, "[00:00.00]one\n[00:01.50]two");
}

#[test]
fn partially_stamped_track_falls_back_to_plain_keeping_all_lines() {
    let got = structured_to_lyrics(&[track(true, vec![line(Some(0), "one"), line(None, "two")])])
        .unwrap();
    assert!(!got.synced);
    // Both lines survive, without any `[mm:ss.xx]` tag.
    assert_eq!(got.content, "one\ntwo");
}

#[test]
fn a_complete_synced_track_wins_over_an_earlier_partial_one() {
    let got = structured_to_lyrics(&[
        track(true, vec![line(Some(0), "partial"), line(None, "gap")]),
        track(
            true,
            vec![line(Some(0), "full"), line(Some(1000), "stamped")],
        ),
    ])
    .unwrap();
    assert!(got.synced);
    assert_eq!(got.content, "[00:00.00]full\n[00:01.00]stamped");
}

#[test]
fn no_usable_content_returns_none() {
    assert!(structured_to_lyrics(&[]).is_none());
    assert!(structured_to_lyrics(&[track(false, vec![line(None, "   ")])]).is_none());
}

#[test]
fn trailing_spaces_and_blank_lines_survive() {
    let got = structured_to_lyrics(&[track(
        false,
        vec![line(None, "hello  "), line(None, ""), line(None, "")],
    )])
    .unwrap();
    // Only the loop's final '\n' is stripped: the trailing spaces on the
    // first line and the two blank lines are preserved.
    assert_eq!(got.content, "hello  \n\n");
}
