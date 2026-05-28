//! Track-related DTOs shared by every WaveFlow client surface.
//!
//! These types describe what the UI sees and what the SQLite
//! repository loads (`TrackRow`). The frontend-facing `Track` carries
//! resolved on-disk artwork paths derived in the app layer; the
//! `TrackListItem` slim shape strips them to keep bulk responses
//! compact. `TrackRow` is the raw `query_as` target shared by every
//! repository method that fans out the same joined projection (track
//! + album + primary artist + artist names + artwork pointer).

use serde::{Deserialize, Serialize};

/// Slim row shipped by the bulk list endpoints (`list_tracks`,
/// `list_playlist_tracks`, `list_liked_tracks`) where 800–1000+
/// rows can land in a single response. Artwork is identified by
/// `(hash, format)` plus thumbnail-existence flags; the absolute
/// path strings are stitched on the frontend from the response-level
/// `artwork_base` so the per-profile prefix isn't repeated thousands
/// of times in the JSON payload (~30 % size reduction).
#[derive(Debug, Clone, Serialize)]
pub struct TrackListItem {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    pub codec: Option<String>,
    pub musical_key: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub added_at: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub artwork_has_1x: bool,
    pub artwork_has_2x: bool,
    pub rating: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListTracksResponse {
    /// Per-profile artwork directory — sent once instead of repeated
    /// as a ~70-char prefix on every row.
    pub artwork_base: String,
    pub items: Vec<TrackListItem>,
}

/// Track row returned to the frontend, already joined with album + primary
/// artist so the UI never has to issue a follow-up query per row. Ordering
/// follows the "Artist → Album → Disc → Track number" convention used by
/// most native music players.
///
/// `artwork_path` is resolved in Rust (not SQL) because the artwork file
/// lives under the per-profile data dir, which the database itself doesn't
/// know about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    /// Comma-joined artist IDs in the same order as `artist_name`'s
    /// `", "`-joined names. Used by the frontend `ArtistLink` to make
    /// each name individually clickable.
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    /// Bits per sample. `None` for lossy codecs that don't expose
    /// it; populated for FLAC/WAV/AIFF and similar lossless masters.
    pub bit_depth: Option<i64>,
    /// Short codec / container label (`"FLAC"`, `"MP3"`, …). Drives
    /// the format chip on the player footer.
    pub codec: Option<String>,
    /// Tagged musical key (`Am`, `F#`, `8A`, …) read at scan time
    /// from `TKEY` (ID3v2) or `INITIALKEY` (Vorbis/MP4/APE). `None`
    /// when the file has no key tag.
    pub musical_key: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub added_at: i64,
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
    /// Raw POPM byte (0-255). `None` when no rating was extracted from
    /// the file's tags or set by the user. The frontend converts this
    /// to a 0-5 star scale with half-step increments.
    pub rating: Option<i64>,
}

/// Raw row shape as it comes out of the shared joined `SELECT`
/// (track + album + primary artist + GROUP_CONCAT'd artists + artwork
/// pointer). Every bulk track endpoint goes through this struct to
/// keep the projection in lockstep; the per-endpoint conversion
/// (resolving thumbnail paths, etc.) happens in `crates/app`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "sqlite", derive(sqlx::FromRow))]
pub struct TrackRow {
    pub id: i64,
    pub library_id: i64,
    pub title: String,
    pub album_id: Option<i64>,
    pub album_title: Option<String>,
    pub artist_id: Option<i64>,
    pub artist_name: Option<String>,
    pub artist_ids: Option<String>,
    pub duration_ms: i64,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub year: Option<i64>,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
    pub bit_depth: Option<i64>,
    pub codec: Option<String>,
    pub musical_key: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub added_at: i64,
    pub artwork_hash: Option<String>,
    pub artwork_format: Option<String>,
    pub rating: Option<i64>,
}
