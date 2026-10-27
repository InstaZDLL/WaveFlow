//! Enriching an artist: the Deezer picture and identifier, the biography
//! from Last.fm or TheAudioDB, and the cache row behind them.

use serde::Serialize;

use waveflow_core::metadata::{
    deezer::{is_placeholder_artist_picture, DeezerClient},
    lastfm::LastfmClient,
    name_match::{normalize_name, select_by_name},
    theaudiodb::{make_summary, TheAudioDbClient},
};

use crate::{
    commands::integration::{read_bio_language, read_bio_source, read_lastfm_api_key, BioSource},
    error::AppResult,
    metadata_artwork,
    state::AppState,
};

use super::*;

// ── Artist enrichment ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DeezerArtistEnrichment {
    pub deezer_id: Option<i64>,
    /// Remote Deezer CDN URL — fallback when the local download failed.
    pub picture_url: Option<String>,
    /// Absolute filesystem path to the locally-cached picture.
    pub picture_path: Option<String>,
    pub picture_path_1x: Option<String>,
    pub picture_path_2x: Option<String>,
    pub fans_count: Option<i64>,
    /// Short biography from Last.fm (if an API key is configured and
    /// the artist matches). HTML stripped.
    pub bio_short: Option<String>,
    /// Full biography from Last.fm. HTML stripped.
    pub bio_full: Option<String>,
    /// Remote TheAudioDB URL of the wide artist fanart (issue #482) —
    /// fallback when the local download failed.
    pub background_url: Option<String>,
    /// Absolute filesystem path to the locally-cached fanart. Feeds the
    /// artist hero; `None` means the artist has no wide image and the
    /// frontend falls back to blurring the square photo.
    pub background_path: Option<String>,
    /// The artist's **own** image — an `artist.jpg` sidecar the scanner
    /// linked, or a picture the user set by hand — resolved from
    /// `artist.artwork_id` in the profile artwork dir.
    ///
    /// It outranks `picture_path`, and every surface that shows an
    /// artist photo has to prefer it: the artist page and the library
    /// grid already did, through `get_artist_detail` /
    /// `list_library_artists`, while everything reached through this
    /// enrichment (the now-playing panel, the cover slideshow) showed
    /// Deezer's picture instead — a curated image ignored when Deezer
    /// had one, and no image at all when Deezer had none (#701).
    pub artwork_path: Option<String>,
    pub artwork_path_1x: Option<String>,
    pub artwork_path_2x: Option<String>,
}

impl DeezerArtistEnrichment {
    fn empty() -> Self {
        Self {
            deezer_id: None,
            picture_url: None,
            picture_path: None,
            picture_path_1x: None,
            picture_path_2x: None,
            fans_count: None,
            bio_short: None,
            bio_full: None,
            background_url: None,
            background_path: None,
            artwork_path: None,
            artwork_path_1x: None,
            artwork_path_2x: None,
        }
    }
}

#[tauri::command]
pub async fn enrich_artist_deezer(
    state: tauri::State<'_, AppState>,
    artist_id: i64,
) -> AppResult<DeezerArtistEnrichment> {
    // One snapshot rather than two awaits: pairing a pool with an id
    // read after a `switch_profile` would point the artwork dir at the
    // other profile.
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    enrich_artist_deezer_with_pool(state, &pool, &artwork_dir, artist_id).await
}

