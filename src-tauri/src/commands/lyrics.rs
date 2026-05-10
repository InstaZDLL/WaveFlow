//! Lyrics fetch + cache.
//!
//! Lazy three-tier lookup, in order:
//!   1. Local DB cache (`lyrics` table, keyed by `track_id`)
//!   2. Embedded `USLT` / lyrics tag inside the audio file (via lofty)
//!   3. LRCLIB public API (matched by artist + track + album + duration)
//!
//! Whichever tier hits first becomes the cached entry. We never refetch
//! once a row exists — the user can manually overwrite by importing a
//! `.lrc` file via [`import_lrc_file`].

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use lofty::file::{FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    lrclib::LrclibClient,
    state::AppState,
};

/// Guards against two concurrent prefetch runs and exposes a
/// cancellation flag the user can flip from the UI. Module-local — the
/// prefetch is a single global operation, so a bare `AtomicBool` pair
/// is enough; no need to thread a token through `AppState`.
static PREFETCH_RUNNING: AtomicBool = AtomicBool::new(false);
static PREFETCH_CANCEL: AtomicBool = AtomicBool::new(false);

/// LRCLIB throttle — be a polite guest on the public instance. 500 ms
/// per call ≈ 2 req/s, which clears a 10k-track library in ~1h30 even
/// when every track misses the embedded tag and goes to the network.
const LRCLIB_THROTTLE: Duration = Duration::from_millis(500);

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Format flags returned to the frontend.
///
/// `Plain` = unsynced text. `Lrc` = `[mm:ss.xx]`-prefixed lines.
/// `EnhancedLrc` is the per-word timed variant; we accept it from
/// imports but don't currently produce it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsFormat {
    Plain,
    Lrc,
    EnhancedLrc,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSource {
    Embedded,
    LrcFile,
    Api,
    Manual,
}

#[derive(Debug, Clone, Serialize)]
pub struct LyricsPayload {
    pub track_id: i64,
    pub content: String,
    pub format: LyricsFormat,
    pub source: LyricsSource,
}

fn parse_format(s: &str) -> LyricsFormat {
    match s {
        "lrc" => LyricsFormat::Lrc,
        "enhanced_lrc" => LyricsFormat::EnhancedLrc,
        _ => LyricsFormat::Plain,
    }
}

fn parse_source(s: &str) -> LyricsSource {
    match s {
        "lrc_file" => LyricsSource::LrcFile,
        "api" => LyricsSource::Api,
        "manual" => LyricsSource::Manual,
        _ => LyricsSource::Embedded,
    }
}

/// Heuristic: any line starting with `[mm:ss` (zero-padded or not) is
/// treated as LRC. We don't try to detect enhanced LRC from text — if
/// you imported `.lrc` from a "enhanced" source, pass the format
/// explicitly via [`import_lrc_file`].
fn detect_format(content: &str) -> LyricsFormat {
    let has_timestamp = content.lines().take(20).any(|line| {
        let line = line.trim_start();
        line.starts_with('[')
            && line.len() >= 7
            && line[1..].chars().take(2).all(|c| c.is_ascii_digit())
            && line.as_bytes().get(3) == Some(&b':')
    });
    if has_timestamp {
        LyricsFormat::Lrc
    } else {
        LyricsFormat::Plain
    }
}

fn format_to_db(fmt: &LyricsFormat) -> &'static str {
    match fmt {
        LyricsFormat::Plain => "plain",
        LyricsFormat::Lrc => "lrc",
        LyricsFormat::EnhancedLrc => "enhanced_lrc",
    }
}

fn source_to_db(src: &LyricsSource) -> &'static str {
    match src {
        LyricsSource::Embedded => "embedded",
        LyricsSource::LrcFile => "lrc_file",
        LyricsSource::Api => "api",
        LyricsSource::Manual => "manual",
    }
}

