//! WaveFlow Web Radio plugin — guest side.
//!
//! Implements `waveflow:source/provider` against the
//! [radio-browser.info](https://www.radio-browser.info/) federated
//! API. Stations are fetched live on every `resolve` so the plugin
//! itself stays tiny (no embedded SQLite catalogue) — Phase 4.2
//! will ship a `receiver.db` snapshot as a sidecar asset for
//! offline browsing.
//!
//! All outbound HTTP is gated through `waveflow:host/http`, with
//! the manifest's allowlist scoped to `*.api.radio-browser.info`
//! + `radio-browser.info`.

#[allow(warnings)]
mod bindings;

use bindings::exports::waveflow::source::provider::{Entry, Guest, Track};
use bindings::waveflow::host::http::{self, Request};
use bindings::waveflow::host::log::{self, Level};

use serde::Deserialize;

/// Default federated mirror. radio-browser.info uses round-robin
/// DNS across regional mirrors (`de1`, `at1`, `us1`, …); the
/// chosen prefix doesn't matter — any of them serves the same
/// catalogue. We pin one rather than resolving via
/// `/json/servers` to avoid an extra round-trip per session.
const MIRROR: &str = "https://de1.api.radio-browser.info";

/// `User-Agent` radio-browser.info asks for in their API
/// guidelines. Bumped automatically with the crate version so
/// their analytics can attribute traffic per release.
const USER_AGENT: &str = concat!("WaveFlow/Web-Radio/", env!("CARGO_PKG_VERSION"));

/// Per-page cap on resolves. radio-browser's defaults are 100 000
/// rows; we trim to a UI-friendly chunk + lean on `hidebroken=true`
/// to drop stations the federated bot has flagged as unreachable.
const PAGE_LIMIT: u32 = 50;

struct WebRadio;

impl Guest for WebRadio {
    /// Top-level categories the host renders in the source picker.
    /// Each `query` is an opaque token the host hands back to
    /// `resolve`; the plugin parses it.
    fn list_entries() -> Result<Vec<Entry>, String> {
        // Catalogue tuned for the music-app context — popularity
        // genres plus a top-overall slot for casual browsing. The
        // user-facing label is the only thing the host shows; the
        // `query` is the wire format we parse below.
        Ok(vec![
            entry("Top stations", "top"),
            entry("Trending now", "trending"),
            entry("Jazz", "tag:jazz"),
            entry("Rock", "tag:rock"),
            entry("Pop", "tag:pop"),
            entry("Electronic", "tag:electronic"),
            entry("Classical", "tag:classical"),
            entry("News", "tag:news"),
            entry("Hip-Hop", "tag:hiphop"),
            entry("Country", "tag:country"),
            entry("Lofi", "tag:lofi"),
            entry("Ambient", "tag:ambient"),
        ])
    }

    /// Translate an entry token (or a free-form search string from
    /// the host's search box) into a list of tracks. Limited to
    /// [`PAGE_LIMIT`] per call — the UI paginates from there.
    fn resolve(query: String) -> Result<Vec<Track>, String> {
        log::emit(Level::Debug, &format!("web-radio resolve: {query}"));
        let url = build_url(&query)?;
        let body = fetch_json(&url)?;
        let stations: Vec<RbStation> = serde_json::from_slice(&body)
            .map_err(|e| format!("radio-browser response parse: {e}"))?;
        Ok(stations.into_iter().filter_map(to_track).collect())
    }

    /// The track `id` we hand back from `resolve` already carries
    /// the resolved stream URL (`url:<stream>`), so this is a pure
    /// extract — no network hit, no state lookup. radio-browser
    /// hot-swaps the URL behind the same `stationuuid` on rare
    /// occasion; if a stream goes 404, the user reopens the
    /// category and the next resolve picks up the new URL.
    fn stream_url(track_id: String) -> Result<String, String> {
        track_id
            .strip_prefix("url:")
            .map(str::to_string)
            .ok_or_else(|| format!("invalid track id: {track_id}"))
    }
}

bindings::export!(WebRadio with_types_in bindings);

fn entry(label: &str, query: &str) -> Entry {
    Entry {
        label: label.to_string(),
        query: query.to_string(),
        icon_url: None,
    }
}

/// Decode the entry / search token into a radio-browser API URL.
/// Tokens we understand:
///
/// - `top` → top-voted stations
/// - `trending` → most-recently-updated stations
/// - `tag:<name>` → stations tagged `<name>`, ordered by votes
/// - `search:<term>` → explicit name search prefix
/// - anything else non-empty → treated as a free-text search term
///   (the host's search box hands its raw input here and expects
///   matches against station names)
fn build_url(query: &str) -> Result<String, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err("empty query".into());
    }
    if trimmed.eq_ignore_ascii_case("top") {
        return Ok(format!(
            "{MIRROR}/json/stations/topvote/{PAGE_LIMIT}?hidebroken=true"
        ));
    }
    if trimmed.eq_ignore_ascii_case("trending") {
        return Ok(format!(
            "{MIRROR}/json/stations/lastchange/{PAGE_LIMIT}?hidebroken=true"
        ));
    }
    if let Some(tag) = trimmed.strip_prefix("tag:") {
        let tag = url_encode(tag.trim());
        if tag.is_empty() {
            return Err("empty tag".into());
        }
        return Ok(format!(
            "{MIRROR}/json/stations/bytag/{tag}?limit={PAGE_LIMIT}&order=votes&reverse=true&hidebroken=true"
        ));
    }
    // `country:<ISO2>` → stations in one country, by ISO 3166-1
    // alpha-2 code (e.g. `country:FR`). The host hands the code from
    // its country picker / "local stations" shortcut. We validate the
    // shape (exactly two ASCII letters) so a malformed token can't be
    // smuggled into the path segment, and upper-case it because
    // radio-browser's `bycountrycodeexact` is case-sensitive on the
    // canonical upper form.
    if let Some(code) = trimmed.strip_prefix("country:") {
        let code = code.trim();
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err("country code must be ISO 3166-1 alpha-2".into());
        }
        let code = code.to_ascii_uppercase();
        return Ok(format!(
            "{MIRROR}/json/stations/bycountrycodeexact/{code}?limit={PAGE_LIMIT}&order=votes&reverse=true&hidebroken=true"
        ));
    }
    // Both the explicit `search:` form and the free-text fallback
    // hit the same endpoint. The prefix is kept as a hint for the
    // host UI to disambiguate "this was a search" from "this was a
    // category click" in user-facing breadcrumbs.
    let term_raw = trimmed.strip_prefix("search:").unwrap_or(trimmed).trim();
    let term = url_encode(term_raw);
    if term.is_empty() {
        return Err("empty search term".into());
    }
    Ok(format!(
        "{MIRROR}/json/stations/search?name={term}&limit={PAGE_LIMIT}&order=votes&reverse=true&hidebroken=true"
    ))
}

