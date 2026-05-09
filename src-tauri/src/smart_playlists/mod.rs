//! Auto-generated playlist engine ("Daily Mix", future "On Repeat", etc.).
//!
//! Reads listening history from `play_event`, groups the user's most-listened
//! artists by tempo, materializes each group as an `is_smart = 1` playlist,
//! and renders a composite cover from the top artists' Deezer pictures.
//!
//! Smart playlists live in the same `playlist` table as user-curated ones;
//! the `is_smart` flag and a JSON `smart_rules` blob (see
//! [`SmartPlaylistRules`]) tell the regenerator which rows to overwrite on
//! the next pass.

pub mod cover;
pub mod generator;

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
}

impl SmartPlaylistRules {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("SmartPlaylistRules serialize")
    }
}