/// Enrich an artist by **name**, for a remote artist (RFC-005) that has no
/// local row — the remote artist view and the "About the artist" panel use
/// it to show the Deezer photo, the TheAudioDB hero background and the
/// Last.fm bio the server itself does not carry. Same shared cache and
/// offline behaviour as [`enrich_artist_deezer`]; there's just no local
/// artist to link the resolved deezer_id back to.
#[tauri::command]
pub async fn enrich_artist_by_name(
    state: tauri::State<'_, AppState>,
    name: String,
) -> AppResult<DeezerArtistEnrichment> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Ok(DeezerArtistEnrichment::empty());
    }
    let pool = state.require_profile_pool().await?;
    // Reuse a previously-resolved Deezer id for this name so the None path
    // doesn't re-hit the network on every remote track change / view open.
    //
    // A bounded, case-insensitive exact match evaluated inside SQLite —
    // rather than loading the whole cache into Rust to fuzzy-match every
    // row — catches the common same-name/different-case hit. A diacritic-
    // only variant ("Beyonce" vs a cached "Beyoncé") misses here and falls
    // through to a fresh fetch, which does the accent-insensitive match
    // against Deezer's own results and then caches it for next time.
    let cached_deezer_id: Option<i64> = match sqlx::query_scalar::<_, i64>(
        "SELECT deezer_id FROM app.metadata_artist
          WHERE deezer_id IS NOT NULL AND name = ? COLLATE NOCASE
          ORDER BY fetched_at DESC LIMIT 1",
    )
    .bind(&name)
    .fetch_optional(&*pool)
    .await
    {
        Ok(id) => id,
        Err(err) => {
            // A lookup failure is not fatal — fall through to a fresh fetch
            // — but it is distinct from a clean cache miss, so log it.
            tracing::debug!(%err, artist = %name, "cached deezer id lookup failed");
            None
        }
    };
    enrich_artist_named(state, (*pool).clone(), name, cached_deezer_id).await
}

/// Enrichment body, pinned to a caller-supplied pool.
///
/// Split out of [`enrich_artist_deezer`] so the batch path can enrich a
/// whole artist list against the pool it read that list from. Going
/// through the command wrapper instead would re-resolve the active
/// profile once per artist, and a `switch_profile` mid-batch would then
/// write the remaining artists into the *new* profile.
pub(super) async fn enrich_artist_deezer_with_pool(
    state: tauri::State<'_, AppState>,
    pool: &sqlx::SqlitePool,
    artwork_dir: &std::path::Path,
    artist_id: i64,
) -> AppResult<DeezerArtistEnrichment> {
    // Per-profile bio override (issue #323) wins over any fetched bio
    // and works offline. We still let the inner path fetch + cache the
    // online bio (keeping the shared cross-profile `metadata_artist`
    // cache correct for profiles WITHOUT an override) and just swap the
    // returned bio here. Offline mode short-circuits before the network
    // inside, so the override is the only bio offline users ever see.
    let custom_bio: Option<String> =
        sqlx::query_scalar("SELECT custom_bio FROM artist WHERE id = ?")
            .bind(artist_id)
            .fetch_optional(pool)
            .await?
            .flatten()
            .map(|b: String| b.trim().to_string())
            .filter(|b| !b.is_empty());

    // Pass the SAME pool into the inner so the custom_bio lookup and the
    // enrichment stay scoped to one profile — re-resolving inside could
    // straddle a switch_profile and apply one profile's override to
    // another's artist.
    let mut enrichment =
        enrich_artist_deezer_inner(state, pool.clone(), artwork_dir, artist_id).await?;
    if let Some(bio) = custom_bio {
        // Synthesize a truncated lead-in the same way the online sources
        // do (issue #343) — without it bio_short == bio_full verbatim and
        // the frontend's length-based "Read more" toggle never appears.
        enrichment.bio_short = Some(make_summary(&bio));
        enrichment.bio_full = Some(bio);
    }
    Ok(enrichment)
}

