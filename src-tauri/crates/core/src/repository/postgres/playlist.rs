//! Tenant-scoped Postgres playlist repository for `waveflow-server`.
//!
//! Same design as [`super::library::PostgresLibraryRepository`] — a
//! playlist belongs directly to a profile (not to a library), so the
//! ownership chain is the shorter `playlist → profile → user`, same
//! depth as library. This repository does NOT implement the
//! single-tenant [`crate::repository::playlist::PlaylistRepository`]
//! trait — that surface has no notion of tenancy and a careless
//! trait dispatch over this backend would let user A read user B's
//! playlists.
//!
//! Schema is the server's `playlist` migration (added in 1.b.5c on
//! the waveflow-server side). Smart playlists (`is_smart = 1`,
//! `smart_rules` JSON) aren't supported by this server-side repo
//! yet — every `insert_for_profile` writes a custom playlist
//! (`is_smart = 0`, `smart_rules = NULL`). The smart-playlist
//! engine still lives in [`crate::smart_playlists`] and consumes the
//! single-tenant SQLite repo on the desktop; porting it to the
//! server is a later phase.
//!
//! Denormalised counts (`track_count`, `total_duration_ms`) are
//! stubbed at `0` for now — the server's `playlist_track` join table
//! doesn't ship in 1.b.5c, it lands when tracks-in-playlist routes
//! follow in 1.c+. The wire shape stays identical so the web client
//! doesn't need to adapt when the joins materialise.
//!
//! `cover_path` is always `NULL` on the server — the desktop
//! resolves it from `cover_hash` against the per-profile artwork
//! dir, which the server doesn't own. Matches the desktop's own
//! SELECT pattern (`NULL AS cover_path`).

use sqlx::PgPool;

use crate::{
    domain::playlist::Playlist,
    error::CoreResult,
    repository::playlist::{PlaylistDraft, PlaylistUpdate},
};

#[derive(Debug, Clone)]
pub struct PostgresPlaylistRepository {
    pool: PgPool,
}

