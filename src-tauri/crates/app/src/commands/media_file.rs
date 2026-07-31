//! Shared helper for user-picked mp4 media files hash-addressed into a
//! never-evicted per-profile directory. Backs both the manual album motion
//! cover ([`super::motion_artwork`], issue #408) and the per-track Canvas
//! ([`super::canvas`], issue #442): both take a local mp4, validate it,
//! BLAKE3-hash it and write it as `<hash>.mp4`, differing only in their SQL
//! and their target directory (which the caller supplies).

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::AsyncReadExt;

use crate::error::{AppError, AppResult};

/// Per-call counter making each in-flight temp file name unique, so two
/// concurrent imports of the same hash in this process never collide on the
/// staging path.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

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
        // Atomic publish: stage into a unique temp in the same directory,
        // then rename onto `target` only if nobody published it first. Rename
        // is atomic within a filesystem, so a concurrent reader never sees a
        // half-written file — unlike a bare `write`, which truncates then
        // fills. Same-hash ⇒ identical bytes, so on a lost race we simply drop
        // our temp rather than overwrite the winner's file.
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{hash}.{}.{seq}.part", std::process::id()));
        tokio::fs::write(&tmp, &bytes).await?;
        if tokio::fs::try_exists(&target).await? {
            let _ = tokio::fs::remove_file(&tmp).await;
        } else if let Err(e) = tokio::fs::rename(&tmp, &target).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e.into());
        }
    }
    Ok(hash)
}
