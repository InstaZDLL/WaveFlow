//! `PlaylistRepository`: the storage surface for the `playlist` and
//! `playlist_track` tables of a profile's `data.db`. Smart-playlist
//! generation logic stays out of this trait — the repository only
//! moves rows around; rule evaluation lives in the smart-playlist
//! engine at [`crate::smart_playlists`] (path:
//! `src-tauri/crates/core/src/smart_playlists`), which consumes a
//! `PathsContext` to stay portable on the future `waveflow-server`.

use async_trait::async_trait;

use crate::{domain::playlist::Playlist, error::CoreResult};

/// Payload for [`PlaylistRepository::insert_custom`]. `position` and
/// `is_smart` are not exposed — the repository writes `0` for both;
/// dedicated methods would model smart-playlist insertion separately
/// in a later refactor.
#[derive(Debug, Clone)]
pub struct PlaylistDraft {
    pub name: String,
    pub description: Option<String>,
    pub color_id: String,
    pub icon_id: String,
    pub now_ms: i64,
}

/// Partial update payload. Every field is optional; the repository
/// writes `COALESCE(?, column)` so `None` preserves the existing
/// value.
#[derive(Debug, Clone, Default)]
pub struct PlaylistUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

#[async_trait]
pub trait PlaylistRepository: Send + Sync {
    /// Every playlist in the active profile, ordered by `position` ASC
    /// then `updated_at` DESC. Denormalised `track_count` +
    /// `total_duration_ms` come from a single LEFT JOIN so the sidebar
    /// can render the secondary line without an extra round-trip per
    /// row.
    async fn list_all_with_counts(&self) -> CoreResult<Vec<Playlist>>;

    /// Single playlist with the same denormalised counts as
    /// [`Self::list_all_with_counts`]. The cover-path resolution stays
    /// in the caller because it depends on the desktop's per-profile
    /// artwork dir.
    async fn get_with_counts(&self, id: i64) -> CoreResult<Option<Playlist>>;

    /// Just `name` — handy for the M3U exporter which needs the
    /// playlist name + a guarded "not found" error without the full
    /// row.
    async fn get_name(&self, id: i64) -> CoreResult<Option<String>>;

    async fn exists(&self, id: i64) -> CoreResult<bool>;

    /// Insert a brand-new user-curated playlist (`is_smart = 0`,
    /// `position = 0`). Returns the freshly assigned rowid.
    async fn insert_custom(&self, draft: &PlaylistDraft) -> CoreResult<i64>;

    /// Partial update. Bumps `updated_at`.
    async fn update(&self, id: i64, patch: &PlaylistUpdate, now_ms: i64) -> CoreResult<()>;

    /// Delete a playlist and let `ON DELETE CASCADE` walk the
    /// `playlist_track` links. The underlying `track` rows survive.
    async fn delete(&self, id: i64) -> CoreResult<bool>;

    async fn touch_updated_at(&self, id: i64, now_ms: i64) -> CoreResult<()>;

    /// User-curated playlists (smart ones excluded) that currently
    /// contain `track_id`. Drives the `+` popover's "already in"
    /// checkmarks.
    async fn list_user_playlists_containing(&self, track_id: i64) -> CoreResult<Vec<i64>>;

    /// Append a single track at the end. Idempotent — duplicates are
    /// silently skipped via `INSERT OR IGNORE`. Bumps `updated_at` in
    /// the same call so the caller doesn't need a follow-up.
    async fn append_track(&self, playlist_id: i64, track_id: i64, now_ms: i64) -> CoreResult<()>;

    /// Bulk append, in order, under one transaction. Returns the count
    /// of rows that were actually inserted (the rest were duplicates).
    /// Bumps `updated_at` once at the end.
    async fn append_tracks(
        &self,
        playlist_id: i64,
        track_ids: &[i64],
        now_ms: i64,
    ) -> CoreResult<u32>;

    /// Remove a single track and renumber the tail in a single tx so
    /// positions stay contiguous. `Ok(false)` when the track is not in
    /// the playlist (no error — the UI may legitimately re-issue the
    /// command after an optimistic delete).
    async fn remove_track(&self, playlist_id: i64, track_id: i64, now_ms: i64) -> CoreResult<bool>;

    /// Move a track to a new absolute position, shifting the
    /// surrounding rows so positions stay dense. `new_position` is
    /// clamped to `[0, len - 1]` internally. Returns `Ok(false)` when
    /// the track isn't in the playlist; `Ok(true)` on success
    /// (including a no-op `from == to` reorder).
    async fn reorder_track(
        &self,
        playlist_id: i64,
        track_id: i64,
        new_position: i64,
        now_ms: i64,
    ) -> CoreResult<bool>;

    /// Atomic "create custom playlist + append tracks" — used by the
    /// M3U importer so a partial failure can't leave an empty
    /// playlist behind. Returns `(new_playlist_id, inserted_count)`.
    async fn create_with_tracks(
        &self,
        draft: &PlaylistDraft,
        track_ids: &[i64],
    ) -> CoreResult<(i64, u32)>;
}
