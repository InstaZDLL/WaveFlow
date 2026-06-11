//! HTTP guards shared across every provider.
//!
//! Three concerns ride here together because they all interact with the
//! same `reqwest::Client` and `reqwest::Response`:
//!
//! 1. **Body size caps** — every `.text()` / `.json()` call on a
//!    third-party response is a latent DoS: a 100 MB lyric body is
//!    rare in practice but trivial to host, and the desktop process
//!    has no business buffering megabytes of HTML to scrape a `<span>`
//!    out of it. [`safe_text`] and [`safe_json`] cap the read and
//!    short-circuit on `Content-Length` when the server is honest.
//!
//! 2. **Redirect host pinning** — provider responses sometimes carry
//!    URLs the client is then expected to follow (Genius search →
//!    song page, Megalobiz HTML scrape → lyric page). The default
//!    `reqwest` redirect policy allows ten hops to any host, so a
//!    compromised provider response could redirect us into an
//!    internal address (SSRF) or any attacker-controlled host. The
//!    client built by [`build_client`] caps redirects at 3 AND
//!    rejects every redirect that leaves the allowlist below.
//!
//! 3. **URL log redaction** — Musixmatch's request URL carries
//!    `usertoken=` as a query parameter; a `Debug`-formatted
//!    `reqwest::Error` echoes the URL, which lands in our rolling
//!    log file. [`redact_url`] strips userinfo + query before any
//!    tracing macro sees the URL.

use std::time::Duration;

use reqwest::{redirect, Client, Response};
use url::Url;

use crate::{Error, Result};

/// Upper bound on a single provider response body (per call). Picked to
/// be generous for HTML lyric pages (Megalobiz, Genius) while still
/// containing a hostile or compromised host.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Hostnames + suffixes the redirect policy will follow. Anything else
/// terminates the chain with an error instead of hopping further. The
/// suffix variant catches CDN edges (e.g. `*.musixmatch.com`).
const ALLOWED_HOSTS: &[&str] = &[
    "apic-desktop.musixmatch.com",
    "lrclib.net",
    "music.163.com",
    "www.megalobiz.com",
    "api.genius.com",
    "genius.com",
];

/// Suffix allowlist for CDNs and locale subdomains. Keep both arrays
/// small — every entry is a place an attacker could host a payload.
const ALLOWED_SUFFIXES: &[&str] = &[".musixmatch.com", ".genius.com"];

fn host_is_allowed(host: &str) -> bool {
    if ALLOWED_HOSTS.contains(&host) {
        return true;
    }
    ALLOWED_SUFFIXES.iter().any(|suffix| host.ends_with(suffix))
}

/// Build the syncedlyrics shared `reqwest::Client` with the redirect +
/// timeout guards every provider needs. Use this instead of
/// `reqwest::Client::new()` so every provider inherits the same
/// SSRF / DoS guards by construction.
pub fn build_client() -> Result<Client> {
    let policy = redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 3 {
            return attempt.error("too many redirects");
        }
        let host = attempt.url().host_str().map(str::to_string);
        match host {
            Some(host) if host_is_allowed(&host) => attempt.follow(),
            Some(host) => attempt.error(format!("redirect to disallowed host: {host}")),
            None => attempt.error("redirect target has no host"),
        }
    });
    let client = Client::builder()
        .user_agent("WaveFlow/1.4 (https://github.com/InstaZDLL/WaveFlow)")
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .redirect(policy)
        .build()?;
    Ok(client)
}

/// Read a response body into a `String`, refusing to allocate beyond
/// `MAX_BODY_BYTES`. The check happens twice: once against
/// `Content-Length` when present (so an honest server is short-
/// circuited before any bytes flow) and once incrementally as chunks
/// arrive (so a server that lies about Content-Length or omits it
/// can't bypass the cap).
pub async fn safe_text(response: Response) -> Result<String> {
    let bytes = safe_bytes(response).await?;
    String::from_utf8(bytes)
        .map_err(|e| Error::Provider(format!("response is not valid utf-8: {e}")))
}

