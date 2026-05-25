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
pub mod custom;
pub mod generator;
pub mod on_repeat;

use serde::{Deserialize, Serialize};

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
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("SmartPlaylistRules serialize")
    }
}
