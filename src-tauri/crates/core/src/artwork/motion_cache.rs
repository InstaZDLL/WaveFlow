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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::error::describe_http_error;

/// Per-file cap on a downloaded motion mp4 — refuses a hostile/oversized body
/// before it reaches disk.
///
/// Sized for "Apple's 1080 H.264 renditions are a few MB", which stopped
/// being the whole story once plugins started offering 4K. Going over it is
/// not a visible failure — the caller degrades to streaming the remote URL —
/// so a 4K cover played fine and simply could never be cached, however many
/// times the user re-opened the album, with a `warn!` as the only trace.
/// Raised with the manual caps so "save motion covers locally" means what it
/// says for the covers people actually want saved.
const MAX_MP4_BYTES: u64 = 256 * 1024 * 1024;

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
/// `https` and reject loopback / private / link-local / unspecified /
/// broadcast / "this network" / carrier-grade NAT IP literals + `localhost`.
/// A hostname that resolves to an internal IP at connect time (DNS
/// rebinding) is a residual we accept — plugins are curated and
/// user-installed, and a literal + scheme check covers the practical case.
///
/// The host is the one `reqwest` will connect to: the URL goes through
/// [`url::Url::parse`], the WHATWG parser `reqwest` itself uses, never a
/// hand-split of the string. That parser reads every IPv4 shorthand as an
/// address — decimal `2130706433`, hex `0x7f000001`, octal `0177.0.0.1`, the
/// one-, two- and three-part forms `127.1` / `10.1.1` — and hands back
/// [`url::Host::Ipv4`] for all of them, so they meet the same IP rules as a
/// dotted quad instead of passing as a hostname. It also ends the authority
/// on a `\` and drops a userinfo the way the connection will, so
/// `https://127.0.0.1\@example.com/` is judged on `127.0.0.1`. A string it
/// cannot parse is refused: `reqwest` could not fetch it either.
pub fn is_safe_motion_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    // Scheme must be https; the parser has lowercased it.
    if parsed.scheme() != "https" {
        return false;
    }
    match parsed.host() {
        Some(url::Host::Domain(name)) => {
            // Strip a trailing FQDN dot: `localhost.` resolves to the same
            // target but would otherwise slip past the check below
            // (`"localhost."` != `"localhost"`). A dotted IPv4 with a final
            // dot (`127.0.0.1.`) never lands here: the parser reads it as
            // an address.
            let name = name.trim_end_matches('.');
            // The parser has lowercased the name and mapped it through
            // IDNA, so a full-width or circled `localhost` arrives as the
            // ASCII word.
            !(name.is_empty() || name == "localhost" || name.ends_with(".localhost"))
        }
        Some(url::Host::Ipv4(v4)) => !is_internal_v4(v4),
        Some(url::Host::Ipv6(v6)) => !is_internal_ip(IpAddr::V6(v6)),
        None => false,
    }
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

/// Loopback, private, link-local, unspecified or broadcast, or inside
/// `0.0.0.0/8` ("this network") or the `100.64.0.0/10` carrier-grade NAT
/// range, which sit inside a provider's (or a mesh VPN's) network rather
/// than on the internet. The same ranges as the downloader's
/// `is_public_ipv4` in the app crate.
fn is_internal_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || a == 0
        || (a == 100 && (b & 0xc0) == 64)
}

/// Redact a plugin-supplied URL for a logged error message: keep the scheme,
/// host and path but drop any userinfo, query and fragment. Those can carry
/// credentials or signed tokens on a CDN URL, and every failure string here is
/// logged. Not a validator — purely cosmetic redaction for the error text.
/// Shared with [`super::metadata`], whose download failures are logged too.
pub(super) fn redact_url(url: &str) -> String {
    let no_query = url.split(['?', '#']).next().unwrap_or(url);
    match no_query.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = match rest.split_once('/') {
                Some((a, p)) => (a, Some(p)),
                None => (rest, None),
            };
            // Drop `user:pass@` — the host is everything after the last `@`.
            let host = authority.rsplit('@').next().unwrap_or(authority);
            match path {
                Some(p) => format!("{scheme}://{host}/{p}"),
                None => format!("{scheme}://{host}"),
            }
        }
        None => no_query.to_string(),
    }
}

