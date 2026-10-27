//! Lyrics for Web Radio and remote-source tracks, which have no
//! library row to key the cache on.

#[cfg(feature = "sync_v2")]
use crate::error::AppError;
use crate::{error::AppResult, state::AppState};

use super::*;

// ── Web Radio lyrics (no library row → keyed by artist + title) ──────

/// Normalize an (artist, title) pair into the `radio_lyrics` primary
/// key: blake3 over lowercased `artist \x1f title` so ICY casing /
/// spacing noise still collapses to one cache row.
fn radio_lyrics_key(artist: &str, title: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(artist.trim().to_lowercase().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(title.trim().to_lowercase().as_bytes());
    hasher.finalize().to_hex().to_string()
}

async fn read_radio_cached(
    pool: &sqlx::SqlitePool,
    key: &str,
) -> AppResult<Option<(String, String, String, Option<String>)>> {
    let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT content, format, source, provider
           FROM app.radio_lyrics WHERE artist_title_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Upsert a radio-lyrics row. An empty `content` is a cached miss (see
/// the migration); `source` is always `'api'` here (the manual path
/// doesn't exist for radio yet).
async fn upsert_radio_lyrics(
    pool: &sqlx::SqlitePool,
    key: &str,
    artist: &str,
    title: &str,
    content: &str,
    format: &LyricsFormat,
    provider: Option<&str>,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO app.radio_lyrics
            (artist_title_key, artist, title, content, format, source, provider, fetched_at)
         VALUES (?, ?, ?, ?, ?, 'api', ?, ?)
         ON CONFLICT(artist_title_key) DO UPDATE SET
            artist = excluded.artist,
            title = excluded.title,
            content = excluded.content,
            format = excluded.format,
            source = excluded.source,
            provider = excluded.provider,
            fetched_at = excluded.fetched_at",
    )
    .bind(key)
    .bind(artist)
    .bind(title)
    .bind(content)
    .bind(format_to_db(format))
    .bind(provider)
    .bind(now_ms())
    .execute(&mut *tx)
    .await?;
    // Replacing the primary document ends the bundle it belonged to.
    // Translations and pronunciations are cached as part of ONE fetch
    // result (issue #585), so leaving them behind would pair new lyrics
    // with the previous provider's companions — and nothing downstream
    // could tell they were never published together. Every tier that
    // writes a primary comes through here, so the invariant holds
    // without each of them having to remember it.
    sqlx::query("DELETE FROM app.radio_lyrics_associated WHERE artist_title_key = ?")
        .bind(key)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(())
}

/// Fetch + cache lyrics for a now-playing Web Radio song.
///
/// Radio has no library row (negative sentinel id, no file hash), so the
/// regular [`fetch_lyrics`] waterfall can't help. This keys the shared
/// `radio_lyrics` cache by the (artist, title) parsed from the ICY title
/// and queries the external providers (LRCLIB + the query-based
/// fallback) for the rest.
///
/// Returns `Some(payload)` on a cache hit or a fresh network hit; `None`
/// when the song has no lyrics (the miss is cached so the same song
/// recurring on a station doesn't re-hit the network), when offline with
/// nothing cached, or on a transient provider error (NOT cached, so a
/// later attempt retries).
///
/// `track_id` is echoed into the returned payload so the frontend can
/// tie the result to the current radio session; it is never a cache key.
/// The lyrics may be synced (LRC) — the radio panel renders them
/// statically because the live stream position can't align to a song the
/// listener joined mid-play.
#[tauri::command]
pub async fn fetch_radio_lyrics(
    state: tauri::State<'_, AppState>,
    artist: String,
    title: String,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let artist = artist.trim();
    let title = title.trim();
    // The external query is "{title} {artist}"; a blank either side gives
    // nothing usable to search.
    if title.is_empty() || artist.is_empty() {
        return Ok(None);
    }
    let pool = state.require_profile_pool().await?;
    let key = radio_lyrics_key(artist, title);

    // 1. Cache — a hit OR a previously-cached miss (empty content).
    if let Some((content, fmt, src, provider)) = read_radio_cached(&pool, &key).await? {
        if content.is_empty() {
            return Ok(None);
        }
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format: parse_format(&fmt),
            source: parse_source(&src),
            provider,
            tag_write_skipped: None,
            associated: Vec::new(),
        }));
    }

    // 2. Network. Offline → bail WITHOUT caching a miss (the song stays
    //    fetchable once back online).
    if crate::offline::is_offline() {
        return Ok(None);
    }

    let meta = TrackMeta {
        file_path: String::new(),
        file_hash: String::new(),
        title: title.to_string(),
        artist_name: Some(artist.to_string()),
        album_title: None,
        duration_ms: 0,
    };
    // The query-based chain, LRCLIB first (best-curated, often synced).
    // No album / duration context, so this goes through the query search
    // rather than LRCLIB's exact-match `get`.
    let result = match search_fallback_chain(&pool, &meta, false).await {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(?err, "radio lyrics external search failed");
            return Ok(None);
        }
    };

    match result {
        SearchOutcome::Found(r) => {
            let format = external_format_to_app(r.format);
            let provider = r.provider.as_str();
            upsert_radio_lyrics(
                &pool,
                &key,
                artist,
                title,
                &r.content,
                &format,
                Some(provider),
            )
            .await?;
            Ok(Some(LyricsPayload {
                track_id,
                content: r.content,
                format,
                source: LyricsSource::Api,
                provider: Some(provider.to_string()),
                tag_write_skipped: None,
                associated: Vec::new(),
            }))
        }
        SearchOutcome::Miss { partial: false } => {
            // Cache the miss (empty content) so a recurring song on the
            // station's rotation doesn't re-hit the network every time.
            upsert_radio_lyrics(&pool, &key, artist, title, "", &LyricsFormat::Plain, None).await?;
            Ok(None)
        }
        // Not every provider was heard, and the radio cache has no expiry
        // to put on a partial verdict. Left uncached: the providers that
        // failed are cooling down, so the next spin costs little.
        SearchOutcome::Miss { partial: true } => Ok(None),
        // Nothing was queried, so there is no verdict to remember — leave
        // the cache untouched and let the next spin of this song retry.
        SearchOutcome::Unavailable => Ok(None),
    }
}

