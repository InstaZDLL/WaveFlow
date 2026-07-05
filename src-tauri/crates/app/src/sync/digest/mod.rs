//! Local-side digest computation, mirror of the server's
//! `GET /api/v1/sync/digest` (RFC-003 §4 / `waveflow-server`
//! `src/db.rs::digest_read`).
//!
//! For each synced entity the digest is `(set_hash, version,
//! max_hlc, members)` where:
//!
//! - `set_hash` is BLAKE3 over the sorted `(canonical_id,
//!   payload_hash)` pairs (see [`waveflow_core::sync::digest`] for
//!   the shared algorithm).
//! - `version` is the monotone counter from
//!   `metadata_digest_version` that B.0a/B.0-rest bumps on every
//!   row-level change.
//! - `max_hlc` is the highest §2 total-order triple across the
//!   non-NULL `payload_hash` rows (the same filter the server
//!   applies — rows whose hash hasn't been stamped yet stay out
//!   of the set).
//! - `members` is the sorted list itself, kept around so the
//!   diff layer ([`diff`]) can identify which rows differ once a
//!   `set_hash` mismatch has flagged divergence.
//!
//! ## Per-entity SQL shape
//!
//! Mirrors the server's `db::digest_read::members_*` exactly:
//!
//! | Entity         | Source table       | Canonical id                                | Sort by                  |
//! |----------------|--------------------|---------------------------------------------|--------------------------|
//! | `library`      | `library`          | `library.canonical_id`                      | `canonical_id`           |
//! | `playlist`     | `playlist`         | `playlist.canonical_id`                     | `canonical_id`           |
//! | `track`        | `track + library`  | `format!("{lib_canonical}\\u{{1F}}{file_path}")` | `(lib_canonical, file_path)` |
//! | `liked_track`  | `liked_track + track` | `track.file_hash`                        | `file_hash`              |
//! | `track_rating` | `track`            | `track.file_hash`                           | `file_hash`              |
//!
//! For `track_rating` we read the `rating_*` mirror column quartet
//! co-located on the `track` row (the desktop holds the rating as a
//! column rather than a sibling table — see the A.3 migration
//! header). `liked_track` joins back to `track` for the file_hash
//! because the local table is keyed on `track_id`.
//!
//! ## Filters mirror the server
//!
//! - `payload_hash IS NOT NULL` — rows that pre-date B.0 stamping
//!   are excluded so a partial-backfill desktop doesn't disagree
//!   with the server on the set membership.
//! - For profile-scoped entities, `canonical_id IS NOT NULL` —
//!   defensive against a row that lost its mapping (shouldn't
//!   happen post-1.f.desktop.4b, but the filter keeps the
//!   invariant explicit).

pub mod client;
pub mod diff;
pub mod entity_client;

use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;
use waveflow_core::sync::digest as core_digest;

use crate::error::{AppError, AppResult};

/// One member of the local digest, raw-bytes form so the
/// `set_hash` feed runs without a round-trip through hex.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalMember {
    pub canonical_id: String,
    /// Raw BLAKE3-256 bytes from the row's `payload_hash` column.
    /// Encoded to hex only when crossing the diff boundary against
    /// the server's response.
    #[serde(serialize_with = "serialize_bytes_hex")]
    pub payload_hash: Vec<u8>,
}

/// The local mirror of the server's `MaxHlc` triple. `None` when
/// the entity set is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MaxHlcLocal {
    pub wall: i64,
    pub logical: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_device_id: Option<Uuid>,
}

/// Output of [`read_local_digest`] — same shape as the server's
/// `DigestResponse` modulo internal representation (raw bytes vs
/// hex strings). The diff layer compares against the deserialised
/// server response in [`client::RemoteDigest`].
#[derive(Debug, Clone, Serialize)]
pub struct LocalDigest {
    pub entity: String,
    #[serde(serialize_with = "serialize_array_hex")]
    pub set_hash: [u8; 32],
    pub version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_hlc: Option<MaxHlcLocal>,
    pub members: Vec<LocalMember>,
}

fn serialize_bytes_hex<S: serde::Serializer>(bytes: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&hex::encode(bytes))
}

