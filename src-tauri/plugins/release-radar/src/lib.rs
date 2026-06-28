#[allow(warnings)]
mod bindings;

use bindings::exports::waveflow::ui::extension::{Guest, MountPoint};
use bindings::waveflow::host::http::{self, Request};
use bindings::waveflow::host::library;
use bindings::waveflow::host::log::{self, Level};
use bindings::waveflow::host::storage;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USER_AGENT: &str = concat!("WaveFlow/Release-Radar/", env!("CARGO_PKG_VERSION"));
const CACHE_KEY: &str = "releases.json";
const DISMISSED_KEY: &str = "dismissed.json";
const CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_ARTISTS: u32 = 8;
const PER_ARTIST_LIMIT: u32 = 6;

struct ReleaseRadar;

impl Guest for ReleaseRadar {
    fn manifest() -> MountPoint {
        MountPoint {
            sidebar_label: "Release Radar".into(),
            sidebar_icon: Some("Radar".into()),
            initial_path: "/".into(),
        }
    }

    fn render(_path: String) -> Result<String, String> {
        render_descriptor(false)
    }

    fn on_event(event: String, payload: String) -> Result<String, String> {
        match event.as_str() {
            "refresh" => render_descriptor(true),
            "dismiss" => {
                if !payload.trim().is_empty() {
                    let mut dismissed = read_dismissed();
                    dismissed.insert(payload);
                    write_json(DISMISSED_KEY, &dismissed)?;
                }
                render_descriptor(false)
            }
            _ => Err(format!("unknown event: {event}")),
        }
    }
}

bindings::export!(ReleaseRadar with_types_in bindings);

fn render_descriptor(force_refresh: bool) -> Result<String, String> {
    let now = now_ms();
    let dismissed = read_dismissed();
    let mut cache = read_cache();
    let needs_refresh = force_refresh
        || cache
            .as_ref()
            .map(|c| now.saturating_sub(c.last_refresh_at) > CACHE_TTL_MS)
            .unwrap_or(true);

    let mut status = "cached";
    if needs_refresh {
        match refresh_releases(now) {
            Ok(next) => {
                write_json(CACHE_KEY, &next)?;
                cache = Some(next);
                status = "fresh";
            }
            Err(err) => {
                log::emit(Level::Warn, &format!("release radar refresh failed: {err}"));
                status = if cache.is_some() { "stale" } else { "error" };
            }
        }
    }

    let releases = cache.map(|c| c.releases).unwrap_or_default();
    let mut visible: Vec<ReleaseItem> = releases
        .into_iter()
        .filter(|r| !dismissed.contains(&r.id))
        .collect();
    visible.sort_by(|a, b| {
        b.release_date
            .cmp(&a.release_date)
            .then_with(|| a.artist.cmp(&b.artist))
            .then_with(|| a.title.cmp(&b.title))
    });

    let descriptor = ViewDescriptor {
        schema_version: 1,
        title: "Release Radar".into(),
        subtitle: "New releases from artists in your local library".into(),
        status: status.into(),
        last_updated_at: now,
        actions: vec![Action::event("Refresh", "refresh", "")],
        sections: vec![Section {
            title: "New releases".into(),
            items: visible.into_iter().map(item_to_view).collect(),
        }],
        empty_title: "No releases found".into(),
        empty_hint: "Try Refresh after adding more artists to your library.".into(),
    };
    serde_json::to_string(&descriptor).map_err(|e| e.to_string())
}

