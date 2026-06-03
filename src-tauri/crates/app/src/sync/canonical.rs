//! Canonical entity-id mapping. Phase 1.f.desktop.4b.
//!
//! Every syncable entity carries two identifiers:
//!
//! - **`local_id` (`i64`)** — the SQLite rowid the rest of the app
//!   keys on. Differs per device. Stable for the lifetime of the row
//!   on a given install.
//! - **`canonical_id` (UUID v4 stringified)** — minted on the device
//!   that originally inserted the entity and carried verbatim through
//!   every outbound op. Lets a peer device translate an inbound
//!   `entity_id` back to its own local rowid via [`sync_id_map`].
//!
//! The mapping table is the source of truth for the local↔canonical
//! pairing. The `playlist.canonical_id` column is kept in sync as a
//! convenience for queries that need to project the canonical id
//! without a JOIN (the WS apply path uses both). Two rows for the
//! same entity ALWAYS land or roll back together — both writes share
//! the caller's transaction.
//!
//! ## Migration backfill
//!
//! [`20260603000000_sync_canonical_id`](../../../migrations/profile/20260603000000_sync_canonical_id.sql)
//! plants a fresh UUID on every pre-existing `playlist` row + seeds
//! the `sync_id_map` table so the helpers below can assume an entity
//! that exists locally is mapping-resolvable in O(1).

use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::error::AppResult;

/// Entity tags. Free-form `TEXT` in the schema so a new family
/// doesn't need a CHECK-constraint migration — pinning them as
/// constants keeps the call sites typo-safe.
pub const ENTITY_PLAYLIST: &str = "playlist";

/// Library entity tag. Phase 1.f.desktop.5 — same canonical-id-UUID
/// pattern as playlist (user-curated, no natural cross-device key).
pub const ENTITY_LIBRARY: &str = "library";

/// Liked-track entity tag. Phase 1.f.desktop.5. `entity_id` carries
/// the BLAKE3 `track.file_hash` directly — same physical file on a
/// peer device hashes identically, so no mapping table is needed.
pub const ENTITY_LIKED_TRACK: &str = "liked_track";

/// Track-rating entity tag. Phase 1.f.desktop.5. Same key strategy
/// as `liked_track` — `entity_id` is the BLAKE3 file hash, the
/// payload carries the 0-5 rating (or null to clear).
pub const ENTITY_TRACK_RATING: &str = "track_rating";

/// Resolve a local rowid to its canonical UUID. Returns `None` when
/// no mapping row exists — the caller decides whether that's a hard
/// error (outbound hook: the entity was just inserted and its mapping
/// should have been planted in the same tx) or a soft path (inbound
/// op against an entity that hasn't been seen locally yet).
pub async fn canonical_for_local(
    conn: &mut SqliteConnection,
    entity: &str,
    local_id: i64,
) -> AppResult<Option<String>> {
    let row: Option<String> = sqlx::query_scalar(
        "SELECT canonical_id FROM sync_id_map WHERE entity = ? AND local_id = ?",
    )
    .bind(entity)
    .bind(local_id)
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

/// Reverse lookup — the inbound WS path's hot operation. Given a
/// canonical UUID broadcast by another device, return the local
/// rowid if this device has already seen it; `None` otherwise (the
/// apply path will create + map a fresh row).
pub async fn local_for_canonical(
    conn: &mut SqliteConnection,
    entity: &str,
    canonical_id: &str,
) -> AppResult<Option<i64>> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT local_id FROM sync_id_map WHERE entity = ? AND canonical_id = ?",
    )
    .bind(entity)
    .bind(canonical_id)
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

/// Ensure a freshly-inserted local row has a canonical id + mapping
/// row. Idempotent on the mapping (`INSERT OR IGNORE`) but assumes
/// the canonical column on the entity table is currently NULL — used
/// from outbound paths right after the entity INSERT.
///
/// Returns the canonical id that's now active for the row. When a
/// row already had a canonical (e.g. mapping seeded by the migration
/// backfill), the existing value is returned unchanged.
pub async fn ensure_local_playlist(
    conn: &mut SqliteConnection,
    local_id: i64,
) -> AppResult<String> {
    // Prefer the existing canonical if one was planted by the
    // migration backfill — avoids minting a fresh UUID and silently
    // breaking the mapping a future remote op would resolve against.
    if let Some(existing) = canonical_for_local(conn, ENTITY_PLAYLIST, local_id).await? {
        return Ok(existing);
    }
    // Read the column directly too — covers the corner case where a
    // future code path inserts a `playlist` row with a canonical id
    // baked in (e.g. M3U import that reused a known UUID) before
    // calling this helper. The column wins so we don't overwrite the
    // caller's choice.
    let existing_col: Option<String> =
        sqlx::query_scalar("SELECT canonical_id FROM playlist WHERE id = ?")
            .bind(local_id)
            .fetch_optional(&mut *conn)
            .await?;
    let canonical = match existing_col.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => Uuid::new_v4().to_string(),
    };
    set_canonical_playlist(conn, local_id, &canonical).await?;
    Ok(canonical)
}