fn serialize_array_hex<S: serde::Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&hex::encode(bytes))
}

/// Whitelist of entities the digest endpoint accepts on either
/// side. Same set the server's API layer matches in
/// `api::sync::get_digest`. `playlist_track` is deliberately out —
/// the server has no `canonical_fields`/`payload_hash` stamping
/// for it (see Phase B.0-rest memory entry).
pub const SUPPORTED_ENTITIES: &[&str] = &[
    "library",
    "playlist",
    "track",
    "liked_track",
    "track_rating",
];

/// Read the local digest for `entity` from the active profile's
/// SQLite pool. Returns `Err` for unknown entities so a caller
/// typo surfaces instead of silently producing an empty set.
pub async fn read_local_digest(pool: &SqlitePool, entity: &str) -> AppResult<LocalDigest> {
    let (members, max_hlc) = match entity {
        "library" => read_library_members(pool).await?,
        "playlist" => read_playlist_members(pool).await?,
        "track" => read_track_members(pool).await?,
        "liked_track" => read_liked_members(pool).await?,
        "track_rating" => read_rating_members(pool).await?,
        other => {
            return Err(AppError::Other(format!(
                "sync::digest::read_local_digest: unknown entity '{other}'",
            )))
        }
    };
    let version = read_version(pool, entity).await?;
    let set_hash = compute_set_hash_from(&members);
    Ok(LocalDigest {
        entity: entity.to_string(),
        set_hash,
        version,
        max_hlc,
        members,
    })
}

/// Read the monotone counter for `entity` from
/// `metadata_digest_version`. Missing rows surface as `0` to match
/// the server's `unwrap_or(0)` behaviour — both replicas treat
/// "no row yet" as version 0.
async fn read_version(pool: &SqlitePool, entity: &str) -> AppResult<i64> {
    let row: Option<i64> =
        sqlx::query_scalar("SELECT version FROM metadata_digest_version WHERE entity = ?")
            .bind(entity)
            .fetch_optional(pool)
            .await?;
    Ok(row.unwrap_or(0))
}

/// Build the BLAKE3 feed from the local members. Cheap zero-copy
/// pairs — the helper just borrows.
fn compute_set_hash_from(members: &[LocalMember]) -> [u8; 32] {
    let pairs: Vec<(&str, &[u8])> = members
        .iter()
        .map(|m| (m.canonical_id.as_str(), m.payload_hash.as_slice()))
        .collect();
    core_digest::compute_set_hash(&pairs)
}

// ── Per-entity readers ────────────────────────────────────────────

