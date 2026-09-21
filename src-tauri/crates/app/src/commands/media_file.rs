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
/// Bytes the way the interface spells them, so a refusal reads like
/// the sizes shown everywhere else in the app.
///
/// Mirrors `src/lib/format.ts`: binary steps, decimal labels, one
/// decimal below 100 and none above. Deliberately not localised — it
/// lands in an error string that already carries untranslated
/// technical detail, and a number the reader can compare against their
/// file manager is worth more than a translated unit.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 100.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

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
        // The read was capped at `max_bytes + 1`, so `bytes.len()` says
        // "over the limit" and nothing about how far over. Stat the file
        // for the real number — one syscall, on the path that is already
        // failing — because "85.2 MB (max 256 MB)" tells the user whether
        // to re-encode or give up, and "max 67108864 bytes" tells them
        // to go and do the arithmetic.
        let actual = tokio::fs::metadata(file_path)
            .await
            .map(|m| format!("{} ", human_bytes(m.len())))
            .unwrap_or_default();
        return Err(AppError::Other(format!(
            "file too large: {actual}(max {})",
            human_bytes(max_bytes)
        )));
    }
    if !is_mp4(&bytes) {
        return Err(AppError::Other("unsupported format (expected mp4)".into()));
    }

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let target = dir.join(format!("{hash}.mp4"));
    // Fast path: already published (hash-addressed ⇒ identical bytes).
    if !tokio::fs::try_exists(&target).await? {
        // Publish atomically WITHOUT ever replacing an existing target: stage
        // the complete file into a unique temp, then hard-link it onto
        // `target`. `hard_link` is atomic and fails with `AlreadyExists` if
        // another importer published first (same hash ⇒ identical bytes), so a
        // concurrent reader sees either nothing or the whole file — never a
        // half-written one — and the winner's file is never truncated. The
        // temp is always removed; a non-`AlreadyExists` error still propagates.
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{hash}.{}.{seq}.part", std::process::id()));
        tokio::fs::write(&tmp, &bytes).await?;
        let link = tokio::fs::hard_link(&tmp, &target).await;
        let _ = tokio::fs::remove_file(&tmp).await;
        if let Err(e) = link {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(e.into());
            }
        }
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    /// The sizes that actually reach a user: the cap, and a file just
    /// over it. Both have to read like the numbers the rest of the app
    /// shows, or the refusal is arithmetic homework.
    #[test]
    fn reads_like_the_rest_of_the_interface() {
        assert_eq!(human_bytes(256 * 1024 * 1024), "256 MB");
        assert_eq!(human_bytes(89_128_960), "85.0 MB");
        assert_eq!(human_bytes(64 * 1024 * 1024), "64.0 MB");
    }

    /// Below a kilobyte there is nothing to scale, and the loop must
    /// stop at the last unit rather than running off the end of the
    /// table on an absurd value.
    #[test]
    fn holds_at_both_ends() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(9_007_199_254_740_992), "8192 TB");
    }
}