/// Issue a GET, surface the body bytes. The host enforces the
/// manifest's HTTP allowlist + the 10 MB response cap + the offline
/// short-circuit, so failure modes here are limited to network
/// errors and non-2xx HTTP statuses (which we propagate verbatim).
fn fetch_json(url: &str) -> Result<Vec<u8>, String> {
    let req = Request {
        method: "GET".into(),
        url: url.into(),
        headers: vec![
            ("User-Agent".into(), USER_AGENT.into()),
            ("Accept".into(), "application/json".into()),
        ],
        body: None,
    };
    let resp = http::send(&req).map_err(|e| format!("http: {e}"))?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("http status {}", resp.status));
    }
    Ok(resp.body)
}

/// Minimal `application/x-www-form-urlencoded`-style encoder for
/// the path / query fragments we send. radio-browser is tolerant
/// to a bare unescaped subset (alphanum + `-_.`), so this covers
/// the spaces + non-ASCII + punctuation a user might type without
/// pulling in a crate dependency.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

/// radio-browser station row shape — only the fields we actually
/// surface to the host are deserialised.
#[derive(Deserialize)]
struct RbStation {
    name: String,
    /// Federation-verified URL. `null` for stations the bot
    /// couldn't reach; we fall back to `url` (raw author submission)
    /// when missing.
    url_resolved: Option<String>,
    url: String,
    /// Comma-separated tags ("jazz,piano,smooth"). We surface only
    /// the first one as a pseudo-album label so the host's track
    /// table has something to group by.
    tags: String,
    country: String,
    favicon: Option<String>,
    bitrate: Option<u32>,
}

/// Lift a radio-browser row into a Track the host can render +
/// queue. Returns `None` when the row has no usable stream URL,
/// so the resolve list automatically drops dead entries instead
/// of polluting the UI with "0 ms" rows.
fn to_track(s: RbStation) -> Option<Track> {
    // radio-browser sometimes emits `url_resolved == Some("")` when
    // the federation bot reached the source but couldn't extract a
    // playable URL (typical of streams gated behind an HTML
    // landing page). Treat an empty `url_resolved` exactly like a
    // null one — fall back to `url`, then drop the row only if
    // both are empty.
    let stream = s
        .url_resolved
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .unwrap_or(s.url);
    if stream.is_empty() {
        return None;
    }
    let first_tag = s.tags.split(',').next().unwrap_or("").trim();
    let album = if first_tag.is_empty() {
        None
    } else {
        Some(capitalise(first_tag))
    };
    let artist = if s.country.is_empty() {
        "Internet Radio".to_string()
    } else {
        s.country
    };
    // Radio streams are open-ended, so we report 0 ms. The host's
    // player treats `duration-ms == 0` as "live" and disables the
    // seek bar accordingly.
    Some(Track {
        id: format!("url:{stream}"),
        title: s.name.trim().to_string(),
        artist,
        album,
        duration_ms: 0,
        artwork_url: s.favicon.filter(|s| !s.is_empty()),
        icy_url: bitrate_icy_hint(&stream, s.bitrate),
    })
}

/// Title-case a tag string for display ("jazz" → "Jazz",
/// "hip hop" → "Hip Hop"). ASCII-only — radio-browser tags are
/// almost exclusively lower-case Latin, the few Cyrillic /
/// CJK exceptions render fine as-is.
fn capitalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut next_upper = true;
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '-' {
            next_upper = true;
            out.push(ch);
        } else if next_upper {
            for c in ch.to_uppercase() {
                out.push(c);
            }
            next_upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// ICY metadata is fetched over the same HTTP connection as the
/// audio stream — the host's player polls it directly without
/// re-routing through the plugin. Returning the stream URL itself
/// (when the bitrate hints at a SHOUTcast / icecast endpoint) tells
/// the host "yes, this will speak ICY metadata"; a non-shoutcast
/// codec (HLS, DASH) gets `None` so the host doesn't waste a poll.
fn bitrate_icy_hint(stream: &str, bitrate: Option<u32>) -> Option<String> {
    // Conservative: most radio-browser entries are MP3/AAC SHOUTcast
    // streams. Skip when the URL ends in `.m3u8` (HLS) or `.mpd`
    // (DASH) where ICY metadata isn't carried.
    if stream.ends_with(".m3u8") || stream.ends_with(".mpd") {
        return None;
    }
    bitrate.filter(|&b| b > 0).map(|_| stream.to_string())
}
