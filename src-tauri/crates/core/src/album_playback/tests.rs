use super::*;

fn album(album_id: i64, track_count: i64) -> AlbumCandidate {
    AlbumCandidate {
        album_id,
        track_count,
    }
}

/// The whole point: the budget is spent in whole records, so a
/// session never stops halfway through one.
#[test]
fn albums_are_taken_whole_or_not_at_all() {
    let candidates = [album(1, 10), album(2, 12), album(3, 9)];
    let chosen = fit_albums_to_budget(&candidates, 25);
    assert_eq!(chosen, vec![1, 2], "22 fits, 31 would not");
}

/// An album that does not fit is skipped rather than ending the
/// selection, so a long record early in the list does not starve
/// the short ones behind it.
#[test]
fn an_album_that_does_not_fit_is_skipped_not_final() {
    let candidates = [album(1, 8), album(2, 40), album(3, 6)];
    assert_eq!(fit_albums_to_budget(&candidates, 20), vec![1, 3]);
}

/// A budget smaller than the shortest record still produces a
/// session — an empty Daily Mix is worse than a long one.
#[test]
fn the_first_album_is_taken_however_long_it_is() {
    let candidates = [album(1, 80), album(2, 4)];
    assert_eq!(fit_albums_to_budget(&candidates, 50), vec![1]);
}

/// …and that exception is spent once. The over-long first album
/// has already blown the budget, so nothing follows it.
#[test]
fn one_over_long_album_does_not_licence_a_second() {
    let candidates = [album(1, 80), album(2, 80), album(3, 2)];
    assert_eq!(fit_albums_to_budget(&candidates, 50), vec![1]);
}

/// An album whose tracks are all unavailable or unanalysed carries
/// no weight and must not take a slot.
#[test]
fn empty_albums_are_ignored() {
    let candidates = [album(1, 0), album(2, -3), album(3, 5)];
    assert_eq!(fit_albums_to_budget(&candidates, 10), vec![3]);
}

#[test]
fn an_empty_candidate_list_yields_an_empty_session() {
    assert!(fit_albums_to_budget(&[], 50).is_empty());
}
