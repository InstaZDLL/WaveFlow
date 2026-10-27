//! The library-wide prefetch that fills the cache in the background.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use waveflow_core::error::describe_http_error;
use waveflow_core::metadata::lrclib::LrclibClient;
use waveflow_syncedlyrics::{LyricsFormat as ExternalLyricsFormat, Provider};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use crate::commands::lyrics_providers;

use super::*;

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
/// to populate the cache using the same local/network priority as
/// [`fetch_lyrics`]. Throttles network calls at ~2 req/s. Cancellable
/// via [`cancel_lyrics_prefetch`].
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
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline mode is enabled".into()));
    }
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

/// The prefetch's cached miss: [`cache_lyrics_miss`] without the payload
/// it builds for the panel, which nobody reads here.
async fn cache_prefetch_miss(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    partial: bool,
) -> AppResult<()> {
    upsert_lyrics_until(
        pool,
        file_hash,
        "",
        &LyricsFormat::Plain,
        &LyricsSource::Api,
        None,
        miss_retry_after(partial),
    )
    .await
}

/// One track the prefetch may look up: id, path, file hash, title,
/// artist, album, duration (ms), and its genres joined on U+001F.
type PendingRow = (
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
);

async fn run_prefetch(
    app: &AppHandle,
    state: &tauri::State<'_, AppState>,
) -> AppResult<LyricsPrefetchSummary> {
    let pool = state.require_profile_pool().await?;

    // Announced with an unknown total: the pending list is only built
    // by the query below, and a row that appears in the status bar the
    // moment the work starts is more useful than one that appears
    // after a multi-second query with an exact count (#601). The total
    // is filled in as soon as it is known.
    let task = crate::tasks::start(
        app,
        crate::tasks::TaskKind::LyricsPrefetch,
        0,
        crate::tasks::cancel_fn(|| {
            PREFETCH_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
    );

    // Pending = available tracks without a cached lyric row, or whose
    // row is a partial miss past its date (#720), deduped by `file_hash`
    // (the cache key). We pick the lowest `track.id` per hash to get a
    // stable representative.
    //
    // Genres come along, joined on the unit separator (a character no
    // tag editor puts in a genre), so the excluded ones (#721) can be
    // dropped here — before the total is counted, so the progress never
    // shows them as failures.
    let rows: Vec<PendingRow> = sqlx::query_as(
        "SELECT t.id, t.file_path, t.file_hash, t.title,
                    ar.name AS artist_name,
                    al.title AS album_title,
                    t.duration_ms,
                    (SELECT GROUP_CONCAT(g.name, char(31))
                       FROM track_genre tg
                       JOIN genre g ON g.id = tg.genre_id
                      WHERE tg.track_id = t.id) AS genres
               FROM track t
               LEFT JOIN artist ar ON ar.id = t.primary_artist
               LEFT JOIN album  al ON al.id = t.album_id
               LEFT JOIN app.lyrics l ON l.file_hash = t.file_hash
              WHERE t.is_available = 1
                AND (l.file_hash IS NULL OR l.retry_after <= ?)
              GROUP BY t.file_hash
              ORDER BY t.id",
    )
    .bind(now_ms())
    .fetch_all(&*pool)
    .await?;

    // Skipped outright, local tiers included: the prefetch fills gaps
    // ahead of time, and an excluded track's local lyrics are read the
    // moment its panel opens anyway.
    let excluded_genres = lyrics_providers::excluded_genres(&pool).await?;
    let before = rows.len();
    let pending: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            !row.7.as_deref().is_some_and(|genres| {
                lyrics_providers::any_genre_excluded(genres.split('\u{1f}'), &excluded_genres)
            })
        })
        .map(|(id, path, hash, title, artist, album, duration, _)| {
            (id, path, hash, title, artist, album, duration)
        })
        .collect();
    if pending.len() < before {
        tracing::info!(
            skipped = before - pending.len(),
            "lyrics prefetch skips tracks in an excluded genre"
        );
    }

    let total = pending.len() as u32;
    let mut processed = 0u32;
    let mut hits = 0u32;
    let mut misses = 0u32;
    let mut failed = 0u32;

    // Emit an initial frame so the UI can show the total — and explicitly
    // surface the "nothing to do" case (total == 0) which otherwise looks
    // like the button does nothing.
    let _ = app.emit(
        "lyrics:prefetch-progress",
        LyricsPrefetchProgress {
            processed: 0,
            total,
            hits: 0,
            misses: 0,
            failed: 0,
            current_title: None,
        },
    );

    if let Some(task) = task.as_ref() {
        task.progress(0, total as u64);
    }

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
        if let Some(task) = task.as_ref() {
            task.progress(processed as u64, total as u64);
            task.detail(title.clone());
        }

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
            if let Err(e) = upsert_lyrics(&pool, &file_hash, &content, &format, &source, None).await
            {
                tracing::warn!(track_id, ?e, "persist embedded lyrics failed");
                failed += 1;
            } else {
                hits += 1;
            }
            processed += 1;
            continue;
        }

        // 2. Local sidecar `.ttml` / `.lrc` / `.txt`. Cheap; runs before the
        //    network so a user prefetching with bundled lyrics never
        //    hits LRCLIB unnecessarily.
        let path_for_sidecar = file_path.clone();
        let local = tokio::task::spawn_blocking(move || {
            read_local_after_embedded(Path::new(&path_for_sidecar))
        })
        .await
        .ok()
        .flatten();
        if let Some((content, source)) = local {
            let format = detect_format(&content);
            if let Err(e) = upsert_lyrics(&pool, &file_hash, &content, &format, &source, None).await
            {
                tracing::warn!(track_id, ?e, "persist sidecar lyrics failed");
                failed += 1;
            } else {
                hits += 1;
            }
            processed += 1;
            continue;
        }

        let meta = TrackMeta {
            file_path: file_path.clone(),
            file_hash: file_hash.clone(),
            title: title.clone(),
            artist_name: artist_name.clone(),
            album_title: album_title.clone(),
            duration_ms,
        };

        // 3. Musixmatch enhanced. If word-level timing exists, keep it
        //    before LRCLIB's line-level result can fill the cache.
        //    Prefetch path deliberately stays translation-free
        //    (`lang = None`) — bulk runs would hammer Musixmatch's
        //    rate limit with one extra hop per track.
        let musixmatch = search_musixmatch_tier(&meta, None).await;
        // `Unavailable` is the one outcome that sent no request: the
        // opt-in is off, offline, or Musixmatch is cooling down.
        let musixmatch_asked = !matches!(musixmatch, Ok(SearchOutcome::Unavailable));
        match musixmatch {
            Ok(SearchOutcome::Found(result))
                if matches!(result.format, ExternalLyricsFormat::EnhancedLrc) =>
            {
                if let Err(e) = cache_external_lyrics(&pool, track_id, &file_hash, result).await {
                    tracing::warn!(track_id, ?e, "persist Musixmatch enhanced lyrics failed");
                    failed += 1;
                } else {
                    hits += 1;
                }
                processed += 1;
                tokio::time::sleep(LRCLIB_THROTTLE).await;
                continue;
            }
            // No enhanced hit (or a transient failure): fall through to LRCLIB.
            Ok(_) => {}
            Err(err) => tracing::warn!(track_id, ?err, "Musixmatch enhanced prefetch failed"),
        }
        // Throttle the Musixmatch call we just made before firing
        // LRCLIB right after. Without this the prefetch loop hits two
        // distinct backends back-to-back per track for any
        // Musixmatch-attempted slot, doubling the effective request
        // rate on the user's network. Only sleep when Musixmatch was
        // actually asked.
        if musixmatch_asked {
            tokio::time::sleep(LRCLIB_THROTTLE).await;
        }

        // 4. LRCLIB. Skip if metadata is too thin to match.
        let Some(artist) = artist_name.as_deref() else {
            misses += 1;
            processed += 1;
            continue;
        };
        let primary_artist = artist.split("; ").next().unwrap_or(artist);
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
                        Some(Provider::Lrclib.as_str()),
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
                        if let Err(e) = upsert_lyrics(
                            &pool,
                            &file_hash,
                            &content,
                            &format,
                            &LyricsSource::Api,
                            Some(Provider::Lrclib.as_str()),
                        )
                        .await
                        {
                            tracing::warn!(track_id, ?e, "persist LRCLIB lyrics failed");
                            failed += 1;
                        } else {
                            hits += 1;
                        }
                    } else {
                        // Row exists but neither synced nor plain
                        // lyrics. Try query-based providers before
                        // caching this as a miss.
                        match search_fallback_chain(&pool, &meta, true).await {
                            Ok(SearchOutcome::Found(result)) => {
                                if let Err(e) =
                                    cache_external_lyrics(&pool, track_id, &file_hash, result).await
                                {
                                    tracing::warn!(track_id, ?e, "persist external lyrics failed");
                                    failed += 1;
                                } else {
                                    hits += 1;
                                }
                            }
                            Ok(SearchOutcome::Miss { partial }) => {
                                let _ = cache_prefetch_miss(&pool, &file_hash, partial).await;
                                misses += 1;
                            }
                            // Nothing was queried — don't record a negative
                            // for a track we never actually asked about.
                            Ok(SearchOutcome::Unavailable) => {
                                failed += 1;
                            }
                            Err(err) => {
                                tracing::warn!(track_id, ?err, "external lyrics prefetch failed");
                                failed += 1;
                            }
                        }
                    }
                }
            }
            Ok(None) => {
                // LRCLIB 404. Try query-based providers before caching
                // as empty.
                match search_fallback_chain(&pool, &meta, true).await {
                    Ok(SearchOutcome::Found(result)) => {
                        if let Err(e) =
                            cache_external_lyrics(&pool, track_id, &file_hash, result).await
                        {
                            tracing::warn!(track_id, ?e, "persist external lyrics failed");
                            failed += 1;
                        } else {
                            hits += 1;
                        }
                    }
                    Ok(SearchOutcome::Miss { partial }) => {
                        // No provider had lyrics. Cache as empty so re-runs
                        // of the prefetch and re-opens of the lyrics panel
                        // skip this track. User can force a re-search
                        // per-track via the "Refetch" button.
                        let _ = cache_prefetch_miss(&pool, &file_hash, partial).await;
                        misses += 1;
                    }
                    // Nothing was queried — don't record a negative for a
                    // track we never actually asked about.
                    Ok(SearchOutcome::Unavailable) => {
                        failed += 1;
                    }
                    Err(err) => {
                        tracing::warn!(track_id, ?err, "external lyrics prefetch failed");
                        failed += 1;
                    }
                }
            }
            Err(err) => {
                let error = describe_http_error(&err);
                tracing::warn!(track_id, %error, "LRCLIB prefetch failed");
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
