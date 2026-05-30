//! Tenant-scoped Postgres track repository for `waveflow-server`.
//!
//! Same design as [`super::library::PostgresLibraryRepository`], one
//! level deeper: every method takes a `library_id`, a `profile_id`
//! AND a `user_id`, and the SQL validates the full
//! `track → library → profile → user` ownership chain in a single
//! statement. The repository does NOT implement the single-tenant
//! [`crate::repository::track::TrackRepository`] trait — that surface
//! has no notion of tenancy and a careless trait dispatch over this
//! backend would let user A read user B's tracks.
//!
//! Schema is the server's `track` migration (added in 1.b.5b on the
//! waveflow-server side): a thin row carrying the columns that
//! actually exist on disk for every file. The album / artist /
//! artwork joins from the desktop's `TrackRow` projection still
//! materialise in the response shape — they're projected as `NULL`
//! casts of the right Postgres type until the album / artist /
//! artwork tables ship on the server. Same pattern as
//! `library.track_count = 0::bigint`: keep the wire field so the
//! client doesn't need to adapt when the joins become real.
//!
//! Insert + update follow the race-free patterns established by
//! `PostgresProfileRepository::rename_for_user` and
//! `PostgresLibraryRepository::insert_for_profile`:
//! - `INSERT … SELECT FROM library … WHERE … AND p.user_id = $`
//!   makes a foreign library / profile fail atomically in the same
//!   round-trip as the write (no check-then-insert window).
//! - `UPDATE … RETURNING …` hands back the post-update row directly
//!   so a concurrent DELETE can't flip a successful update into a
//!   misleading 404.

use sqlx::PgPool;

use crate::{
    domain::track::TrackRow,
    error::CoreResult,
    repository::track::{TrackDraft, TrackUpdate},
};

#[derive(Debug, Clone)]
pub struct PostgresTrackRepository {
    pool: PgPool,
}

/// Shared `SELECT … FROM track t` projection, NULL-cast for every
/// join the server doesn't ship yet (album / artist / artwork). The
/// casts are mandatory — without them `sqlx::query_as::<_, TrackRow>`
/// can't decide which Rust type to map the literal `NULL` to.
const SELECT_TRACK_ROW: &str = "SELECT t.id,
        t.library_id,
        t.title,
        NULL::bigint  AS album_id,
        NULL::text    AS album_title,
        NULL::bigint  AS artist_id,
        NULL::text    AS artist_name,
        NULL::text    AS artist_ids,
        t.duration_ms,
        t.track_number,
        t.disc_number,
        t.year,
        t.bitrate,
        t.sample_rate,
        t.channels,
        t.bit_depth,
        t.codec,
        t.musical_key,
        t.file_path,
        t.file_size,
        t.added_at,
        NULL::text    AS artwork_hash,
        NULL::text    AS artwork_format,
        t.rating
   FROM track t";