fn refresh_releases(now: i64) -> Result<ReleaseCache, String> {
    let artists = library::list_artists(MAX_ARTISTS)?;
    if artists.is_empty() {
        return Ok(ReleaseCache {
            last_refresh_at: now,
            releases: Vec::new(),
        });
    }

    let (from, to) = release_window();
    let mut releases = Vec::new();
    let mut seen = HashSet::new();
    for (idx, artist) in artists.iter().enumerate() {
        if idx > 0 {
            std::thread::sleep(Duration::from_millis(1100));
        }
        let query = format!(
            "artistname:\"{}\" AND firstreleasedate:[{} TO {}] AND (primarytype:album OR primarytype:single OR primarytype:ep)",
            escape_query(&artist.name),
            from,
            to
        );
        let url = format!(
            "https://musicbrainz.org/ws/2/release-group/?query={}&fmt=json&limit={PER_ARTIST_LIMIT}",
            url_encode(&query)
        );
        let body = http_get(&url)?;
        let parsed: MbSearch = serde_json::from_slice(&body)
            .map_err(|e| format!("musicbrainz parse for {}: {e}", artist.name))?;
        for group in parsed.release_groups {
            if group.first_release_date.trim().is_empty() {
                continue;
            }
            if !seen.insert(group.id.clone()) {
                continue;
            }
            releases.push(ReleaseItem {
                id: group.id.clone(),
                title: group.title,
                artist: artist.name.clone(),
                release_type: group.primary_type.unwrap_or_else(|| "Release".into()),
                release_date: group.first_release_date,
                musicbrainz_url: format!("https://musicbrainz.org/release-group/{}", group.id),
                artwork_url: Some(format!(
                    "https://coverartarchive.org/release-group/{}/front-250",
                    group.id
                )),
            });
        }
    }

    releases.sort_by(|a, b| b.release_date.cmp(&a.release_date));
    releases.truncate(60);
    Ok(ReleaseCache {
        last_refresh_at: now,
        releases,
    })
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let resp = http::send(&Request {
        method: "GET".into(),
        url: url.into(),
        headers: vec![
            ("User-Agent".into(), USER_AGENT.into()),
            ("Accept".into(), "application/json".into()),
        ],
        body: None,
    })?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("GET {url} returned HTTP {}", resp.status));
    }
    Ok(resp.body)
}

fn read_cache() -> Option<ReleaseCache> {
    storage::read_state(CACHE_KEY)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn read_dismissed() -> HashSet<String> {
    storage::read_state(DISMISSED_KEY)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json<T: Serialize>(key: &str, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    storage::write_state(key, &bytes)
}

fn item_to_view(item: ReleaseItem) -> ViewItem {
    ViewItem {
        id: item.id.clone(),
        title: item.title.clone(),
        subtitle: item.artist.clone(),
        detail: item.release_date.clone(),
        image_url: item.artwork_url,
        badges: vec![item.release_type],
        actions: vec![
            Action::open_url("MusicBrainz", &item.musicbrainz_url),
            Action::open_url(
                "Search YouTube",
                &format!(
                    "https://www.youtube.com/results?search_query={}",
                    url_encode(&format!("{} {}", item.artist, item.title))
                ),
            ),
            Action::open_url(
                "Search Spotify",
                &format!(
                    "https://open.spotify.com/search/{}",
                    url_encode(&format!("{} {}", item.artist, item.title))
                ),
            ),
            Action::event("Dismiss", "dismiss", &item.id),
        ],
    }
}

fn release_window() -> (String, String) {
    let today_days = unix_days();
    (date_from_days(today_days - 60), date_from_days(today_days + 30))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn unix_days() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}

fn date_from_days(days_since_epoch: i64) -> String {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!("{year:04}-{m:02}-{d:02}")
}

fn escape_query(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Debug, Serialize, Deserialize)]
struct ReleaseCache {
    last_refresh_at: i64,
    releases: Vec<ReleaseItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReleaseItem {
    id: String,
    title: String,
    artist: String,
    release_type: String,
    release_date: String,
    musicbrainz_url: String,
    artwork_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MbSearch {
    #[serde(default, rename = "release-groups")]
    release_groups: Vec<MbReleaseGroup>,
}

#[derive(Debug, Deserialize)]
struct MbReleaseGroup {
    id: String,
    title: String,
    #[serde(default, rename = "primary-type")]
    primary_type: Option<String>,
    #[serde(default, rename = "first-release-date")]
    first_release_date: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewDescriptor {
    schema_version: u32,
    title: String,
    subtitle: String,
    status: String,
    last_updated_at: i64,
    actions: Vec<Action>,
    sections: Vec<Section>,
    empty_title: String,
    empty_hint: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Section {
    title: String,
    items: Vec<ViewItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewItem {
    id: String,
    title: String,
    subtitle: String,
    detail: String,
    image_url: Option<String>,
    badges: Vec<String>,
    actions: Vec<Action>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Action {
    kind: String,
    label: String,
    event: Option<String>,
    payload: Option<String>,
    url: Option<String>,
}

impl Action {
    fn event(label: &str, event: &str, payload: &str) -> Self {
        Self {
            kind: "event".into(),
            label: label.into(),
            event: Some(event.into()),
            payload: Some(payload.into()),
            url: None,
        }
    }

    fn open_url(label: &str, url: &str) -> Self {
        Self {
            kind: "open-url".into(),
            label: label.into(),
            event: None,
            payload: None,
            url: Some(url.into()),
        }
    }
}