/// Library variant of [`ensure_local_playlist`]. Same flow: prefer
/// an existing mapping, fall back to the column the migration's
/// AFTER INSERT trigger filled in, mint a fresh UUID only if both
/// are missing. Single trip through [`set_canonical_library`] keeps
/// the playlist column + sync_id_map row coherent.
pub async fn ensure_local_library(conn: &mut SqliteConnection, local_id: i64) -> AppResult<String> {
    if let Some(existing) = canonical_for_local(conn, ENTITY_LIBRARY, local_id).await? {
        return Ok(existing);
    }
    let existing_col: Option<String> =
        sqlx::query_scalar("SELECT canonical_id FROM library WHERE id = ?")
            .bind(local_id)
            .fetch_optional(&mut *conn)
            .await?;
    let canonical = match existing_col.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => Uuid::new_v4().to_string(),
    };
    set_canonical_library(conn, local_id, &canonical).await?;
    Ok(canonical)
}

/// Library variant of [`set_canonical_playlist`]. Same defensive
/// DELETE of any prior `(entity, local_id)` row pointing at a
/// different canonical before the INSERT OR IGNORE.
pub async fn set_canonical_library(
    conn: &mut SqliteConnection,
    local_id: i64,
    canonical_id: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE library SET canonical_id = ? WHERE id = ?")
        .bind(canonical_id)
        .bind(local_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "DELETE FROM sync_id_map
          WHERE entity = ?
            AND local_id = ?
            AND canonical_id != ?",
    )
    .bind(ENTITY_LIBRARY)
    .bind(local_id)
    .bind(canonical_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO sync_id_map (entity, canonical_id, local_id)
         VALUES (?, ?, ?)",
    )
    .bind(ENTITY_LIBRARY)
    .bind(canonical_id)
    .bind(local_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Look up a local `track.id` by its BLAKE3 file hash. Tracks don't
/// carry a canonical_id column — the file content already is the
/// cross-device key, so the inbound apply path resolves
/// `liked_track` / `track_rating` ops by hash → local track id
/// directly against `track`. Returns `None` when the file hasn't
/// been scanned on this device yet (the apply branch surfaces it as
/// Ignored so the cursor still advances).
pub async fn local_track_for_hash(
    conn: &mut SqliteConnection,
    file_hash: &str,
) -> AppResult<Option<i64>> {
    let row: Option<i64> = sqlx::query_scalar("SELECT id FROM track WHERE file_hash = ?")
        .bind(file_hash)
        .fetch_optional(conn)
        .await?;
    Ok(row)
}

/// Read a track's `file_hash` by its local rowid. Used by the
/// outbound hooks (`toggle_like_track`, `set_track_rating`) to
/// stamp the op's `entity_id` with the file-content key the peer
/// device will use for the reverse lookup.
pub async fn file_hash_for_local_track(
    conn: &mut SqliteConnection,
    local_id: i64,
) -> AppResult<Option<String>> {
    let row: Option<String> = sqlx::query_scalar("SELECT file_hash FROM track WHERE id = ?")
        .bind(local_id)
        .fetch_optional(conn)
        .await?;
    Ok(row)
}

/// Mint a canonical id on the local row + plant the mapping. Shared
/// between [`ensure_local_playlist`] (outbound) and the apply path
/// (inbound) where the canonical is server-supplied.
///
/// Idempotent on `(entity, local_id)`: a prior mapping pointing the
/// same local row at a DIFFERENT canonical is dropped first, so a
/// re-mint never leaves orphan rows in `sync_id_map`. The current
/// callers can't trigger this (ensure_local_playlist short-circuits
/// on existing mapping, apply's insert branch always operates on a
/// fresh local_id), but the defensive DELETE keeps the invariant
/// "exactly one (entity, local_id) row" intact against future call
/// sites — and the cost is one cheap DELETE against the same index
/// the INSERT below uses.
pub async fn set_canonical_playlist(
    conn: &mut SqliteConnection,
    local_id: i64,
    canonical_id: &str,
) -> AppResult<()> {
    sqlx::query("UPDATE playlist SET canonical_id = ? WHERE id = ?")
        .bind(canonical_id)
        .bind(local_id)
        .execute(&mut *conn)
        .await?;
    // Drop any prior `(entity, local_id)` row pointing at a
    // different canonical. Skipped when the existing row already
    // matches `canonical_id` (the INSERT OR IGNORE below would
    // otherwise no-op cleanly, but pre-deleting is cheaper than
    // walking the UNIQUE index a second time).
    sqlx::query(
        "DELETE FROM sync_id_map
          WHERE entity = ?
            AND local_id = ?
            AND canonical_id != ?",
    )
    .bind(ENTITY_PLAYLIST)
    .bind(local_id)
    .bind(canonical_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT OR IGNORE INTO sync_id_map (entity, canonical_id, local_id)
         VALUES (?, ?, ?)",
    )
    .bind(ENTITY_PLAYLIST)
    .bind(canonical_id)
    .bind(local_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Drop a mapping row. Used by the apply path's `delete` branch
/// AFTER the local row is gone so a future replay of the same
/// canonical id surfaces as an "unknown entity" (the WS subscriber
/// silently ignores instead of resurrecting). Idempotent.
pub async fn drop_mapping(
    conn: &mut SqliteConnection,
    entity: &str,
    canonical_id: &str,
) -> AppResult<()> {
    sqlx::query("DELETE FROM sync_id_map WHERE entity = ? AND canonical_id = ?")
        .bind(entity)
        .bind(canonical_id)
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE playlist (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                canonical_id TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE sync_id_map (
                entity TEXT NOT NULL,
                canonical_id TEXT NOT NULL,
                local_id INTEGER NOT NULL,
                PRIMARY KEY (entity, canonical_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn insert_playlist(pool: &SqlitePool, name: &str) -> i64 {
        let r: (i64,) = sqlx::query_as(
            "INSERT INTO playlist (name, canonical_id) VALUES (?, NULL) RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
        r.0
    }

    /// `max_connections = 1` + `:memory:` means a conn-holding test
    /// MUST release the conn before calling `pool.fetch_*` (the
    /// pool would otherwise hand it the only slot we already have).
    /// Scoping the conn in a block solves it.
    #[tokio::test]
    async fn ensure_local_playlist_mints_uuid_and_maps() {
        let pool = pool().await;
        let id = insert_playlist(&pool, "p1").await;
        let (canonical, again) = {
            let mut conn = pool.acquire().await.unwrap();
            let c = ensure_local_playlist(&mut conn, id).await.unwrap();
            let again = ensure_local_playlist(&mut conn, id).await.unwrap();
            (c, again)
        };
        Uuid::parse_str(&canonical).expect("canonical is a valid UUID");
        assert_eq!(canonical, again);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_id_map")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn lookup_round_trips_both_directions() {
        let pool = pool().await;
        let id = insert_playlist(&pool, "p1").await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = ensure_local_playlist(&mut conn, id).await.unwrap();
        let resolved_canonical = canonical_for_local(&mut conn, ENTITY_PLAYLIST, id)
            .await
            .unwrap();
        let resolved_local = local_for_canonical(&mut conn, ENTITY_PLAYLIST, &canonical)
            .await
            .unwrap();
        assert_eq!(resolved_canonical, Some(canonical));
        assert_eq!(resolved_local, Some(id));
    }

    #[tokio::test]
    async fn set_canonical_overwrites_local_canonical_column() {
        let pool = pool().await;
        let id = insert_playlist(&pool, "p1").await;
        let server_canonical = Uuid::new_v4().to_string();
        let resolved_local = {
            let mut conn = pool.acquire().await.unwrap();
            set_canonical_playlist(&mut conn, id, &server_canonical)
                .await
                .unwrap();
            local_for_canonical(&mut conn, ENTITY_PLAYLIST, &server_canonical)
                .await
                .unwrap()
        };
        let row: Option<String> =
            sqlx::query_scalar("SELECT canonical_id FROM playlist WHERE id = ?")
                .bind(id)
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert_eq!(row, Some(server_canonical));
        assert_eq!(resolved_local, Some(id));
    }

    /// `set_canonical_playlist` must keep exactly one mapping row
    /// per `(entity, local_id)` pair even when called twice with
    /// different canonical UUIDs. Without the defensive DELETE the
    /// second INSERT OR IGNORE would leave the stale first row
    /// hanging — the reverse-lookup of the OLD canonical would
    /// still resolve to the local row, which is precisely the
    /// inconsistency the helper exists to prevent.
    #[tokio::test]
    async fn set_canonical_replaces_prior_mapping_for_same_local() {
        let pool = pool().await;
        let id = insert_playlist(&pool, "p1").await;
        let canonical_a = Uuid::new_v4().to_string();
        let canonical_b = Uuid::new_v4().to_string();
        {
            let mut conn = pool.acquire().await.unwrap();
            set_canonical_playlist(&mut conn, id, &canonical_a)
                .await
                .unwrap();
            set_canonical_playlist(&mut conn, id, &canonical_b)
                .await
                .unwrap();
        }
        // Exactly one mapping row for the local id.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sync_id_map WHERE entity = ? AND local_id = ?",
        )
        .bind(ENTITY_PLAYLIST)
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        // Stale canonical is unreachable; fresh one resolves.
        let mut conn = pool.acquire().await.unwrap();
        assert!(
            local_for_canonical(&mut conn, ENTITY_PLAYLIST, &canonical_a)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            local_for_canonical(&mut conn, ENTITY_PLAYLIST, &canonical_b)
                .await
                .unwrap(),
            Some(id)
        );
    }

    #[tokio::test]
    async fn drop_mapping_is_idempotent() {
        let pool = pool().await;
        let id = insert_playlist(&pool, "p1").await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = ensure_local_playlist(&mut conn, id).await.unwrap();
        drop_mapping(&mut conn, ENTITY_PLAYLIST, &canonical)
            .await
            .unwrap();
        assert!(local_for_canonical(&mut conn, ENTITY_PLAYLIST, &canonical)
            .await
            .unwrap()
            .is_none());
        drop_mapping(&mut conn, ENTITY_PLAYLIST, &canonical)
            .await
            .unwrap();
    }
}
