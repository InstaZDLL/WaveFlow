//! Offline Web Radio catalogue — native counterpart to the `web-radio`
//! WASM plugin.
//!
//! The plugin queries radio-browser live over HTTP (gated by the manifest
//! allowlist + the offline short-circuit), so it can't browse offline. A
//! WASM guest also can't host SQLite (no sqlite in `wasm32`, and the ~20 MB
//! station dump dwarfs the 10 MB plugin scratch quota), so the offline
//! catalogue lives here as native commands backed by an app.db table +
//! FTS5 index.
//!
//! `download_radio_catalogue` snapshots the directory; `resolve_radio_catalogue`
//! answers the SAME opaque query tokens the plugin's `build_url` understands
//! (`top` / `trending` / `tag:<name>` / `country:<ISO2>` / free text) and
//! returns the SAME [`PluginTrack`] shape, so the frontend swaps one call for
//! the other without touching its category list or playback path (the stream
//! url rides inside the track id as `url:<stream>`; minting it is a pure
//! string strip, no network).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::{
    commands::plugins::PluginTrack,
    error::{AppError, AppResult},
    state::AppState,
};

/// radio-browser mirror. Matches the plugin's `MIRROR` so the offline
/// snapshot is drawn from the same federation node the live path uses.
const MIRROR: &str = "https://de1.api.radio-browser.info";
const USER_AGENT: &str = "WaveFlow/1.0";

/// Hard cap on the station dump download. The full `hidebroken` directory is
/// ~20-30 MB of JSON; 128 MiB is generous headroom while still guarding
/// against a hostile mirror streaming unbounded data into process memory.
const MAX_DUMP_BYTES: usize = 128 * 1024 * 1024;

/// Default + max result count per `resolve` call. The plugin pages at 50;
/// offline we can afford a little more without a network round-trip.
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;

const KEY_LAST_SYNCED: &str = "radio.catalogue.last_synced_at";
const KEY_LOCAL_FIRST: &str = "radio.catalogue.local_first";
/// User-pinned ISO 3166-1 alpha-2 country code for the "Local stations"
/// shortcut. When set, overrides the webview-locale detection so users on
/// an EN-US Windows who are not in the US get the right country after
/// picking it once from the country picker.
const KEY_PREFERRED_COUNTRY: &str = "radio.preferred_country";

/// Sorted list of ISO 3166-1 alpha-2 codes accepted by the Web Radio country
/// picker. Kept in sync with `src/lib/webRadioCountries.ts`. Using a sorted
/// slice + `binary_search` avoids any additional dependency (e.g. `phf`).
const SUPPORTED_COUNTRY_CODES: &[&str] = &[
    "AD", "AE", "AF", "AG", "AI", "AL", "AM", "AO", "AR", "AT", "AU", "AW", "AX", "AZ", "BA",
    "BB", "BD", "BE", "BF", "BG", "BH", "BI", "BJ", "BM", "BN", "BO", "BQ", "BR", "BS", "BT",
    "BW", "BY", "BZ", "CA", "CD", "CG", "CH", "CI", "CK", "CL", "CM", "CN", "CO", "CR", "CU",
    "CV", "CW", "CY", "CZ", "DE", "DJ", "DK", "DM", "DO", "DZ", "EC", "EE", "EG", "ER", "ES",
    "ET", "FI", "FJ", "FK", "FM", "FO", "FR", "GA", "GB", "GD", "GE", "GF", "GG", "GH", "GI",
    "GL", "GM", "GN", "GP", "GQ", "GR", "GT", "GU", "GW", "GY", "HK", "HN", "HR", "HT", "HU",
    "ID", "IE", "IL", "IM", "IN", "IQ", "IR", "IS", "IT", "JE", "JM", "JO", "JP", "KE", "KG",
    "KH", "KI", "KM", "KN", "KP", "KR", "KW", "KY", "KZ", "LA", "LB", "LC", "LI", "LK", "LR",
    "LS", "LT", "LU", "LV", "LY", "MA", "MC", "MD", "ME", "MG", "MH", "MK", "ML", "MM", "MN",
    "MO", "MP", "MQ", "MR", "MS", "MT", "MU", "MV", "MW", "MX", "MY", "MZ", "NA", "NC", "NE",
    "NG", "NI", "NL", "NO", "NP", "NR", "NU", "NZ", "OM", "PA", "PE", "PF", "PG", "PH", "PK",
    "PL", "PM", "PR", "PS", "PT", "PW", "PY", "QA", "RE", "RO", "RS", "RU", "RW", "SA", "SB",
    "SC", "SD", "SE", "SG", "SI", "SK", "SL", "SM", "SN", "SO", "SR", "SS", "ST", "SV", "SX",
    "SY", "SZ", "TC", "TD", "TG", "TH", "TJ", "TL", "TM", "TN", "TO", "TR", "TT", "TV", "TW",
    "TZ", "UA", "UG", "US", "UY", "UZ", "VA", "VC", "VE", "VG", "VI", "VN", "VU", "WS", "YE",
    "ZA", "ZM", "ZW",
];

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

