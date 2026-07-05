//! Opt-in on-disk cache for animated-album-artwork (`.mp4`) motion covers.
//!
//! Portable business logic (no Tauri, no DB) — the desktop command layer owns
//! the toggle + `State` plumbing and just delegates here. Holds the download,
//! the hash-addressed store, LRU eviction, and the SSRF guard on
//! plugin-supplied URLs. Mirrors the shape of [`super::metadata`] (async
//! reqwest + synchronous `std::fs` writes; a few MB per file is well within
//! what a sync write costs).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// Per-file cap on a downloaded motion mp4 — refuses a hostile/oversized body
/// before it reaches disk. Apple's 1080 H.264 renditions are a few MB.
const MAX_MP4_BYTES: u64 = 64 * 1024 * 1024;

/// Default ceiling on the whole cache (LRU-evicted down to this).
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 1024 * 1024 * 1024;

/// A `.part` temp older than this is a crashed-download orphan (a real
/// download renames within milliseconds), so eviction may prune it without
/// racing an in-flight write.
const STALE_PART_AGE: Duration = Duration::from_secs(600);

/// Process-unique suffix source so two concurrent downloads of the same URL
/// never write to the same temp file (the final name is shared, but each
/// attempt stages through its own temp then renames atomically).
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Validate a plugin-supplied motion URL before the host fetches it. A plugin
/// is sandboxed for its OWN HTTP (host allowlist), but the cache download runs
/// in the app process, so without this guard a malicious plugin could point it
/// at `http://localhost:…` or `http://169.254.169.254/…` (SSRF). We require
/// `https` and reject loopback / private / link-local / unspecified IP
/// literals + `localhost`. A hostname that resolves to an internal IP at
/// connect time (DNS rebinding) is a residual we accept — plugins are curated
/// and user-installed, and a literal + scheme check covers the practical case.
pub fn is_safe_motion_url(url: &str) -> bool {
    // Scheme must be https (case-insensitive), nothing else.
    let rest = match url.get(..8) {
        Some(p) if p.eq_ignore_ascii_case("https://") => &url[8..],
        _ => return false,
    };
    // Authority runs up to the first `/`, `?` or `#`.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest);
    // Drop any userinfo, then the port.
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        // Bracketed IPv6 literal: `[::1]:443`.
        stripped.split(']').next().unwrap_or(stripped)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    if host.is_empty() {
        return false;
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        return false;
    }
    // Reject internal IP literals; a plain hostname passes.
    if let Ok(ip) = IpAddr::from_str(host) {
        return !is_internal_ip(ip);
    }
    true
}

fn is_internal_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_internal_v4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            // Both IPv4-mapped (`::ffff:a.b.c.d`) and the deprecated
            // IPv4-compatible (`::a.b.c.d`) forms reach the same v4 host, so
            // classify via the v4 rules — `to_ipv4()` covers both (unlike
            // `to_ipv4_mapped()`, which only handles the mapped form).
            if let Some(v4) = v6.to_ipv4() {
                return is_internal_v4(v4);
            }
            let seg = v6.segments()[0];
            // fc00::/7 unique-local, fe80::/10 link-local (is_unique_local /
            // is_unicast_link_local are unstable, so match the prefixes).
            (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
    }
}

fn is_internal_v4(v4: std::net::Ipv4Addr) -> bool {
    v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
}

/// Return the local path of `url`'s cached mp4, downloading it first if absent.
/// Hash-addressed by the (stable, per-album) source URL. On a hit the mtime is
/// bumped for access-LRU; on a miss the body is streamed under [`MAX_MP4_BYTES`],
/// staged through a unique temp then renamed, and the cache is evicted back
/// under `max_cache_bytes`. Caller MUST have validated the URL with
/// [`is_safe_motion_url`] first.
pub async fn cache_mp4(dir: &Path, url: &str, max_cache_bytes: u64) -> Result<PathBuf, String> {
    let hash = blake3::hash(url.as_bytes()).to_hex().to_string();
    let path = dir.join(format!("{hash}.mp4"));

    if path.exists() {
        // Best-effort access-LRU bump; a Windows share lock (webview reading
        // the file) just leaves the mtime at download time, which is fine.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
        return Ok(path);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {url}: HTTP {}", resp.status()));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_MP4_BYTES {
            return Err(format!("motion mp4 too large: {len} bytes (max {MAX_MP4_BYTES})"));
        }
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("read motion mp4 body: {e}"))?
    {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_MP4_BYTES {
            return Err(format!("motion mp4 exceeds {MAX_MP4_BYTES} bytes — refusing"));
        }
        bytes.extend_from_slice(&chunk);
    }

    std::fs::create_dir_all(dir).map_err(|e| format!("create cache dir: {e}"))?;
    // Unique temp per attempt (pid + monotonic seq) so concurrent downloads of
    // the SAME url can't clobber each other's partial write. Each stages its
    // own complete file then renames over the shared final name (a file rename
    // replaces atomically; last writer wins, both contents are complete).
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{hash}.{}.{seq}.part", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write motion mp4: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("publish motion mp4: {e}"));
    }
    evict_lru(dir, max_cache_bytes);
    Ok(path)
}

