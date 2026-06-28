//! Pure-Rust scanner helpers shared between the desktop walker
//! (`crates/app/src/commands/scan.rs`) and the future
//! `waveflow-server` ingest job (RFC-001).
//!
//! The actual file-walk + Tauri-event orchestration stays in
//! `crates/app` for now — what lives here is everything that can run
//! standalone given just a path + a SQLite connection: cover / artist
//! image extraction, lofty tag → struct mapping (less the DSD dispatch
//! which still uses `crate::audio::dsd::*`), album-grouping policy,
//! and the row upserts that wrap each helper for transactional writes.

pub mod canonical;
pub mod extract;

// `upserts` runs raw SQLite statements against a
// `&mut sqlx::SqliteConnection`. It's the only sqlite-specific surface
// in the scanner; the extract helpers above are pure (lofty + image).
// The future `waveflow-server` ingest job will provide a parallel
// `upserts_pg` module behind `feature = "postgres"` once the schema
// settles — for now the postgres build skips this entirely.
#[cfg(feature = "sqlite")]
pub mod upserts;

pub use canonical::canonical_name;
pub use extract::{
    extension_for_mime, extract_artist_image, extract_compilation_flag, extract_cover,
    extract_folder_cover, extract_musical_key, extract_rating, file_type_label,
    find_artist_image_in_dir, hash_file, hash_file_full, write_artist_image, ExtractedCover,
    ExtractedFile, AUDIO_EXTENSIONS,
};
#[cfg(feature = "sqlite")]
pub use upserts::{
    link_local_artist_image, link_va_artist_image, maybe_link_artist_images,
    merge_implicit_compilations, now_millis, resolve_album_artist, split_artist_name, upsert_album,
    upsert_artist, upsert_artist_list, upsert_artwork, upsert_genre, ArtistImageScanCache,
    VARIOUS_ARTISTS_LABEL,
};

/// Helper used inside the audio file extractors. Pulled out into the
/// module root so the `extract_album_artist` wrapper that pairs it
/// with `extract_compilation_flag` lives next to its consumers.
pub use extract::extract_album_artist;
