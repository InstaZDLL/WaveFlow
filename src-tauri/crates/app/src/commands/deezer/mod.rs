//! Tauri commands for Deezer metadata enrichment.
//!
//! Each command follows a **cache-first** strategy:
//! 1. Check if the local entity already has a `deezer_id`.
//! 2. If yes, check the `deezer_*` cache table for a non-expired entry.
//! 3. If the cache is valid, return it immediately (and resolve the local
//!    artwork path from the stored hash so the UI can render offline).
//! 4. Otherwise, search or fetch from the Deezer public API, download the
//!    artwork into the shared `metadata_artwork/` directory, upsert the cache
//!    row (with the hash) and link the `deezer_id` on the local entity.
//!
//! On any network error the command returns an **empty enrichment** (all
//! fields `None`) rather than propagating an error — the frontend can
//! display local data without interruption.

use chrono::Utc;

mod album;
mod artist;
mod artwork;
mod backdrop;
mod batch;
mod download;

pub use album::*;
pub use artist::*;
pub use artwork::*;
pub use backdrop::*;
pub use batch::*;
pub(crate) use download::*;

/// TTL for cached Deezer entries: 30 days in milliseconds.
const CACHE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// How long a row stands when the biography could not be asked for: an
/// hour, so a refused key or a spent quota is retried soon without
/// re-running the whole enrichment on every visit and every batch pass.
const RETRY_AFTER_MS: i64 = 60 * 60 * 1000;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests;