impl PostgresTrackRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Tracks owned by `library_id`, most-recently-added first. The
    /// nested `EXISTS` validates that the *profile* under the library
    /// is owned by the *user*, so user A passing user B's `library_id`
    /// gets an empty list — never a tenancy bypass.
    pub async fn list_for_library(
        &self,
        library_id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Vec<TrackRow>> {
        let sql = format!(
            "{SELECT_TRACK_ROW}\n              WHERE t.library_id = $1\n                AND EXISTS (\n                    SELECT 1 FROM library l\n                      JOIN profile p ON p.id = l.profile_id\n                     WHERE l.id = t.library_id\n                       AND l.profile_id = $2\n                       AND p.user_id = $3\n                )\n              ORDER BY t.added_at DESC"
        );
        let rows = sqlx::query_as::<_, TrackRow>(&sql)
            .bind(library_id)
            .bind(profile_id)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    /// Fetch one track by id, scoped to the entire ownership chain.
    /// `Ok(None)` blurs "no such id", "id belongs to another library",
    /// "library belongs to another profile", and "profile belongs to
    /// another user" — the API never tells a non-owner that a row
    /// exists somewhere else on the box.
    pub async fn get_for_library(
        &self,
        id: i64,
        library_id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<TrackRow>> {
        let sql = format!(
            "{SELECT_TRACK_ROW}\n              WHERE t.id = $1\n                AND t.library_id = $2\n                AND EXISTS (\n                    SELECT 1 FROM library l\n                      JOIN profile p ON p.id = l.profile_id\n                     WHERE l.id = t.library_id\n                       AND l.profile_id = $3\n                       AND p.user_id = $4\n                )"
        );
        let row = sqlx::query_as::<_, TrackRow>(&sql)
            .bind(id)
            .bind(library_id)
            .bind(profile_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    /// Insert a track under `library_id`. The `INSERT … SELECT …
    /// WHERE EXISTS` pattern lets a non-owned library fail in the
    /// same round-trip as the write — no race window. Returns
    /// `Some(track)` on success, `None` when the (library, profile,
    /// user) chain doesn't hold.
    ///
    /// The desktop scanner is the production path for inserting
    /// tracks today; this method exists so the server can hand-craft
    /// rows for tests + future web import flows without going
    /// through the full scan pipeline.
    pub async fn insert_for_library(
        &self,
        draft: &TrackDraft,
        library_id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<TrackRow>> {
        let sql = "INSERT INTO track (
                library_id, title, file_path, file_size,
                duration_ms, track_number, disc_number, year,
                bitrate, sample_rate, channels, bit_depth,
                codec, musical_key, added_at
            )
            SELECT $1, $2, $3, $4,
                   $5, $6, $7, $8,
                   $9, $10, $11, $12,
                   $13, $14, $15
              FROM library l
              JOIN profile p ON p.id = l.profile_id
             WHERE l.id = $1
               AND l.profile_id = $16
               AND p.user_id = $17
         RETURNING id,
                   library_id,
                   title,
                   NULL::bigint  AS album_id,
                   NULL::text    AS album_title,
                   NULL::bigint  AS artist_id,
                   NULL::text    AS artist_name,
                   NULL::text    AS artist_ids,
                   duration_ms,
                   track_number,
                   disc_number,
                   year,
                   bitrate,
                   sample_rate,
                   channels,
                   bit_depth,
                   codec,
                   musical_key,
                   file_path,
                   file_size,
                   added_at,
                   NULL::text    AS artwork_hash,
                   NULL::text    AS artwork_format,
                   rating";
        let row = sqlx::query_as::<_, TrackRow>(sql)
            .bind(library_id)
            .bind(&draft.title)
            .bind(&draft.file_path)
            .bind(draft.file_size)
            .bind(draft.duration_ms)
            .bind(draft.track_number)
            .bind(draft.disc_number)
            .bind(draft.year)
            .bind(draft.bitrate)
            .bind(draft.sample_rate)
            .bind(draft.channels)
            .bind(draft.bit_depth)
            .bind(draft.codec.as_deref())
            .bind(draft.musical_key.as_deref())
            .bind(draft.now_ms)
            .bind(profile_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    /// Partial update via SQL `COALESCE` — `None` fields preserve the
    /// existing value. Uses the same `UPDATE … RETURNING …` shape as
    /// [`super::library::PostgresLibraryRepository::update_for_profile`]
    /// so a concurrent DELETE can't flip a successful update into a
    /// misleading 404. `Ok(None)` when the track isn't owned by the
    /// `(library, profile, user)` chain.
    pub async fn update_for_library(
        &self,
        id: i64,
        patch: &TrackUpdate,
        library_id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<Option<TrackRow>> {
        let sql = "UPDATE track
                SET title        = COALESCE($1, title),
                    track_number = COALESCE($2, track_number),
                    disc_number  = COALESCE($3, disc_number),
                    year         = COALESCE($4, year),
                    rating       = COALESCE($5, rating)
              WHERE id = $6
                AND library_id = $7
                AND EXISTS (
                    SELECT 1 FROM library l
                      JOIN profile p ON p.id = l.profile_id
                     WHERE l.id = track.library_id
                       AND l.profile_id = $8
                       AND p.user_id = $9
                )
          RETURNING id,
                    library_id,
                    title,
                    NULL::bigint  AS album_id,
                    NULL::text    AS album_title,
                    NULL::bigint  AS artist_id,
                    NULL::text    AS artist_name,
                    NULL::text    AS artist_ids,
                    duration_ms,
                    track_number,
                    disc_number,
                    year,
                    bitrate,
                    sample_rate,
                    channels,
                    bit_depth,
                    codec,
                    musical_key,
                    file_path,
                    file_size,
                    added_at,
                    NULL::text    AS artwork_hash,
                    NULL::text    AS artwork_format,
                    rating";
        let row = sqlx::query_as::<_, TrackRow>(sql)
            .bind(patch.title.as_deref())
            .bind(patch.track_number)
            .bind(patch.disc_number)
            .bind(patch.year)
            // `u8` isn't natively bindable on Postgres (no unsigned
            // integer types), so widen to `i64` for the BIGINT column.
            // The `u8` typing at the struct level already guarantees
            // `0..=255` — the cast is a no-op semantically.
            .bind(patch.rating.map(i64::from))
            .bind(id)
            .bind(library_id)
            .bind(profile_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }

    /// Tenant-scoped delete. Returns `Ok(true)` when a row was
    /// actually removed, `Ok(false)` when nothing matched (no-leak
    /// blur of missing / foreign-library / foreign-profile /
    /// foreign-user, same rationale as `get_for_library`). Future
    /// `ON DELETE CASCADE` on `track_artist` / `track_genre` /
    /// `play_event` cleans dependents in one statement once those
    /// tables ship.
    pub async fn delete_for_library(
        &self,
        id: i64,
        library_id: i64,
        profile_id: i64,
        user_id: i64,
    ) -> CoreResult<bool> {
        let deleted = sqlx::query(
            "DELETE FROM track
              WHERE id = $1
                AND library_id = $2
                AND EXISTS (
                    SELECT 1 FROM library l
                      JOIN profile p ON p.id = l.profile_id
                     WHERE l.id = track.library_id
                       AND l.profile_id = $3
                       AND p.user_id = $4
                )",
        )
        .bind(id)
        .bind(library_id)
        .bind(profile_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(deleted.rows_affected() > 0)
    }
}
