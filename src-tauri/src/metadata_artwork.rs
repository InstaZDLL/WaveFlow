//! Shared on-disk cache for remote artwork (Deezer artist pictures, album
//! covers, …).
//!
//! Files live at `<root>/metadata_artwork/<blake3_hash>.jpg` so identical
//! images coming from different lookups dedupe to a single file. The blake3
//! hash is also persisted in the `metadata_artist.picture_hash` /
//! `metadata_album.cover_hash` columns so a cache hit on the metadata table
//! avoids re-downloading the bytes.
//!
//! Deezer always serves JPEG, so the extension is hardcoded to keep the
//! schema column hash-only.

use std::path::{Path, PathBuf};

const ARTWORK_EXT: &str = "jpg";
const DOWNLOAD_TIMEOUT_SECS: u64 = 10;
const USER_AGENT: &str = "WaveFlow/0.1";
/// Hard cap on a single image download. Deezer's `picture_xl` is ~200 KB; we
/// allow 4 MB to leave headroom for upstream changes without exposing the
/// process to runaway responses.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Resolve the absolute on-disk path for a hash, regardless of whether the
/// file exists yet.
pub fn path_for_hash(dir: &Path, hash: &str) -> PathBuf {
    dir.join(format!("{hash}.{ARTWORK_EXT}"))
}

/// Return the absolute path for a stored hash if (and only if) the file
/// actually exists on disk. Used to suppress stale `picture_path` values when
/// the cache directory has been wiped but the DB still references the hash.
pub fn existing_path(dir: &Path, hash: &str) -> Option<String> {
    let p = path_for_hash(dir, hash);
    if p.exists() {
        Some(p.to_string_lossy().to_string())
    } else {
        None
    }
}

/// Download `url`, blake3-hash the bytes and write the file to
/// `<dir>/<hash>.jpg` if missing. Returns the hex hash on success.
///
/// All failures (network, http != 2xx, oversize body, write error) are logged
/// at WARN level and surfaced as `None`. Enrichment is best-effort: the
/// caller should fall back to the remote URL.
pub async fn download_and_cache(url: &str, dir: &Path) -> Option<String> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
        .build()
        .ok()?;

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(%url, ?err, "metadata artwork download failed");
            return None;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(%url, status = %resp.status(), "metadata artwork unexpected status");
        return None;
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(%url, ?err, "metadata artwork read failed");
            return None;
        }
    };
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        tracing::warn!(%url, len = bytes.len(), "metadata artwork rejected (size)");
        return None;
    }

    let hash = blake3::hash(&bytes).to_hex().to_string();
    let out = path_for_hash(dir, &hash);
    if !out.exists() {
        if let Err(err) = std::fs::write(&out, &bytes) {
            tracing::warn!(path = %out.display(), ?err, "metadata artwork write failed");
            return None;
        }
    }
    crate::thumbnails::spawn_thumbnail_job(out, dir.to_path_buf(), hash.clone());
    Some(hash)
}