impl PostgresPlaylistRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Playlists owned by `profile_id`, ordered by `position ASC`
    /// then `updated_at DESC` — the same ordering the desktop's
    /// sidebar uses so the web client renders the list in the same
    /// shape. The `EXISTS` clause cross-validates that the *user*
    /// owns the profile, so user A passing user B's `profile_id`
    /// gets an empty list — never a tenancy bypass.
    pub async fn list_for_profile(
        &self,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Vec<Playlist>> {
        let playlists = sqlx::query_as::<_, Playlist>(
            "SELECT pl.id,
                    pl.profile_id,
                    pl.name,
                    pl.description,
                    pl.color_id,
                    pl.icon_id,
                    pl.is_smart,
                    pl.cover_hash,
                    NULL::text   AS cover_path,
                    pl.cover_is_auto,
                    pl.position,
                    pl.created_at,
                    pl.updated_at,
                    0::bigint    AS track_count,
                    0::bigint    AS total_duration_ms,
                    pl.smart_rules
               FROM playlist pl
              WHERE pl.profile_id = $1
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = pl.profile_id
                       AND p.user_id = $2
                )
              ORDER BY pl.position ASC, pl.updated_at DESC",
        )
        .bind(profile_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(playlists)
    }

    /// Fetch one playlist by id, scoped to both the profile and the
    /// user. `Ok(None)` blurs "no such id", "id belongs to another
    /// profile", and "profile belongs to another user" so the API
    /// never leaks the existence of foreign rows — same rationale as
    /// [`super::library::PostgresLibraryRepository::get_for_profile`].
    pub async fn get_for_profile(
        &self,
        id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<Playlist>> {
        let playlist = sqlx::query_as::<_, Playlist>(
            "SELECT pl.id,
                    pl.profile_id,
                    pl.name,
                    pl.description,
                    pl.color_id,
                    pl.icon_id,
                    pl.is_smart,
                    pl.cover_hash,
                    NULL::text   AS cover_path,
                    pl.cover_is_auto,
                    pl.position,
                    pl.created_at,
                    pl.updated_at,
                    0::bigint    AS track_count,
                    0::bigint    AS total_duration_ms,
                    pl.smart_rules
               FROM playlist pl
              WHERE pl.id = $1
                AND pl.profile_id = $2
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = pl.profile_id
                       AND p.user_id = $3
                )",
        )
        .bind(id)
        .bind(profile_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(playlist)
    }

    /// Insert a custom playlist owned by `profile_id`. Smart
    /// playlists aren't supported here yet — `is_smart` is hardcoded
    /// to `0`, `smart_rules` to `NULL`, `position` to `0`,
    /// `cover_hash` to `NULL` and `cover_is_auto` to `0` (no
    /// auto-cover pipeline on the server). The `INSERT … SELECT …
    /// WHERE EXISTS` pattern makes a foreign / non-existent profile
    /// fail in the same round-trip as the write — no race window
    /// between an existence check and the insert.
    ///
    /// Returns `Some(playlist)` on success, `None` when the profile
    /// isn't owned by `user_id`.
    pub async fn insert_for_profile(
        &self,
        draft: &PlaylistDraft,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<Playlist>> {
        let playlist = sqlx::query_as::<_, Playlist>(
            "INSERT INTO playlist (
                profile_id, name, description, color_id, icon_id,
                is_smart, cover_hash, cover_is_auto, position,
                created_at, updated_at, smart_rules
            )
            SELECT $1, $2, $3, $4, $5,
                   0, NULL, 0, 0,
                   $6, $6, NULL
              FROM profile p
             WHERE p.id = $1 AND p.user_id = $7
         RETURNING id,
                   profile_id,
                   name,
                   description,
                   color_id,
                   icon_id,
                   is_smart,
                   cover_hash,
                   NULL::text   AS cover_path,
                   cover_is_auto,
                   position,
                   created_at,
                   updated_at,
                   0::bigint    AS track_count,
                   0::bigint    AS total_duration_ms,
                   smart_rules",
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
        Ok(playlist)
    }

    /// Partial update via SQL `COALESCE` — `None` fields preserve
    /// the existing value. Returns the updated row in one round-trip
    /// (same `UPDATE … RETURNING …` pattern as the other tenant
    /// repos) so a concurrent DELETE can't flip a successful update
    /// into a misleading 404. `Ok(None)` when the playlist isn't
    /// owned by the (profile_id, user_id) pair.
    pub async fn update_for_profile(
        &self,
        id: i64,
        patch: &PlaylistUpdate,
        now_ms: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<Playlist>> {
        let playlist = sqlx::query_as::<_, Playlist>(
            "UPDATE playlist
                SET name        = COALESCE($1, name),
                    description = COALESCE($2, description),
                    color_id    = COALESCE($3, color_id),
                    icon_id     = COALESCE($4, icon_id),
                    updated_at  = $5
              WHERE id = $6
                AND profile_id = $7
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = playlist.profile_id
                       AND p.user_id = $8
                )
          RETURNING id,
                    profile_id,
                    name,
                    description,
                    color_id,
                    icon_id,
                    is_smart,
                    cover_hash,
                    NULL::text   AS cover_path,
                    cover_is_auto,
                    position,
                    created_at,
                    updated_at,
                    0::bigint    AS track_count,
                    0::bigint    AS total_duration_ms,
                    smart_rules",
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
        Ok(playlist)
    }

    /// Tenant-scoped delete. Returns `Ok(true)` when a row was
    /// removed, `Ok(false)` when nothing matched (same no-leak blur
    /// as `get_for_profile`). The future `playlist_track` table will
    /// carry `ON DELETE CASCADE` on `playlist_id` so the dependent
    /// links go away in one statement once that schema lands.
    pub async fn delete_for_profile(
        &self,
        id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<bool> {
        let deleted = sqlx::query(
            "DELETE FROM playlist
              WHERE id = $1
                AND profile_id = $2
                AND EXISTS (
                    SELECT 1 FROM profile p
                     WHERE p.id = playlist.profile_id
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
