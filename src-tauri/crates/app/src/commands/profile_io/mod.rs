//! Profile export / import.
//!
//! Bundles a profile's per-user state into a single `.waveflow` (zip)
//! file the user can shuttle between machines or stash as a backup.
//!
//! What ends up in the archive:
//!   - `manifest.json` — schema version, profile name, export timestamp.
//!     A future version bump lets us refuse incompatible imports rather
//!     than silently breaking on a missing column.
//!   - `data.db` — the per-profile SQLite database (playlists, liked,
//!     stats, EQ, sleep-timer / A-B visibility, shortcut overrides…).
//!   - `artwork/**` — manual covers the user uploaded.
//!   - `metadata_artwork/**` — *optional*, gated by the
//!     `backup.include_metadata_artwork` app setting (default ON). This
//!     is the shared Deezer cover/artist-picture cache; bundling it
//!     means a restore on an offline machine still shows the artwork
//!     immediately. Users who want lean archives can disable the toggle
//!     in Settings → Sauvegardes and re-fetch on demand via "Récupérer
//!     toutes les pochettes manquantes".
//!
//! What we deliberately **don't** bundle:
//!   - The shared `app.db` (Last.fm key, Discord opt-in, app-wide
//!     settings) — those belong to the install, not the profile.
//!   - WAL / SHM sidecars — we run a `WAL_CHECKPOINT(TRUNCATE)` before
//!     copying so the bundled `data.db` is self-contained.

use serde::{Deserialize, Serialize};

mod archive;
mod export;
mod import;
mod migrations;
mod preview;

pub(crate) use archive::*;
pub use export::*;
pub use import::*;
pub(crate) use migrations::*;
pub use preview::*;

/// Bumped when the on-disk shape of a `.waveflow` archive changes in
/// an incompatible way (renamed manifest fields, removed `artwork/`
/// dir, etc.). Schema-level differences inside `data.db` are caught
/// by sqlx migration replay at first switch — see `import_profile`.
pub(crate) const ARCHIVE_VERSION: u32 = 1;

const MANIFEST_FILENAME: &str = "manifest.json";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ArchiveManifest {
    pub archive_version: u32,
    /// `CARGO_PKG_VERSION` of the WaveFlow build that produced the
    /// archive. Surfaced for diagnostics; not used for compatibility
    /// gating (versions diverge faster than the archive shape).
    pub app_version: String,
    pub profile_name: String,
    /// Source profile id at export time. Purely informational — the
    /// new profile gets a fresh id at import.
    pub source_profile_id: i64,
    pub exported_at: String,
}

/// What a successful import hands back: the new profile's id and the
/// name it was filed under, which is the archive's own unless another
/// profile already held it (#767).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedProfile {
    pub id: i64,
    pub name: String,
}

#[cfg(test)]
mod tests;