async fn read_library_members(
    pool: &SqlitePool,
) -> AppResult<(Vec<LocalMember>, Option<MaxHlcLocal>)> {
    let rows: Vec<(String, Vec<u8>, i64, i32, Option<String>)> = sqlx::query_as(
        "SELECT canonical_id, payload_hash, hlc_wall, hlc_logical, origin_device_id
           FROM library
          WHERE canonical_id IS NOT NULL
            AND payload_hash IS NOT NULL
          ORDER BY canonical_id ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(collect_simple_members(rows))
}

async fn read_playlist_members(
    pool: &SqlitePool,
) -> AppResult<(Vec<LocalMember>, Option<MaxHlcLocal>)> {
    let rows: Vec<(String, Vec<u8>, i64, i32, Option<String>)> = sqlx::query_as(
        "SELECT canonical_id, payload_hash, hlc_wall, hlc_logical, origin_device_id
           FROM playlist
          WHERE canonical_id IS NOT NULL
            AND payload_hash IS NOT NULL
          ORDER BY canonical_id ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(collect_simple_members(rows))
}

async fn read_track_members(
    pool: &SqlitePool,
) -> AppResult<(Vec<LocalMember>, Option<MaxHlcLocal>)> {
    // Mirror `db::digest_read::track_members` — composite
    // canonical key is `<library.canonical_id>\u{1F}<track.file_path>`.
    // The U+001F separator is illegal in real filesystem paths, so
    // the split round-trips uniquely on the diff side.
    let rows: Vec<(String, String, Vec<u8>, i64, i32, Option<String>)> = sqlx::query_as(
        "SELECT l.canonical_id, t.file_path, t.payload_hash, t.hlc_wall, t.hlc_logical, t.origin_device_id
           FROM track t
           JOIN library l ON l.id = t.library_id
          WHERE l.canonical_id IS NOT NULL
            AND t.payload_hash IS NOT NULL
          ORDER BY l.canonical_id ASC, t.file_path ASC",
    )
    .fetch_all(pool)
    .await?;
    let mut members = Vec::with_capacity(rows.len());
    let mut max: Option<(i64, i32, Option<Uuid>)> = None;
    for (lib_canonical, file_path, payload_hash, hlc_wall, hlc_logical, origin) in rows {
        let composite = format!("{lib_canonical}\u{001F}{file_path}");
        members.push(LocalMember {
            canonical_id: composite,
            payload_hash,
        });
        update_max(&mut max, hlc_wall, hlc_logical, origin.as_deref());
    }
    Ok((members, max.map(into_local_max)))
}

async fn read_liked_members(
    pool: &SqlitePool,
) -> AppResult<(Vec<LocalMember>, Option<MaxHlcLocal>)> {
    // The desktop's `liked_track` is keyed on `track_id`. The
    // server's digest emits `track.file_hash` as the canonical id,
    // so we join back.
    let rows: Vec<(String, Vec<u8>, i64, i32, Option<String>)> = sqlx::query_as(
        "SELECT t.file_hash, lt.payload_hash, lt.hlc_wall, lt.hlc_logical, lt.origin_device_id
           FROM liked_track lt
           JOIN track t ON t.id = lt.track_id
          WHERE lt.payload_hash IS NOT NULL
          ORDER BY t.file_hash ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(collect_simple_members(rows))
}

async fn read_rating_members(
    pool: &SqlitePool,
) -> AppResult<(Vec<LocalMember>, Option<MaxHlcLocal>)> {
    // The rating quartet (`rating_hlc_wall` / `rating_hlc_logical` /
    // `rating_origin_device_id` / `rating_payload_hash`) is co-
    // located on the same `track` row as the metadata HLC — see
    // the A.3 migration header.
    let rows: Vec<(String, Vec<u8>, i64, i32, Option<String>)> = sqlx::query_as(
        "SELECT file_hash, rating_payload_hash, rating_hlc_wall, rating_hlc_logical, rating_origin_device_id
           FROM track
          WHERE rating_payload_hash IS NOT NULL
          ORDER BY file_hash ASC",
    )
    .fetch_all(pool)
    .await?;
    Ok(collect_simple_members(rows))
}

fn collect_simple_members(
    rows: Vec<(String, Vec<u8>, i64, i32, Option<String>)>,
) -> (Vec<LocalMember>, Option<MaxHlcLocal>) {
    let mut members = Vec::with_capacity(rows.len());
    let mut max: Option<(i64, i32, Option<Uuid>)> = None;
    for (canonical_id, payload_hash, hlc_wall, hlc_logical, origin) in rows {
        members.push(LocalMember {
            canonical_id,
            payload_hash,
        });
        update_max(&mut max, hlc_wall, hlc_logical, origin.as_deref());
    }
    (members, max.map(into_local_max))
}

fn update_max(
    current: &mut Option<(i64, i32, Option<Uuid>)>,
    wall: i64,
    logical: i32,
    origin: Option<&str>,
) {
    // A bad UUID string in the row is treated as `None` — same as
    // server's `Option<Uuid>` fetch which would surface a sqlx
    // decode error. We swallow + log so a single corrupt row
    // doesn't take the whole digest down; the row stays out of
    // the max_hlc tuple but still contributes to set_hash.
    let parsed = match origin {
        Some(s) => match Uuid::parse_str(s) {
            Ok(u) => Some(u),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    value = s,
                    "digest: ignoring malformed origin_device_id",
                );
                None
            }
        },
        None => None,
    };
    let candidate = (wall, logical, parsed);
    match current {
        Some(curr) if *curr >= candidate => {}
        _ => *current = Some(candidate),
    }
}

fn into_local_max((wall, logical, origin_device_id): (i64, i32, Option<Uuid>)) -> MaxHlcLocal {
    MaxHlcLocal {
        wall,
        logical,
        origin_device_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        // Minimal schema covering every entity the digest reads.
        sqlx::query(
            "CREATE TABLE library (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                canonical_id TEXT,
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
            "CREATE TABLE playlist (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                canonical_id TEXT,
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
            "CREATE TABLE track (
                id INTEGER PRIMARY KEY,
                library_id INTEGER NOT NULL,
                file_path TEXT NOT NULL,
                file_hash TEXT NOT NULL,
                hlc_wall INTEGER NOT NULL DEFAULT 0,
                hlc_logical INTEGER NOT NULL DEFAULT 0,
                origin_device_id TEXT,
                payload_hash BLOB,
                rating_hlc_wall INTEGER NOT NULL DEFAULT 0,
                rating_hlc_logical INTEGER NOT NULL DEFAULT 0,
                rating_origin_device_id TEXT,
                rating_payload_hash BLOB
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE liked_track (
                track_id INTEGER PRIMARY KEY,
                liked_at INTEGER NOT NULL,
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
        for e in SUPPORTED_ENTITIES {
            sqlx::query("INSERT INTO metadata_digest_version (entity, version) VALUES (?, 0)")
                .bind(e)
                .execute(&pool)
                .await
                .unwrap();
        }
        pool
    }

    fn h(byte: u8) -> Vec<u8> {
        vec![byte; 32]
    }

    #[tokio::test]
    async fn read_library_empty_returns_zero_version_and_no_max_hlc() {
        let pool = pool().await;
        let d = read_local_digest(&pool, "library").await.unwrap();
        assert_eq!(d.entity, "library");
        assert_eq!(d.version, 0);
        assert!(d.members.is_empty());
        assert!(d.max_hlc.is_none());
        // Empty set hash matches the empty BLAKE3 digest (same
        // sanity assertion as the core test).
        assert_eq!(
            hex::encode(d.set_hash),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        );
    }

    #[tokio::test]
    async fn read_library_returns_sorted_members_and_max_hlc() {
        let pool = pool().await;
        // Two rows with deliberately reversed canonical_id ASC
        // ordering vs insert order — the SQL ORDER BY has to
        // dominate, otherwise the set_hash would mismatch the
        // server's.
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, hlc_wall, hlc_logical, origin_device_id, payload_hash)
             VALUES (1, 'B', 'zzz', 10, 1, NULL, ?)",
        )
        .bind(h(0xBB))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, hlc_wall, hlc_logical, origin_device_id, payload_hash)
             VALUES (2, 'A', 'aaa', 20, 5, NULL, ?)",
        )
        .bind(h(0xAA))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE metadata_digest_version SET version = 7 WHERE entity = 'library'")
            .execute(&pool)
            .await
            .unwrap();

        let d = read_local_digest(&pool, "library").await.unwrap();
        assert_eq!(d.version, 7);
        assert_eq!(d.members.len(), 2);
        assert_eq!(d.members[0].canonical_id, "aaa");
        assert_eq!(d.members[1].canonical_id, "zzz");
        // max_hlc is (20, 5, None) — strictly greater than (10, 1, None).
        let max = d.max_hlc.expect("max_hlc present with members");
        assert_eq!(max.wall, 20);
        assert_eq!(max.logical, 5);
        assert_eq!(max.origin_device_id, None);
    }

    #[tokio::test]
    async fn read_library_skips_rows_missing_canonical_or_payload_hash() {
        let pool = pool().await;
        // canonical_id NULL — excluded.
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, payload_hash)
             VALUES (1, 'X', NULL, ?)",
        )
        .bind(h(0x11))
        .execute(&pool)
        .await
        .unwrap();
        // payload_hash NULL — excluded.
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, payload_hash)
             VALUES (2, 'Y', 'yyy', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Both present — included.
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, payload_hash)
             VALUES (3, 'Z', 'zzz', ?)",
        )
        .bind(h(0x33))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "library").await.unwrap();
        assert_eq!(d.members.len(), 1);
        assert_eq!(d.members[0].canonical_id, "zzz");
    }

    #[tokio::test]
    async fn read_track_uses_composite_canonical_key() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, payload_hash)
             VALUES (1, 'L', 'lib-canon-uuid', ?)",
        )
        .bind(h(0xAA))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, payload_hash)
             VALUES (1, 1, '/music/a.flac', 'hash-a', ?)",
        )
        .bind(h(0x01))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "track").await.unwrap();
        assert_eq!(d.members.len(), 1);
        assert_eq!(
            d.members[0].canonical_id,
            "lib-canon-uuid\u{001F}/music/a.flac"
        );
    }

    #[tokio::test]
    async fn read_track_skips_rows_whose_library_lacks_canonical() {
        let pool = pool().await;
        // Library without canonical_id — track is excluded even if
        // payload_hash is set.
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, payload_hash)
             VALUES (1, 'L', NULL, ?)",
        )
        .bind(h(0xAA))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, payload_hash)
             VALUES (1, 1, '/music/a.flac', 'hash-a', ?)",
        )
        .bind(h(0x01))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "track").await.unwrap();
        assert!(d.members.is_empty());
    }

    #[tokio::test]
    async fn read_liked_track_joins_back_to_file_hash() {
        let pool = pool().await;
        sqlx::query("INSERT INTO library (id, name, canonical_id) VALUES (1, 'L', 'lib')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash)
             VALUES (1, 1, '/a.flac', 'hash-a')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO liked_track (track_id, liked_at, payload_hash)
             VALUES (1, 1000, ?)",
        )
        .bind(h(0xCC))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "liked_track").await.unwrap();
        assert_eq!(d.members.len(), 1);
        assert_eq!(d.members[0].canonical_id, "hash-a");
    }

    #[tokio::test]
    async fn read_track_rating_uses_rating_payload_hash_column() {
        let pool = pool().await;
        sqlx::query("INSERT INTO library (id, name, canonical_id) VALUES (1, 'L', 'lib')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, rating_payload_hash, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/a.flac', 'hash-a', ?, 42, 0)",
        )
        .bind(h(0xDD))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "track_rating").await.unwrap();
        assert_eq!(d.members.len(), 1);
        assert_eq!(d.members[0].canonical_id, "hash-a");
        let max = d.max_hlc.expect("rating max_hlc present");
        assert_eq!(max.wall, 42);
    }

    #[tokio::test]
    async fn read_track_rating_ignores_rows_without_rating_hash() {
        // Track with metadata payload_hash but no rating_payload_hash —
        // doesn't contribute to the rating set.
        let pool = pool().await;
        sqlx::query("INSERT INTO library (id, name, canonical_id) VALUES (1, 'L', 'lib')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, payload_hash, rating_payload_hash)
             VALUES (1, 1, '/a.flac', 'hash-a', ?, NULL)",
        )
        .bind(h(0x01))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "track_rating").await.unwrap();
        assert!(d.members.is_empty());
    }

    #[tokio::test]
    async fn unknown_entity_errors_with_typo_friendly_message() {
        let pool = pool().await;
        let err = read_local_digest(&pool, "playlist_track")
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("unknown entity 'playlist_track'"));
    }

    #[tokio::test]
    async fn max_hlc_uses_largest_tuple_not_first_inserted() {
        // Insert in non-monotonic order, confirm max_hlc reflects
        // the actual MAX across the set.
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, hlc_wall, hlc_logical, payload_hash)
             VALUES (1, 'A', 'aaa', 100, 5, ?)",
        )
        .bind(h(0x01))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO library (id, name, canonical_id, hlc_wall, hlc_logical, payload_hash)
             VALUES (2, 'B', 'bbb', 50, 9, ?)",
        )
        .bind(h(0x02))
        .execute(&pool)
        .await
        .unwrap();
        let d = read_local_digest(&pool, "library").await.unwrap();
        let max = d.max_hlc.expect("max_hlc present");
        // (100, 5) > (50, 9) under §2 lexicographic tuple order.
        assert_eq!(max.wall, 100);
        assert_eq!(max.logical, 5);
    }
}