/// Why [`cache_mp4`] failed. The distinction is security-relevant to callers:
/// [`CacheError::UnsafeUrl`] means the initial url OR a redirect hop failed the
/// SSRF guard (or the redirect chain was aborted) — a hard rejection the caller
/// MUST NOT paper over by streaming the raw url to a `<video>`, which would
/// follow that same unsafe redirect unchecked. [`CacheError::Other`] is an
/// ordinary failure (network, HTTP status, oversize, disk) the caller may
/// degrade past by streaming the (already initial-validated) url.
#[derive(Debug)]
pub enum CacheError {
    /// The initial url or a redirect target failed [`is_safe_motion_url`], or
    /// the redirect chain was aborted. Never stream the url after this.
    UnsafeUrl,
    /// Any other failure. Carries a redacted, human-readable message.
    Other(String),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // No url interpolation — an unsafe url can carry credentials.
            CacheError::UnsafeUrl => f.write_str("refusing unsafe url"),
            CacheError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for CacheError {}

/// Decision for a single redirect hop, split out of the [`cache_mp4`] redirect
/// policy so the hop budget + per-hop SSRF re-validation are unit-testable
/// without a live server. (A local mock server can't exercise this path: every
/// hop to `127.0.0.1` is refused as [`RedirectDecision::Unsafe`] by the SSRF
/// guard before the count ever matters, and the initial request to it is
/// rejected up front — so the limit is only reachable as pure logic.)
#[derive(Debug, PartialEq, Eq)]
enum RedirectDecision {
    /// Safe target within the hop budget — follow it.
    Follow,
    /// Hop budget exhausted (matches reqwest's default of 10). Abort.
    TooMany,
    /// Target failed [`is_safe_motion_url`]. Abort.
    Unsafe,
}

/// `previous_hops` counts redirects already followed. Allow up to 10 and reject
/// the 11th (reqwest's default), and re-validate each hop's target — the hop
/// limit is checked first so an over-long chain fails closed regardless.
fn redirect_decision(previous_hops: usize, url: &str) -> RedirectDecision {
    if previous_hops > 10 {
        RedirectDecision::TooMany
    } else if is_safe_motion_url(url) {
        RedirectDecision::Follow
    } else {
        RedirectDecision::Unsafe
    }
}

/// Return the local path of `url`'s cached mp4, downloading it first if absent.
/// Hash-addressed by the (stable, per-album) source URL. On a hit the mtime is
/// bumped for access-LRU; on a miss the body is streamed under [`MAX_MP4_BYTES`],
/// staged through a unique temp then renamed, and the cache is evicted back
/// under `max_cache_bytes`. Caller MUST have validated the URL with
/// [`is_safe_motion_url`] first. On failure the [`CacheError`] variant tells the
/// caller whether it was a security rejection (never stream the url) or an
/// ordinary failure it may degrade past.
pub async fn cache_mp4(dir: &Path, url: &str, max_cache_bytes: u64) -> Result<PathBuf, CacheError> {
    // Defence-in-depth: the doc says the caller must have validated `url`, but
    // re-check the initial URL here so `cache_mp4` can't be made to fetch an
    // unsafe target by a caller that forgot — rejected before any filesystem or
    // network work. The redirect policy below covers hops 2+; this covers hop 1.
    if !is_safe_motion_url(url) {
        return Err(CacheError::UnsafeUrl);
    }
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

    // Follow redirects, but re-validate EVERY hop against the SSRF guard: the
    // caller only checked the initial `url`, and reqwest's default policy would
    // otherwise chase a 3xx to an internal / loopback / non-https target and
    // slip a request past that check.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(
            |attempt| match redirect_decision(attempt.previous().len(), attempt.url().as_str()) {
                RedirectDecision::Follow => attempt.follow(),
                RedirectDecision::TooMany => attempt.error("too many redirects"),
                RedirectDecision::Unsafe => attempt.error("unsafe redirect target"),
            },
        ))
        .build()
        .map_err(|e| CacheError::Other(format!("http client: {e}")))?;
    let mut resp = match client.get(url).send().await {
        Ok(r) => r,
        // The redirect policy aborts (unsafe hop or too many hops) as a
        // `reqwest::Error` flagged `is_redirect()` — treat it as a security
        // rejection, not an ordinary failure the caller may stream past.
        Err(e) if e.is_redirect() => return Err(CacheError::UnsafeUrl),
        Err(e) => {
            // `describe_http_error`: reqwest's Display appends the full
            // URL, which would put back the query `redact_url` just took off.
            return Err(CacheError::Other(format!(
                "download {}: {}",
                redact_url(url),
                describe_http_error(&e)
            )));
        }
    };
    if !resp.status().is_success() {
        return Err(CacheError::Other(format!(
            "download {}: HTTP {}",
            redact_url(url),
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_MP4_BYTES {
            return Err(CacheError::Other(format!(
                "motion mp4 too large: {len} bytes (max {MAX_MP4_BYTES})"
            )));
        }
    }
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| {
        CacheError::Other(format!("read motion mp4 body: {}", describe_http_error(&e)))
    })? {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_MP4_BYTES {
            return Err(CacheError::Other(format!(
                "motion mp4 exceeds {MAX_MP4_BYTES} bytes — refusing"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    std::fs::create_dir_all(dir)
        .map_err(|e| CacheError::Other(format!("create cache dir: {e}")))?;
    // Unique temp per attempt (pid + monotonic seq) so concurrent downloads of
    // the SAME url can't clobber each other's partial write. Each stages its
    // own complete file then renames over the shared final name (a file rename
    // replaces atomically; last writer wins, both contents are complete).
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".{hash}.{}.{seq}.part", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(CacheError::Other(format!("write motion mp4: {e}")));
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(CacheError::Other(format!("publish motion mp4: {e}")));
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
/// [`STALE_PART_AGE`] so it's never touched). Best-effort throughout. Also
/// keeps the desktop's own folder of converted clips in bounds.
pub fn evict_lru(dir: &Path, cap: u64) {
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
mod tests;