/// Fetch lyrics for a now-playing remote-source track (RFC-005).
///
/// The server is the priority source (`GET /api/v2/tracks/{id}/lyrics`,
/// which serves the embedded + sidecar lyrics its scanner extracted); on a
/// miss we fall back to LRCLIB + the query chain, searched by artist +
/// title (`external_lyrics_search` / `external_query`) — the duration is
/// carried on the meta but not part of the query. Unlike radio, a remote
/// track has a stable identity and a known length, so its lyrics CAN be
/// synced and the panel renders them so.
///
/// `track_id` is the negative sentinel echoed into the payload for the
/// frontend; `remote_track_id` is the server UUID the lyrics are keyed by.
/// Not cached: remote playback is necessarily online, so a re-query is
/// cheap and always current.
#[cfg(feature = "sync_v2")]
#[tauri::command]
pub async fn fetch_remote_lyrics(
    state: tauri::State<'_, AppState>,
    remote_track_id: String,
    artist: String,
    title: String,
    duration_ms: i64,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    // 1. Server first — the priority source.
    if let Some(server) =
        crate::remote::lyrics::fetch_server_lyrics(&state, &remote_track_id).await?
    {
        return Ok(Some(LyricsPayload {
            track_id,
            content: server.content,
            format: if server.synced {
                LyricsFormat::Lrc
            } else {
                LyricsFormat::Plain
            },
            source: LyricsSource::Api,
            // The server itself, not one of the query providers.
            provider: None,
            tag_write_skipped: None,
            associated: Vec::new(),
        }));
    }

    // 2. LRCLIB (+ query fallback chain) by name — needs an artist + title.
    let artist = artist.trim();
    let title = title.trim();
    if artist.is_empty() || title.is_empty() || crate::offline::is_offline() {
        return Ok(None);
    }
    let meta = TrackMeta {
        file_path: String::new(),
        file_hash: String::new(),
        title: title.to_string(),
        artist_name: Some(artist.to_string()),
        album_title: None,
        duration_ms,
    };
    // The chain reads this profile's provider switches. A profile that
    // went away while the remote queue was playing leaves nothing to show,
    // exactly as before the chain needed a pool.
    let pool = match state.require_profile_pool().await {
        Ok(pool) => pool,
        Err(AppError::NoActiveProfile) => return Ok(None),
        Err(err) => return Err(err),
    };
    match search_fallback_chain(&pool, &meta, false).await {
        Ok(SearchOutcome::Found(r)) => {
            let format = external_format_to_app(r.format);
            let provider = r.provider.as_str().to_string();
            Ok(Some(LyricsPayload {
                track_id,
                content: r.content,
                format,
                source: LyricsSource::Api,
                provider: Some(provider),
                tag_write_skipped: None,
                associated: Vec::new(),
            }))
        }
        // Miss / unavailable / transient error → nothing to show.
        Ok(_) => Ok(None),
        Err(err) => {
            tracing::warn!(?err, "remote lyrics fallback search failed");
            Ok(None)
        }
    }
}
