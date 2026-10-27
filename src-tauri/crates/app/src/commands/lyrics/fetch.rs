//! The lookup commands: the cache-only read, the tiered fetch and its
//! refetch, and the `.lrc` import.

use waveflow_core::error::describe_http_error;
use waveflow_core::metadata::lrclib::LrclibClient;
use waveflow_syncedlyrics::{LyricsFormat as ExternalLyricsFormat, Provider, SearchMode};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

use crate::commands::lyrics_providers;

use super::*;

// ── Tauri commands ───────────────────────────────────────────────────

/// Cache-only lookup. Returns `None` if the track has no cached
/// lyrics — the frontend then calls [`fetch_lyrics`] explicitly.
#[tauri::command]
pub async fn get_lyrics(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let pool = state.require_profile_pool().await?;
    read_cached_or_new_sidecar(&pool, track_id).await
}

#[tauri::command]
pub async fn fetch_lyrics(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    fetch_lyrics_honouring(state, track_id, GenreRule::Apply).await
}

/// The genre that keeps this track out of the online lyrics search
/// (#721), or `None`. Asked by the lyrics panel after a lookup came back
/// empty: `fetch_lyrics` answers `None` both for "searched, nothing
/// found" and for "not searched, excluded genre", and the panel said the
/// first when it meant the second.
///
/// A track with a cache row was searched — a Search anyway (a refetch,
/// which ignores the genres) stores its miss — so it answers `None`
/// whatever its genre: the panel must not call a searched track
/// unsearched the next time it plays.
#[tauri::command]
pub async fn lyrics_excluded_genre(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    let searched: Option<i64> = sqlx::query_scalar(
        "SELECT 1
           FROM track t
           JOIN app.lyrics l ON l.file_hash = t.file_hash
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(&*pool)
    .await?;
    if searched.is_some() {
        return Ok(None);
    }
    lyrics_providers::excluded_genre_of(&pool, track_id).await
}

/// Whether a lookup honours the excluded genres (#721).
#[derive(Clone, Copy, PartialEq, Eq)]
enum GenreRule {
    Apply,
    /// A manual Refetch: an explicit request about one track, which a
    /// genre filter would make look broken.
    Ignore,
}

async fn fetch_lyrics_honouring(
    state: tauri::State<'_, AppState>,
    track_id: i64,
    genres: GenreRule,
) -> AppResult<Option<LyricsPayload>> {
    let pool = state.require_profile_pool().await?;

    // 1. Cache. A cached miss is checked against the sidecar first.
    if let Some(cached) = read_cached_or_new_sidecar(&pool, track_id).await? {
        return Ok(Some(cached));
    }

    // Track metadata drives every tier below (file path for the local
    // tiers, artist/title/album for the network match).
    let meta = match read_track_meta(&pool, track_id).await? {
        Some(m) => m,
        None => return Ok(None),
    };

    // 2–3. Local tiers (embedded tag → sidecar). Local-first by default;
    //       under `lyrics.prefer_lrclib` they're deferred and run only as
    //       the fallback after the online providers miss (issue #378), so
    //       the network gets first crack while a track absent from LRCLIB
    //       still shows its own embedded lyrics instead of nothing.
    let prefer_lrclib = read_prefer_lrclib(&pool).await?;
    if !prefer_lrclib {
        if let Some(payload) = try_local_lyrics(&pool, track_id, &meta).await? {
            return Ok(Some(payload));
        }
    }

    // An excluded genre (#721) stops here, after the local tiers and
    // before every network one, plugins included. Nothing is cached: the
    // track is not known to be lyric-less, only not worth asking about,
    // so taking the genre off the list must bring the lookup back —
    // an empty row would outlive the setting.
    if genres == GenreRule::Apply && lyrics_providers::track_is_excluded(&pool, track_id).await? {
        return if prefer_lrclib {
            try_local_lyrics(&pool, track_id, &meta).await
        } else {
            Ok(None)
        };
    }

    // 4. Plugins declaring `waveflow:metadata/v2`. Ahead of every
    //    network tier below, because a plugin is here only because the
    //    user installed and enabled it — an explicit choice outranks a
    //    built-in default — and because this is the only tier that can
    //    return translations and a pronunciation alongside the lyrics
    //    (issue #585). Offline is checked here as well as inside the
    //    host imports: a guest that ignores a denied fetch would still
    //    burn the timeout.
    //
    //    Only a synced answer ends the waterfall here (#668). An unsynced
    //    one — Apple serves lyrics it has no timing for as untimed TTML —
    //    is held while the network tiers look for synced lyrics, and used
    //    only if none of them finds any: the plugin still wins a tie, but
    //    karaoke beats static text.
    let mut held: Option<PluginAnswer> = None;
    if !crate::offline::is_offline() {
        if let Some(answer) = try_plugin_lyrics(&state, &pool, track_id, &meta, None).await? {
            if lyrics_are_synced(&answer.payload.format, &answer.payload.content) {
                return Ok(Some(answer.payload));
            }
            held = Some(answer);
        }
    }

    let Some(held) = held else {
        return fetch_after_plugins(&pool, track_id, &meta, prefer_lrclib).await;
    };
    match fetch_after_plugins(&pool, track_id, &meta, prefer_lrclib).await {
        Ok(Some(payload)) if lyrics_are_synced(&payload.format, &payload.content) => {
            Ok(Some(payload))
        }
        rest => {
            // Anything short of synced lyrics — plain text, an instrumental
            // verdict, a miss, a network failure — leaves the plugin's
            // answer standing. The tiers below write their own result
            // (a plain row, an empty miss row), so it is cached again
            // rather than assumed to still be there.
            if let Err(err) = &rest {
                tracing::debug!(?err, "no synced lyrics after an unsynced plugin answer");
            }
            let PluginAnswer {
                payload,
                bundle,
                plugin_id,
            } = held;
            Ok(Some(
                cache_lyrics_bundle(&pool, track_id, &meta.file_hash, bundle, &plugin_id)
                    .await?
                    .unwrap_or(payload),
            ))
        }
    }
}

/// Tiers 5 and after of [`fetch_lyrics`]: Musixmatch word-level, LRCLIB,
/// the query-based fallback chain, and the local tiers when
/// `lyrics.prefer_lrclib` deferred them. Split out so `fetch_lyrics` can
/// weigh their answer against an unsynced plugin one it is holding.
async fn fetch_after_plugins(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
    prefer_lrclib: bool,
) -> AppResult<Option<LyricsPayload>> {
    // 5. Musixmatch enhanced fallback. This runs before LRCLIB only
    //    when it returns true word-level LRC; regular line-level LRC
    //    still lets the stricter metadata LRCLIB lookup below win.
    if !crate::offline::is_offline() {
        // User-driven path: opt into the per-profile translation
        // language so a hit comes back with each line followed by
        // its `(translation)` companion (issue #208). Prefetch +
        // scanner paths stay `None` to avoid the extra Musixmatch
        // hop per track on bulk operations.
        let translation_lang = read_translation_lang(pool).await.unwrap_or_else(|err| {
            tracing::warn!(
                ?err,
                "read_translation_lang failed; serving untranslated lyrics"
            );
            None
        });
        // A Musixmatch failure here is non-fatal: fall through to LRCLIB
        // rather than aborting the whole lookup or caching a miss.
        match search_musixmatch_tier(meta, translation_lang.as_deref()).await {
            Ok(SearchOutcome::Found(result))
                if matches!(result.format, ExternalLyricsFormat::EnhancedLrc) =>
            {
                return cache_external_lyrics(pool, track_id, &meta.file_hash, result)
                    .await
                    .map(Some);
            }
            // Anything else (line-level hit, miss, or Musixmatch not opted
            // in) just falls through to LRCLIB — nothing is cached here.
            Ok(_) => {}
            Err(err) => tracing::debug!(?err, "Musixmatch enhanced lookup failed"),
        }
    }

    // 6. LRCLIB fallback. Skip if we have no artist (matching is
    //    useless without one) or if offline mode is on. In both cases,
    //    under prefer-LRCLIB the local tiers were deferred and haven't run
    //    yet — fall back to them here so the toggle never *loses* lyrics
    //    the file already carries.
    if crate::offline::is_offline() {
        return if prefer_lrclib {
            try_local_lyrics(pool, track_id, meta).await
        } else {
            Ok(None)
        };
    }
    let Some(artist_name) = meta.artist_name.as_deref() else {
        return if prefer_lrclib {
            try_local_lyrics(pool, track_id, meta).await
        } else {
            Ok(None)
        };
    };
    let primary_artist = artist_name.split("; ").next().unwrap_or(artist_name);
    let duration_seconds = (meta.duration_ms.max(0) as u64).div_ceil(1000);
    // The exact lookup sitting out its cooldown is not asked again: it
    // would cost its connect timeout on every track.
    let lookup = if lrclib_get_cooling_down() {
        None
    } else {
        let result = LrclibClient::new()
            .get(
                primary_artist,
                &meta.title,
                meta.album_title.as_deref(),
                duration_seconds,
            )
            .await;
        record_lrclib_get(&result);
        Some(result)
    };
    let resp = match lookup {
        Some(Ok(Some(r))) => r,
        Some(Ok(None)) => {
            // LRCLIB 404 — try the broader provider chain (and, under
            // prefer-LRCLIB, the deferred local tiers) before caching a
            // miss. These sources are query-based and less strict than
            // LRCLIB's metadata endpoint, so they only run after the exact
            // lookup fails. See `resolve_after_lrclib_miss` for the
            // error-handling contract (provider Err is non-fatal here).
            return resolve_after_lrclib_miss(pool, track_id, meta, prefer_lrclib).await;
        }
        Some(Err(err)) => {
            let error = describe_http_error(&err);
            tracing::warn!(%error, "LRCLIB fetch failed");
            return resolve_while_lrclib_unreachable(
                pool,
                track_id,
                meta,
                prefer_lrclib,
                format!("LRCLIB request failed: {error}"),
            )
            .await;
        }
        None => {
            return resolve_while_lrclib_unreachable(
                pool,
                track_id,
                meta,
                prefer_lrclib,
                "LRCLIB is unreachable; it is tried again in a few minutes".into(),
            )
            .await;
        }
    };

    if resp.instrumental == Some(true) {
        // Under prefer-LRCLIB, a user's own embedded/sidecar lyrics beat
        // an empty "instrumental" verdict — try the deferred local tiers
        // before caching the miss, so the toggle never hides lyrics the
        // file actually carries.
        if prefer_lrclib {
            if let Some(payload) = try_local_lyrics(pool, track_id, meta).await? {
                return Ok(Some(payload));
            }
        }
        // Instrumental: cache an empty plain entry so we don't refetch.
        // Attribute the row to LRCLIB so the UI badge reflects who told
        // us this track is instrumental — the user can still try a
        // different provider via `refetch_lyrics` if they disagree.
        let empty = String::new();
        upsert_lyrics(
            pool,
            &meta.file_hash,
            &empty,
            &LyricsFormat::Plain,
            &LyricsSource::Api,
            Some(Provider::Lrclib.as_str()),
        )
        .await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content: empty,
            format: LyricsFormat::Plain,
            source: LyricsSource::Api,
            provider: Some(Provider::Lrclib.as_str().to_string()),
            tag_write_skipped: None,
            associated: Vec::new(),
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
            // Same as the 404 branch above: LRCLIB returned an entry
            // but it was empty, so fall through to the fallback chain
            // (and the deferred local tiers under prefer-LRCLIB).
            return resolve_after_lrclib_miss(pool, track_id, meta, prefer_lrclib).await;
        }
    };

    let source = LyricsSource::Api;
    let provider = Provider::Lrclib.as_str();
    upsert_lyrics(
        pool,
        &meta.file_hash,
        &content,
        &format,
        &source,
        Some(provider),
    )
    .await?;
    Ok(Some(LyricsPayload {
        track_id,
        content,
        format,
        source,
        provider: Some(provider.to_string()),
        tag_write_skipped: None,
        associated: Vec::new(),
    }))
}

