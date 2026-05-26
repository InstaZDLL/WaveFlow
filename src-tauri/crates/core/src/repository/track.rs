//! `TrackRepository`: every read + simple write on the `track` table
//! and its sibling `liked_track`. The advanced search variant
//! (`search_tracks_advanced` on desktop) stays in `crates/app` for
//! now — its dynamic SQL is too client-shaped to land cleanly here
//! before the future server makes its own filter contract.

use async_trait::async_trait;

use crate::{domain::track::TrackRow, error::CoreResult};

/// "What to list" predicate for [`TrackRepository::list`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackListFilter {
    /// `Some` restricts to a single library; `None` returns tracks
    /// across **every** library — the "Ma musique" mode.
    pub library_id: Option<i64>,
}

/// Sort column for [`TrackRepository::list`]. Mapped to a whitelisted
/// `ORDER BY` clause inside the SQLite implementation — never accept
/// raw user input here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrackSortColumn {
    /// "Artist → Album → Disc → Track" — what the music apps
    /// converge on, also what we use when the caller doesn't pick.
    #[default]
    Default,
    Title,
    Artist,
    Album,
    DurationMs,
    Year,
    AddedAt,
    Rating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Asc,
    Desc,
}

/// Sort spec — `direction = None` falls back to the column's natural
/// default (DESC for `rating` / `duration_ms` / `added_at` / `year`,
/// ASC otherwise). The default sort itself ignores direction.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrackSort {
    pub column: TrackSortColumn,
    pub direction: Option<SortDirection>,
}

/// "List the tracks belonging to a folder / album / artist", used by
/// `add_source_to_playlist`. The discriminant decides the column the
/// repository filters on; deliberately kept narrow so the trait
/// surface doesn't grow a method per source.
#[derive(Debug, Clone, Copy)]
pub enum TrackSource {
    Folder(i64),
    Album(i64),
    Artist(i64),
}

#[async_trait]
pub trait TrackRepository: Send + Sync {
    /// Single track by id, with the shared joined projection. `None`
    /// when the row was deleted between the original lookup and this
    /// call (race-tolerant).
    async fn get(&self, id: i64) -> CoreResult<Option<TrackRow>>;

    /// Bulk list with optional library scope + sort. Used by both the
    /// "Tracks" tab and the "Ma musique" all-libraries view.
    async fn list(
        &self,
        filter: TrackListFilter,
        sort: TrackSort,
    ) -> CoreResult<Vec<TrackRow>>;

    /// Tracks of a playlist in the user's stored order
    /// (`pt.position ASC`). Skips unavailable rows so the UI never
    /// shows a track whose file vanished.
    async fn list_in_playlist(&self, playlist_id: i64) -> CoreResult<Vec<TrackRow>>;

    /// Every liked track, most-recently-liked first.
    async fn list_liked(&self) -> CoreResult<Vec<TrackRow>>;

    /// FTS5 search. `fts_query` is expected to already carry the
    /// `*` prefix-matching shape the caller wants — the repository
    /// does *not* tokenise / escape it because what counts as a
    /// "word" is a frontend UX decision.
    async fn search_fts(&self, fts_query: &str, limit: i64) -> CoreResult<Vec<TrackRow>>;

    /// Track ids belonging to a folder / album / artist. The variant
    /// picks the column; ordering follows the natural "Disc → Track →
    /// Title" for folders/albums and pure title for artist.
    async fn list_ids_in_source(&self, source: TrackSource) -> CoreResult<Vec<i64>>;

    // ── liked_track ────────────────────────────────────────────────

    /// Just the ids, most-recently-liked first. Backs the "render
    /// hearts without N+1" pattern in the frontend.
    async fn liked_ids(&self) -> CoreResult<Vec<i64>>;

    async fn is_liked(&self, track_id: i64) -> CoreResult<bool>;

    async fn like(&self, track_id: i64, now_ms: i64) -> CoreResult<()>;

    async fn unlike(&self, track_id: i64) -> CoreResult<()>;

    // ── misc ──────────────────────────────────────────────────────

    /// Resolve a track id to its on-disk absolute path. Used by
    /// `set_rating` (file write) and the audio engine.
    async fn get_file_path(&self, track_id: i64) -> CoreResult<Option<String>>;

    /// Update the raw POPM byte (0-255) in the database. The file-side
    /// tag write happens in the desktop crate alongside the
    /// pause-if-playing handshake.
    async fn set_rating(&self, track_id: i64, rating: Option<u8>) -> CoreResult<()>;
}
