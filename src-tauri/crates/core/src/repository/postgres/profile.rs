//! Tenant-scoped Postgres profile repository for `waveflow-server`.
//!
//! Unlike [`crate::repository::sqlite::profile::SqliteProfileRepository`],
//! this type deliberately **does not implement [`ProfileRepository`]**.
//! The trait surface is single-tenant (no `user_id` parameter), and
//! exposing it on a multi-tenant Postgres backend would let a careless
//! caller bypass user isolation through `list_all() / get(id) /
//! insert(draft)`. Server handlers consume the inherent `*_for_user`
//! methods on this struct directly; if a future admin tool needs
//! cross-tenant reads it'll get its own dedicated type (or a
//! `PostgresAdminProfileRepository` newtype) rather than re-opening
//! this footgun.
//!
//! Conventions vs. the SQLite sibling:
//!
//! - `$1`, `$2`, … placeholders instead of `?`
//! - `RETURNING id` instead of `last_insert_rowid()`
//! - `delete_guarded_for_user` opens a transaction, takes
//!   `SELECT id FROM profile WHERE user_id = $1 ORDER BY id FOR
//!   UPDATE` up-front (row-level locks on every profile the user
//!   owns, in a deterministic order to avoid cross-transaction
//!   deadlocks), then re-checks the per-user COUNT before the
//!   DELETE. Concurrent deletes from the same user serialise on
//!   those row locks so neither can race past the count check and
//!   empty the user's profile set; deletes from a different user
//!   touch disjoint rows and proceed in parallel.
//!
//! Schema (`profile.user_id BIGINT NOT NULL REFERENCES users(id)
//! ON DELETE CASCADE`) lives in `waveflow-server/migrations/`
//! (see RFC-001 §6.5).

use sqlx::PgPool;

use crate::{
    domain::profile::Profile,
    error::CoreResult,
    repository::profile::{ProfileDeleteOutcome, ProfileDraft},
};

#[derive(Debug, Clone)]
pub struct PostgresProfileRepository {
    pool: PgPool,
}

