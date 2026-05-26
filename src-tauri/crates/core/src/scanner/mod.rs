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

pub mod extract;
pub mod upserts;

pub use extract::{
    extension_for_mime, extract_artist_image, extract_compilation_flag, extract_cover,
    extract_folder_cover, extract_musical_key, extract_rating, file_type_label, find_artist_image_in_dir,
    hash_file, write_artist_image, ExtractedCover, ExtractedFile, AUDIO_EXTENSIONS,
};
pub use upserts::{
    canonical_name, link_local_artist_image, maybe_link_artist_images,
    merge_implicit_compilations, now_millis, resolve_album_artist, split_artist_name,
    upsert_album, upsert_artist, upsert_artist_list, upsert_artwork, upsert_genre,
    VARIOUS_ARTISTS_LABEL,
};

/// Helper used inside the audio file extractors. Pulled out into the
/// module root so the `extract_album_artist` wrapper that pairs it
/// with `extract_compilation_flag` lives next to its consumers.
pub use extract::extract_album_artist;
