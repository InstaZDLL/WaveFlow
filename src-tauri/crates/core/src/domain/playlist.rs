//! Playlist DTOs. Covers user-curated playlists and the smart-playlist
//! family (Daily Mix slots, On Repeat, custom rule trees) — the
//! distinction is carried by `is_smart` + the JSON blob in
//! `smart_rules`, which the frontend parses to dispatch on the
//! discriminant.

use serde::{Deserialize, Serialize};

/// Playlist row returned to the frontend, with the track count + total
/// duration denormalised so the sidebar can show "Playlist · N titres"
/// without issuing a follow-up query per playlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(any(feature = "sqlite", feature = "postgres"), derive(sqlx::FromRow))]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color_id: String,
    pub icon_id: String,
    pub is_smart: i64,
    /// Blake3 hash of the cover image stored in the shared
    /// `metadata_artwork` cache. `None` for user-curated playlists that
    /// haven't been given a custom cover — the frontend renders the
    /// `icon_id` + `color_id` gradient instead. Smart playlists populate
    /// this with a composite generated from the cluster's top artists.
    pub cover_hash: Option<String>,
    /// Absolute on-disk path resolved from `cover_hash` if the file is
    /// actually present, ready to be passed to `convertFileSrc`. The
    /// frontend prefers this over re-resolving the hash itself so a
    /// stale `cover_hash` (cache wiped) doesn't render a broken image.
    pub cover_path: Option<String>,
    /// `1` when the cover is managed by the auto-regen pipeline
    /// (Spotify-style 2×2 grid of the first 4 album artworks; refreshed
    /// after every mutation that could change the first-4 set). `0` when
    /// the user uploaded their own image — playlist mutations then leave
    /// the cover alone.
    pub cover_is_auto: i64,
    pub position: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub track_count: i64,
    pub total_duration_ms: i64,
    /// Raw JSON payload from `playlist.smart_rules`. `None` for user
    /// playlists (`is_smart = 0`); for smart playlists the frontend
    /// parses the `kind` discriminant to distinguish Daily Mix slots,
    /// On Repeat, and custom rule sets without an extra round-trip.
    pub smart_rules: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlaylistInput {
    pub name: String,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

/// Partial update payload — any field left as `None` is preserved via
/// SQL `COALESCE`. Same shape as [`crate::domain::library::UpdateLibraryInput`].
#[derive(Debug, Deserialize)]
pub struct UpdatePlaylistInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}
