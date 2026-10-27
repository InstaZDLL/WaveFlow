use super::*;

#[test]
fn token_subset_scores_like_query_match() {
    assert_eq!(
        str_score("Gloria Estefan - Get on Your Feet", "get on your feet"),
        100.0
    );
}

#[test]
fn plaintext_drops_word_stamps() {
    let enhanced = "[00:01.00]<00:01.00>Hello <00:01.50>world";
    assert_eq!(synced_to_plaintext(enhanced), "Hello world");
}

#[test]
fn plaintext_keeps_non_timestamp_brackets() {
    let line = "[00:01.00]a < b and c > d";
    assert_eq!(synced_to_plaintext(line), "a < b and c > d");
}
