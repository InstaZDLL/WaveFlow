use super::{pinyin_blob, split_genres};

#[test]
fn han_gives_syllables_and_initials() {
    // The issue's own example: both forms have to reach the title.
    let blob = pinyin_blob("中国人").unwrap();
    assert_eq!(blob, "zhongguoren zgr");
    assert!(blob.contains("zhongguo"));
    assert!(blob.contains("zgr"));
}

#[test]
fn a_latin_title_has_nothing_to_romanise() {
    // The common case, and the reason this is an Option: a column of
    // empty strings for every Western library would be paid for by
    // everyone and reachable by no one.
    assert_eq!(pinyin_blob("Dark Side of the Moon"), None);
    assert_eq!(pinyin_blob(""), None);
    assert_eq!(pinyin_blob("!!! (2004)"), None);
}

#[test]
fn latin_around_han_is_skipped_not_romanised() {
    // A mixed title keeps only what pinyin can say something about.
    assert_eq!(pinyin_blob("周杰伦 - Album").unwrap(), "zhoujielun zjl");
}

#[test]
fn traditional_characters_are_covered_too() {
    // zh-TW is a shipped locale; a traditional library has the same
    // defect as a simplified one.
    assert_eq!(pinyin_blob("愛").unwrap(), "ai a");
    assert_eq!(pinyin_blob("中國").unwrap(), "zhongguo zg");
}

#[test]
fn genres_split_on_semicolons_nul_and_backslashes() {
    assert_eq!(split_genres(["Rock; Jazz"]), ["Rock", "Jazz"]);
    assert_eq!(split_genres(["Rock\0Jazz"]), ["Rock", "Jazz"]);
    // Repeated values, and a list inside one of them.
    assert_eq!(
        split_genres(["Ambient", "Trip Hop;Downtempo"]),
        ["Ambient", "Trip Hop", "Downtempo"]
    );
    // A tag editor's multi-value separator, written as text.
    assert_eq!(
        split_genres(["Alternative\\\\Rock"]),
        ["Alternative", "Rock"]
    );
    assert_eq!(split_genres(["Alternative\\Rock"]), ["Alternative", "Rock"]);
    // Names that hold a slash or a comma are one genre each.
    assert_eq!(split_genres(["Hip-Hop/Rap"]), ["Hip-Hop/Rap"]);
    assert_eq!(
        split_genres(["Folk, World, & Country"]),
        ["Folk, World, & Country"]
    );
}

#[test]
fn a_genre_named_twice_is_kept_once_in_its_first_spelling() {
    assert_eq!(
        split_genres(["Rock", "rock ; JAZZ", "Jazz!"]),
        ["Rock", "JAZZ"]
    );
    assert!(split_genres(["  ;  ", ""]).is_empty());
}

#[test]
fn the_two_halves_cannot_be_matched_across() {
    // The space is load-bearing: a term never contains one, so
    // "renz" must not be found in "zhongguoren zgr".
    let blob = pinyin_blob("中国人").unwrap();
    assert!(!blob.contains("renz"));
}