/// `(path, size, mtime)` for every complete `.mp4` in `dir` (the LRU set).
fn mp4_entries(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((path, meta.len(), mtime));
        }
    }
    out
}

/// Evict oldest complete `.mp4`s (by mtime) until at/under `cap`, and prune any
/// stale `.part` orphan (a crashed download; an in-flight one is younger than
/// [`STALE_PART_AGE`] so it's never touched). Best-effort throughout.
fn evict_lru(dir: &Path, cap: u64) {
    // Prune crashed-download orphans first so they don't inflate the total.
    if let Ok(rd) = std::fs::read_dir(dir) {
        let now = SystemTime::now();
        for entry in rd.flatten() {
            let path = entry.path();
            let is_part = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".part"));
            if !is_part {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let age = meta
                    .modified()
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .unwrap_or_default();
                if age >= STALE_PART_AGE {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    let mut entries = mp4_entries(dir);
    let mut total: u64 = entries.iter().map(|(_, size, _)| *size).sum();
    if total <= cap {
        return;
    }
    entries.sort_by_key(|(_, _, mtime)| *mtime); // oldest first
    for (path, size, _) in entries {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

/// Total on-disk footprint of the cache — counts complete `.mp4`s AND any
/// `.part` temporaries so the reported size reflects real disk usage.
pub fn stats(dir: &Path) -> (u64, u64) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (0, 0);
    };
    let mut size = 0u64;
    let mut count = 0u64;
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.ends_with(".mp4") || name.ends_with(".part")) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            size += meta.len();
            // Only complete covers count toward the user-facing tally.
            if name.ends_with(".mp4") {
                count += 1;
            }
        }
    }
    (size, count)
}

/// Delete every cached motion mp4 (and any leftover `.part` temporaries).
pub fn clear(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let is_cache_file = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".mp4") || n.ends_with(".part"));
        if is_cache_file {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_internal_and_non_https_urls() {
        assert!(is_safe_motion_url(
            "https://mvod.itunes.apple.com/itunes-assets/x.mp4"
        ));
        assert!(is_safe_motion_url("https://example.com:443/a.mp4"));
        // scheme
        assert!(!is_safe_motion_url("http://example.com/a.mp4"));
        assert!(!is_safe_motion_url("file:///etc/passwd"));
        // localhost / loopback
        assert!(!is_safe_motion_url("https://localhost/a.mp4"));
        assert!(!is_safe_motion_url("https://127.0.0.1/a.mp4"));
        assert!(!is_safe_motion_url("https://[::1]:8443/a.mp4"));
        // private / link-local / cloud-metadata
        assert!(!is_safe_motion_url("https://10.0.0.5/a.mp4"));
        assert!(!is_safe_motion_url("https://192.168.1.1/a.mp4"));
        assert!(!is_safe_motion_url("https://172.16.0.1/a.mp4"));
        assert!(!is_safe_motion_url("https://169.254.169.254/latest/meta-data"));
        assert!(!is_safe_motion_url("https://[fd00::1]/a.mp4"));
        // IPv4-mapped (`::ffff:a.b.c.d`) + IPv4-compatible (`::a.b.c.d`) IPv6
        // must not slip past the v6 branch.
        assert!(!is_safe_motion_url("https://[::ffff:127.0.0.1]/a.mp4"));
        assert!(!is_safe_motion_url("https://[::ffff:10.0.0.1]/a.mp4"));
        assert!(!is_safe_motion_url("https://[::ffff:169.254.169.254]/a.mp4"));
        assert!(!is_safe_motion_url("https://[::127.0.0.1]/a.mp4"));
        assert!(!is_safe_motion_url("https://[::192.168.0.1]/a.mp4"));
    }

    #[test]
    fn ipv4_embedded_loopback_is_internal() {
        use std::net::Ipv6Addr;
        let internal = [
            "::ffff:127.0.0.1", // mapped loopback
            "::ffff:192.168.0.1", // mapped private
            "::127.0.0.1",      // compatible loopback
            "::192.168.0.1",    // compatible private
        ];
        for s in internal {
            assert!(
                is_internal_ip(s.parse::<Ipv6Addr>().unwrap().into()),
                "{s} should be classified internal"
            );
        }
        // A genuine global v6 address is not internal.
        assert!(!is_internal_ip("2606:4700:4700::1111".parse::<Ipv6Addr>().unwrap().into()));
    }
}
