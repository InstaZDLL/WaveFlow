//! `LibraryRepository`: the storage surface for a profile's `library` and
//! `library_folder` tables.

use async_trait::async_trait;

use crate::{
    domain::library::{Library, LibraryFolder},
    error::CoreResult,
};

/// Payload for [`LibraryRepository::insert`]. Mirrors the columns the
/// repository writes; counts are zero on a freshly created library.
#[derive(Debug, Clone)]
pub struct LibraryDraft {
    pub name: String,
    pub description: Option<String>,
    pub color_id: String,
    pub icon_id: String,
    /// Epoch milliseconds. Used for both `created_at` and the initial
    /// `updated_at`.
    pub now_ms: i64,
}

/// Partial update payload for [`LibraryRepository::update`]. Every field
/// is optional; the repository writes `COALESCE(?, column)` for each so
/// `None` preserves the existing value. The caller is responsible for
/// any trimming / validation before calling.
#[derive(Debug, Clone, Default)]
pub struct LibraryUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

#[async_trait]
pub trait LibraryRepository: Send + Sync {
    /// Every library in the active profile, most-recently-updated first,
    /// with denormalised `track_count` / `album_count` / `artist_count`
    /// / `genre_count` / `folder_count` so the sidebar can render
    /// "X titres · Y albums" without an extra round-trip per library.
    async fn list_all_with_counts(&self) -> CoreResult<Vec<Library>>;

    /// `SELECT 1 FROM library WHERE id = ?` — handy guard for commands
    /// that want a precise "library not found" error instead of a
    /// foreign-key constraint failure deeper in the call.
    async fn exists(&self, id: i64) -> CoreResult<bool>;

    /// Insert a new library row. Returns the freshly assigned rowid.
    async fn insert(&self, draft: &LibraryDraft) -> CoreResult<i64>;

    /// Partial update. Returns `Ok(())` regardless of whether a row
    /// matched; callers that need to enforce existence should pair this
    /// with [`Self::exists`].
    async fn update(&self, id: i64, patch: &LibraryUpdate, now_ms: i64) -> CoreResult<()>;

    /// Delete a library and let `ON DELETE CASCADE` clean up tracks /
    /// folders / playlist links / scrobble queue entries / etc. Returns
    /// `true` when a row was actually removed.
    async fn delete(&self, id: i64) -> CoreResult<bool>;

    /// Bump `updated_at` without changing any other column. Triggered
    /// after scans / imports so any UI listener keyed on this field
    /// re-renders.
    async fn touch_updated_at(&self, id: i64, now_ms: i64) -> CoreResult<()>;

    /// Every folder for the library, ordered by id so a re-render is
    /// stable.
    async fn list_folders(&self, library_id: i64) -> CoreResult<Vec<LibraryFolder>>;

    /// Just the folder rowids for the library — the scanner needs only
    /// the ids, fetching the full rows would be wasteful for a rescan
    /// loop.
    async fn list_folder_ids(&self, library_id: i64) -> CoreResult<Vec<i64>>;

    /// Insert a new `library_folder` row. The `(library_id, path)`
    /// unique constraint is left to surface as `CoreError::Database` if
    /// the path already exists.
    async fn insert_folder(&self, library_id: i64, path: &str) -> CoreResult<i64>;

    /// `INSERT OR IGNORE` followed by a `SELECT id` fallback when the
    /// insert was a no-op. Used by drag-and-drop import where the same
    /// folder may legitimately be re-added.
    async fn insert_or_get_folder(&self, library_id: i64, path: &str) -> CoreResult<i64>;

    /// Delete a folder and every track that lived under it, in a single
    /// transaction so neither side can be partially observed. The
    /// caller is responsible for detaching the in-memory watcher before
    /// calling.
    async fn delete_folder_with_tracks(&self, folder_id: i64) -> CoreResult<()>;
}
