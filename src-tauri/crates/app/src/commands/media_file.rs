//! Shared helper for user-picked mp4 media files hash-addressed into a
//! never-evicted per-profile directory. Backs both the manual album motion
//! cover ([`super::motion_artwork`], issue #408) and the per-track Canvas
//! ([`super::canvas`], issue #442): both take a local mp4, validate it,
//! BLAKE3-hash it and write it as `<hash>.mp4`, differing only in their SQL
//! and their target directory (which the caller supplies).

use std::path::Path;

use tokio::io::AsyncReadExt;

use crate::error::{AppError, AppResult};

/// First box of an ISO base media file (mp4/mov) is required to be `ftyp`
/// when present, which in practice means always for a real-world mp4 — the
/// "check the magic bytes, don't fully parse" approach `detect_image_format`
/// (`deezer.rs`) uses for jpg/png/webp.
fn is_mp4(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[4..8] == b"ftyp"
}

/// Read `file_path` (bounded), validate it is an mp4, BLAKE3-hash it and
/// write it into `dir` as `<hash>.mp4` if not already present. Returns the
/// hex hash for the caller to persist.
///
/// The read is capped at `max_bytes + 1` so an oversized (or maliciously
/// huge) file is never fully buffered into memory before the size check
/// below rejects it — reading one extra byte still lets the check see "over
/// the limit". `dir` is created if missing.
pub async fn store_hash_addressed_mp4(
    dir: &Path,
    file_path: &str,
    max_bytes: u64,
) -> AppResult<String> {
    tokio::fs::create_dir_all(dir).await?;

    let file = tokio::fs::File::open(file_path).await?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > max_bytes {
        return Err(AppError::Other(format!(
            "file too large (max {max_bytes} bytes)"
        )));
    }
    if !is_mp4(&bytes) {
        return Err(AppError::Other("unsupported format (expected mp4)".into()));
    }

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = dir.join(format!("{hash}.mp4"));
    if !tokio::fs::try_exists(&target).await? {
        tokio::fs::write(&target, &bytes).await?;
    }
    Ok(hash)
}
