//! Importing a playlist: from a file (M3U, CSV, JSON) or a public Deezer
//! playlist.
//!
//! Every source reads into the same entries (`waveflow_core::playlist_import`)
//! and goes through the same matcher; what the library holds lands in
//! `playlist_track`, the rest stays as placeholders that a later scan fills
//! in ([`fill_placeholders_after_scan`]). The playlist is created whole in
//! one transaction, so a failed import leaves nothing half-written.

use serde::Serialize;
use tauri::Emitter;

use waveflow_core::metadata::deezer::{DeezerClient, DeezerError};
use waveflow_core::playlist_import::deezer_link::{self, DeezerLink};
use waveflow_core::playlist_import::matching::{LibraryIndex, Wanted};
use waveflow_core::playlist_import::{
    csv, json, m3u, normalize_isrc, ImportEntry, ImportSource, ParseError, ParsedPlaylist,
    MAX_ENTRIES,
};
use waveflow_core::repository::playlist::PlaylistDraft;
use waveflow_core::repository::sqlite::playlist::insert_custom_conn;
use waveflow_core::repository::sqlite::playlist_placeholder::{
    self as placeholders, ImportCounts, Placeholder, ResolvedEntry,
};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

// The refusals the import dialog recognises and translates. Each error
// starts with one of these; what follows is detail for the log.
pub const ERR_UNSUPPORTED_FILE: &str = "playlist-import: unsupported file";
pub const ERR_UNREADABLE: &str = "playlist-import: unreadable";
pub const ERR_NO_TITLE_COLUMN: &str = "playlist-import: no title column";
pub const ERR_EMPTY: &str = "playlist-import: empty";
pub const ERR_NOT_A_LINK: &str = "playlist-import: not a deezer playlist link";
pub const ERR_OFFLINE: &str = "playlist-import: offline";
pub const ERR_NOT_FOUND: &str = "playlist-import: not found";
pub const ERR_UNREACHABLE: &str = "playlist-import: unreachable";

/// The largest file read as a playlist. Ten thousand entries of a rich
/// CSV export stay far below it; anything bigger is not a playlist, and
/// is refused before it is read into memory.
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// The event a playlist's contents changed under it on. The payload is the
/// playlist's id — the same event the smart-playlist commands emit.
pub const EVENT_PLAYLIST_CHANGED: &str = "playlist:changed";

