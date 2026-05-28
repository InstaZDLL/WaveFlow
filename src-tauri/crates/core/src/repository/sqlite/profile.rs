//! SQLite implementation of [`ProfileRepository`].

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::{
    domain::profile::Profile,
    error::CoreResult,
    repository::profile::{ProfileDeleteOutcome, ProfileDraft, ProfileRepository},
};

#[derive(Debug, Clone)]
pub struct SqliteProfileRepository {
    pool: SqlitePool,
}

impl SqliteProfileRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProfileRepository for SqliteProfileRepository {
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
               FROM profile WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(profile)
    }

    async fn insert(&self, draft: &ProfileDraft) -> CoreResult<i64> {
        let inserted = sqlx::query(
            "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES (?, ?, ?, '', ?, ?)",
        )
        .bind(&draft.name)
        .bind(&draft.color_id)
        .bind(draft.avatar_hash.as_deref())
        .bind(draft.now_ms)
        .bind(draft.now_ms)
        .execute(&self.pool)
        .await?;
        Ok(inserted.last_insert_rowid())
    }

    async fn set_data_dir(&self, id: i64, data_dir: &str) -> CoreResult<()> {
        sqlx::query("UPDATE profile SET data_dir = ? WHERE id = ?")
            .bind(data_dir)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn rename(&self, id: i64, new_name: &str) -> CoreResult<bool> {
        let updated = sqlx::query("UPDATE profile SET name = ? WHERE id = ?")
            .bind(new_name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(updated.rows_affected() > 0)
    }

    async fn touch_last_used(&self, id: i64, now_ms: i64) -> CoreResult<()> {
        sqlx::query("UPDATE profile SET last_used_at = ? WHERE id = ?")
            .bind(now_ms)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn delete_guarded(&self, id: i64) -> CoreResult<ProfileDeleteOutcome> {
        // Atomic DELETE guarded by an in-statement subquery: the "more than one
        // profile must remain" check and the delete are evaluated together, so
        // a concurrent deletion can never leave the table empty (TOCTOU-free).
        let deleted = sqlx::query(
            "DELETE FROM profile
              WHERE id = ?
                AND (SELECT COUNT(*) FROM profile) > 1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;

        if deleted.rows_affected() > 0 {
            return Ok(ProfileDeleteOutcome::Deleted);
        }
        // Disambiguate so the caller can show a meaningful error.
        if self.exists(id).await? {
            Ok(ProfileDeleteOutcome::WasLast)
        } else {
            Ok(ProfileDeleteOutcome::NotFound)
        }
    }

    async fn exists(&self, id: i64) -> CoreResult<bool> {
        let row: Option<i64> = sqlx::query_scalar("SELECT id FROM profile WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }
}