async fn enrich_artist_deezer_inner(
    state: tauri::State<'_, AppState>,
    pool: sqlx::SqlitePool,
    artwork_dir: &std::path::Path,
    artist_id: i64,
) -> AppResult<DeezerArtistEnrichment> {
    // 1. Read local artist, with the image it owns: an `artist.jpg`
    //    sidecar the scanner linked, or one the user set by hand.
    type LocalArtistRow = (
        String,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let local: Option<LocalArtistRow> = sqlx::query_as(
        "SELECT ar.name, ar.deezer_id, aw.hash, aw.format, bg.hash, bg.format
           FROM artist ar
           LEFT JOIN artwork aw ON aw.id = ar.artwork_id
           LEFT JOIN artwork bg ON bg.id = ar.background_artwork_id
          WHERE ar.id = ?",
    )
    .bind(artist_id)
    .fetch_optional(&pool)
    .await?;

    let Some((
        artist_name,
        existing_deezer_id,
        artwork_hash,
        artwork_format,
        background_hash,
        background_format,
    )) = local
    else {
        return Ok(DeezerArtistEnrichment::empty());
    };

    // The dir is handed down rather than re-derived from the active
    // profile, for the reason the pool is: a `switch_profile` mid-batch
    // would otherwise point the remaining artists at the new profile's
    // artwork dir while they are read from the old profile's database.
    let artwork_path = metadata_artwork::existing_profile_path(
        artwork_dir,
        artwork_hash.as_deref(),
        artwork_format.as_deref(),
    );
    // The tiers only make sense alongside the full image: without it
    // the row points at a file that is gone, and a thumbnail of it
    // would be the one thing still on screen.
    let (artwork_path_1x, artwork_path_2x) = match (&artwork_path, artwork_hash.as_deref()) {
        (Some(_), Some(hash)) => crate::thumbnails::thumbnail_paths_for(artwork_dir, hash),
        _ => (None, None),
    };

    let mut enrichment =
        enrich_artist_named(state, pool.clone(), artist_name, existing_deezer_id).await?;
    enrichment.artwork_path = artwork_path;
    enrichment.artwork_path_1x = artwork_path_1x;
    enrichment.artwork_path_2x = artwork_path_2x;

    // A backdrop the user chose replaces the cached fanart here too
    // (#693): the artist page paints the hero from this enrichment, so
    // resolving it only in `get_artist_detail` would let the automatic
    // one win a moment later.
    if let Some(path) = metadata_artwork::existing_profile_path(
        artwork_dir,
        background_hash.as_deref(),
        background_format.as_deref(),
    ) {
        enrichment.background_path = Some(path);
        enrichment.background_url = None;
    }

    // 8. Link the discovered deezer_id back onto the local artist, so the
    //    next pass hits the cache by id instead of re-searching by name.
    if existing_deezer_id.is_none() {
        if let Some(did) = enrichment.deezer_id {
            sqlx::query("UPDATE artist SET deezer_id = ? WHERE id = ?")
                .bind(did)
                .bind(artist_id)
                .execute(&pool)
                .await?;
        }
    }
    Ok(enrichment)
}

/// Enrichment core keyed on an artist **name** rather than a local row.
///
/// A remote artist (RFC-005) has no local id, so this is what gives its
/// view the same Deezer photo, TheAudioDB hero background and Last.fm bio
/// as a library artist. Pass a known `deezer_id` to reuse the shared
/// `app.metadata_artist` cache; `None` searches Deezer by name. The result
/// is cached by the resolved deezer_id either way — the caller links it to
/// a local row when there is one.
async fn enrich_artist_named(
    state: tauri::State<'_, AppState>,
    pool: sqlx::SqlitePool,
    artist_name: String,
    existing_deezer_id: Option<i64>,
) -> AppResult<DeezerArtistEnrichment> {
    let artwork_dir = state.paths.metadata_artwork_dir.clone();
    let now = now_ms();

    // Active bio provider + language (issue #295). Read up-front so the
    // cache check can invalidate a bio fetched under a different source
    // / language even when the rest of the row is still fresh.
    let active_source = read_bio_source(&state).await?;
    let active_lang = read_bio_language(&state).await?;

    // 2. Cache hit? (includes bio fields populated in a previous pass)
    if let Some(did) = existing_deezer_id {
        #[allow(clippy::type_complexity)]
        let cached: Option<(
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            i64,
        )> = sqlx::query_as(
            "SELECT picture_url, picture_hash, fans_count, bio_short, bio_full,
                    bio_source, bio_language,
                    background_url, background_hash, background_fetched_at, expires_at
               FROM app.metadata_artist WHERE deezer_id = ?",
        )
        .bind(did)
        .fetch_optional(&pool)
        .await?;

        if let Some((
            picture_url,
            picture_hash,
            fans_count,
            bio_short,
            bio_full,
            cached_bio_source,
            cached_bio_language,
            background_url,
            background_hash,
            background_fetched_at,
            expires_at,
        )) = cached
        {
            // The bio part is only reusable when it was fetched under the
            // currently-selected source (and language, for TheAudioDB);
            // otherwise we fall through and re-fetch it.
            let bio_fresh = BioSource::parse(cached_bio_source.as_deref()) == active_source
                && (active_source != BioSource::TheAudioDb
                    || cached_bio_language.as_deref() == Some(active_lang.as_str()));
            // A row written before issue #482 never looked for fanart —
            // `background_fetched_at IS NULL` is the marker, and a NULL
            // hash alone can't say it apart from "looked, found nothing".
            // Falling through backfills it once, then this stays true
            // for the rest of the row's TTL.
            let background_fresh = background_fetched_at.is_some();
            if expires_at > now && bio_fresh && background_fresh {
                // A row cached before #406 may hold a Deezer placeholder
                // URL (and a grey-blob hash). Drop both so we surface the
                // initial-letter avatar instead of the grey box; the row's
                // own TTL refresh re-fetches through `best_picture`, and a
                // placeholder means the artist has no real Deezer photo to
                // heal to anyway.
                let placeholder = picture_url
                    .as_deref()
                    .is_some_and(is_placeholder_artist_picture);
                let (picture_url, picture_hash) = if placeholder {
                    (None, None)
                } else {
                    (picture_url, picture_hash)
                };
                let picture_path = picture_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
                let (picture_path_1x, picture_path_2x) = match picture_hash.as_deref() {
                    Some(h) => crate::thumbnails::thumbnail_paths_for(&artwork_dir, h),
                    None => (None, None),
                };
                let background_path = background_hash
                    .as_deref()
                    .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
                return Ok(DeezerArtistEnrichment {
                    deezer_id: Some(did),
                    picture_url,
                    picture_path,
                    picture_path_1x,
                    picture_path_2x,
                    fans_count,
                    bio_short,
                    bio_full,
                    background_url,
                    background_path,
                    // Filled by the by-id path, which is the only one
                    // with a local row to read an image from.
                    artwork_path: None,
                    artwork_path_1x: None,
                    artwork_path_2x: None,
                });
            }
        }
    }

    // 3. Fetch from Deezer (picture + fans). Short-circuit when
    //    offline mode is on so we don't poke the network for stale
    //    cache entries.
    if crate::offline::is_offline() {
        return Ok(DeezerArtistEnrichment {
            deezer_id: existing_deezer_id,
            ..DeezerArtistEnrichment::empty()
        });
    }
    let client = DeezerClient::new();
    let hit = if let Some(did) = existing_deezer_id {
        match client.get_artist(did).await {
            Ok(h) => Some(h),
            Err(err) => {
                tracing::warn!(?err, "Deezer get_artist failed");
                return Ok(DeezerArtistEnrichment {
                    deezer_id: Some(did),
                    ..DeezerArtistEnrichment::empty()
                });
            }
        }
    } else {
        match client.search_artist(&artist_name).await {
            Ok(hits) => {
                // Share TheAudioDB's fuzzy matcher (#342): Deezer's search
                // is accent-insensitive too, so an exact-equality filter
                // dropped the picture/cover for "Celine Dion" ↔ "Céline
                // Dion" and superset names like "Bob Marley & The Wailers".
                select_by_name(hits, &normalize_name(&artist_name), |h| {
                    Some(h.name.as_str())
                })
            }
            Err(err) => {
                tracing::warn!(?err, "Deezer search_artist failed");
                return Ok(DeezerArtistEnrichment::empty());
            }
        }
    };

    let Some(hit) = hit else {
        return Ok(DeezerArtistEnrichment::empty());
    };

    // 4. TheAudioDB lookup — one call, two consumers. The wide fanart
    //    backing the artist hero (issue #482) is fetched whatever the
    //    selected bio source is: Last.fm has no equivalent image, so
    //    gating this on `bio_source` would leave every Last.fm user
    //    with no hero at all. The bio half of the same response is only
    //    used when TheAudioDB IS the selected source — one request
    //    instead of two, which matters on their rate-limited free key.
    let audiodb_result = TheAudioDbClient::new()
        .artist_info(&artist_name, &active_lang)
        .await;
    // A *reached* API — match or not — is what licenses stamping
    // `background_fetched_at` below. A transport error leaves it NULL so
    // the next visit retries instead of caching a network blip as "this
    // artist has no fanart" for the whole 30-day TTL.
    let audiodb_reached = audiodb_result.is_ok();
    let audiodb = match audiodb_result {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!(?err, "TheAudioDB artist_info failed");
            None
        }
    };

    // 5. Fetch the bio from the selected source (issue #295). A missing
    //    match is an answer and is cached like one. A failure to ask —
    //    the network, a refused key, a spent quota — is not: the row
    //    keeps the biography it had and expires within the hour, so a
    //    later visit asks again (see `upsert_artist_row`). The
    //    source/language we used is stored alongside so a later switch
    //    re-fetches instead of serving the wrong bio.
    let (bio_short, bio_full, bio_reached) = match active_source {
        BioSource::Lastfm => match read_lastfm_api_key(&state).await? {
            Some(api_key) => {
                let lastfm = LastfmClient::new();
                match lastfm.artist_get_info(&artist_name, &api_key).await {
                    Ok(Some(info)) => (info.bio_summary, info.bio_full, true),
                    Ok(None) => (None, None, true),
                    Err(err) => {
                        tracing::warn!(?err, "Last.fm artist_get_info failed");
                        (None, None, false)
                    }
                }
            }
            // Nothing was asked, so nothing failed: no key is a setting,
            // and the row records it as an absence like before.
            None => (None, None, true),
        },
        BioSource::TheAudioDb => match audiodb.as_ref() {
            Some(info) => (info.bio_short.clone(), info.bio_full.clone(), true),
            None => (None, None, audiodb_reached),
        },
    };

    let picture_url = hit.best_picture();
    // Every fanart the artist has, kept so the backdrop picker has
    // something to offer (#693); the first is the one we paint by
    // default, which is TheAudioDB's own preferred order.
    let background_urls: Vec<String> = audiodb.map(|info| info.fanart_urls).unwrap_or_default();
    let background_url = background_urls.first().cloned();
    // NULL when TheAudioDB could not be reached, which is also what
    // holds the upsert's CASE above: the row keeps the backdrop it had
    // rather than being emptied by a failure to ask.
    let background_urls_json = audiodb_reached
        .then(|| serde_json::to_string(&background_urls).ok())
        .flatten();

    // 6. Download artwork into the shared cache (best-effort).
    let picture_hash = match picture_url.as_deref() {
        Some(url) => metadata_artwork::download_and_cache(url, &artwork_dir).await,
        None => None,
    };
    let picture_path = picture_hash
        .as_deref()
        .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));
    let (picture_path_1x, picture_path_2x) = match picture_hash.as_deref() {
        Some(h) => crate::thumbnails::thumbnail_paths_for(&artwork_dir, h),
        None => (None, None),
    };
    // The hero paints the fanart full-bleed behind the header, so it's
    // the one image we deliberately keep at full resolution — hence the
    // `_full_res` variant, which skips the `_1x` / `_2x` thumbnail job:
    // downscaling would only soften the crop, and nothing reads the tiers.
    let background_hash = match background_url.as_deref() {
        Some(url) => metadata_artwork::download_and_cache_full_res(url, &artwork_dir).await,
        None => None,
    };
    let background_path = background_hash
        .as_deref()
        .and_then(|h| metadata_artwork::existing_path(&artwork_dir, h));

    // 7. Upsert into the metadata cache (Deezer + bio fields land in the
    //    unified `metadata_artist` table in app.db so every profile
    //    shares the same cache). `bio_source` / `bio_language` record
    //    which provider produced the bio so a later switch invalidates
    //    it (see the cache-hit check above). Language is only meaningful
    //    for TheAudioDB, so Last.fm stores NULL.
    let stored_lang: Option<&str> =
        matches!(active_source, BioSource::TheAudioDb).then_some(active_lang.as_str());
    // What the row holds after the write, which is what to show: on a
    // failure to ask, the biography it already had rather than nothing.
    let (bio_short, bio_full) = upsert_artist_row(
        &pool,
        &ArtistRow {
            deezer_id: hit.id,
            name: &hit.name,
            picture_url: picture_url.as_deref(),
            picture_hash: picture_hash.as_deref(),
            fans_count: hit.nb_fan,
            bio: BioAnswer {
                reached: bio_reached,
                short: bio_short.as_deref(),
                full: bio_full.as_deref(),
                source: active_source.as_str(),
                language: stored_lang,
            },
            background_url: background_url.as_deref(),
            background_hash: background_hash.as_deref(),
            // NULL when TheAudioDB could not be reached, so the row still
            // reads as "never looked" rather than as "this artist has no
            // fanart".
            background_urls_json: background_urls_json.as_deref(),
            // Stamped even when the lookup came back empty — that's the
            // whole point of the column: "we asked, TheAudioDB has
            // nothing". Left NULL when the API couldn't be reached at all,
            // so that retries.
            background_fetched_at: audiodb_reached.then_some(now),
            now,
        },
    )
    .await?;

    Ok(DeezerArtistEnrichment {
        deezer_id: Some(hit.id),
        picture_url,
        picture_path,
        picture_path_1x,
        picture_path_2x,
        fans_count: hit.nb_fan,
        bio_short,
        bio_full,
        background_url,
        background_path,
        artwork_path: None,
        artwork_path_1x: None,
        artwork_path_2x: None,
    })
}

