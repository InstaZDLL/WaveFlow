//! Canonical-fields builder + `(hlc, payload_hash)` stamp for the
//! `track` entity.
//!
//! The 18-key field set mirrors the server's
//! `apply::track::canonical_fields` (waveflow-server `src/apply.rs`,
//! the block inside `track::insert`) exactly — same key names, same
//! `Value::Null` shape on `None`, arrays preserve source order. The
//! Phase B backfill protocol diffs `(canonical_id, payload_hash)`
//! pairs between server and desktop; any drift here would surface
//! every track row as a false-positive "needs re-sync".
//!
//! ## Why fields come from the wire, not the row
//!
//! Unlike [`super::library`] / [`super::playlist`] which read back
//! from the row in [`fields_from_row`] for defence-in-depth against
//! future normalisation, track's canonical fields span three tables
//! (`track`, `album`, `track_artist` → `artist`) and the `artists`
//! key is a multi-row GROUP_CONCAT. Reading them back would double
//! the scanner's slow-path latency (one extra SELECT per imported
//! file). The wire is already constructed by the caller
//! (`commands/scan.rs::emit_track_insert_from_extracted`,
//! `commands/edit.rs::update_track_tags`, etc.) and carries the
//! same values the server will receive over HTTP — trusting the
//! wire keeps desktop's `payload_hash` byte-identical to whatever
//! the server computes on its end of the round-trip.
//!
//! Net effect: the canonical wire form IS the source of truth for
//! the hash; the row UPDATE writes back the resulting bytes.

use serde_json::{Map, Value};
use sqlx::SqliteConnection;
use waveflow_core::sync::canon;

use crate::error::{AppError, AppResult};
use crate::sync::hooks::EnqueuedStamp;
use crate::sync::payload::{bump_digest_in_tx, payload_hash};
use crate::sync::track_emit::TrackInsertWire;

/// Build the canonical-fields map for a `track` row, sourced from
/// the same [`TrackInsertWire`] the desktop is about to push to the
/// server. The 18 keys match the server's `apply::track` block (no
/// `file_modified` here — that field rides on the wire so a peer
/// device's scanner fast-path can match by mtime, but it is NOT in
/// the payload-hash canonical form because the same logical track
/// re-imported from a copy with a different mtime would otherwise
/// diverge across devices).
pub fn canonical_fields_from_wire(wire: &TrackInsertWire<'_>) -> Map<String, Value> {
    let mut m = Map::new();
    canon::string(&mut m, "title", wire.title);
    canon::string(&mut m, "file_hash", wire.file_hash);
    canon::i64(&mut m, "file_size", wire.file_size);
    canon::i64(&mut m, "duration_ms", wire.duration_ms);
    canon::opt_i64(&mut m, "track_number", wire.track_number);
    canon::opt_i64(&mut m, "disc_number", wire.disc_number);
    canon::opt_i64(&mut m, "year", wire.year);
    canon::opt_i64(&mut m, "bitrate", wire.bitrate);
    canon::opt_i64(&mut m, "sample_rate", wire.sample_rate);
    canon::opt_i64(&mut m, "channels", wire.channels);
    canon::opt_i64(&mut m, "bit_depth", wire.bit_depth);
    canon::opt_string(&mut m, "codec", wire.codec);
    canon::opt_string(&mut m, "musical_key", wire.musical_key);
    canon::i64(&mut m, "added_at", wire.added_at);
    canon::opt_string(&mut m, "album_title", wire.album_title);
    canon::opt_string(&mut m, "album_artist_name", wire.album_artist_name);
    canon::bool(&mut m, "is_compilation", wire.is_compilation);
    canon::strings(&mut m, "artists", wire.artists);
    m
}

/// Stamp an INSERT-or-UPDATE on the `track` row identified by
/// `local_id` (the desktop's `track.id` rowid). Writes
/// `(hlc, origin_device_id, payload_hash)` onto the regular HLC
/// columns (the `rating_*` mirror columns are owned by
/// [`super::track_rating`]) and bumps the local
/// `metadata_digest_version` counter for `track`.
///
/// Must be called AFTER the row's data has landed in
/// `commands/scan.rs::upsert_track_row` (or `edit.rs::update_track_tags`)
/// and AFTER [`crate::sync::hooks::enqueue_op_in_tx`] returned
/// `Some(stamp)`.
pub async fn stamp_in_tx(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> AppResult<()> {
    let hash = payload_hash(&fields, stamp);
    let res = sqlx::query(
        "UPDATE track
            SET hlc_wall = ?,
                hlc_logical = ?,
                origin_device_id = ?,
                payload_hash = ?
          WHERE id = ?",
    )
    .bind(stamp.hlc_wall)
    .bind(stamp.hlc_logical)
    .bind(stamp.origin_device_id.map(|u| u.to_string()))
    .bind(&hash[..])
    .bind(local_id)
    .execute(&mut *conn)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::Other(format!(
            "sync::payload::track::stamp_in_tx: no track row matched id {local_id}",
        )));
    }
    bump_digest_in_tx(conn, "track").await?;
    Ok(())
}

