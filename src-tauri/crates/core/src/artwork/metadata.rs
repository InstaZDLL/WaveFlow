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

/// Resolves an artwork path, preferring an existing local sidecar over the shared cache.
///
/// Local artwork uses the supplied hash and format to form `<local_dir>/<hash>.<format>`.
/// If that file is unavailable, the function checks the shared cache. Missing or stale
/// references yield `None`.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// let path = resolve_local_or_cached_path(
///     Path::new("/tmp/profile"),
///     None,
///     None,
///     Path::new("/tmp/cache"),
///     None,
/// );
///
/// assert_eq!(path, None);
/// ```
///
/// # Parameters
///
/// * `local_format` - The file extension of the local sidecar, such as `jpg`, `png`, or `webp`.
///
/// # Returns
///
/// The path to the first existing artwork file, or `None` when no referenced file exists.
pub fn resolve_local_or_cached_path(
    local_dir: &Path,
    local_hash: Option<&str>,
    local_format: Option<&str>,
    cache_dir: &Path,
    cache_hash: Option<&str>,
) -> Option<String> {
    if let (Some(hash), Some(format)) = (local_hash, local_format) {
        let p = local_dir.join(format!("{hash}.{format}"));
        if p.exists() {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    cache_hash.and_then(|h| existing_path(cache_dir, h))
}

/// Downloads an image, caches it under its BLAKE3 hash, and queues thumbnail generation.
///
/// The cached file is written as `<dir>/<hash>.jpg` when it does not already exist.
/// Returns the hexadecimal hash when the download and caching succeed; failures are
/// logged and produce `None`.
///
/// # Examples
///
/// ```no_run
/// # async fn example() {
/// let hash = download_and_cache("https://example.com/artwork.jpg", std::path::Path::new("/tmp/artwork")).await;
/// assert!(hash.is_some());
/// # }
/// ```
///
/// # Returns
///
/// The hexadecimal BLAKE3 hash of the cached image, or `None` if downloading,
/// reading, validation, or writing fails.
pub async fn download_and_cache(url: &str, dir: &Path) -> Option<String>
pub async fn download_and_cache(url: &str, dir: &Path) -> Option<String> {
    download_and_cache_inner(url, dir, true).await
}

/// Downloads an image and caches it without generating thumbnails.
///
/// # Returns
///
/// The BLAKE3 hash of the cached image, or `None` if the download or caching fails.
///
/// # Examples
///
/// ```no_run
/// # async fn example() {
/// let hash = download_and_cache_full_res(
///     "https://example.com/artist-fanart.jpg",
///     std::path::Path::new("/tmp/artwork"),
/// ).await;
/// # }
/// ```
pub async fn download_and_cache_full_res(url: &str, dir: &Path) -> Option<String> {
    download_and_cache_inner(url, dir, false).await
}

/// Downloads artwork, caches it under its content hash, and optionally queues thumbnail generation.
///
/// Returns `None` when the download fails, the response is unsuccessful, the response body is empty or too large, or the cached file cannot be written.
///
/// # Examples
///
/// ```no_run
/// # async fn example() {
/// let hash = download_and_cache_inner(
///     "https://example.com/artwork.jpg",
///     std::path::Path::new("/tmp/artwork"),
///     true,
/// )
/// .await;
/// assert!(hash.is_some());
/// # }
/// ```
async fn download_and_cache_inner(
    url: &str,
    dir: &Path,
    generate_thumbnails: bool,
) -> Option<String> {
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
    if generate_thumbnails {
        crate::artwork::thumbnails::spawn_thumbnail_job(out, dir.to_path_buf(), hash.clone());
    }
    Some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn prefers_existing_local_over_cache() {
        let local = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        touch(local.path(), "abc.png");
        touch(cache.path(), "def.jpg"); // cache also present

        let got = resolve_local_or_cached_path(
            local.path(),
            Some("abc"),
            Some("png"),
            cache.path(),
            Some("def"),
        )
        .unwrap();
        assert_eq!(got, local.path().join("abc.png").to_string_lossy());
    }

    #[test]
    fn falls_back_to_cache_when_local_file_missing() {
        let local = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // Local hash/format point at a file that was wiped; only the cache exists.
        touch(cache.path(), "def.jpg");

        let got = resolve_local_or_cached_path(
            local.path(),
            Some("abc"),
            Some("png"),
            cache.path(),
            Some("def"),
        )
        .unwrap();
        assert_eq!(got, cache.path().join("def.jpg").to_string_lossy());
    }

    #[test]
    fn incomplete_local_skips_to_cache() {
        let local = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        touch(cache.path(), "def.jpg");

        // hash without format → local candidate is not even attempted.
        let got = resolve_local_or_cached_path(
            local.path(),
            Some("abc"),
            None,
            cache.path(),
            Some("def"),
        );
        assert_eq!(got.unwrap(), cache.path().join("def.jpg").to_string_lossy());
    }

    #[test]
    fn none_when_nothing_resolves() {
        let local = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        // Local file missing, no cache hash at all.
        assert!(resolve_local_or_cached_path(
            local.path(),
            Some("abc"),
            Some("png"),
            cache.path(),
            None,
        )
        .is_none());

        // Cache hash given but the file doesn't exist on disk either.
        assert!(resolve_local_or_cached_path(
            local.path(),
            None,
            None,
            cache.path(),
            Some("ghost"),
        )
        .is_none());
    }
}
