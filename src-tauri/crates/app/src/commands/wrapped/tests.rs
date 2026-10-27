use super::*;

#[test]
fn streak_walks_consecutive_days() {
    let days = [
        "2026-01-01",
        "2026-01-02",
        "2026-01-03",
        "2026-01-05", // gap
        "2026-01-06",
        "2026-01-07",
        "2026-01-08",
        "2026-01-09",
    ];
    let streak = compute_longest_streak(days.iter().map(|s| s.to_string())).unwrap();
    assert_eq!(streak.days, 5);
    assert_eq!(streak.start, "2026-01-05");
    assert_eq!(streak.end, "2026-01-09");
}

#[test]
fn streak_handles_single_day() {
    let s = compute_longest_streak(["2026-03-01".to_string()].into_iter()).unwrap();
    assert_eq!(s.days, 1);
    assert_eq!(s.start, "2026-03-01");
    assert_eq!(s.end, "2026-03-01");
}

#[test]
fn streak_handles_empty() {
    assert!(compute_longest_streak(std::iter::empty()).is_none());
}

#[test]
fn energy_label_buckets() {
    assert_eq!(energy_label_from_bpm(70.0), "chill");
    assert_eq!(energy_label_from_bpm(100.0), "warm");
    assert_eq!(energy_label_from_bpm(125.0), "groove");
    assert_eq!(energy_label_from_bpm(150.0), "energetic");
    assert_eq!(energy_label_from_bpm(170.0), "fire");
}

#[test]
fn energy_labels_cover_the_scale() {
    assert_eq!(energy_label_from_energy(0.1), "chill");
    assert_eq!(energy_label_from_energy(0.4), "warm");
    assert_eq!(energy_label_from_energy(0.5), "groove");
    assert_eq!(energy_label_from_energy(0.7), "energetic");
    assert_eq!(energy_label_from_energy(0.9), "fire");
}
