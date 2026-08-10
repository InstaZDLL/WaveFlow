//! Wire shapes for the journal endpoints.
//!
//! Mirrors what the server actually sends, checked against its OpenAPI
//! document and its live responses rather than against prose. Every
//! struct ignores unknown fields by default, which is the forward
//! compatibility the protocol asks for: a server that grows a field must
//! not break a client that has not heard of it.

use serde::Deserialize;

/// `GET /api/v2/sync/snapshot`.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncSnapshot {
    pub cursor: i64,
    #[serde(default)]
    pub playlists: Vec<PlaylistItem>,
    #[serde(default)]
    pub favorites: Vec<StarredEntry>,
    #[serde(default)]
    pub ratings: Vec<RatingItem>,
    #[serde(default)]
    pub queue: Option<QueueItem>,
    #[serde(default)]
    pub history: Vec<HistoryItem>,
    #[serde(default)]
    pub shares: Vec<ShareItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistItem {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub public: bool,
    #[serde(default)]
    pub songs: Vec<SongItem>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

/// A track as the server describes it.
///
/// Only `id` is required here even though the server marks more fields
/// mandatory: a third-party server fills in far less, and refusing the
/// whole page over a missing `suffix` would trade a sparse catalogue for
/// no catalogue. `title` falls back at the storage layer.
#[derive(Debug, Clone, Deserialize)]
pub struct SongItem {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub album_id: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub track: Option<i64>,
    #[serde(default)]
    pub disc: Option<i64>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub bitrate: Option<i64>,
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub artwork_hash: Option<String>,
    #[serde(default)]
    pub library_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StarredEntry {
    pub entity_type: String,
    pub entity_id: String,
    pub starred_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RatingItem {
    pub entity_type: String,
    pub entity_id: String,
    pub rating: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QueueItem {
    #[serde(default)]
    pub songs: Vec<SongItem>,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub position_ms: i64,
    #[serde(default)]
    pub changed_by: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

/// One play.
///
/// There is no scrobble identifier anywhere in the protocol: the journal
/// event's `entity_id` is the **track** id, so two plays of the same
/// track share it. `(track_id, played_at)` is the only pair that tells
/// them apart, which is why the projection keys on it.
///
/// ## What that costs, precisely
///
/// The server stores plays in an auto-incrementing table with no
/// uniqueness constraint: two byte-identical scrobbles both persist
/// (confirmed server-side). Those two rows are distinguishable in
/// `/sync/changes` — they carry different cursors and event ids — but
/// **not** in a snapshot, which describes each play only by track and
/// timestamp.
///
/// So the loss is real but bounded: walking the journal is *more
/// faithful* to the history than bootstrapping from a snapshot, and the
/// collision only bites when two plays share a millisecond, which in
/// practice means a duplicate submission rather than two listens. We
/// take the natural key because it is the only one both feeds carry;
/// keying on the event id would make a snapshot unable to populate the
/// table at all.
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryItem {
    pub track_id: String,
    #[serde(default)]
    pub submission: bool,
    pub played_at: i64,
}

/// A share, without its secret. The journal never carries the bearer
/// token or the public URL; `url` is only ever populated from our own
/// creation response.
#[derive(Debug, Clone, Deserialize)]
pub struct ShareItem {
    pub id: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub track_ids: Vec<String>,
    #[serde(default)]
    pub visit_count: i64,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
}

/// `GET /api/v2/sync/changes`.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncPage {
    #[serde(default)]
    pub changes: Vec<SyncChange>,
    #[serde(default)]
    pub has_more: bool,
    pub next_cursor: i64,
}

/// One journal event.
///
/// `payload` stays a raw `Value` deliberately. Its shape depends on
/// `entity_type` **and** on which endpoint produced it — creating a
/// playlist emits `{id, name, track_ids}` while updating one emits five
/// fields — so an absent key means "unchanged", not "null". Decoding
/// into a typed struct with `Option` fields would erase that distinction
/// and silently blank a comment on every rename.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncChange {
    pub cursor: i64,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub origin_device_id: Option<String>,
    #[serde(default)]
    pub changed_at: i64,
}

#[cfg(test)]
mod tests {
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
}
