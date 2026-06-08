//! Track sync emit helpers (Phase 4.d.0.3).
//!
//! Builds the wire payload expected by the server's track apply
//! pipeline (`apply.rs::track`, phase 4.d.0.2):
//!
//! - `entity: "track"`, `entity_id: <file_path>` — the per-library
//!   natural identity. Using file_hash as identity would break the
//!   tag-editor re-emit (lofty rewrites embedded metadata frames so
//!   file_hash changes while file_path doesn't); the server's
//!   upsert keys on file_path for that reason.
//! - `payload.library_canonical_id`: the tenant scope. Resolved via
//!   [`crate::sync::canonical::ensure_local_library`] inside the
//!   caller's transaction.
//! - `payload.file_hash`: rides as a payload field — joins to
//!   `liked_track` / `track_rating` server-side.
//! - Full audio metadata + the album / artist plumbing (`album_title`,
//!   `album_artist_name`, `is_compilation`, `artists: [String, ...]`).
//!
//! ## Scanner integration
//!
//! [`emit_track_insert_in_tx`] is the single entry point for both
//! the "Brand-new track" and "existing track re-emit" branches in
//! `commands/scan.rs`. The server's upsert merges either way — the
//! desktop doesn't need to distinguish add-vs-update on the wire.
//!
//! For user-initiated track deletes (e.g. the duplicates UI),
//! [`emit_track_delete_in_tx`] enqueues a delete op keyed on the
//! same (library_canonical_id, file_path). Cascade-driven deletes
//! (library / library_folder removal) are NOT explicitly emitted
//! here — the server's library apply pipeline cascades its own
//! `track` rows when the parent library is dropped.

use serde_json::Value;
use sqlx::SqliteConnection;

use crate::error::AppResult;
use crate::sync::{canonical, hooks};

/// Snapshot of an `ExtractedFile`-equivalent payload, decoupled from
/// the `waveflow_core::scanner::extract::ExtractedFile` struct so
/// callers in `duplicates.rs` (which doesn't have an
/// `ExtractedFile`) can pass the same shape.
///
/// Borrowed slice / option-str fields keep allocations out of the
/// hot scanner path.
#[derive(Debug, Clone)]
pub struct TrackInsertWire<'a> {
    pub file_hash: &'a str,
    pub title: &'a str,
    pub file_size: i64,
    /// Last-modified epoch millis from the source filesystem.
    /// Carries the scanner's `(mtime, size)` fast-path key on the
    /// wire so a peer device that pulls the track via sync and
    /// then scans its own copy of the file (shared-drive setups)
    /// can skip the slow re-extract path on rows whose mtime
    /// already matches what the source desktop emitted.
    pub file_modified: i64,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    pub codec: Option<&'a str>,
    pub musical_key: Option<&'a str>,
    pub added_at: i64,
    pub album_title: Option<&'a str>,
    pub album_artist_name: Option<&'a str>,
    pub is_compilation: bool,
    /// Multi-artist list in source order — position = array index.
    /// The server dedupes silently on its side; the desktop SHOULD
    /// pre-split via `split_artist_name` so the list reflects the
    /// `; `-separated convention.
    pub artists: &'a [String],
}

/// Build the JSON payload the server's `apply::track::insert`
/// handler parses. `library_canonical_id` is taken as a separate
/// arg so the caller can resolve it in the same transaction as
/// the entity write.
pub fn build_track_insert_payload(library_canonical_id: &str, wire: &TrackInsertWire<'_>) -> Value {
    serde_json::json!({
        "library_canonical_id": library_canonical_id,
        "file_hash": wire.file_hash,
        "title": wire.title,
        "file_size": wire.file_size,
        "file_modified": wire.file_modified,
        "duration_ms": wire.duration_ms,
        "track_number": wire.track_number,
        "disc_number": wire.disc_number,
        "year": wire.year,
        "bitrate": wire.bitrate,
        "sample_rate": wire.sample_rate,
        "channels": wire.channels,
        "bit_depth": wire.bit_depth,
        "codec": wire.codec,
        "musical_key": wire.musical_key,
        "added_at": wire.added_at,
        "album_title": wire.album_title,
        "album_artist_name": wire.album_artist_name,
        "is_compilation": wire.is_compilation,
        "artists": wire.artists,
    })
}

