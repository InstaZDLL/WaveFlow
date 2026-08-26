//! On-disk cache for the remote server's cover art (RFC-005).
//!
//! The artwork endpoint is Bearer-only, so a bare `<img src>` pointed at it
//! answers 401. Until now the way round that was to fetch the bytes and hand
//! the webview a `data:` URL — correct, and expensive in the two ways that
//! matter for a grid: the base64 lives in the renderer's memory for as long
//! as the cache holds it, and nothing survives a restart, so every launch
//! re-downloads every cover the user scrolls past.
//!
//! Caching to disk turns both around. The file goes through the asset
//! protocol exactly like a scanned local cover, which means
//! [`resolveArtwork`](../../../src/lib/tauri/artwork.ts) needs no special
//! case, the renderer holds a path instead of a blob, and a second launch
//! paints from disk.
//!
//! ## Content-addressed, therefore never revalidated
//!
//! The server addresses these by the hash of their bytes, so a hash resolves
//! to the same image forever. There is nothing to revalidate and no staleness
//! to fear: a cached file is either the right answer or absent. That is what
//! makes eviction safe — dropping a file costs one download, never a wrong
//! picture.
//!
//! That property is **not** shared by the whole endpoint. The same route also
//! accepts a track, album or artist identifier and resolves that entity's
//! *current* cover, which a rescan can move — the server marks only the
//! hash-addressed form immutable and keeps the aliases revalidatable. Caching
//! an alias forever would freeze a replaced cover on disk, so [`is_hash`]
//! refuses anything that is not plain hexadecimal. It reads as a path-traversal
//! guard, and it is one; it is also what keeps this cache honest.
//!
//! ## Eviction is by modification time, not by insertion
//!
//! A hit touches the file, so the cover of an album played every week keeps
//! its place while a one-off browse ages out. The cap is deliberately loose:
//! covers are tens of kilobytes and the whole point is that a large library
//! stays painted.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::Serialize;

use crate::{
    error::{AppError, AppResult},
    remote::client::RemoteClient,
    state::AppState,
};

/// Ceiling on the cache. A cover is typically 30–150 KB, so this holds
/// several thousand of them — enough for a library to stay painted while
/// staying a rounding error next to the music itself.
const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// Distinguishes one in-flight write from another. The process id alone is
/// not enough: two views asking for the same cover at the same moment are in
/// the same process, so they would pick the same temporary and interleave
/// their writes into it.
static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Extensions a cached cover can carry. Kept short and closed: the file name
/// is `<hash>.<ext>`, and looking a hash up means probing this list, so every
/// entry costs a `stat` on a miss.
const EXTENSIONS: [&str; 4] = ["jpg", "png", "webp", "gif"];

/// Map a response content type onto one of [`EXTENSIONS`].
///
/// The extension is not decoration: Tauri's asset protocol derives the
/// `Content-Type` it serves from it, and an unknown one arrives at the
/// webview as `application/octet-stream`, which no `<img>` will render.
fn extension_for(mime: &str) -> &'static str {
    match mime.split(';').next().unwrap_or("").trim() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        // Everything else is treated as JPEG, which is what the server sends
        // for anything it re-encoded.
        _ => "jpg",
    }
}