/// Re-open an MP3 as a typed `Id3v2Tag` and pull the lyrics out of any
/// TXXX user-defined frame whose description matches one of the common
/// lyric aliases (`LYRICS`, `UNSYNCEDLYRICS`, `LYRICS_UNSYNCED`, ...).
///
/// Required because the generic `Tag` interface returned by
/// `read_from_path` doesn't expose unmapped TXXX frames.
fn read_id3v2_txxx_lyrics(path: &Path) -> Option<String> {
    use lofty::config::ParseOptions;
    use lofty::id3::v2::Id3v2Tag;
    use lofty::mpeg::MpegFile;

    let mut file = std::fs::File::open(path).ok()?;
    let mpeg =
        <MpegFile as lofty::file::AudioFile>::read_from(&mut file, ParseOptions::new()).ok()?;
    let tag: &Id3v2Tag = mpeg.id3v2()?;

    const ALIASES: &[&str] = &[
        "UNSYNCEDLYRICS",
        "UNSYNCED LYRICS",
        "UNSYNCED_LYRICS",
        "LYRICS_UNSYNCED",
        "LYRICS",
    ];
    for alias in ALIASES {
        if let Some(s) = tag.get_user_text(alias) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Read the embedded lyrics tag. Lookup order:
///   1. `ItemKey::UnsyncLyrics` — `USLT` (ID3v2), `UNSYNCEDLYRICS`
///      (Vorbis), `©lyr` (MP4)
///   2. `ItemKey::Lyrics` — `LYRICS` (Vorbis), `©lyr` (MP4). Not
///      supported by ID3v2 in lofty.
///   3. ID3v2 TXXX user-defined frames named `LYRICS` or
///      `UNSYNCEDLYRICS` (legacy Mp3tag / foobar2000 / lame --tg
///      output common on K-Pop / J-Pop rips).
///   4. Generic `Description` field as last resort.
fn read_embedded_lyrics(path: &Path) -> Option<String> {
    let probe = Probe::open(path).ok()?.guess_file_type().ok()?;
    let file_type = probe.file_type();
    let tagged = probe.read().ok()?;

    let from_known_key = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(|tag| {
            tag.get_string(ItemKey::UnsyncLyrics)
                .or_else(|| tag.get_string(ItemKey::Lyrics))
                .map(|s| s.to_string())
        });

    // Generic Tag wraps the underlying Id3v2Tag for MP3s, but the
    // SplitAndMergeTag conversion drops unknown TXXX frames. Re-read
    // the file as Id3v2Tag specifically when the standard frames came
    // up empty so we can scan for `TXXX:LYRICS` / `TXXX:UNSYNCEDLYRICS`.
    let from_id3v2_txxx = if file_type == Some(FileType::Mpeg) && from_known_key.is_none() {
        read_id3v2_txxx_lyrics(path)
    } else {
        None
    };

    let from_description = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(|tag| {
            #[allow(deprecated)]
            tag.get_string(ItemKey::Description)
                .filter(|s| s.lines().count() > 3)
                .map(|s| s.to_string())
        });

    let raw = from_known_key.or(from_id3v2_txxx).or(from_description)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Insert (or replace) the lyrics row, keyed by file content hash so the
/// cache is shared across profiles that contain the same audio file.
async fn upsert_lyrics(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app.lyrics (file_hash, content, format, source, language, fetched_at)
         VALUES (?, ?, ?, ?, NULL, ?)
         ON CONFLICT(file_hash) DO UPDATE SET
            content = excluded.content,
            format = excluded.format,
            source = excluded.source,
            fetched_at = excluded.fetched_at",
    )
    .bind(file_hash)
    .bind(content)
    .bind(format_to_db(format))
    .bind(source_to_db(source))
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

/// Read the cached lyrics row, if any. The frontend identifies tracks by
/// numeric `track_id` so we look up the file hash first, then key into the
/// shared `app.lyrics` cache.
async fn read_cached(pool: &sqlx::SqlitePool, track_id: i64) -> AppResult<Option<LyricsPayload>> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT l.content, l.format, l.source
           FROM track t
           JOIN app.lyrics l ON l.file_hash = t.file_hash
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(content, fmt, src)| LyricsPayload {
        track_id,
        content,
        format: parse_format(&fmt),
        source: parse_source(&src),
    }))
}