/// Force a re-fetch for a single track.
///
/// Drops the cached row first so the waterfall (or the single-provider
/// query below) is guaranteed to re-query — without this every refetch
/// would short-circuit on the cache hit. Two modes:
///
/// 1. `provider = None` → re-run the full [`fetch_lyrics`] waterfall:
///    embedded tag → sidecar `.lrc` → Musixmatch (if opt-in) →
///    LRCLIB → external fallback chain.
/// 2. `provider = Some(id)` → bypass local tiers, query ONLY that
///    provider (`"lrclib"` / `"genius"` / `"net_ease"` / `"megalobiz"`
///    / `"musixmatch"`). Used by the lyrics panel's provider picker
///    when the auto-waterfall cached a low-quality hit (e.g. Genius
///    junk per issue #284) and the user wants to try a different
///    source by name.
///
/// In single-provider mode, a miss is cached with the requested
/// provider as the attribution so the UI badge reflects what the user
/// last tried — picking a different provider and trying again is then
/// the natural next step.
#[tauri::command]
pub async fn refetch_lyrics(
    state: tauri::State<'_, AppState>,
    track_id: i64,
    provider: Option<String>,
) -> AppResult<Option<LyricsPayload>> {
    let pool = state.require_profile_pool().await?;

    // Drop the cached row (if any) so the waterfall / single-provider
    // path below is forced to re-query. Look up the file_hash first
    // since `app.lyrics` is keyed by content hash, not track id.
    let file_hash: Option<String> = sqlx::query_scalar("SELECT file_hash FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&*pool)
        .await?;
    let Some(file_hash) = file_hash else {
        return Ok(None);
    };
    sqlx::query("DELETE FROM app.lyrics WHERE file_hash = ?")
        .bind(&file_hash)
        .execute(&*pool)
        .await?;

    // No provider pinned → a fresh `fetch_lyrics` call, since the cache
    // row is gone and the waterfall starts at the embedded tier — minus
    // the excluded genres (#721): a Refetch is a request about this one
    // track, and a genre filter would make the button look broken.
    let Some(provider_str) = provider.as_deref() else {
        return fetch_lyrics_honouring(state, track_id, GenreRule::Ignore).await;
    };

    // A pinned plugin is not a `Provider`: the two namespaces share the
    // `lyrics.provider` column and nothing else. Without this branch the
    // prefix would fall through to `Provider::from_id`, which knows only
    // its own ids, and re-fetching lyrics a plugin had supplied would
    // fail as "unknown lyrics provider" — for a value this code wrote.
    if let Some(plugin_id) = provider_str.strip_prefix(PLUGIN_PROVIDER_PREFIX) {
        if crate::offline::is_offline() {
            return Ok(None);
        }
        let meta = match read_track_meta(&pool, track_id).await? {
            Some(m) => m,
            None => return Ok(None),
        };
        // The plugin the user named answers, synced or not: they asked
        // for this one, not for the best available.
        return Ok(
            try_plugin_lyrics(&state, &pool, track_id, &meta, Some(plugin_id))
                .await?
                .map(|answer| answer.payload),
        );
    }

    let Some(provider) = Provider::from_id(provider_str) else {
        return Err(AppError::Other(format!(
            "unknown lyrics provider: {provider_str}"
        )));
    };

    // Provider pinned: query that one only. Skip embedded / sidecar
    // tiers — the user explicitly asked for a network source, and a
    // local hit would just put us back where we started.
    let meta = match read_track_meta(&pool, track_id).await? {
        Some(m) => m,
        None => return Ok(None),
    };
    if crate::offline::is_offline() {
        return Ok(None);
    }
    // Honour the per-profile Musixmatch translation target if the user
    // picked Musixmatch — same opt-in path the waterfall takes.
    let lang = read_translation_lang(&pool).await.unwrap_or(None);
    match external_lyrics_search(
        &meta,
        vec![provider],
        SearchMode::PreferSynced,
        true,
        lang.as_deref(),
    )
    .await
    {
        Ok(SearchOutcome::Found(result)) => {
            cache_external_lyrics(&pool, track_id, &meta.file_hash, result)
                .await
                .map(Some)
        }
        // The provider was never queried (offline, or the user pinned
        // Musixmatch without the opt-in). Caching a miss attributed to it
        // would blame a provider that never answered, so persist nothing
        // and let the next attempt through.
        Ok(SearchOutcome::Unavailable) => Ok(None),
        Ok(SearchOutcome::Miss { .. }) => {
            // Pinned provider returned nothing. Cache an empty miss
            // attributed to it so the badge reflects what the user
            // just tried — they can pick a different provider and
            // refetch without the cache shortcut blocking them.
            let provider_id = provider.as_str();
            upsert_lyrics(
                &pool,
                &meta.file_hash,
                "",
                &LyricsFormat::Plain,
                &LyricsSource::Api,
                Some(provider_id),
            )
            .await?;
            Ok(Some(LyricsPayload {
                track_id,
                content: String::new(),
                format: LyricsFormat::Plain,
                source: LyricsSource::Api,
                provider: Some(provider_id.to_string()),
                tag_write_skipped: None,
                associated: Vec::new(),
            }))
        }
        Err(err) => Err(err),
    }
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
        .fetch_optional(&*pool)
        .await?
        .ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let path = file_path.clone();
    // Decoded whatever its encoding, like a sidecar (#910).
    let content = tokio::task::spawn_blocking(move || {
        std::fs::read(&path).map(|raw| waveflow_core::text_encoding::decode(&raw).into_owned())
    })
    .await
    .map_err(|e| AppError::Other(format!("lyrics file read panicked: {e}")))?
    .map_err(|e| AppError::Other(format!("read {file_path}: {e}")))?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(AppError::Other("imported lyrics file is empty".into()));
    }
    let format = detect_format(trimmed);
    let source = LyricsSource::LrcFile;
    upsert_lyrics(&pool, &file_hash, trimmed, &format, &source, None).await?;
    Ok(LyricsPayload {
        track_id,
        content: trimmed.to_string(),
        format,
        source,
        provider: None,
        tag_write_skipped: None,
        associated: Vec::new(),
    })
}