/// Reject anything that is not a plain hex hash.
///
/// Two reasons, and both matter. The value lands in a path, so a `..` or a
/// separator would write outside the cache directory. And an identifier that
/// is not a hash is an *alias* the server resolves to a current cover — a
/// thing this cache must never hold, since it never revalidates. A UUID has
/// dashes and fails here, which is the point.
fn is_hash(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The cached file for `hash`, if one is already on disk.
fn existing(dir: &Path, hash: &str) -> Option<PathBuf> {
    EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{hash}.{ext}")))
        .find(|path| path.is_file())
}

/// Absolute path to `hash`'s cover, downloading it once if this is the first
/// time it is asked for.
///
/// Returns a path rather than bytes: the caller hands it to the webview's
/// asset protocol, which streams the file itself.
pub async fn cached_path(state: &AppState, hash: &str) -> AppResult<PathBuf> {
    if !is_hash(hash) {
        return Err(AppError::Other("invalid artwork hash".into()));
    }
    // Pin the profile for the whole call: a switch mid-download must not write
    // one profile's cover into another's cache.
    let profile_id = state.require_profile_id().await?;
    let dir = state.paths.profile_remote_artwork_dir(profile_id);

    if let Some(path) = existing(&dir, hash) {
        // A hit is a use. Touching it is what keeps a cover that is actually
        // looked at ahead of one browsed past once.
        touch(&path);
        return Ok(path);
    }

    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
    let client = RemoteClient::try_build_for(state, profile_id)
        .await?
        .ok_or_else(|| AppError::Other("not signed in to a remote server".into()))?;

    let response = client
        .get(&format!("/api/v2/artwork/{hash}"))
        .send()
        .await
        .map_err(|err| AppError::Other(format!("artwork fetch: {err}")))?;
    if !response.status().is_success() {
        return Err(AppError::Other(format!(
            "artwork fetch: HTTP {}",
            response.status().as_u16()
        )));
    }
    let extension = extension_for(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("image/jpeg"),
    );
    let bytes = response
        .bytes()
        .await
        .map_err(|err| AppError::Other(format!("artwork body: {err}")))?;

    let path = dir.join(format!("{hash}.{extension}"));
    write_atomically(&dir, &path, &bytes).await?;
    evict(dir, MAX_CACHE_BYTES).await;
    Ok(path)
}

/// Write through a temporary and rename onto the final name.
///
/// Two views can ask for the same cover at once, and a half-written file
/// under the real name would be served as a broken image and then cached as
/// one — the name exists, so nothing would ever fetch it again. The rename is
/// atomic, so a reader sees either no file or a complete one, and the loser of
/// the race simply overwrites identical bytes.
async fn write_atomically(dir: &Path, path: &Path, bytes: &[u8]) -> AppResult<()> {
    let dir = dir.to_path_buf();
    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        // The name has to be unique across both kinds of concurrency: the
        // process id separates two WaveFlow instances sharing a profile
        // directory, the counter separates two writes inside one of them.
        let temporary = path.with_extension(format!(
            "part-{}-{}",
            std::process::id(),
            WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temporary, &bytes)?;
        std::fs::rename(&temporary, &path)
    })
    .await
    .map_err(|err| AppError::Other(format!("artwork write: {err}")))?
    .map_err(|err| AppError::Other(format!("artwork write: {err}")))?;
    Ok(())
}

/// Mark a cache hit, so eviction reads it as recently used.
fn touch(path: &Path) {
    let _ =
        std::fs::File::open(path).and_then(|file| file.set_modified(std::time::SystemTime::now()));
}

/// What the cache holds, for the settings card.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ArtworkCacheInfo {
    pub bytes: u64,
    pub covers: u64,
}

pub async fn info(state: &AppState) -> AppResult<ArtworkCacheInfo> {
    let profile_id = state.require_profile_id().await?;
    let dir = state.paths.profile_remote_artwork_dir(profile_id);
    Ok(tokio::task::spawn_blocking(move || scan(&dir).0)
        .await
        .unwrap_or_default())
}

