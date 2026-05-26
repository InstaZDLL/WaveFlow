//! `ProfileRepository`: the storage surface for the `profile` table in
//! the application-wide database (`app.db` on desktop, a dedicated
//! schema on the server).

use async_trait::async_trait;

use crate::{domain::profile::Profile, error::CoreResult};

/// Outcome of a guarded delete. Reported separately so the caller can
/// emit a user-friendly error without an extra round-trip to
/// distinguish "row not found" from "was the last remaining profile".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileDeleteOutcome {
    Deleted,
    NotFound,
    /// The row existed but was the last remaining profile — the
    /// `app` table invariant requires at least one row at all times.
    WasLast,
}

/// Payload for [`ProfileRepository::insert`]. Mirrors the columns the
/// repository writes; `data_dir` is set afterwards by
/// [`ProfileRepository::set_data_dir`] because it depends on the
/// freshly assigned rowid.
#[derive(Debug, Clone)]
pub struct ProfileDraft {
    pub name: String,
    pub color_id: String,
    pub avatar_hash: Option<String>,
    /// Epoch milliseconds. Used for both `created_at` and the initial
    /// `last_used_at` so a brand-new profile sorts to the top of the
    /// most-recently-used list.
    pub now_ms: i64,
}

#[async_trait]
pub trait ProfileRepository: Send + Sync {
    /// Every profile registered in the table, most-recently-used first.
    async fn list_all(&self) -> CoreResult<Vec<Profile>>;

    /// Look up a profile by its rowid.
    async fn get(&self, id: i64) -> CoreResult<Option<Profile>>;

    /// Insert a new profile row with an empty `data_dir`. Returns the
    /// freshly assigned rowid so the caller can derive the directory
    /// layout from it and then call [`Self::set_data_dir`].
    async fn insert(&self, draft: &ProfileDraft) -> CoreResult<i64>;

    /// Record the resolved `data_dir` for a newly inserted profile.
    /// The two-step insert + set lets the caller derive the directory
    /// path from the freshly assigned rowid without reserving the
    /// id ahead of time.
    async fn set_data_dir(&self, id: i64, data_dir: &str) -> CoreResult<()>;

    /// Rename a profile in place. Returns `true` when a row was
    /// updated, `false` when no row matched `id`.
    async fn rename(&self, id: i64, new_name: &str) -> CoreResult<bool>;

    /// Stamp `last_used_at` so the profile sorts to the top of the
    /// most-recently-used list on the next [`Self::list_all`].
    async fn touch_last_used(&self, id: i64, now_ms: i64) -> CoreResult<()>;

    /// Delete a profile row, refusing to leave the table empty.
    /// The check + delete run inside a single SQL statement so a
    /// concurrent delete cannot drop the last remaining row
    /// (TOCTOU-free).
    async fn delete_guarded(&self, id: i64) -> CoreResult<ProfileDeleteOutcome>;

    /// `SELECT 1 FROM profile WHERE id = ?` — used after a guarded
    /// delete to disambiguate `NotFound` from `WasLast`.
    async fn exists(&self, id: i64) -> CoreResult<bool>;
}
