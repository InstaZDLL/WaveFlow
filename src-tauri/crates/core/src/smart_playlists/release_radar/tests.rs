use std::collections::HashMap;

use super::*;
use crate::release_radar::MatchedBy;

fn matched(ids: &[i64]) -> LocalMatch {
    LocalMatch {
        track_ids: ids.to_vec(),
        matched_by: MatchedBy::Album,
    }
}

#[test]
fn rules_json_carries_release_radar_kind() {
    let json = SmartPlaylistRules::ReleaseRadar.to_json().unwrap();
    assert!(json.contains(NEEDLE), "ReleaseRadar serialized as {json}");
}

#[test]
fn releases_come_newest_first_and_a_discovered_one_brings_nothing() {
    let matches = HashMap::from([
        ("new".to_string(), matched(&[1, 2])),
        ("old".to_string(), matched(&[3])),
    ]);
    let order = ["new", "discovered", "old"].map(String::from);
    assert_eq!(pick_tracks(&order, &matches), vec![1, 2, 3]);
}

#[test]
fn one_release_brings_at_most_its_cap_and_a_shared_track_once() {
    let album: Vec<i64> = (10..30).collect();
    let matches = HashMap::from([
        ("single".to_string(), matched(&[10])),
        ("album".to_string(), matched(&album)),
    ]);
    let order = ["single", "album"].map(String::from);
    let picked = pick_tracks(&order, &matches);
    // The single's track, then the album's next five.
    assert_eq!(picked, vec![10, 11, 12, 13, 14, 15]);
}

#[test]
fn the_playlist_stops_at_its_limit() {
    let mut matches = HashMap::new();
    let mut order = Vec::new();
    for r in 0..40 {
        let id = format!("r{r}");
        let base = r * 100;
        matches.insert(id.clone(), matched(&[base, base + 1, base + 2]));
        order.push(id);
    }
    assert_eq!(pick_tracks(&order, &matches).len(), TRACKS_LIMIT);
}
