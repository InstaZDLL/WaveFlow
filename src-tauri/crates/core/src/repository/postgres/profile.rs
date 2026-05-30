//! Postgres implementation of [`ProfileRepository`].
//!
//! Mirrors the SQLite implementation in
//! [`crate::repository::sqlite::profile`] one-for-one — same semantics,
//! same query shape, adjusted for Postgres conventions:
//!
//! - `$1`, `$2`, … placeholders instead of `?`
//! - `RETURNING id` instead of `last_insert_rowid()`
//! - The same atomic guarded-delete pattern: a single statement whose
//!   `WHERE` clause references a subquery counting the surviving rows,
//!   so a concurrent delete cannot leave the table empty (TOCTOU-free).
//!
//! The schema this targets ships in `waveflow-server/migrations/`
//! (see RFC-001 §6.5). `id BIGSERIAL PRIMARY KEY`, the rest of the
//! columns mirror the SQLite shape.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::{
    domain::profile::Profile,
    error::CoreResult,
    repository::profile::{ProfileDeleteOutcome, ProfileDraft, ProfileRepository},
};

#[derive(Debug, Clone)]
pub struct PostgresProfileRepository {
    pool: PgPool,
}

impl PostgresProfileRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProfileRepository for PostgresProfileRepository {
    async fn list_all(&self) -> CoreResult<Vec<Profile>> {
        let profiles = sqlx::query_as::<_, Profile>(
            "SELECT id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
               FROM profile
              ORDER BY last_used_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(profiles)
    }

    async fn get(&self, id: i64) -> CoreResult<Option<Profile>> {
        let profile = sqlx::query_as::<_, Profile>(
            "SELECT id, name, color_id, avatar_hash, data_dir, created_at, last_used_at
               FROM profile WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(profile)
    }

    async fn insert(&self, draft: &ProfileDraft) -> CoreResult<i64> {
        // `RETURNING id` is the Postgres equivalent of
        // `last_insert_rowid()` — fetches the server-assigned BIGSERIAL
        // in the same round-trip as the INSERT.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES ($1, $2, $3, '', $4, $5)
             RETURNING id",
        )
        .bind(&draft.name)
        .bind(&draft.color_id)
        .bind(draft.avatar_hash.as_deref())
        .bind(draft.now_ms)
        .bind(draft.now_ms)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    async fn set_data_dir(&self, id: i64, data_dir: &str) -> CoreResult<()> {
        sqlx::query("UPDATE profile SET data_dir = $1 WHERE id = $2")
            .bind(data_dir)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn rename(&self, id: i64, new_name: &str) -> CoreResult<bool> {
        let updated = sqlx::query("UPDATE profile SET name = $1 WHERE id = $2")
            .bind(new_name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(updated.rows_affected() > 0)
    }

    async fn touch_last_used(&self, id: i64, now_ms: i64) -> CoreResult<()> {
        sqlx::query("UPDATE profile SET last_used_at = $1 WHERE id = $2")
            .bind(now_ms)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_guarded(&self, id: i64) -> CoreResult<ProfileDeleteOutcome> {
        // The SQLite sibling can rely on a single-statement
        // `WHERE … AND (SELECT COUNT(*) … ) > 1` because SQLite
        // serialises every writer at the file level. Postgres' READ
        // COMMITTED isolation does not: two concurrent DELETEs against
        // distinct rows can each read `COUNT = 2`, decide they're safe,
        // lock their own row, and both commit — emptying the table.
        //
        // Take a `SHARE ROW EXCLUSIVE` lock on the table for the
        // duration of the transaction: blocks concurrent writers
        // (DELETE / UPDATE / INSERT) while letting SELECTs proceed.
        // Inside that critical section the `COUNT(*)` re-check is
        // guaranteed to observe the same row set the DELETE will see.
        let mut tx = self.pool.begin().await?;

        sqlx::query("LOCK TABLE profile IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await?;

        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM profile WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;

        if exists.is_none() {
            // Nothing to delete; release the lock cheaply.
            tx.commit().await?;
            return Ok(ProfileDeleteOutcome::NotFound);
        }

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profile")
            .fetch_one(&mut *tx)
            .await?;

        if count <= 1 {
            tx.commit().await?;
            return Ok(ProfileDeleteOutcome::WasLast);
        }

        sqlx::query("DELETE FROM profile WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(ProfileDeleteOutcome::Deleted)
    }

    async fn exists(&self, id: i64) -> CoreResult<bool> {
        let row: Option<i64> = sqlx::query_scalar("SELECT id FROM profile WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }
}