/// Resolve the library's canonical id, build the payload, and
/// enqueue the `track + insert` op. All inside the caller's
/// transaction so the entity write + outbox row + Lamport bump
/// either ALL land or ALL roll back.
///
/// Returns `Ok(true)` when the op was enqueued, `Ok(false)` when
/// the sync gate short-circuited (no JWT for the active profile,
/// or `SyncMode::Local`). Either way the entity write proceeds —
/// the boolean is just for telemetry.
pub async fn emit_track_insert_in_tx(
    conn: &mut SqliteConnection,
    library_id: i64,
    file_path: &str,
    wire: &TrackInsertWire<'_>,
) -> AppResult<bool> {
    let library_canonical = canonical::ensure_local_library(conn, library_id).await?;
    let payload = build_track_insert_payload(&library_canonical, wire);
    hooks::enqueue_op_in_tx(
        conn,
        &hooks::PendingOpDraft {
            entity: "track".into(),
            entity_id: file_path.to_owned(),
            field: None,
            op: "insert".into(),
            payload: Some(payload),
        },
    )
    .await
}

/// Enqueue a `track + delete` op for a file the user explicitly
/// removed (e.g. duplicate cleanup). Cascade-driven deletes (the
/// parent library or folder being dropped) MUST NOT call this —
/// the server-side library apply pipeline already cascades the
/// tracks; an explicit per-track delete would be redundant + race
/// the cascade in the apply order.
pub async fn emit_track_delete_in_tx(
    conn: &mut SqliteConnection,
    library_id: i64,
    file_path: &str,
) -> AppResult<bool> {
    let library_canonical = canonical::ensure_local_library(conn, library_id).await?;
    let payload = serde_json::json!({
        "library_canonical_id": library_canonical,
    });
    hooks::enqueue_op_in_tx(
        conn,
        &hooks::PendingOpDraft {
            entity: "track".into(),
            entity_id: file_path.to_owned(),
            field: None,
            op: "delete".into(),
            payload: Some(payload),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_track_insert_payload_includes_every_field() {
        let artists = vec!["Daft Punk".to_owned(), "Pharrell Williams".to_owned()];
        let wire = TrackInsertWire {
            file_hash: "blake3-abc",
            title: "Get Lucky",
            file_size: 12_345_678,
            file_modified: 1_700_000_001_000,
            duration_ms: 369_000,
            track_number: Some(8),
            disc_number: Some(1),
            year: Some(2013),
            bitrate: Some(1_411_000),
            sample_rate: Some(44_100),
            channels: Some(2),
            bit_depth: Some(16),
            codec: Some("flac"),
            musical_key: Some("F#m"),
            added_at: 1_700_000_000_000,
            album_title: Some("Random Access Memories"),
            album_artist_name: Some("Daft Punk"),
            is_compilation: false,
            artists: &artists,
        };
        let payload = build_track_insert_payload("lib-canonical-uuid", &wire);
        let obj = payload.as_object().expect("payload is an object");
        assert_eq!(obj["library_canonical_id"], "lib-canonical-uuid");
        assert_eq!(obj["file_hash"], "blake3-abc");
        assert_eq!(obj["title"], "Get Lucky");
        assert_eq!(obj["file_size"], 12_345_678);
        assert_eq!(obj["file_modified"], 1_700_000_001_000_i64);
        assert_eq!(obj["duration_ms"], 369_000);
        assert_eq!(obj["track_number"], 8);
        assert_eq!(obj["album_title"], "Random Access Memories");
        assert_eq!(obj["album_artist_name"], "Daft Punk");
        assert_eq!(obj["is_compilation"], false);
        let artists_v = obj["artists"].as_array().expect("artists is an array");
        assert_eq!(artists_v.len(), 2);
        assert_eq!(artists_v[0], "Daft Punk");
        assert_eq!(artists_v[1], "Pharrell Williams");
    }

    #[test]
    fn build_track_insert_payload_emits_null_for_missing_optionals() {
        let wire = TrackInsertWire {
            file_hash: "h",
            title: "T",
            file_size: 1,
            file_modified: 0,
            duration_ms: 1,
            track_number: None,
            disc_number: None,
            year: None,
            bitrate: None,
            sample_rate: None,
            channels: None,
            bit_depth: None,
            codec: None,
            musical_key: None,
            added_at: 0,
            album_title: None,
            album_artist_name: None,
            is_compilation: false,
            artists: &[],
        };
        let payload = build_track_insert_payload("lib", &wire);
        let obj = payload.as_object().unwrap();
        // Server's `payload_optional_string` / `payload_i64_optional`
        // both accept `null` as "absent" — the desktop emits null
        // explicitly rather than omitting the key so the JSON shape
        // stays stable for the receiver.
        assert!(obj["album_title"].is_null());
        assert!(obj["album_artist_name"].is_null());
        assert!(obj["track_number"].is_null());
        assert!(obj["codec"].is_null());
        assert_eq!(
            obj["artists"].as_array().unwrap().len(),
            0,
            "empty artist list serialises as []"
        );
    }
}