// ── radio-browser row ───────────────────────────────────────────────

/// Subset of the radio-browser station JSON we persist. Every field is
/// tolerant of absence — the directory is community-edited and rows skip
/// optional keys freely.
#[derive(Debug, Deserialize)]
struct RbStation {
    #[serde(default)]
    stationuuid: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    url_resolved: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    favicon: Option<String>,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    country: String,
    #[serde(default)]
    countrycode: String,
    #[serde(default)]
    bitrate: Option<u32>,
    #[serde(default)]
    votes: i64,
}

/// Resolve the preferred playable URL (federation-verified `url_resolved`
/// when present + non-empty, else the raw author `url`). Mirrors the
/// plugin's `to_track` so offline + online rows play identically.
fn stream_url_of(s: &RbStation) -> Option<String> {
    let stream = s
        .url_resolved
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| s.url.clone());
    (!stream.is_empty()).then_some(stream)
}

// ── Status DTO ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioCatalogueStatus {
    /// Number of stations currently stored, 0 when never downloaded.
    pub count: i64,
    /// Epoch millis of the last successful download, or `None`.
    pub last_synced_at: Option<i64>,
    /// When true, the Web Radio view browses + searches the local
    /// catalogue first even while online.
    pub local_first: bool,
}

async fn read_setting(pool: &SqlitePool, key: &str) -> AppResult<Option<String>> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT value FROM app_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?,
    )
}

