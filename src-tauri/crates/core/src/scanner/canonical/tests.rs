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

#[test]
fn mbids_are_found_whatever_joins_them() {
    use super::artist_mbids;
    let a = "7B6F3A52-1C1E-4A5E-9F0E-0A3BD6F4A001";
    let b = "8c7a4b63-2d2f-4b6f-8a1f-1b4ce7a5b002";
    // Several values, a "; " join, a "/" join, a NUL join, and junk.
    assert_eq!(
        artist_mbids([a, b]),
        ["7b6f3a52-1c1e-4a5e-9f0e-0a3bd6f4a001", b]
    );
    assert_eq!(artist_mbids([format!("{a}; {b}").as_str()]).len(), 2);
    assert_eq!(artist_mbids([format!("{a}/{b}").as_str()]).len(), 2);
    assert_eq!(artist_mbids([format!("{a}\0{b}").as_str()]).len(), 2);
    assert!(artist_mbids(["", "not an id", "1234-5678"]).is_empty());
    // A malformed id among valid ones would shift the rest onto other names.
    assert!(artist_mbids([a, "7b6f3a52-1c1e-4a5e-9f0e-0a3bd6f4a0", b]).is_empty());
    assert!(artist_mbids([a, "7b6f3a521c1e4a5e9f0e0a3bd6f4a001", b]).is_empty());
}

#[test]
fn an_edited_credit_keeps_only_the_ids_that_still_name_their_artist() {
    use super::reconcile_artist_mbids;
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // Another artist: the id goes.
    assert_eq!(
        reconcile_artist_mbids("LISA", &ids(&["kr"]), "Aimer"),
        Some(vec![])
    );
    // A spelling fix keeps it.
    assert_eq!(reconcile_artist_mbids("LISA", &ids(&["kr"]), "Lisa"), None);
    // A reordered credit takes its ids along.
    assert_eq!(
        reconcile_artist_mbids("LISA; Alpha", &ids(&["kr", "a"]), "alpha; LISA"),
        Some(ids(&["a", "kr"]))
    );
    // One new name among old ones: the list cannot hold a gap.
    assert_eq!(
        reconcile_artist_mbids("LISA; Alpha", &ids(&["kr", "a"]), "LISA; Beta"),
        Some(vec![])
    );
    // A dropped name: the rest keep theirs.
    assert_eq!(
        reconcile_artist_mbids("LISA; Alpha", &ids(&["kr", "a"]), "LISA"),
        Some(ids(&["kr"]))
    );
    // No ids, nothing to do.
    assert_eq!(reconcile_artist_mbids("LISA", &[], "Aimer"), None);
    // Ids that never paired go before they would.
    assert_eq!(
        reconcile_artist_mbids("A; B", &ids(&["x"]), "C"),
        Some(vec![])
    );
    assert_eq!(reconcile_artist_mbids("A; B", &ids(&["x"]), "C; D"), None);
}

#[test]
fn an_album_artist_id_needs_one_name_as_written() {
    use super::album_artist_mbid;
    let a = "7b6f3a52-1c1e-4a5e-9f0e-0a3bd6f4a001";
    assert_eq!(album_artist_mbid("LISA", [a]).as_deref(), Some(a));
    assert_eq!(album_artist_mbid("LISA", [a, a]).as_deref(), Some(a));
    // Two artists of one key are still two names.
    assert_eq!(album_artist_mbid("LiSA; LISA", [a]), None);
    assert_eq!(album_artist_mbid("Alpha; Beta", [a]), None);
}

#[test]
fn a_credit_pairs_names_and_ids_only_when_the_counts_agree() {
    use super::artist_credits;
    let ids = vec!["id-a".to_string(), "id-b".to_string()];
    assert_eq!(
        artist_credits("Alpha; Beta", &ids),
        [
            ("Alpha".to_string(), Some("id-a".to_string())),
            ("Beta".to_string(), Some("id-b".to_string()))
        ]
    );
    // One id for two names says nothing about whose it is.
    assert_eq!(
        artist_credits("Alpha; Beta", &ids[..1]),
        [("Alpha".to_string(), None), ("Beta".to_string(), None)]
    );
    // No id at all: the split the scanner always made.
    assert_eq!(
        artist_credits("Alpha; alpha; Beta", &[]),
        [("Alpha".to_string(), None), ("Beta".to_string(), None)]
    );
}

#[test]
fn two_namesakes_with_their_own_ids_stay_two_artists() {
    use super::{
        artist_credit, artist_credit_names, artist_credit_with_ids, artist_credits,
        reconcile_artist_mbids,
    };
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let pair = |n: &str, i: &str| (n.to_string(), Some(i.to_string()));

    // The credit keeps both, and the ids still pair with them (#1081).
    let (credit, kept) = artist_credit_with_ids([], ["LiSA", "LISA"], &ids(&["a", "b"]));
    assert_eq!(credit.as_deref(), Some("LiSA; LISA"));
    assert_eq!(kept, ids(&["a", "b"]));
    assert_eq!(
        artist_credits("LiSA; LISA", &kept),
        [pair("LiSA", "a"), pair("LISA", "b")]
    );
    assert_eq!(artist_credit_names("LiSA; LISA", &kept), ["LiSA", "LISA"]);

    // The same name under the same id is one artist, and its id goes once.
    let (credit, kept) = artist_credit_with_ids([], ["LiSA", "lisa"], &ids(&["a", "a"]));
    assert_eq!(credit.as_deref(), Some("LiSA"));
    assert_eq!(kept, ids(&["a"]));

    // No id, or ids that do not pair: the name alone decides, as it did.
    assert_eq!(
        artist_credit_with_ids([], ["LiSA", "LISA"], &[])
            .0
            .as_deref(),
        Some("LiSA")
    );
    let (credit, kept) = artist_credit_with_ids([], ["LiSA", "LISA"], &ids(&["a"]));
    assert_eq!(credit.as_deref(), Some("LiSA"));
    assert_eq!(kept, ids(&["a"]));
    assert_eq!(artist_credit([], ["LiSA", "LISA"]).as_deref(), Some("LiSA"));

    // ARTISTS still wins only when it lists more distinct artists.
    let (credit, _) = artist_credit_with_ids(
        ["LiSA", "LISA", "Beta"],
        ["LiSA feat. LISA; Beta"],
        &ids(&["a", "b", "c"]),
    );
    assert_eq!(credit.as_deref(), Some("LiSA; LISA; Beta"));

    // An edit that keeps both spellings keeps both ids; a spelling that
    // fits both names neither.
    let old = ids(&["a", "b"]);
    assert_eq!(
        reconcile_artist_mbids("LiSA; LISA", &old, "LiSA; LISA"),
        None
    );
    assert_eq!(
        reconcile_artist_mbids("LiSA; LISA", &old, "LISA; LiSA"),
        Some(ids(&["b", "a"]))
    );
    assert_eq!(
        reconcile_artist_mbids("LiSA; LISA", &old, "LISA"),
        Some(ids(&["b"]))
    );
    assert_eq!(
        reconcile_artist_mbids("LiSA; LISA", &old, "Lisa"),
        Some(vec![])
    );
    // Names filed together by a scan and one id still pair.
    assert_eq!(
        reconcile_artist_mbids("Beta; BETA", &ids(&["a"]), "Gamma"),
        Some(vec![])
    );
}
