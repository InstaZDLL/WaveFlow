//! Auto-generated playlist engine — "Daily Mix" (tempo-bucketed mixes of
//! favourite artists) and "On Repeat" (top tracks of the last 30 days).
//!
//! Reads listening history from `play_event`, runs family-specific picking
//! logic, materializes the result as one or more `is_smart = 1` playlist
//! rows, and renders a composite cover from the top contributors' album art
//! or Deezer pictures.
//!
//! Smart playlists live in the same `playlist` table as user-curated ones;
//! the `is_smart` flag and a JSON `smart_rules` blob (see
//! [`SmartPlaylistRules`]) tell the regenerator which rows to overwrite on
//! the next pass.

pub mod cover;

// `custom` holds the rule-tree types (`CustomRules`, `RuleNode`, …)
// that `SmartPlaylistRules::Custom` references — keep it always
// compiled so the enum survives a postgres-only build. The sqlite-
// specific `materialize` function inside is feature-gated at the
// function level.
pub mod custom;

// `generator` and `on_repeat` are wholesale SQLite materialisers —
// they build queries against a `sqlx::SqlitePool` and write back the
// resulting smart-playlist rows. Skipped on the postgres-only build
// (used by `waveflow-server`) until a parallel Postgres regenerator
// lands later in Phase 1.
#[cfg(feature = "sqlite")]
pub mod generator;
#[cfg(feature = "sqlite")]
pub mod on_repeat;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Filesystem context the smart-playlist generators need. Owned
/// `PathBuf`s rather than borrows so the struct can be `Clone`d
/// across async boundaries without lifetime gymnastics — the
/// generators are user-triggered (1 call every few minutes at peak)
/// so the allocation is irrelevant.
///
/// Constructed app-side from `AppPaths`; mirrored by a future
/// server-side path resolver in `waveflow-server`.
#[derive(Debug, Clone)]
pub struct PathsContext {
    /// Per-installation shared cache for downloaded artwork
    /// (Deezer pictures, on-disk JPEGs). Mirrors
    /// `AppPaths::metadata_artwork_dir`.
    pub metadata_artwork_dir: PathBuf,
    /// Absolute path to `app.db`. Opened with a short-lived single-
    /// connection pool by the daily-mix generator to look up Deezer
    /// picture hashes without routing through the per-profile pool.
    pub app_db_path: PathBuf,
    /// Per-profile artwork directory
    /// (`<profile_root>/<profile_id>/artwork`). Mirrors the result of
    /// `AppPaths::profile_artwork_dir(profile_id)`.
    pub profile_artwork_dir: PathBuf,
}

/// JSON payload stored in `playlist.smart_rules` so a future regen pass can
/// recognise the row and replace it deterministically. The `kind` discriminant
/// keeps us forward-compatible with other smart-playlist families.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SmartPlaylistRules {
    /// Tempo-bucketed mix of the user's favourite artists. `slot` is 1-based
    /// so the playlist names ("Daily Mix 1") can be reconstructed without a
    /// separate column.
    DailyMix { slot: u8 },
    /// Top ~30 tracks by play count over the last 30 days. Single-slot
    /// family — only one playlist per profile — so the discriminant
    /// carries no payload.
    OnRepeat,
    /// User-defined rule set evaluated by [`custom::materialize`]. Stored
    /// in JSON so the rule editor can round-trip it without a per-field
    /// SQL column.
    Custom { rules: custom::CustomRules },
}

impl SmartPlaylistRules {
    /// Serialize the rule payload for storage in `playlist.smart_rules`.
    /// All variants derive `Serialize` over plain owned data, so the
    /// underlying `serde_json::to_string` call is total in practice —
    /// but the signature is `Result` (not the older `String` with a
    /// fallback) so a hostile future `Custom` payload (e.g. a non-string
    /// map key smuggled in via a schema change) can't sneak a malformed
    /// `smart_rules` blob into the playlist row. Callers must propagate
    /// the error rather than persist a placeholder.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