impl PostgresProfileRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ===== Tenant-scoped inherent methods =====
    //
    // The trait above is single-tenant: it acts on the `profile` table
    // without filtering by owner. `waveflow-server` is multi-tenant, so
    // it needs the same operations scoped to a `user_id`. These methods
    // live as **inherent** impls (not on `ProfileRepository`) so the
    // desktop's `SqliteProfileRepository` doesn't have to gain a fake
    // user-id concept it'd never use — the desktop carries a single
    // implicit user.
    //
    // Every query filters by `user_id` so a request from user A can
    // never read, mutate or delete a profile belonging to user B —
    // tenant isolation is enforced at the storage layer, not just at
    // the handler.

    /// Profiles owned by `user_id`, most-recently-used first.
    pub async fn list_for_user(&self, user_id: i64) -> CoreResult<Vec<Profile>> {
        let profiles = sqlx::query_as::<_, Profile>(
            "SELECT id, user_id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
               FROM profile
              WHERE user_id = $1
              ORDER BY last_used_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(profiles)
    }

    /// Fetch a profile only if it's owned by `user_id`. Returning
    /// `Ok(None)` for an existing-but-foreign profile keeps the
    /// not-found / not-owned cases indistinguishable to the caller —
    /// a deliberate choice so the API doesn't leak the existence of
    /// other users' rows.
    pub async fn get_for_user(&self, id: i64, user_id: i64) -> CoreResult<Option<Profile>> {
        let profile = sqlx::query_as::<_, Profile>(
            "SELECT id, user_id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
               FROM profile WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(profile)
    }

    /// Insert a profile owned by `user_id`. Returns the new row id.
    /// The FK on `profile.user_id REFERENCES users(id)` makes a
    /// dangling user_id fail at the DB layer, so callers don't need
    /// a separate existence check.
    pub async fn insert_for_user(
        &self,
        draft: &ProfileDraft,
        user_id: i64,
    ) -> CoreResult<i64> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO profile (user_id, name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES ($1, $2, $3, $4, '', $5, $6)
             RETURNING id",
        )
        .bind(user_id)
        .bind(&draft.name)
        .bind(&draft.color_id)
        .bind(draft.avatar_hash.as_deref())
        .bind(draft.now_ms)
        .bind(draft.now_ms)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Rename a profile only if owned by `user_id`. `Ok(false)` covers
    /// both "no such id" and "id belongs to another user".
    pub async fn rename_for_user(
        &self,
        id: i64,
        new_name: &str,
        user_id: i64,
    ) -> CoreResult<bool> {
        let updated =
            sqlx::query("UPDATE profile SET name = $1 WHERE id = $2 AND user_id = $3")
                .bind(new_name)
                .bind(id)
                .bind(user_id)
                .execute(&self.pool)
                .await?;
        Ok(updated.rows_affected() > 0)
    }

    /// Stamp `last_used_at` only if owned by `user_id`. No-op when
    /// the row doesn't match (no error — touch is best-effort).
    pub async fn touch_last_used_for_user(
        &self,
        id: i64,
        now_ms: i64,
        user_id: i64,
    ) -> CoreResult<()> {
        sqlx::query("UPDATE profile SET last_used_at = $1 WHERE id = $2 AND user_id = $3")
            .bind(now_ms)
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Tenant-scoped delete. The "more than one profile must remain"
    /// invariant is enforced **per-user** (the COUNT subquery filters
    /// by `user_id`) so user A's delete is never blocked by user B's
    /// row.
    ///
    /// Concurrency: open a transaction and acquire `FOR UPDATE` row
    /// locks on *every* profile row owned by `user_id` up-front. Two
    /// concurrent deletes from the same user serialise on those row
    /// locks, so the COUNT(*) re-check below observes the same row
    /// set the DELETE will see — without holding a table-level lock
    /// that would block writes from other tenants. (A bare
    /// `SELECT ... FOR UPDATE` on the target row alone wouldn't
    /// suffice: two concurrent deletes targeting *different* rows of
    /// the same user could each succeed and empty the set; locking
    /// the whole tenant's row set is what closes that window.)
    pub async fn delete_guarded_for_user(
        &self,
        id: i64,
        user_id: i64,
    ) -> CoreResult<ProfileDeleteOutcome> {
        let mut tx = self.pool.begin().await?;

        // `ORDER BY id` so two transactions that both target the same
        // user acquire their row locks in the same order — without it
        // the lock-acquisition order depends on Postgres' chosen plan
        // and a concurrent INSERT could push us toward an A→B / B→A
        // deadlock loop.
        sqlx::query("SELECT id FROM profile WHERE user_id = $1 ORDER BY id FOR UPDATE")
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?;

        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM profile WHERE id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;

        if exists.is_none() {
            tx.commit().await?;
            return Ok(ProfileDeleteOutcome::NotFound);
        }

        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM profile WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;

        if count <= 1 {
            tx.commit().await?;
            return Ok(ProfileDeleteOutcome::WasLast);
        }

        sqlx::query("DELETE FROM profile WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(ProfileDeleteOutcome::Deleted)
    }

    /// Set the resolved `data_dir` for a freshly inserted profile,
    /// scoped to the owning user. `Ok(false)` covers both "no such
    /// id" and "id belongs to another user" — same shape as
    /// `rename_for_user` so callers can distinguish a no-op from a
    /// successful write (a silently-discarded update would mask
    /// `insert_for_user` + `set_data_dir_for_user` race bugs).
    pub async fn set_data_dir_for_user(
        &self,
        id: i64,
        data_dir: &str,
        user_id: i64,
    ) -> CoreResult<bool> {
        let updated =
            sqlx::query("UPDATE profile SET data_dir = $1 WHERE id = $2 AND user_id = $3")
                .bind(data_dir)
                .bind(id)
                .bind(user_id)
                .execute(&self.pool)
                .await?;
        Ok(updated.rows_affected() > 0)
    }
}

