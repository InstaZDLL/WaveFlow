//! Profile-level metadata helpers for `app.db`.
//!
//! Phase 1.g.3-desktop ships `profile.canonical_id` — the UUID v4
//! that the drain task injects into every outbound `sync_op` so the
//! server (Phase 1.g.0, waveflow-server PR #26) can route the op to a
//! materialised server profile. The migration only widens the schema;
//! UUID generation for pre-existing rows happens here at startup so
//! the entropy + version/variant bits come from Rust's `uuid` crate
//! rather than a brittle hand-rolled SQL `randomblob(16)` twiddle.
//!
//! Three entry points:
//!
//! - [`backfill_canonical_ids`] — startup hook called right after the
//!   app.db migrations run. Idempotent: only rows with
//!   `canonical_id IS NULL` get touched.
//! - [`ensure_canonical_id`] — called by `commands::profile::
//!   create_profile` immediately after the new row is inserted, so a
//!   profile created via the UI never has a NULL canonical id even
//!   for the moment between the INSERT and the next drain pass.
//! - [`canonical_id_for`] — drain-task lookup. Returns `None` if the
//!   row is missing (deleted mid-pass) or the canonical id hasn't
//!   been backfilled yet (theoretically unreachable post-startup; the
//!   drain treats it as "skip injection" rather than error so a corner
//!   case never blocks the wider sync push).

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::AppResult;

/// Mint a fresh UUID v4 for every `profile` row whose `canonical_id`
/// is still NULL. Returns the number of rows updated.
///
/// Wraps the loop in a transaction so a crash mid-backfill leaves the
/// table in a consistent state — either no rows have the new column
/// populated, or all the targeted rows do. The common case (every
/// boot after the first post-migration boot) hits zero rows and
/// commits an empty transaction, which is essentially free.
pub async fn backfill_canonical_ids(pool: &SqlitePool) -> AppResult<usize> {
    let needs_uuid: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM profile WHERE canonical_id IS NULL ORDER BY id")
            .fetch_all(pool)
            .await?;

    if needs_uuid.is_empty() {
        return Ok(0);
    }

    let mut tx = pool.begin().await?;
    for id in &needs_uuid {
        let canonical = Uuid::new_v4().to_string();
        sqlx::query("UPDATE profile SET canonical_id = ? WHERE id = ? AND canonical_id IS NULL")
            .bind(&canonical)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    tracing::info!(count = needs_uuid.len(), "backfilled profile.canonical_id");
    Ok(needs_uuid.len())
}

/// Plant a fresh canonical UUID on the given profile row if it
/// doesn't have one yet. Returns the canonical id (either the freshly
/// minted one or the existing value). Idempotent — concurrent callers
/// race on the `WHERE canonical_id IS NULL` predicate, the winner's
/// UUID lands, the loser SELECTs it back.
pub async fn ensure_canonical_id(pool: &SqlitePool, profile_id: i64) -> AppResult<String> {
    if let Some(existing) = canonical_id_for(pool, profile_id).await? {
        return Ok(existing);
    }
    let candidate = Uuid::new_v4().to_string();
    sqlx::query("UPDATE profile SET canonical_id = ? WHERE id = ? AND canonical_id IS NULL")
        .bind(&candidate)
        .bind(profile_id)
        .execute(pool)
        .await?;
    // Re-read — either we wrote `candidate` or a racing caller wrote
    // its own value first. In both cases the row now has a non-NULL
    // canonical, and the read returns the winning value.
    canonical_id_for(pool, profile_id).await?.ok_or_else(|| {
        crate::error::AppError::Other(format!(
            "profile {profile_id} disappeared mid-ensure_canonical_id"
        ))
    })
}

/// Look up the canonical id of a given profile. `None` for rows that
/// don't exist (deleted) or haven't been backfilled yet.
pub async fn canonical_id_for(pool: &SqlitePool, profile_id: i64) -> AppResult<Option<String>> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT canonical_id FROM profile WHERE id = ?")
            .bind(profile_id)
            .fetch_optional(pool)
            .await?;
    // Outer `Option` = row existence; inner = column NULL-ness.
    Ok(row.flatten())
}
