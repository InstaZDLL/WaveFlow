//! Tenant-scoped Postgres library repository for `waveflow-server`.
//!
//! Same design as [`super::profile::PostgresProfileRepository`]: this
//! type does NOT implement the single-tenant
//! [`crate::repository::library::LibraryRepository`] trait. Every
//! method takes both a `profile_id` (the resource's owning profile)
//! AND a `user_id` (the request's authenticated user), and the SQL
//! enforces `library.profile_id` ↔ `profile.user_id` ownership in the
//! same statement — a careless `Box<dyn LibraryRepository>` over this
//! backend would otherwise let user A read user B's libraries.
//!
//! Counts (`track_count`, `album_count`, `artist_count`, `genre_count`,
//! `folder_count`) are stubbed at `0` for now. They become real
//! aggregates once the track / album / playlist tables ship; the wire
//! shape stays the same so the web client doesn't need to adapt.
//!
//! Schema lives in `waveflow-server/migrations/`:
//! `library.profile_id BIGINT NOT NULL REFERENCES profile(id) ON DELETE CASCADE`.

use sqlx::PgPool;

use crate::{
    domain::library::Library,
    error::CoreResult,
    repository::library::{LibraryDraft, LibraryUpdate},
};

#[derive(Debug, Clone)]
pub struct PostgresLibraryRepository {
    pool: PgPool,
}

impl PostgresLibraryRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Libraries owned by `profile_id`, most-recently-updated first.
    /// The `EXISTS` clause cross-validates that the *user* owns the
    /// profile — so user A passing `profile_id` = user B's profile
    /// gets an empty list, not a tenancy bypass.
    pub async fn list_for_profile(
        &self,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Vec<Library>> {
        let libs = sqlx::query_as::<_, Library>(
            "SELECT l.id,
                    l.profile_id,
                    l.name,
                    l.description,
                    l.color_id,
                    l.icon_id,
                    l.created_at,
                    l.updated_at,
                    0::bigint AS track_count,
                    0::bigint AS album_count,
                    0::bigint AS artist_count,
                    0::bigint AS genre_count,
                    0::bigint AS folder_count
               FROM library l
              WHERE l.profile_id = $1
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = l.profile_id
                       AND p.user_id = $2
                )
              ORDER BY l.updated_at DESC",
        )
        .bind(profile_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(libs)
    }

    /// Fetch one library by id, scoped to both the profile and the
    /// user. `Ok(None)` blurs "no such id", "id belongs to another
    /// profile", and "profile belongs to another user" so the API
    /// never leaks the existence of foreign rows.
    pub async fn get_for_profile(
        &self,
        id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<Library>> {
        let lib = sqlx::query_as::<_, Library>(
            "SELECT l.id,
                    l.profile_id,
                    l.name,
                    l.description,
                    l.color_id,
                    l.icon_id,
                    l.created_at,
                    l.updated_at,
                    0::bigint AS track_count,
                    0::bigint AS album_count,
                    0::bigint AS artist_count,
                    0::bigint AS genre_count,
                    0::bigint AS folder_count
               FROM library l
              WHERE l.id = $1
                AND l.profile_id = $2
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = l.profile_id
                       AND p.user_id = $3
                )",
        )
        .bind(id)
        .bind(profile_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(lib)
    }

    /// Insert a library owned by `profile_id`. The `INSERT ... SELECT
    /// ... WHERE EXISTS` pattern lets us fail fast on a foreign or
    /// non-existent profile in the same round-trip as the write —
    /// no race window for the user_id to flip between an existence
    /// check and the insert.
    ///
    /// Returns the new row via `UPDATE … RETURNING …` semantics:
    /// `Some(library)` on success, `None` when the profile isn't
    /// owned by `user_id`.
    pub async fn insert_for_profile(
        &self,
        draft: &LibraryDraft,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<Library>> {
        let lib = sqlx::query_as::<_, Library>(
            "INSERT INTO library (profile_id, name, description, color_id, icon_id, created_at, updated_at)
             SELECT $1, $2, $3, $4, $5, $6, $6
               FROM profile p
              WHERE p.id = $1 AND p.user_id = $7
             RETURNING id,
                       profile_id,
                       name,
                       description,
                       color_id,
                       icon_id,
                       created_at,
                       updated_at,
                       0::bigint AS track_count,
                       0::bigint AS album_count,
                       0::bigint AS artist_count,
                       0::bigint AS genre_count,
                       0::bigint AS folder_count",
        )
        .bind(profile_id)
        .bind(&draft.name)
        .bind(draft.description.as_deref())
        .bind(&draft.color_id)
        .bind(&draft.icon_id)
        .bind(draft.now_ms)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(lib)
    }

    /// Partial update via SQL `COALESCE` — `None` fields preserve the
    /// existing value. Returns the updated row in one round-trip
    /// (same `UPDATE ... RETURNING` pattern as
    /// [`super::profile::PostgresProfileRepository::rename_for_user`])
    /// so a concurrent DELETE can't flip a successful update into a
    /// misleading 404. `Ok(None)` when the library isn't owned by
    /// the (profile_id, user_id) pair.
    pub async fn update_for_profile(
        &self,
        id: i64,
        patch: &LibraryUpdate,
        now_ms: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<Library>> {
        let lib = sqlx::query_as::<_, Library>(
            "UPDATE library
                SET name        = COALESCE($1, name),
                    description = COALESCE($2, description),
                    color_id    = COALESCE($3, color_id),
                    icon_id     = COALESCE($4, icon_id),
                    updated_at  = $5
              WHERE id = $6
                AND profile_id = $7
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = library.profile_id
                       AND p.user_id = $8
                )
          RETURNING id,
                    profile_id,
                    name,
                    description,
                    color_id,
                    icon_id,
                    created_at,
                    updated_at,
                    0::bigint AS track_count,
                    0::bigint AS album_count,
                    0::bigint AS artist_count,
                    0::bigint AS genre_count,
                    0::bigint AS folder_count",
        )
        .bind(patch.name.as_deref())
        .bind(patch.description.as_deref())
        .bind(patch.color_id.as_deref())
        .bind(patch.icon_id.as_deref())
        .bind(now_ms)
        .bind(id)
        .bind(profile_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(lib)
    }

    /// Tenant-scoped delete. Returns `Ok(true)` when a row was actually
    /// removed, `Ok(false)` when nothing matched (which blurs missing /
    /// foreign-profile / foreign-user, same no-leak rationale as
    /// `get_for_profile`). `ON DELETE CASCADE` on
    /// `library.profile_id` plus on the future track / folder tables
    /// cleans the dependents in one statement.
    pub async fn delete_for_profile(
        &self,
        id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<bool> {
        let deleted = sqlx::query(
            "DELETE FROM library
              WHERE id = $1
                AND profile_id = $2
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = library.profile_id
                       AND p.user_id = $3
                )",
        )
        .bind(id)
        .bind(profile_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(deleted.rows_affected() > 0)
    }
}