/// Look up the track's metadata needed to call LRCLIB and to read the
/// embedded tag.
async fn read_track_meta(pool: &sqlx::SqlitePool, track_id: i64) -> AppResult<Option<TrackMeta>> {
    let row: Option<(String, String, String, Option<String>, Option<String>, i64)> =
        sqlx::query_as(
            "SELECT t.file_path, t.file_hash, t.title,
                    ar.name AS artist_name,
                    al.title AS album_title,
                    t.duration_ms
               FROM track t
               LEFT JOIN artist ar ON ar.id = t.primary_artist
               LEFT JOIN album  al ON al.id = t.album_id
              WHERE t.id = ?",
        )
        .bind(track_id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(
        |(file_path, file_hash, title, artist_name, album_title, duration_ms)| TrackMeta {
            file_path,
            file_hash,
            title,
            artist_name,
            album_title,
            duration_ms,
        },
    ))
}

struct TrackMeta {
    file_path: String,
    file_hash: String,
    title: String,
    artist_name: Option<String>,
    album_title: Option<String>,
    duration_ms: i64,
}

// ── Tauri commands ───────────────────────────────────────────────────

/// Cache-only lookup. Returns `None` if the track has no cached
/// lyrics — the frontend then calls [`fetch_lyrics`] explicitly.
#[tauri::command]
pub async fn get_lyrics(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let pool = state.require_profile_pool().await?;
    read_cached(&pool, track_id).await
}

/// Three-tier lookup: cache → embedded tag → LRCLIB. Caches the first
/// hit and returns it. Returns `None` if every tier failed.
#[tauri::command]
pub async fn fetch_lyrics(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let pool = state.require_profile_pool().await?;

    // 1. Cache.
    if let Some(cached) = read_cached(&pool, track_id).await? {
        return Ok(Some(cached));
    }

    // 2. Embedded tag. Lofty I/O is blocking — push to spawn_blocking.
    let meta = match read_track_meta(&pool, track_id).await? {
        Some(m) => m,
        None => return Ok(None),
    };

    let path_clone = meta.file_path.clone();
    let embedded =
        tokio::task::spawn_blocking(move || read_embedded_lyrics(Path::new(&path_clone)))
            .await
            .ok()
            .flatten();

    if let Some(content) = embedded {
        let format = detect_format(&content);
        let source = LyricsSource::Embedded;
        upsert_lyrics(&pool, &meta.file_hash, &content, &format, &source).await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format,
            source,
        }));
    }

    // 3. LRCLIB fallback. Skip if we have no artist (matching is
    //    useless without one).
    let Some(artist_name) = meta.artist_name.as_deref() else {
        return Ok(None);
    };
    let primary_artist = artist_name.split(", ").next().unwrap_or(artist_name);
    let duration_seconds = (meta.duration_ms.max(0) as u64).div_ceil(1000);
    let client = LrclibClient::new();
    let resp = match client
        .get(
            primary_artist,
            &meta.title,
            meta.album_title.as_deref(),
            duration_seconds,
        )
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            // LRCLIB 404 — track unknown to the database. Cache as an
            // empty row so we don't re-hit the network on every panel
            // open. The user can force a re-search by clicking
            // "Refetch" in the lyrics panel (clears the row, re-runs
            // the waterfall) when LRCLIB might have added the track.
            let empty = String::new();
            upsert_lyrics(
                &pool,
                &meta.file_hash,
                &empty,
                &LyricsFormat::Plain,
                &LyricsSource::Api,
            )
            .await?;
            return Ok(Some(LyricsPayload {
                track_id,
                content: empty,
                format: LyricsFormat::Plain,
                source: LyricsSource::Api,
            }));
        }
        Err(err) => {
            // Surface transient network failures (timeout, DNS, refused
            // connection…) as an error so the UI can prompt the user to
            // retry — silently returning None made it look like LRCLIB
            // didn't have the track when in reality the request never
            // completed. A real 404 is already mapped to Ok(None) above.
            tracing::warn!(?err, "LRCLIB fetch failed");
            return Err(AppError::Other(format!("LRCLIB request failed: {err}")));
        }
    };

    if resp.instrumental == Some(true) {
        // Instrumental: cache an empty plain entry so we don't refetch.
        let empty = String::new();
        upsert_lyrics(
            &pool,
            &meta.file_hash,
            &empty,
            &LyricsFormat::Plain,
            &LyricsSource::Api,
        )
        .await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content: empty,
            format: LyricsFormat::Plain,
            source: LyricsSource::Api,
        }));
    }

    // Prefer synced lyrics when available — the UI can fall back to
    // plain rendering if it can't parse them. A row with neither
    // synced nor plain content is treated like a 404 and cached as
    // empty (same "no re-fetch on every visit" reasoning).
    let (content, format) = match (resp.synced_lyrics, resp.plain_lyrics) {
        (Some(s), _) if !s.trim().is_empty() => (s, LyricsFormat::Lrc),
        (_, Some(p)) if !p.trim().is_empty() => (p, LyricsFormat::Plain),
        _ => {
            let empty = String::new();
            upsert_lyrics(
                &pool,
                &meta.file_hash,
                &empty,
                &LyricsFormat::Plain,
                &LyricsSource::Api,
            )
            .await?;
            return Ok(Some(LyricsPayload {
                track_id,
                content: empty,
                format: LyricsFormat::Plain,
                source: LyricsSource::Api,
            }));
        }
    };

    let source = LyricsSource::Api;
    upsert_lyrics(&pool, &meta.file_hash, &content, &format, &source).await?;
    Ok(Some(LyricsPayload {
        track_id,
        content,
        format,
        source,
    }))
}

