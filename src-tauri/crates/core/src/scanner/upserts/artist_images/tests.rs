use super::*;

#[test]
fn a_name_without_a_canonical_form_does_not_shift_the_others() {
    let names: Vec<String> = ["A", "!!!", "B", "C"].map(String::from).into();
    // `!!!` has no artist row, so the upsert returned three ids.
    let ids = [1, 2, 3];
    let pairs: Vec<(&str, i64)> = named_ids(&names, &ids)
        .map(|(name, id)| (name.as_str(), *id))
        .collect();
    assert_eq!(pairs, [("A", 1), ("B", 2), ("C", 3)]);
}