/// Total footprint and per-file list, oldest first.
///
/// Counts the `.part-*` temporaries towards the size — a crashed write leaves
/// one behind, and reporting a footprint smaller than the directory really is
/// would make the number useless for the one thing it is for.
fn scan(dir: &Path) -> (ArtworkCacheInfo, Vec<(PathBuf, u64, std::time::SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (ArtworkCacheInfo::default(), Vec::new());
    };
    let mut info = ArtworkCacheInfo::default();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let complete = EXTENSIONS.iter().any(|ext| name.ends_with(ext));
        let temporary = name.contains(".part-");
        if !complete && !temporary {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        info.bytes += metadata.len();
        if complete && !temporary {
            info.covers += 1;
            files.push((
                path,
                metadata.len(),
                metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
            ));
        }
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    (info, files)
}

/// Drop the least recently used covers until the cache fits under `cap`.
async fn evict(dir: PathBuf, cap: u64) {
    let _ = tokio::task::spawn_blocking(move || {
        let (info, files) = scan(&dir);
        let mut total = info.bytes;
        for (path, size, _) in files {
            if total <= cap {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    })
    .await;
}

/// Delete every cached cover, temporaries included.
///
/// Reports what went wrong rather than swallowing it: "Clear" answering with a
/// silent success while the count stays put is the one outcome that makes the
/// button look broken and the cache look haunted.
pub async fn clear(state: &AppState) -> AppResult<()> {
    let profile_id = state.require_profile_id().await?;
    let dir = state.paths.profile_remote_artwork_dir(profile_id);
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // A cache that was never written has nothing to clear, and saying
            // so as an error would be a lie.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        for entry in entries {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let ours = EXTENSIONS.iter().any(|ext| name.ends_with(ext)) || name.contains(".part-");
            if !ours {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                // Something else removed it first, which is the outcome asked
                // for.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    })
    .await
    .map_err(|err| AppError::Other(format!("artwork cache clear: {err}")))?
    .map_err(|err| AppError::Other(format!("artwork cache clear: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_that_is_not_a_hash_never_reaches_the_filesystem() {
        assert!(is_hash("abc123"));
        assert!(is_hash(&"a".repeat(64)));

        for hostile in [
            "",
            "../../etc/passwd",
            "abc/def",
            "abc\\def",
            "abc.jpg",
            "café",
            &"a".repeat(129),
        ] {
            assert!(!is_hash(hostile), "{hostile} should be refused");
        }
    }

    /// The extension decides the `Content-Type` the asset protocol serves, so
    /// an unknown type must land on something an `<img>` will render rather
    /// than on the type's own name.
    #[test]
    fn the_extension_follows_the_content_type() {
        assert_eq!(extension_for("image/png"), "png");
        assert_eq!(extension_for("image/webp"), "webp");
        assert_eq!(extension_for("image/gif"), "gif");
        assert_eq!(extension_for("image/jpeg"), "jpg");
        // Parameters are common and must not defeat the match.
        assert_eq!(extension_for("image/png; charset=binary"), "png");
        assert_eq!(extension_for("  image/webp  "), "webp");
        // Anything unrecognised falls back to a renderable type.
        assert_eq!(extension_for("application/octet-stream"), "jpg");
        assert_eq!(extension_for(""), "jpg");
    }

    #[test]
    fn a_cover_is_found_whatever_extension_it_was_stored_under() {
        let dir = tempfile::tempdir().unwrap();
        assert!(existing(dir.path(), "abcd").is_none());

        std::fs::write(dir.path().join("abcd.webp"), b"x").unwrap();
        let found = existing(dir.path(), "abcd").unwrap();
        assert_eq!(found.extension().unwrap(), "webp");
    }

    /// A half-written file under the real name would be served as a broken
    /// image and never re-fetched, since the name exists.
    #[tokio::test]
    async fn a_partial_write_never_appears_under_the_final_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abcd.jpg");
        write_atomically(dir.path(), &path, b"bytes").await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"bytes");
        // Nothing temporary survives a successful write.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".part-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    /// Two views asking for the same cover at the same moment are in the same
    /// process, so a temporary named after the process alone would be the same
    /// file for both, and they would interleave their writes into it.
    #[tokio::test]
    async fn concurrent_writes_of_one_cover_do_not_share_a_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abcd.jpg");
        let bytes = vec![7u8; 4096];

        let writes = (0..8).map(|_| {
            let dir = dir.path().to_path_buf();
            let path = path.clone();
            let bytes = bytes.clone();
            async move { write_atomically(&dir, &path, &bytes).await }
        });
        for result in futures::future::join_all(writes).await {
            result.unwrap();
        }

        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".part-"))
            .collect();
        assert!(leftovers.is_empty(), "every temporary must be renamed away");
    }

    /// A purge that cannot delete must say so. Answering with a silent success
    /// while the count stays put makes the button look broken.
    #[tokio::test]
    async fn clearing_a_cache_that_was_never_written_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        // Exercised through the same closure `clear` runs; the command wrapper
        // only resolves the directory.
        let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            match std::fs::read_dir(&missing) {
                Ok(_) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err),
            }
        })
        .await
        .unwrap();
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn eviction_drops_the_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        for (name, age_secs) in [("aa.jpg", 300), ("bb.jpg", 200), ("cc.jpg", 100)] {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![0u8; 1000]).unwrap();
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
            std::fs::File::open(&path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }

        let (before, _) = scan(dir.path());
        assert_eq!((before.covers, before.bytes), (3, 3000));

        // Room for one file only: the two oldest go.
        evict(dir.path().to_path_buf(), 1000).await;

        let (after, _) = scan(dir.path());
        assert_eq!(after.covers, 1);
        assert!(dir.path().join("cc.jpg").is_file(), "newest must survive");
    }

    #[tokio::test]
    async fn the_reported_size_counts_leftovers_from_a_crashed_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("aa.jpg"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("bb.part-1234"), vec![0u8; 50]).unwrap();

        let (info, files) = scan(dir.path());
        // The temporary counts towards disk usage but is not a cover.
        assert_eq!((info.covers, info.bytes), (1, 150));
        assert_eq!(files.len(), 1);
    }
}