/// Read a `.lrc` (or any text) file from disk and store it as the
/// track's lyrics, replacing whatever was cached. Format is detected
/// heuristically (`[mm:ss…]` → LRC, else plain).
#[tauri::command]
pub async fn import_lrc_file(
    state: tauri::State<'_, AppState>,
    track_id: i64,
    file_path: String,
) -> AppResult<LyricsPayload> {
    let pool = state.require_profile_pool().await?;
    let file_hash: String = sqlx::query_scalar("SELECT file_hash FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&pool)
        .await?
        .ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let path = file_path.clone();
    let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path))
        .await
        .map_err(|e| AppError::Other(format!("lyrics file read panicked: {e}")))?
        .map_err(|e| AppError::Other(format!("read {file_path}: {e}")))?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(AppError::Other("imported lyrics file is empty".into()));
    }
    let format = detect_format(trimmed);
    let source = LyricsSource::LrcFile;
    upsert_lyrics(&pool, &file_hash, trimmed, &format, &source).await?;
    Ok(LyricsPayload {
        track_id,
        content: trimmed.to_string(),
        format,
        source,
    })
}

// ── Library-wide prefetch ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct LyricsPrefetchProgress {
    pub processed: u32,
    pub total: u32,
    pub hits: u32,
    pub misses: u32,
    pub failed: u32,
    pub current_title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LyricsPrefetchSummary {
    pub processed: u32,
    pub hits: u32,
    pub misses: u32,
    pub failed: u32,
    pub cancelled: bool,
}