/// Read a response body as JSON, refusing to allocate beyond
/// `MAX_BODY_BYTES`.
pub async fn safe_json<T: serde::de::DeserializeOwned>(response: Response) -> Result<T> {
    let bytes = safe_bytes(response).await?;
    serde_json::from_slice(&bytes).map_err(Error::from)
}

/// Workhorse for [`safe_text`] / [`safe_json`]. Returns the response
/// body as raw bytes, hard-capped at [`MAX_BODY_BYTES`].
pub async fn safe_bytes(mut response: Response) -> Result<Vec<u8>> {
    if let Some(len) = response.content_length() {
        if len as usize > MAX_BODY_BYTES {
            return Err(Error::Provider(format!(
                "response advertises {len} bytes, over cap of {MAX_BODY_BYTES}"
            )));
        }
    }
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    while let Some(chunk) = response.chunk().await? {
        if buf.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(Error::Provider(format!(
                "response exceeded body cap of {MAX_BODY_BYTES}"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Validate that `candidate` is an absolute http(s) URL pointing at one
/// of `allowed_hosts`. Used by Genius / Megalobiz before they follow a
/// URL plucked out of a third-party response — without the host pin,
/// poisoned upstream content could redirect the desktop client into an
/// internal address (SSRF).
pub fn validate_outbound_url(candidate: &str, allowed_hosts: &[&str]) -> Result<Url> {
    let url = Url::parse(candidate)
        .map_err(|e| Error::Provider(format!("invalid url {candidate}: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(Error::Provider(format!(
                "url {candidate} uses unsupported scheme {other}"
            )))
        }
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Provider(format!("url {candidate} has no host")))?;
    if !allowed_hosts.contains(&host) {
        return Err(Error::Provider(format!(
            "url {candidate} host {host} not in allowlist"
        )));
    }
    Ok(url)
}

/// Strip userinfo + query string from a URL before it lands in a
/// tracing log. Musixmatch's URLs carry `usertoken=` and similar
/// credentials in the query string; without this guard a
/// `Debug`-formatted `reqwest::Error` would echo the secret to the
/// rolling log file.
///
/// Kept exported so future callers in `commands/lyrics.rs` (or any
/// debugging path that needs to surface the URL of a failed provider
/// call) have a safe option instead of inlining their own redactor.
#[allow(dead_code)]
pub fn redact_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(parsed) => {
            let scheme = parsed.scheme();
            let host = parsed.host_str().unwrap_or("");
            let port = match parsed.port() {
                Some(p) => format!(":{p}"),
                None => String::new(),
            };
            let path = parsed.path();
            format!("{scheme}://{host}{port}{path}")
        }
        Err(_) => "<unparseable url>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_strips_userinfo_and_query() {
        assert_eq!(
            redact_url("https://user:secret@host.example/path?token=xyz"),
            "https://host.example/path"
        );
        assert_eq!(
            redact_url("https://lrclib.net/api/get?artist_name=a&track_name=b"),
            "https://lrclib.net/api/get"
        );
        assert_eq!(redact_url("not a url"), "<unparseable url>");
    }

    #[test]
    fn host_allowlist_round_trips() {
        assert!(host_is_allowed("apic-desktop.musixmatch.com"));
        assert!(host_is_allowed("lrclib.net"));
        // Suffix match for CDNs.
        assert!(host_is_allowed("static.musixmatch.com"));
        assert!(host_is_allowed("img.genius.com"));
        // Adjacent but distinct hosts must not slip through.
        assert!(!host_is_allowed("musixmatch.com.attacker.com"));
        assert!(!host_is_allowed("evil.com"));
    }

    #[test]
    fn validate_outbound_url_rejects_off_host() {
        let allowed = &["genius.com"];
        assert!(validate_outbound_url("https://genius.com/song", allowed).is_ok());
        assert!(validate_outbound_url("https://evil.com/song", allowed).is_err());
        assert!(validate_outbound_url("ftp://genius.com/song", allowed).is_err());
        assert!(validate_outbound_url("file:///etc/passwd", allowed).is_err());
        assert!(validate_outbound_url("garbage", allowed).is_err());
    }
}
