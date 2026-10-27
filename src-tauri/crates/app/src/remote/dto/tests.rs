use super::*;

#[test]
fn a_snapshot_with_only_a_cursor_decodes() {
    // Every collection defaults, so an account with nothing in it is
    // a valid snapshot rather than a decode failure.
    let snapshot: SyncSnapshot = serde_json::from_str(r#"{"cursor": 0}"#).unwrap();
    assert_eq!(snapshot.cursor, 0);
    assert!(snapshot.playlists.is_empty());
    assert!(snapshot.queue.is_none());
}

#[test]
fn unknown_fields_are_ignored_rather_than_fatal() {
    // The protocol requires forward compatibility: a server that
    // grows a field must not break an older client.
    let snapshot: SyncSnapshot = serde_json::from_str(
        r#"{"cursor": 3, "playlists": [], "something_new": {"nested": true}}"#,
    )
    .unwrap();
    assert_eq!(snapshot.cursor, 3);
}

#[test]
fn a_song_survives_a_sparse_third_party_server() {
    // Only the identifier is genuinely required; a server that fills
    // in less must still yield a usable row.
    let song: SongItem = serde_json::from_str(r#"{"id": "t1"}"#).unwrap();
    assert_eq!(song.id, "t1");
    assert!(song.title.is_none());
    assert!(song.duration_ms.is_none());
}

#[test]
fn a_change_keeps_its_payload_untyped() {
    let change: SyncChange = serde_json::from_str(
        r#"{"cursor":7,"entity_type":"playlist","entity_id":"p1","action":"upsert",
                "payload":{"id":"p1","name":"Mix","track_ids":["a","b"]},
                "event_id":"e","operation_id":"o","changed_at":10}"#,
    )
    .unwrap();
    assert_eq!(change.cursor, 7);
    // The distinction this preserves: `comment` is absent, which is
    // not the same as being null.
    assert!(change.payload.get("comment").is_none());
    assert_eq!(change.payload["track_ids"].as_array().unwrap().len(), 2);
}

#[test]
fn a_page_that_returns_nothing_still_carries_its_cursor() {
    let page: SyncPage =
        serde_json::from_str(r#"{"changes":[],"has_more":false,"next_cursor":42}"#).unwrap();
    assert_eq!(page.next_cursor, 42);
    assert!(!page.has_more);
}