/// Walk every available track that doesn't have a cached lyric and try
/// to populate the cache (embedded tag → LRCLIB). Throttles network
/// calls at ~2 req/s. Cancellable via [`cancel_lyrics_prefetch`].
///
/// Idempotent: the `WHERE l.file_hash IS NULL` filter skips anything
/// already cached, so re-running after a partial cancel just resumes.
/// Tracks sharing a `file_hash` are deduped via `GROUP BY` because the
/// cache is keyed on hash, not track id.
#[tauri::command]
pub async fn prefetch_library_lyrics(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> AppResult<LyricsPrefetchSummary> {
    if PREFETCH_RUNNING.swap(true, Ordering::SeqCst) {
        return Err(AppError::Other("lyrics prefetch already running".into()));
    }
    PREFETCH_CANCEL.store(false, Ordering::SeqCst);

    // Wrap the body so we always clear the running flag, even on early
    // return / error.
    let result = run_prefetch(&app, &state).await;
    PREFETCH_RUNNING.store(false, Ordering::SeqCst);
    PREFETCH_CANCEL.store(false, Ordering::SeqCst);
    result
}

async fn run_prefetch(
    app: &AppHandle,
    state: &tauri::State<'_, AppState>,
) -> AppResult<LyricsPrefetchSummary> {
    let pool = state.require_profile_pool().await?;

    // Pending = available tracks without a cached lyric row, deduped by
    // `file_hash` (the cache key). We pick the lowest `track.id` per
    // hash to get a stable representative.
    let pending: Vec<(
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
    )> = sqlx::query_as(
        "SELECT t.id, t.file_path, t.file_hash, t.title,
                    ar.name AS artist_name,
                    al.title AS album_title,
                    t.duration_ms
               FROM track t
               LEFT JOIN artist ar ON ar.id = t.primary_artist
               LEFT JOIN album  al ON al.id = t.album_id
               LEFT JOIN app.lyrics l ON l.file_hash = t.file_hash
              WHERE t.is_available = 1
                AND l.file_hash IS NULL
              GROUP BY t.file_hash
              ORDER BY t.id",
    )
    .fetch_all(&pool)
    .await?;

    let total = pending.len() as u32;
    let mut processed = 0u32;
    let mut hits = 0u32;
    let mut misses = 0u32;
    let mut failed = 0u32;

    let client = LrclibClient::new();
    let mut cancelled = false;

    for (track_id, file_path, file_hash, title, artist_name, album_title, duration_ms) in pending {
        if PREFETCH_CANCEL.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }

        let _ = app.emit(
            "lyrics:prefetch-progress",
            LyricsPrefetchProgress {
                processed,
                total,
                hits,
                misses,
                failed,
                current_title: Some(title.clone()),
            },
        );

        // 1. Embedded tag (free, no network).
        let path_clone = file_path.clone();
        let embedded =
            tokio::task::spawn_blocking(move || read_embedded_lyrics(Path::new(&path_clone)))
                .await
                .ok()
                .flatten();

        if let Some(content) = embedded {
            let format = detect_format(&content);
            let source = LyricsSource::Embedded;
            if let Err(e) = upsert_lyrics(&pool, &file_hash, &content, &format, &source).await {
                tracing::warn!(track_id, ?e, "persist embedded lyrics failed");
                failed += 1;
            } else {
                hits += 1;
            }
            processed += 1;
            continue;
        }

        // 2. LRCLIB. Skip if metadata is too thin to match.
        let Some(artist) = artist_name.as_deref() else {
            misses += 1;
            processed += 1;
            continue;
        };
        let primary_artist = artist.split(", ").next().unwrap_or(artist);
        let duration_seconds = (duration_ms.max(0) as u64).div_ceil(1000);

        match client
            .get(
                primary_artist,
                &title,
                album_title.as_deref(),
                duration_seconds,
            )
            .await
        {
            Ok(Some(resp)) => {
                if resp.instrumental == Some(true) {
                    let _ = upsert_lyrics(
                        &pool,
                        &file_hash,
                        "",
                        &LyricsFormat::Plain,
                        &LyricsSource::Api,
                    )
                    .await;
                    hits += 1;
                } else {
                    let pick = match (resp.synced_lyrics, resp.plain_lyrics) {
                        (Some(s), _) if !s.trim().is_empty() => Some((s, LyricsFormat::Lrc)),
                        (_, Some(p)) if !p.trim().is_empty() => Some((p, LyricsFormat::Plain)),
                        _ => None,
                    };
                    if let Some((content, format)) = pick {
                        if let Err(e) =
                            upsert_lyrics(&pool, &file_hash, &content, &format, &LyricsSource::Api)
                                .await
                        {
                            tracing::warn!(track_id, ?e, "persist LRCLIB lyrics failed");
                            failed += 1;
                        } else {
                            hits += 1;
                        }
                    } else {
                        // Row exists but neither synced nor plain
                        // lyrics — treat like a 404 and cache empty.
                        let _ = upsert_lyrics(
                            &pool,
                            &file_hash,
                            "",
                            &LyricsFormat::Plain,
                            &LyricsSource::Api,
                        )
                        .await;
                        misses += 1;
                    }
                }
            }
            Ok(None) => {
                // LRCLIB 404. Cache as empty so re-runs of the
                // prefetch and re-opens of the lyrics panel skip this
                // track. User can force a re-search per-track via the
                // "Refetch" button in the lyrics panel.
                let _ = upsert_lyrics(
                    &pool,
                    &file_hash,
                    "",
                    &LyricsFormat::Plain,
                    &LyricsSource::Api,
                )
                .await;
                misses += 1;
            }
            Err(err) => {
                tracing::warn!(track_id, ?err, "LRCLIB prefetch failed");
                failed += 1;
            }
        }

        processed += 1;
        // Throttle only after a network call; embedded hits skipped above.
        tokio::time::sleep(LRCLIB_THROTTLE).await;
    }

    let summary = LyricsPrefetchSummary {
        processed,
        hits,
        misses,
        failed,
        cancelled,
    };
    let _ = app.emit(
        "lyrics:prefetch-progress",
        LyricsPrefetchProgress {
            processed,
            total,
            hits,
            misses,
            failed,
            current_title: None,
        },
    );
    Ok(summary)
}