/// What an import did.
#[derive(Debug, Clone, Serialize)]
pub struct ImportPlaylistResult {
    pub playlist_id: i64,
    pub name: String,
    /// Tracks the library holds, now in the playlist.
    pub imported: u32,
    /// Entries kept as placeholders.
    pub placeholders: u32,
    /// Entries naming a track the playlist already held.
    pub duplicates: u32,
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn parse_error(err: ParseError) -> AppError {
    let prefix = match err {
        ParseError::NoTitleColumn => ERR_NO_TITLE_COLUMN,
        ParseError::Empty => ERR_EMPTY,
        ParseError::Json(_) => ERR_UNREADABLE,
    };
    AppError::Other(format!("{prefix}: {err}"))
}

/// The entries of an M3U, each path resolved against the playlist's own
/// folder. An entry with no `#EXTINF` is named after its file, so it still
/// reads as something in the playlist view.
fn m3u_entries(text: &str, folder: &std::path::Path) -> ParsedPlaylist {
    let parsed = m3u::parse(text);
    let entries = parsed
        .entries
        .into_iter()
        .map(|entry| {
            let path = std::path::PathBuf::from(&entry.path);
            let resolved = if path.is_absolute() {
                path
            } else {
                folder.join(path)
            };
            // Named after its file, split on either separator: an M3U
            // written on another system keeps that system's.
            let title = entry.title.unwrap_or_else(|| {
                let name =
                    waveflow_core::playlist_import::file_name(&entry.path).unwrap_or(&entry.path);
                name.rsplit_once('.')
                    .map_or(name, |(stem, _)| stem)
                    .to_string()
            });
            ImportEntry {
                title,
                artist: entry.artist,
                album: None,
                duration_ms: entry.duration_ms,
                isrc: None,
                locator: Some(resolved.to_string_lossy().into_owned()),
            }
        })
        .collect();
    ParsedPlaylist {
        name: parsed.name,
        entries,
    }
}

/// Read the file at `path` into entries, by its extension.
fn read_file(path: &std::path::Path) -> AppResult<(ImportSource, ParsedPlaylist)> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let source = match extension.as_str() {
        "m3u" | "m3u8" => ImportSource::M3u,
        "csv" | "tsv" | "txt" => ImportSource::Csv,
        "json" => ImportSource::Json,
        other => return Err(AppError::Other(format!("{ERR_UNSUPPORTED_FILE}: .{other}"))),
    };
    // Bounded by the read itself, not by the reported length, which a
    // pipe or a growing file does not give honestly.
    let mut raw = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            use std::io::Read;
            file.take(MAX_FILE_BYTES + 1).read_to_end(&mut raw)
        })
        .map_err(|e| AppError::Other(format!("{ERR_UNREADABLE}: {e}")))?;
    if raw.len() as u64 > MAX_FILE_BYTES {
        return Err(AppError::Other(format!(
            "{ERR_UNREADABLE}: larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    // A `.m3u8` is UTF-8; a legacy `.m3u` — or a CSV out of a spreadsheet —
    // is in the code page of the machine that wrote it.
    let text = waveflow_core::text_encoding::decode(&raw);
    let parsed = match source {
        ImportSource::M3u => {
            let folder = path.parent().unwrap_or_else(|| std::path::Path::new(""));
            let parsed = m3u_entries(&text, folder);
            if parsed.entries.is_empty() {
                return Err(parse_error(ParseError::Empty));
            }
            parsed
        }
        ImportSource::Csv => csv::parse(&text).map_err(parse_error)?,
        ImportSource::Json => json::parse(&text).map_err(parse_error)?,
        ImportSource::Deezer => unreachable!("not a file source"),
    };
    Ok((source, parsed))
}

/// Match every entry against the library. Paths are compared only when an
/// entry names one: computing a track's comparable path touches the disk.
async fn resolve(
    pool: &sqlx::SqlitePool,
    entries: Vec<ImportEntry>,
) -> AppResult<Vec<ResolvedEntry>> {
    let mut candidates = placeholders::load_candidates(pool).await?;
    // Indexing a large library and matching a long playlist is CPU work:
    // off the runtime.
    tokio::task::spawn_blocking(move || {
        if entries.iter().any(|e| e.locator.is_some()) {
            placeholders::index_paths(&mut candidates);
        }
        let index = LibraryIndex::new(candidates);
        entries
            .into_iter()
            .map(|entry| {
                let keys = entry.locator.as_deref().map(placeholders::locator_keys);
                let track_id = index.find(&Wanted {
                    title: &entry.title,
                    artist: entry.artist.as_deref(),
                    duration_ms: entry.duration_ms,
                    isrc: entry.isrc.as_deref(),
                    path_key: keys.as_ref().map(|(key, _)| key.as_str()),
                    file_name: keys.as_ref().and_then(|(_, name)| name.as_deref()),
                });
                ResolvedEntry { entry, track_id }
            })
            .collect()
    })
    .await
    .map_err(|e| AppError::Other(format!("playlist import join: {e}")))
}

/// Create the playlist and write its entries, in one transaction.
async fn create(
    pool: &sqlx::SqlitePool,
    name: &str,
    source: ImportSource,
    resolved: &[ResolvedEntry],
) -> AppResult<(i64, ImportCounts)> {
    let now = now_millis();
    let draft = PlaylistDraft {
        name: name.to_string(),
        description: None,
        color_id: "violet".to_string(),
        color_mode: "auto".to_string(),
        icon_id: "music".to_string(),
        now_ms: now,
    };
    let mut tx = pool.begin().await?;
    let playlist_id = insert_custom_conn(&mut tx, &draft).await?;
    let counts = placeholders::write_entries(&mut tx, playlist_id, resolved, source, now).await?;
    tx.commit().await?;
    Ok((playlist_id, counts))
}

/// The shared tail of every import: match, create, cover, report.
async fn import_parsed(
    state: &AppState,
    source: ImportSource,
    fallback_name: String,
    parsed: ParsedPlaylist,
) -> AppResult<ImportPlaylistResult> {
    // One read for both: a profile switch between two would create the
    // playlist in one profile and draw its cover into another's folders.
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let name = parsed
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(fallback_name);
    let resolved = resolve(&pool, parsed.entries).await?;
    let (playlist_id, counts) = create(&pool, &name, source, &resolved).await?;
    tracing::info!(
        playlist_id,
        source = source.as_str(),
        imported = counts.imported,
        placeholders = counts.placeholders,
        duplicates = counts.duplicates,
        "playlist imported"
    );
    super::playlist_cover::maybe_regen_auto_cover(&pool, &state.paths, profile_id, playlist_id)
        .await;
    Ok(ImportPlaylistResult {
        playlist_id,
        name,
        imported: counts.imported,
        placeholders: counts.placeholders,
        duplicates: counts.duplicates,
    })
}

/// Import an `.m3u` / `.m3u8`, `.csv` or `.json` playlist file.
#[tauri::command]
pub async fn import_playlist_file(
    state: tauri::State<'_, AppState>,
    source_path: String,
) -> AppResult<ImportPlaylistResult> {
    let path = std::path::PathBuf::from(&source_path);
    let (source, parsed) = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || read_file(&path))
            .await
            .map_err(|e| AppError::Other(format!("playlist import join: {e}")))??
    };
    let fallback = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported playlist".to_string());
    import_parsed(&state, source, fallback, parsed).await
}

/// Follow a Deezer share link to the playlist it names — through Deezer's
/// hosts only, a handful of hops at most.
async fn follow_share_link(url: &str) -> AppResult<u64> {
    let client = reqwest::Client::builder()
        .user_agent("WaveFlow/0.1")
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if deezer_link::is_deezer_host(attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| AppError::Other(format!("http client build failed: {e}")))?;
    let response = client.get(url).send().await.map_err(|e| {
        AppError::Other(format!(
            "{ERR_UNREACHABLE}: {}",
            waveflow_core::error::describe_http_error(&e)
        ))
    })?;
    match deezer_link::parse(response.url().as_str()) {
        Some(DeezerLink::Playlist(id)) => Ok(id),
        _ => Err(AppError::Other(format!(
            "{ERR_NOT_A_LINK}: the share link does not lead to a playlist"
        ))),
    }
}

fn deezer_error(err: DeezerError) -> AppError {
    // Deezer's in-band refusal is a missing or private playlist — unless
    // it is the rate limiter, which means "try again in a moment".
    let prefix = match &err {
        DeezerError::Api(api) if !api.is_quota_exceeded() => ERR_NOT_FOUND,
        _ => ERR_UNREACHABLE,
    };
    AppError::Other(format!("{prefix}: {err}"))
}

/// Import a public Deezer playlist from its link (or its id). No account
/// is involved: the public API answers for a public playlist, and a
/// private one is refused like a missing one.
#[tauri::command]
pub async fn import_playlist_deezer(
    state: tauri::State<'_, AppState>,
    link: String,
) -> AppResult<ImportPlaylistResult> {
    if crate::offline::is_offline() {
        return Err(AppError::Other(format!(
            "{ERR_OFFLINE}: offline mode is enabled"
        )));
    }
    let id = match deezer_link::parse(&link) {
        Some(DeezerLink::Playlist(id)) => id,
        Some(DeezerLink::Share(url)) => follow_share_link(&url).await?,
        None => {
            return Err(AppError::Other(format!(
                "{ERR_NOT_A_LINK}: {}",
                link.trim()
            )))
        }
    };
    let client = DeezerClient::new();
    let playlist = client.get_playlist(id).await.map_err(deezer_error)?;
    // Page by page, paced: Deezer allows about 50 anonymous requests in 5
    // seconds, and a long playlist is a hundred pages.
    let mut tracks = Vec::new();
    loop {
        let (page, more) = client
            .get_playlist_tracks_page(id, tracks.len())
            .await
            .map_err(deezer_error)?;
        tracks.extend(page);
        if !more || tracks.len() >= MAX_ENTRIES {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    tracks.truncate(MAX_ENTRIES);
    let entries: Vec<ImportEntry> = tracks
        .into_iter()
        .filter(|t| !t.title.trim().is_empty())
        .map(|t| ImportEntry {
            duration_ms: t.duration_ms(),
            isrc: t.isrc.as_deref().and_then(normalize_isrc),
            artist: t.artist.map(|a| a.name).filter(|n| !n.trim().is_empty()),
            album: t
                .album
                .and_then(|a| a.title)
                .filter(|n| !n.trim().is_empty()),
            title: t.title,
            locator: None,
        })
        .collect();
    if entries.is_empty() {
        return Err(parse_error(ParseError::Empty));
    }
    let parsed = ParsedPlaylist {
        name: Some(playlist.title),
        entries,
    };
    import_parsed(&state, ImportSource::Deezer, format!("Deezer {id}"), parsed).await
}

/// The placeholders of a playlist, in its order — `position` in the
/// numbering of `playlist_track.position`, for the view to interleave them
/// with the tracks.
#[tauri::command]
pub async fn list_playlist_placeholders(
    state: tauri::State<'_, AppState>,
    playlist_id: i64,
) -> AppResult<Vec<Placeholder>> {
    let pool = state.require_profile_pool().await?;
    Ok(placeholders::list(&pool, playlist_id).await?)
}

/// Remove a placeholder the user no longer wants waiting.
#[tauri::command]
pub async fn remove_playlist_placeholder(
    state: tauri::State<'_, AppState>,
    placeholder_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let mut conn = pool.acquire().await?;
    placeholders::delete(&mut conn, placeholder_id).await?;
    Ok(())
}

/// A fingerprint of the tracks a folder makes available — how many, and
/// which. A scan that leaves it as it was, and adds or rewrites nothing,
/// brought no track a placeholder could be waiting for. `None` when it
/// cannot be read, which never equals a value read.
pub(crate) async fn available_in_folder(
    pool: &sqlx::SqlitePool,
    folder_id: i64,
) -> Option<(i64, i64)> {
    sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(id), 0) FROM track
          WHERE folder_id = ? AND is_available = 1",
    )
    .bind(folder_id)
    .fetch_one(pool)
    .await
    .ok()
}

/// Fill the placeholders the scan just brought tracks for, and say which
/// playlists changed. Never fails the scan: an index that indexed every
/// track correctly is not undone by a playlist that could not be updated.
pub(crate) async fn fill_placeholders_after_scan(
    pool: &sqlx::SqlitePool,
    app: Option<&tauri::AppHandle>,
) -> usize {
    match placeholders::fill_pending(pool, now_millis()).await {
        Ok(changed) => {
            if !changed.is_empty() {
                tracing::info!(playlists = changed.len(), "placeholders filled after scan");
            }
            if let Some(app) = app {
                for playlist_id in &changed {
                    let _ = app.emit(EVENT_PLAYLIST_CHANGED, *playlist_id);
                }
            }
            changed.len()
        }
        Err(err) => {
            tracing::warn!(?err, "filling playlist placeholders failed (non-fatal)");
            0
        }
    }
}

#[cfg(test)]
mod tests;