/// Upsert an app-setting row. Generic over the executor so it can run on the
/// pool directly OR inside an open transaction (the catalogue mutations fold
/// the `last_synced_at` write into the same tx as the data for atomicity).
async fn write_setting<'e, E>(executor: E, key: &str, value: &str) -> AppResult<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .bind(now_ms())
    .execute(executor)
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn radio_catalogue_status(
    state: tauri::State<'_, AppState>,
) -> AppResult<RadioCatalogueStatus> {
    let pool = &state.app_db;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM radio_station")
        .fetch_one(pool)
        .await?;
    let last_synced_at = read_setting(pool, KEY_LAST_SYNCED)
        .await?
        .and_then(|v| v.parse::<i64>().ok());
    let local_first = read_setting(pool, KEY_LOCAL_FIRST).await?.as_deref() == Some("true");
    Ok(RadioCatalogueStatus {
        count,
        last_synced_at,
        local_first,
    })
}
/// Returns the user-pinned ISO 3166-1 alpha-2 country code, or `None` when
/// the user has not picked a country yet and locale-detection should be used.
#[tauri::command]
pub async fn get_radio_preferred_country(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<String>> {
    Ok(read_setting(&state.app_db, KEY_PREFERRED_COUNTRY)
        .await?
        .filter(|s| !s.is_empty()))
}

/// Persist the user's chosen ISO 3166-1 alpha-2 country code so the "Local
/// stations" shortcut remembers it across sessions. An empty string clears
/// the preference (resets to locale detection).
#[tauri::command]
pub async fn set_radio_preferred_country(
    state: tauri::State<'_, AppState>,
    code: String,
) -> AppResult<()> {
    let code = code.trim().to_uppercase();
    if !code.is_empty() && SUPPORTED_COUNTRY_CODES.binary_search(&code.as_str()).is_err() {
        return Err(AppError::Other(
            "country code must be one of the supported ISO 3166-1 alpha-2 codes".into(),
        ));
    }
    write_setting(&state.app_db, KEY_PREFERRED_COUNTRY, &code).await
}

#[tauri::command]
pub async fn set_radio_catalogue_local_first(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    write_setting(
        &state.app_db,
        KEY_LOCAL_FIRST,
        if enabled { "true" } else { "false" },
    )
    .await
}

#[tauri::command]
pub async fn clear_radio_catalogue(state: tauri::State<'_, AppState>) -> AppResult<()> {
    let pool = &state.app_db;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM radio_station")
        .execute(&mut *tx)
        .await?;
    // Contentless FTS5: a bare DELETE isn't supported — the `'delete-all'`
    // command empties the index in one shot.
    sqlx::query("INSERT INTO radio_station_fts(radio_station_fts) VALUES('delete-all')")
        .execute(&mut *tx)
        .await?;
    // Clear the sync marker in the same tx so the row count and the
    // "last synced" state can't disagree on a partial failure.
    write_setting(&mut *tx, KEY_LAST_SYNCED, "").await?;
    tx.commit().await?;
    Ok(())
}

// ── Download ────────────────────────────────────────────────────────

/// Download the full radio-browser station directory and rebuild the local
/// catalogue. Returns the number of stations stored. Emits
/// `radio-catalogue:progress` `{ phase, current, total }` events so the
/// Settings card can render a progress bar (`phase`: `download` → `insert`).
#[tauri::command]
pub async fn download_radio_catalogue(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<i64> {
    // Downloading is inherently a network op — refuse up front when offline
    // mode is on rather than failing deep in the fetch.
    if crate::offline::is_offline() {
        return Err(AppError::Other(
            "offline mode is enabled — disable it to download the station catalogue".into(),
        ));
    }

    let _ = app.emit(
        "radio-catalogue:progress",
        serde_json::json!({ "phase": "download", "current": 0, "total": 0 }),
    );

    // `hidebroken=true` drops unreachable stations; `order=votes` puts the
    // popular ones first so the offline "Top stations" view matches the live
    // one. `limit` is generous — the directory is ~35k usable rows.
    let url = format!(
        "{MIRROR}/json/stations/search?hidebroken=true&order=votes&reverse=true&limit=100000"
    );
    let bytes = download_dump(&url).await?;

    let stations: Vec<RbStation> = serde_json::from_slice(&bytes)
        .map_err(|e| AppError::Other(format!("parse station dump: {e}")))?;
    let total = stations.len();

    // Commit in batches so the import doesn't hold one multi-second write
    // lock over the whole ~35k-row directory — the single-writer convention
    // big import paths (scanner, tag editor) follow. The old catalogue is
    // wiped at the head of the first batch; a mid-import failure leaves a
    // partial catalogue with no sync marker, which the next download
    // replaces wholesale.
    const BATCH: usize = 200;
    let pool = &state.app_db;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM radio_station")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO radio_station_fts(radio_station_fts) VALUES('delete-all')")
        .execute(&mut *tx)
        .await?;
    // Invalidate the sync marker up front: because the import commits in
    // batches, a mid-import failure would otherwise leave partial rows under
    // a *stale* `last_synced_at` (from a prior successful run), and
    // `resolve_radio_catalogue` would serve that incomplete catalogue. With
    // the marker cleared here and only re-stamped in the final tx, an
    // interrupted rebuild reads as "no valid catalogue" until it completes.
    write_setting(&mut *tx, KEY_LAST_SYNCED, "").await?;

    let mut id: i64 = 0;
    let mut in_batch: usize = 0;
    for (i, s) in stations.iter().enumerate() {
        let Some(stream) = stream_url_of(s) else {
            continue;
        };
        let name = s.name.trim();
        if name.is_empty() {
            continue;
        }
        id += 1;
        sqlx::query(
            "INSERT INTO radio_station
                (id, stationuuid, name, stream_url, homepage, favicon,
                 country, country_code, tags, bitrate, votes)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(&s.stationuuid)
        .bind(name)
        .bind(&stream)
        .bind(s.homepage.as_deref())
        .bind(s.favicon.as_deref().filter(|f| !f.is_empty()))
        .bind(&s.country)
        .bind(s.countrycode.to_uppercase())
        .bind(&s.tags)
        .bind(s.bitrate.map(|b| b as i64))
        .bind(s.votes)
        .execute(&mut *tx)
        .await?;
        // Keep the contentless FTS rowid aligned with the base row id.
        sqlx::query(
            "INSERT INTO radio_station_fts (rowid, name, tags, country) VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(&s.tags)
        .bind(&s.country)
        .execute(&mut *tx)
        .await?;

        in_batch += 1;
        if in_batch >= BATCH {
            tx.commit().await?;
            tx = pool.begin().await?;
            in_batch = 0;
        }

        // Throttle progress emits — one per ~2000 rows keeps the event
        // channel quiet while still animating the bar.
        if i % 2000 == 0 {
            let _ = app.emit(
                "radio-catalogue:progress",
                serde_json::json!({ "phase": "insert", "current": i, "total": total }),
            );
        }
    }

    // Stamp the sync marker inside the final tx so the data and its
    // "last synced" timestamp commit atomically together.
    write_setting(&mut *tx, KEY_LAST_SYNCED, &now_ms().to_string()).await?;
    tx.commit().await?;

    let _ = app.emit(
        "radio-catalogue:progress",
        serde_json::json!({ "phase": "insert", "current": total, "total": total }),
    );
    Ok(id)
}

/// Streamed download with a hard size cap. radio-browser doesn't always send
/// a `Content-Length` for the big dump, so we pull chunk-by-chunk and bail if
/// it overshoots `MAX_DUMP_BYTES`.
async fn download_dump(url: &str) -> AppResult<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(90))
        .build()
        .map_err(|e| AppError::Other(format!("http client build: {e}")))?;
    let mut resp = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| AppError::Other(format!("catalogue download failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Other(format!(
            "catalogue download status {}",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_DUMP_BYTES {
            return Err(AppError::Other(format!(
                "station dump too large ({len} bytes, max {MAX_DUMP_BYTES})"
            )));
        }
    }
    let mut bytes = Vec::with_capacity(8 * 1024 * 1024);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| AppError::Other(format!("catalogue read failed: {e}")))?
    {
        if bytes.len() + chunk.len() > MAX_DUMP_BYTES {
            return Err(AppError::Other(format!(
                "station dump exceeds max size ({MAX_DUMP_BYTES} bytes)"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

// ── Resolve (offline counterpart to the plugin's resolve) ───────────

/// Answer one of the plugin's opaque query tokens against the local
/// catalogue. `limit` defaults to [`DEFAULT_LIMIT`], clamped to [`MAX_LIMIT`].
#[tauri::command]
pub async fn resolve_radio_catalogue(
    state: tauri::State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> AppResult<Vec<PluginTrack>> {
    let pool = &state.app_db;

    // Only serve a catalogue that finished syncing. The marker is cleared at
    // the head of a download and re-stamped in the final batch's tx, so an
    // absent / empty / unparseable value means "never synced" or "rebuild in
    // progress / interrupted" — return nothing rather than partial rows.
    let synced = read_setting(pool, KEY_LAST_SYNCED)
        .await?
        .and_then(|v| v.parse::<i64>().ok())
        .is_some();
    if !synced {
        return Ok(Vec::new());
    }

    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as i64;
    let q = query.trim();

    let rows: Vec<StationRow> = if q.is_empty() || q == "top" || q == "trending" {
        // No offline notion of "last changed", so trending falls back to the
        // popularity ranking — same first-screen impression as the live view.
        sqlx::query_as(
            "SELECT stream_url, name, country, tags, favicon, bitrate
               FROM radio_station ORDER BY votes DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else if let Some(code) = q.strip_prefix("country:") {
        let code = code.trim().to_uppercase();
        if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(AppError::Other(
                "country code must be ISO 3166-1 alpha-2".into(),
            ));
        }
        sqlx::query_as(
            "SELECT stream_url, name, country, tags, favicon, bitrate
               FROM radio_station WHERE country_code = ? ORDER BY votes DESC LIMIT ?",
        )
        .bind(code)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else if let Some(tag) = q.strip_prefix("tag:") {
        // Substring match on the comma-joined tag list. Broader than the
        // live `bytag` exact match, but fine for offline browsing.
        let tag = tag.trim().to_lowercase();
        sqlx::query_as(
            "SELECT stream_url, name, country, tags, favicon, bitrate
               FROM radio_station WHERE lower(tags) LIKE '%' || ? || '%'
              ORDER BY votes DESC LIMIT ?",
        )
        .bind(tag)
        .bind(limit)
        .fetch_all(pool)
        .await?
    } else {
        // Free-text search via FTS5 over name + tags + country.
        let Some(fts) = fts_query(q) else {
            return Ok(Vec::new());
        };
        sqlx::query_as(
            "SELECT s.stream_url, s.name, s.country, s.tags, s.favicon, s.bitrate
               FROM radio_station_fts f JOIN radio_station s ON s.id = f.rowid
              WHERE radio_station_fts MATCH ? ORDER BY s.votes DESC LIMIT ?",
        )
        .bind(fts)
        .bind(limit)
        .fetch_all(pool)
        .await?
    };

    Ok(rows.into_iter().map(StationRow::into_track).collect())
}

#[derive(sqlx::FromRow)]
struct StationRow {
    stream_url: String,
    name: String,
    country: String,
    tags: String,
    favicon: Option<String>,
    bitrate: Option<i64>,
}

impl StationRow {
    /// Mirror the plugin's `to_track`: id carries the stream url, country is
    /// the pseudo-artist, the first tag is the pseudo-album, live = 0 ms.
    fn into_track(self) -> PluginTrack {
        let first_tag = self.tags.split(',').next().unwrap_or("").trim();
        let album = (!first_tag.is_empty()).then(|| capitalise(first_tag));
        let artist = if self.country.is_empty() {
            "Internet Radio".to_string()
        } else {
            self.country
        };
        let icy_url = match self.bitrate {
            Some(b) if b > 0 && !is_segmented(&self.stream_url) => Some(self.stream_url.clone()),
            _ => None,
        };
        PluginTrack {
            id: format!("url:{}", self.stream_url),
            title: self.name.trim().to_string(),
            artist,
            album,
            duration_ms: 0,
            artwork_url: self.favicon.filter(|f| !f.is_empty()),
            icy_url,
        }
    }
}

/// HLS / DASH manifests don't carry ICY metadata — skip the poll hint
/// (matches the plugin's `bitrate_icy_hint`).
fn is_segmented(url: &str) -> bool {
    url.ends_with(".m3u8") || url.ends_with(".mpd")
}

/// Title-case a tag for display ("jazz" → "Jazz"). ASCII-first, same as the
/// plugin's `capitalise`.
fn capitalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut next_upper = true;
    for ch in s.chars() {
        if ch.is_whitespace() || ch == '-' {
            next_upper = true;
            out.push(ch);
        } else if next_upper {
            out.extend(ch.to_uppercase());
            next_upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build a safe FTS5 MATCH expression from raw user input. Each
/// whitespace-separated token is reduced to its alphanumeric core, wrapped in
/// double quotes (so FTS operators in the input can't break the query), and
/// suffixed with `*` for prefix matching. Returns `None` when nothing usable
/// survives (so the caller can short-circuit to an empty result instead of
/// issuing a syntactically-invalid MATCH).
fn fts_query(input: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in input.split_whitespace() {
        let cleaned: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>();
        if !cleaned.is_empty() {
            terms.push(format!("\"{cleaned}\"*"));
        }
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}
