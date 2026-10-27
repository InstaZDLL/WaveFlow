use super::*;

#[test]
fn the_kind_rotates_by_day_and_by_click() {
    let a = kind_order(20_000, 0);
    let b = kind_order(20_001, 0);
    let c = kind_order(20_000, 1);
    assert_ne!(a[0], b[0]);
    assert_eq!(b[0], c[0]);
    // Every kind is tried, each once.
    for order in [a, b, c] {
        for kind in KINDS {
            assert_eq!(order.iter().filter(|k| **k == kind).count(), 1);
        }
    }
    // Three clicks come back to the day's kind.
    assert_eq!(kind_order(20_000, 3)[0], a[0]);
}

#[test]
fn the_draw_is_stable_and_in_range() {
    for len in 1..30 {
        let first = pick_index(len, 20_000, 2, SuggestionKind::Vibe);
        assert!(first < len);
        assert_eq!(first, pick_index(len, 20_000, 2, SuggestionKind::Vibe));
    }
    assert_eq!(pick_index(0, 20_000, 0, SuggestionKind::Unplayed), 0);
    // Another click is another draw, not always the same place.
    let draws: std::collections::HashSet<usize> = (0..12)
        .map(|attempt| pick_index(20, 20_000, attempt, SuggestionKind::Forgotten))
        .collect();
    assert!(draws.len() > 3);
}

#[test]
fn the_shortlist_caps_each_artist() {
    let ranked = vec![
        (1, Some(7)),
        (2, Some(7)),
        (3, Some(7)),
        (4, None),
        (5, Some(8)),
    ];
    let picked = shortlist(ranked, |(_, artist)| *artist, 2, 10);
    let ids: Vec<i32> = picked.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, vec![1, 2, 4, 5]);
    let short = shortlist(vec![1, 2, 3], |_| None, 1, 2);
    assert_eq!(short, vec![1, 2]);
}

#[test]
fn a_track_near_the_recent_listening_is_closer() {
    let recent = RecentProfile {
        energy: Some(0.7),
        bpm: Some(124.0),
    };
    let near = vibe_distance(recent, Some(0.65), Some(122.0));
    let far = vibe_distance(recent, Some(0.2), Some(70.0));
    assert!(near < far);
    // A tempo read an octave out is as close as the tempo itself.
    let octave = vibe_distance(recent, Some(0.65), Some(61.0));
    assert!((octave - near).abs() < 0.03);
    // Unknown values count halfway: never ahead of a close match.
    let unknown = vibe_distance(recent, None, None);
    assert!((unknown - 1.0).abs() < f64::EPSILON);
    assert!(near < unknown);
}