/// What the selected bio provider said, for one write of the cache row.
pub(super) struct BioAnswer<'a> {
    /// Whether the provider answered at all. `false` is a failure to ask,
    /// which must neither erase a cached biography nor be cached itself.
    pub(super) reached: bool,
    pub(super) short: Option<&'a str>,
    pub(super) full: Option<&'a str>,
    pub(super) source: &'a str,
    pub(super) language: Option<&'a str>,
}

/// One write of an artist's `app.metadata_artist` row.
pub(super) struct ArtistRow<'a> {
    pub(super) deezer_id: i64,
    pub(super) name: &'a str,
    pub(super) picture_url: Option<&'a str>,
    pub(super) picture_hash: Option<&'a str>,
    pub(super) fans_count: Option<i64>,
    pub(super) bio: BioAnswer<'a>,
    pub(super) background_url: Option<&'a str>,
    pub(super) background_hash: Option<&'a str>,
    pub(super) background_urls_json: Option<&'a str>,
    pub(super) background_fetched_at: Option<i64>,
    pub(super) now: i64,
}

/// Upsert the Deezer + bio cache row and return the biography it holds
/// afterwards.
///
/// The unified `metadata_artist` table lives in app.db so every profile
/// shares the same cache. `bio_source` / `bio_language` record which
/// provider produced the bio so a later switch invalidates it (see the
/// cache-hit check in `enrich_artist_deezer`). Language is only
/// meaningful for TheAudioDB, so Last.fm stores NULL.
///
/// The bio columns follow the rule the background columns already did.
/// When the provider answered, its answer replaces what was there, empty
/// or not — an artist whose biography was removed upstream must lose it
/// here too, so this tests whether the provider was reached rather than
/// whether the new value is empty. When it was not reached, the row keeps
/// the biography and the source it had, and **expires after
/// `RETRY_AFTER_MS`**: the Deezer half was fetched and is stored, but the
/// visit that failed to ask for a biography must not stand for thirty
/// days — nor be repeated on every open while the key stays refused. A
/// fresh row in that case records no source.
pub(super) async fn upsert_artist_row(
    pool: &sqlx::SqlitePool,
    row: &ArtistRow<'_>,
) -> Result<(Option<String>, Option<String>), sqlx::Error> {
    let expires = if row.bio.reached {
        row.now + CACHE_TTL_MS
    } else {
        row.now + RETRY_AFTER_MS
    };
    let (source, language) = if row.bio.reached {
        (Some(row.bio.source), row.bio.language)
    } else {
        (None, None)
    };
    let (short, full, stored_source, stored_language): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "INSERT INTO app.metadata_artist
            (deezer_id, name, picture_url, picture_hash, fans_count, bio_short, bio_full,
             bio_source, bio_language, background_url, background_hash, background_urls,
             background_fetched_at, fetched_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(deezer_id) DO UPDATE SET
           name = excluded.name,
           picture_url = excluded.picture_url,
           picture_hash = excluded.picture_hash,
           fans_count = excluded.fans_count,
           bio_short = CASE WHEN ?16 THEN excluded.bio_short ELSE bio_short END,
           bio_full = CASE WHEN ?16 THEN excluded.bio_full ELSE bio_full END,
           bio_source = CASE WHEN ?16 THEN excluded.bio_source ELSE bio_source END,
           bio_language = CASE WHEN ?16 THEN excluded.bio_language ELSE bio_language END,
           -- The three background columns are replaced only when
           -- TheAudioDB actually answered, which `background_fetched_at`
           -- records (it is NULL exactly when the API could not be
           -- reached). Writing `excluded` unconditionally meant a
           -- network blip during any *Deezer* refresh erased a perfectly
           -- good cached fanart for every profile, and the hero fell
           -- back to the blurred photo until the next pass.
           -- An answer that carries nothing still wins: an artist whose
           -- fanart was removed upstream must lose it here too, which is
           -- why this tests whether we reached the API rather than
           -- whether the new value is empty -- a COALESCE would keep a
           -- backdrop TheAudioDB no longer lists, for ever.
           background_url = CASE WHEN excluded.background_fetched_at IS NULL
                                 THEN background_url ELSE excluded.background_url END,
           background_hash = CASE WHEN excluded.background_fetched_at IS NULL
                                  THEN background_hash ELSE excluded.background_hash END,
           background_urls = CASE WHEN excluded.background_fetched_at IS NULL
                                  THEN background_urls ELSE excluded.background_urls END,
           background_fetched_at = excluded.background_fetched_at,
           fetched_at = excluded.fetched_at,
           expires_at = excluded.expires_at
         RETURNING bio_short, bio_full, bio_source, bio_language",
    )
    .bind(row.deezer_id)
    .bind(row.name)
    .bind(row.picture_url)
    .bind(row.picture_hash)
    .bind(row.fans_count)
    .bind(row.bio.short)
    .bind(row.bio.full)
    .bind(source)
    .bind(language)
    .bind(row.background_url)
    .bind(row.background_hash)
    .bind(row.background_urls_json)
    .bind(row.background_fetched_at)
    .bind(row.now)
    .bind(expires)
    .bind(row.bio.reached)
    .fetch_one(pool)
    .await?;
    // A biography kept through a failure was fetched under whatever source
    // and language were selected then. Shown now, it would answer a
    // question the user no longer asks — the cache-hit check refuses it for
    // the same reason — so it stays in the row and is not returned.
    let asked_for = stored_source.as_deref() == Some(row.bio.source)
        && stored_language.as_deref() == row.bio.language;
    if row.bio.reached || asked_for {
        Ok((short, full))
    } else {
        Ok((None, None))
    }
}
