use super::*;

#[test]
fn strip_header_drops_badge_title_and_lyrics_token() {
    let input = "17 ContributorsFall on Me Lyrics\n[Chorus]\nFall on me";
    assert_eq!(
        strip_genius_header(input),
        "[Chorus]\nFall on me".to_string()
    );
}

#[test]
fn strip_header_returns_empty_for_stub_pages() {
    // Genius renders this layout for songs added to the catalogue
    // without transcribed lyrics. The bare header is the only
    // content the parser sees, and the caller treats empty output
    // as a miss so the next provider gets a turn.
    let input = "2 ContributorsSolamente Una Vez Lyrics";
    assert_eq!(strip_genius_header(input), "".to_string());
}

#[test]
fn strip_header_preserves_leading_whitespace_outside_pattern() {
    // Genius occasionally pads the container with leading
    // whitespace from inline scripts that don't decode to text.
    // The trim_start on the digit probe must NOT eat newlines that
    // belong to the lyrics body when no header is present.
    let input = "\n[Verse 1]\nA real lyric";
    assert_eq!(strip_genius_header(input), input.to_string());
}

#[test]
fn strip_header_no_op_when_no_digit_prefix() {
    let input = "[Verse 1]\nA real lyric without a Genius header";
    assert_eq!(strip_genius_header(input), input.to_string());
}

#[test]
fn strip_header_no_op_when_contributors_token_missing() {
    // A track titled with a leading number (e.g. "99 Red Balloons")
    // must not trigger the strip — only " Contributors" right
    // after the digits qualifies as a Genius header.
    let input = "99 Red Balloons Lyrics\n[Verse 1]";
    assert_eq!(strip_genius_header(input), input.to_string());
}

#[test]
fn strip_header_no_op_when_lyrics_token_missing() {
    // Defensive: a Genius layout change that drops the trailing
    // " Lyrics" word would otherwise let us swallow the entire
    // body up to the first match. Leave the input alone instead
    // so the user sees raw content and we notice the regression
    // in bug reports rather than silent data loss.
    let input = "5 ContributorsSong Title Without Trailing Word\n[Verse]";
    assert_eq!(strip_genius_header(input), input.to_string());
}

#[test]
fn strip_header_handles_multibyte_title() {
    // Solamente Una Vez carries no multibyte chars but other Latin
    // titles do (Naïve, Déjà Vu). The strip walks bytes via
    // `match_indices(" Lyrics")` + `len()`, both byte-safe.
    let input = "3 ContributorsDéjà Vu Lyrics\n[Verse]\nMidnight again";
    assert_eq!(
        strip_genius_header(input),
        "[Verse]\nMidnight again".to_string()
    );
}

#[test]
fn strip_header_picks_closer_when_title_contains_lyrics_word() {
    // CodeRabbit-flagged edge case (PR #287): a song titled
    // "No Lyrics Needed" plants a " Lyrics" inside the title
    // BEFORE the trailing closer. A naive `find(" Lyrics")` would
    // cut at the title's occurrence and leave " Needed Lyrics\n…"
    // in the output. The closer is the FIRST " Lyrics" followed
    // by a newline or end-of-string — that's the shape we anchor
    // on.
    let input = "4 ContributorsNo Lyrics Needed Lyrics\n[Verse]\nReal body";
    assert_eq!(strip_genius_header(input), "[Verse]\nReal body".to_string());
}

#[test]
fn strip_header_does_not_swallow_body_containing_lyrics_word() {
    // Complementary to the above: if we'd swapped `find` for
    // `rfind` (the original CodeRabbit suggestion) to handle the
    // title case, a body line like "Some Lyrics" would now match
    // LAST and the strip would eat half the verse. Newline-
    // anchored matching keeps both shapes correct.
    let input = "5 ContributorsSong Title Lyrics\n[Verse 1]\nSome Lyrics line";
    assert_eq!(
        strip_genius_header(input),
        "[Verse 1]\nSome Lyrics line".to_string()
    );
}

#[test]
fn strip_header_handles_singular_contributor_badge() {
    // CodeRabbit-flagged edge case: Genius pluralises the badge
    // conditionally — a song with a single transcriber renders
    // "1 Contributor" (no trailing 's'). The strip MUST consume
    // the singular form too, otherwise a 1-transcriber song
    // ships the bare header to the user verbatim.
    let input = "1 ContributorSomething Lyrics\n[Intro]\nReal body";
    assert_eq!(strip_genius_header(input), "[Intro]\nReal body".to_string());
}