/// Flip the cancel flag. The running prefetch picks it up on the next
/// loop iteration. Returns `true` when a prefetch was actually running
/// at the time of the call.
#[tauri::command]
pub fn cancel_lyrics_prefetch() -> bool {
    if PREFETCH_RUNNING.load(Ordering::Relaxed) {
        PREFETCH_CANCEL.store(true, Ordering::SeqCst);
        true
    } else {
        false
    }
}

// ── User-edited lyrics ──────────────────────────────────────────────

/// Format hint coming from the in-app editor. The frontend always
/// passes "plain" or "lrc" — the backend re-runs `detect_format` on
/// the content as a safety net so a mistyped header still ends up in
/// the right bucket.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSaveFormat {
    Plain,
    Lrc,
}

#[derive(Debug, Deserialize)]
pub struct SaveLyricsPayload {
    pub content: String,
    pub format: LyricsSaveFormat,
    /// When `true`, also write the lyrics back into the audio file's
    /// USLT/LYRICS frame. When `false`, only the DB cache is updated
    /// (fastest, no file lock dance, no rescan churn).
    #[serde(default)]
    pub write_to_file: bool,
}

/// Persist user-edited lyrics for a track. Always upserts the cache
/// row with `source = manual`; optionally writes the same content into
/// the audio file's embedded lyrics frame so other players (and a
/// future re-scan) see the same text. File writes follow the same
/// pause-if-current pattern as the tag editor on Windows.
#[tauri::command]
pub async fn save_lyrics(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    app: AppHandle,
    track_id: i64,
    payload: SaveLyricsPayload,
) -> AppResult<LyricsPayload> {
    let pool = state.require_profile_pool().await?;

    // Pull file_path + file_hash up front. We need the path even when
    // write_to_file is false because a file write would otherwise
    // change the hash, and the cache is keyed on hash — better to
    // fail fast on a missing track than mid-write.
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT file_path, file_hash FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&pool)
            .await?;
    let (file_path, mut file_hash) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let trimmed = payload
        .content
        .trim_end_matches(['\n', '\r', ' '])
        .to_string();
    // Re-detect from content so a "plain" payload with [mm:ss] stamps
    // is correctly stored as lrc, and vice versa. The frontend hint is
    // the user's intent, but content is the source of truth.
    let detected = detect_format(&trimmed);
    let format = match (&payload.format, &detected) {
        // Trust the user when they picked Plain even if their text
        // happens to start with [...]; otherwise pick whichever of
        // Lrc / EnhancedLrc the parser identified.
        (LyricsSaveFormat::Plain, LyricsFormat::Plain) => LyricsFormat::Plain,
        (LyricsSaveFormat::Plain, _) => LyricsFormat::Plain,
        (LyricsSaveFormat::Lrc, _) => detected,
    };

    if payload.write_to_file {
        let active = engine
            .shared()
            .current_track_id
            .load(std::sync::atomic::Ordering::Acquire);
        if active == track_id {
            let _ = engine.send(crate::audio::AudioCmd::Pause);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let path = std::path::PathBuf::from(&file_path);
        let content_for_write = trimmed.clone();
        tokio::task::spawn_blocking(move || write_lyrics_to_file(&path, &content_for_write))
            .await
            .map_err(|e| AppError::Other(format!("lyrics write panicked: {e}")))?
            .map_err(|e| AppError::Other(format!("lyrics tag write failed: {e}")))?;

        // The file changed — recompute its blake3 hash so the cache
        // row stays addressable. We update the track row + the lyrics
        // row in the same transaction below.
        let path_for_hash = file_path.clone();
        let new_hash = tokio::task::spawn_blocking(move || hash_file_blake3(&path_for_hash))
            .await
            .map_err(|e| AppError::Other(format!("rehash panicked: {e}")))??;

        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE track SET file_hash = ? WHERE id = ?")
            .bind(&new_hash)
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        // Drop any cache row keyed on the old hash so we don't end up
        // with a stale embedded payload pointing at the previous
        // content.
        sqlx::query("DELETE FROM app.lyrics WHERE file_hash = ?")
            .bind(&file_hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        file_hash = new_hash;
    }

    let source = LyricsSource::Manual;
    upsert_lyrics(&pool, &file_hash, &trimmed, &format, &source).await?;

    let _ = app.emit("lyrics:updated", track_id);
    Ok(LyricsPayload {
        track_id,
        content: trimmed,
        format,
        source,
    })
}

fn hash_file_blake3(path: &str) -> AppResult<String> {
    let bytes = std::fs::read(path).map_err(|e| AppError::Other(format!("read for hash: {e}")))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Write the unsynchronized lyrics back into the audio file. Uses
/// `ItemKey::UnsyncLyrics` (USLT for ID3v2, UNSYNCEDLYRICS for Vorbis,
/// `©lyr` for MP4). Empty content removes the frame entirely so the
/// file doesn't carry a phantom "" lyric tag.
fn write_lyrics_to_file(
    path: &Path,
    content: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::Tag;

    let mut tagged = lofty::read_from_path(path)?;
    if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
        let preferred = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(preferred));
    }
    let tag = if tagged.primary_tag().is_some() {
        tagged.primary_tag_mut().expect("checked")
    } else {
        tagged.first_tag_mut().ok_or("no tag")?
    };

    if content.trim().is_empty() {
        tag.remove_key(ItemKey::UnsyncLyrics);
        tag.remove_key(ItemKey::Lyrics);
    } else {
        // insert_text overwrites any existing item with the same key.
        // For ID3v2 this writes a USLT frame; for Vorbis it writes
        // UNSYNCEDLYRICS; for MP4 it writes ©lyr.
        tag.insert_text(ItemKey::UnsyncLyrics, content.to_string());
    }

    tagged.save_to_path(path, lofty::config::WriteOptions::default())?;
    Ok(())
}

/// Drop the cached lyrics row so the next fetch re-runs the waterfall.
#[tauri::command]
pub async fn clear_lyrics(state: tauri::State<'_, AppState>, track_id: i64) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    sqlx::query(
        "DELETE FROM app.lyrics
          WHERE file_hash = (SELECT file_hash FROM track WHERE id = ?)",
    )
    .bind(track_id)
    .execute(&pool)
    .await?;
    Ok(())
}
