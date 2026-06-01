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
/// (`library`, `track`, …) doesn't need a CHECK-constraint migration —
/// pinning them as constants keeps the call sites typo-safe.
pub const ENTITY_PLAYLIST: &str = "playlist";

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

/// Mint a canonical id on the local row + plant the mapping. Shared
/// between [`ensure_local_playlist`] (outbound) and the apply path
/// (inbound) where the canonical is server-supplied. Idempotent on
/// the mapping: a duplicate `(entity, canonical_id)` keeps the
/// existing row.
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