/// Bump-only path for `track` DELETE ops. The row is already gone
/// (caller DELETEd it before emitting), so there's nothing to UPDATE
/// — but the digest still has to reflect that the set member left.
/// Used by `commands/duplicates.rs::delete_tracks` and
/// `commands/library.rs::remove_folder_from_library` after their
/// per-row DELETE FROM track.
pub async fn bump_for_delete_in_tx(conn: &mut SqliteConnection) -> AppResult<()> {
    bump_digest_in_tx(conn, "track").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn sample_wire<'a>(artists: &'a [String]) -> TrackInsertWire<'a> {
        TrackInsertWire {
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
            artists,
        }
    }

    async fn pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE track (
                id INTEGER PRIMARY KEY,
                file_path TEXT NOT NULL,
                hlc_wall INTEGER NOT NULL DEFAULT 0,
                hlc_logical INTEGER NOT NULL DEFAULT 0,
                origin_device_id TEXT,
                payload_hash BLOB
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE metadata_digest_version (
                entity TEXT PRIMARY KEY,
                version INTEGER NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO metadata_digest_version (entity, version) VALUES ('track', 0)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[test]
    fn canonical_fields_from_wire_has_eighteen_keys() {
        let artists = vec!["Daft Punk".into(), "Pharrell Williams".into()];
        let m = canonical_fields_from_wire(&sample_wire(&artists));
        assert_eq!(m.len(), 18);
        assert!(m.contains_key("title"));
        assert!(m.contains_key("file_hash"));
        assert!(m.contains_key("artists"));
        assert!(m.contains_key("is_compilation"));
    }

    #[test]
    fn canonical_fields_excludes_file_modified() {
        // file_modified rides on the wire (fast-path matching for
        // peer-device scanners on shared drives) but is NOT in the
        // canonical payload-hash form — the server's
        // apply::track block doesn't hash it. Keeping it out here
        // avoids cross-device hash divergence on identical content
        // copied with a different mtime.
        let artists: Vec<String> = vec![];
        let m = canonical_fields_from_wire(&sample_wire(&artists));
        assert!(!m.contains_key("file_modified"));
    }

    #[test]
    fn canonical_fields_emits_explicit_null_on_missing_optionals() {
        let artists: Vec<String> = vec![];
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
            artists: &artists,
        };
        let m = canonical_fields_from_wire(&wire);
        assert_eq!(m.get("album_title").unwrap(), &Value::Null);
        assert_eq!(m.get("track_number").unwrap(), &Value::Null);
        assert_eq!(m.get("codec").unwrap(), &Value::Null);
        assert_eq!(m.get("artists").unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn canonical_fields_preserves_artist_order() {
        // RFC-003 §4 — array source order is preserved through
        // `canon::strings`. A swap (e.g. featured-artist drift on
        // re-tag) MUST hash differently so the desktop's apply
        // pipeline can't silently flip primary/feature artists on
        // a re-emit.
        let a = vec!["Tyler".into(), "Earl".into()];
        let b = vec!["Earl".into(), "Tyler".into()];
        let ma = canonical_fields_from_wire(&sample_wire(&a));
        let mb = canonical_fields_from_wire(&sample_wire(&b));
        assert_ne!(ma.get("artists"), mb.get("artists"));
    }

    #[tokio::test]
    async fn stamp_in_tx_writes_hash_and_bumps_digest() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        sqlx::query("INSERT INTO track (id, file_path) VALUES (1, '/m/a.flac')")
            .execute(&mut *conn)
            .await
            .unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1_700_000_000_001,
            hlc_logical: 9,
            origin_device_id: None,
        };
        let artists: Vec<String> = vec!["Daft Punk".into()];
        let fields = canonical_fields_from_wire(&sample_wire(&artists));
        stamp_in_tx(&mut conn, 1, fields, stamp).await.unwrap();

        let row: (i64, i32, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT hlc_wall, hlc_logical, payload_hash FROM track WHERE id = 1",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(row.0, 1_700_000_000_001);
        assert_eq!(row.1, 9);
        assert_eq!(row.2.as_deref().map(|b| b.len()), Some(32));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'track'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }

    #[tokio::test]
    async fn stamp_in_tx_errors_when_row_missing() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let stamp = EnqueuedStamp {
            hlc_wall: 1,
            hlc_logical: 0,
            origin_device_id: None,
        };
        let artists: Vec<String> = vec![];
        let fields = canonical_fields_from_wire(&sample_wire(&artists));
        let err = stamp_in_tx(&mut conn, 999, fields, stamp).await.unwrap_err();
        assert!(format!("{err}").contains("no track row matched id 999"));

        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'track'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 0);
    }

    #[tokio::test]
    async fn bump_for_delete_in_tx_increments_digest_only() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        bump_for_delete_in_tx(&mut conn).await.unwrap();
        let v: i64 = sqlx::query_scalar(
            "SELECT version FROM metadata_digest_version WHERE entity = 'track'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(v, 1);
    }
}
