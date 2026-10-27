//! The online tiers: Musixmatch, LRCLIB and the query-based fallback
//! chain, plus the cooldown of an LRCLIB that stopped answering.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use waveflow_core::error::describe_error;
use waveflow_syncedlyrics::{
    LyricsFormat as ExternalLyricsFormat, LyricsResult as ExternalLyricsResult, Provider,
    SearchMode, SearchOptions, SyncedLyricsClient,
};

use crate::error::{AppError, AppResult};

use crate::commands::lyrics_providers;

use super::*;

/// Process-wide Musixmatch opt-in. Off by default because the upstream
/// `syncedlyrics` Python project — and therefore this Rust port — hits
/// Musixmatch's `apic-desktop.musixmatch.com` private endpoint via a
/// reverse-engineered desktop-app `app_id`. That is not an authorised
/// integration; Musixmatch has historically issued takedowns against
/// clients that ship it on by default. Hydrated from
/// `app_setting['lyrics.musixmatch_enabled']` at startup. Users who
/// want the word-level provider can opt in via SQL today; a Settings
/// toggle ships in v1.6.
static MUSIXMATCH_ENABLED: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn musixmatch_enabled() -> bool {
    MUSIXMATCH_ENABLED.load(Ordering::Acquire)
}

#[inline]
pub fn set_musixmatch_enabled(value: bool) {
    MUSIXMATCH_ENABLED.store(value, Ordering::Release);
}

/// Filter Musixmatch out of a provider list when the opt-in is off.
/// Used at every external-search call site so the opt-in is a single
/// chokepoint instead of N scattered checks.
fn filter_providers(providers: Vec<Provider>) -> Vec<Provider> {
    if musixmatch_enabled() {
        providers
    } else {
        providers
            .into_iter()
            .filter(|p| !matches!(p, Provider::Musixmatch))
            .collect()
    }
}

pub(super) fn external_format_to_app(format: ExternalLyricsFormat) -> LyricsFormat {
    match format {
        ExternalLyricsFormat::Plain => LyricsFormat::Plain,
        ExternalLyricsFormat::Lrc => LyricsFormat::Lrc,
        ExternalLyricsFormat::EnhancedLrc => LyricsFormat::EnhancedLrc,
    }
}

fn external_query(title: &str, artist_name: Option<&str>) -> String {
    match artist_name {
        Some(artist) if !artist.trim().is_empty() => {
            let primary_artist = artist.split("; ").next().unwrap_or(artist);
            format!("{title} {primary_artist}")
        }
        _ => title.to_string(),
    }
}

/// Process-wide shared `SyncedLyricsClient`. Standing one up per call
/// re-initialises rustls + builds a fresh reqwest connection pool every
/// time `external_lyrics_search` runs — wasted work, especially during
/// `prefetch_library_lyrics` where the same waterfall fires N times in
/// quick succession. The shared client caches its connection pool, so
/// back-to-back calls reuse keep-alive sockets to the providers.
fn shared_external_client() -> AppResult<&'static SyncedLyricsClient> {
    static CLIENT: OnceLock<SyncedLyricsClient> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c);
    }
    // `try_new` is fallible (TLS backend init can fail), so we can't
    // use `OnceLock::get_or_init` which only accepts an infallible
    // initializer. Build + insert via `get_or_try_init` shape: try to
    // initialise, fall back to whatever `set` lost the race to.
    let built = SyncedLyricsClient::try_new()
        .map_err(|err| AppError::Other(format!("lyrics client init failed: {err}")))?;
    match CLIENT.set(built) {
        Ok(()) => Ok(CLIENT.get().expect("just set")),
        // A concurrent caller initialised it first; ours is dropped
        // (and its idle pool too — cost-of-once on cold startup race).
        Err(_) => Ok(CLIENT.get().expect("set by concurrent caller")),
    }
}

/// Outcome of one external provider query (#391).
///
/// `Miss` and `Unavailable` are kept apart **here**, at the source, rather
/// than reconstructed by callers: only this function knows whether any
/// request was actually issued. A caller that tried to infer it afterwards
/// — say by re-reading the offline flag — would be guessing against state
/// that can change between the call and the check, and a wrong guess
/// caches a permanent "no lyrics".
pub(super) enum SearchOutcome {
    Found(ExternalLyricsResult),
    /// At least one provider answered and none had lyrics. A real
    /// negative — complete when every provider asked answered, `partial`
    /// when some failed or were left out (#720).
    Miss {
        partial: bool,
    },
    /// Nobody answered — nothing was queried at all (offline mode, an
    /// empty provider list once the Musixmatch opt-in filter ran, every
    /// provider cooling down). Never a negative.
    Unavailable,
}

pub(super) async fn external_lyrics_search(
    meta: &TrackMeta,
    providers: Vec<Provider>,
    mode: SearchMode,
    enhanced: bool,
    lang: Option<&str>,
) -> AppResult<SearchOutcome> {
    // Defense in depth: every outbound HTTP path must honour offline mode
    // regardless of caller. Short-circuit before any request is issued
    // (see the process-wide offline contract).
    if crate::offline::is_offline() {
        return Ok(SearchOutcome::Unavailable);
    }
    // Apply the Musixmatch opt-in here — at a single chokepoint — so
    // call sites don't have to know whether Musixmatch is in their
    // provider list. An empty list after filtering means "nothing left
    // to query": short-circuit instead of building a no-op client.
    let providers = filter_providers(providers);
    if providers.is_empty() {
        return Ok(SearchOutcome::Unavailable);
    }
    // The syncedlyrics client treats `lang = Some(_)` as a hard
    // filter for Musixmatch-only — passing it to a fallback chain
    // that excludes Musixmatch would short-circuit every provider.
    // Drop the lang silently when no Musixmatch provider is in
    // play; the caller stays oblivious to which providers landed
    // here post-filter.
    let lang = if providers.contains(&Provider::Musixmatch) {
        lang.map(str::to_string)
    } else {
        None
    };
    let query = external_query(&meta.title, meta.artist_name.as_deref());
    let client = shared_external_client()?;
    let mut report = client
        .search(SearchOptions {
            query,
            mode,
            providers,
            enhanced,
            lang,
            genius_cookie: std::env::var("SYNCEDLYRICS_GENIUS_COOKIE").ok(),
            netease_cookie: std::env::var("SYNCEDLYRICS_NETEASE_COOKIE").ok(),
        })
        .await;
    for provider in &report.answered {
        lyrics_providers::record_answer(*provider);
    }
    for failure in &report.failures {
        lyrics_providers::record_failure(failure.provider, &failure.error);
    }
    if let Some(result) = report.result.take() {
        return Ok(SearchOutcome::Found(result));
    }
    if report.has_verdict() {
        return Ok(SearchOutcome::Miss {
            partial: !report.failures.is_empty(),
        });
    }
    // Nobody answered. An error rather than `Unavailable` when something
    // was asked and failed, so a user who picked one provider by name is
    // told it failed instead of being shown nothing.
    match report.failures.pop() {
        Some(failure) => Err(AppError::Other(format!(
            "external lyrics search failed: {}: {}",
            failure.provider.as_str(),
            describe_error(&failure.error)
        ))),
        None => Ok(SearchOutcome::Unavailable),
    }
}

/// Tier 5, the Musixmatch word-level lookup, as an automatic lookup asks
/// it: skipped while Musixmatch sits out a cooldown (#720), exactly as the
/// chain skips its own providers. Picking Musixmatch by name in the panel
/// goes through [`external_lyrics_search`] directly and is not skipped.
pub(super) async fn search_musixmatch_tier(
    meta: &TrackMeta,
    lang: Option<&str>,
) -> AppResult<SearchOutcome> {
    if lyrics_providers::is_cooling_down(Provider::Musixmatch) {
        return Ok(SearchOutcome::Unavailable);
    }
    external_lyrics_search(
        meta,
        vec![Provider::Musixmatch],
        SearchMode::SyncedOnly,
        true,
        lang,
    )
    .await
}

/// Ask the query-based chain, as this profile has configured it.
///
/// Every automatic lookup that walks the chain comes through here —
/// the panel, the bulk prefetch, radio, remote tracks — so the user's
/// switches (#722) and the provider cooldown (#720) apply everywhere
/// without each caller spelling them. A provider named in the panel's
/// picker does not: that is a request about that one provider.
///
/// A provider left out for its cooldown makes a miss partial, exactly
/// as if it had been asked and failed: it was not heard. A provider the
/// user switched off does not — they decided not to ask it.
pub(super) async fn search_fallback_chain(
    pool: &sqlx::SqlitePool,
    meta: &TrackMeta,
    enhanced: bool,
) -> AppResult<SearchOutcome> {
    let (cooling, live): (Vec<Provider>, Vec<Provider>) = lyrics_providers::enabled_chain(pool)
        .await?
        .into_iter()
        .partition(|p| lyrics_providers::is_cooling_down(*p));
    let outcome = external_lyrics_search(
        meta,
        live,
        SearchMode::PreferSynced,
        enhanced,
        // The chain excludes Musixmatch, so `external_lyrics_search`
        // would drop a language anyway.
        None,
    )
    .await?;
    Ok(match outcome {
        SearchOutcome::Miss { partial } => SearchOutcome::Miss {
            partial: partial || !cooling.is_empty(),
        },
        other => other,
    })
}

/// What the post-LRCLIB fallback chain concluded (#391).
///
/// The distinction that matters is `Miss` vs `Unavailable`. Collapsing
/// both into "no lyrics" is what let a one-off provider outage be written
/// as an empty row — and because the waterfall is cache-first and never
/// re-fetches once a row exists, that transient blip marked the track as
/// permanently lyric-less until the user hit Refetch by hand.
enum FallbackOutcome {
    /// A provider had lyrics — already cached.
    Found(LyricsPayload),
    /// Providers answered and none had lyrics. A real negative, safe to
    /// remember so the panel stops re-hitting the network on each open —
    /// for good when complete, until [`PARTIAL_MISS_RETRY`] when `partial`.
    Miss { partial: bool },
    /// Nobody in the chain answered. NOT a negative — nothing may be
    /// cached, so the next attempt retries.
    Unavailable,
}

/// Walk the post-LRCLIB fallback chain and persist the first hit.
///
/// A provider `Err` stays non-fatal for the waterfall — LRCLIB has already
/// given its verdict and this chain is best-effort enrichment, so we don't
/// fail the whole lookup. When nobody in the chain answered it surfaces as
/// [`FallbackOutcome::Unavailable`] rather than masquerading as a miss, so
/// the caller knows not to persist anything.
async fn try_external_fallback(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
) -> AppResult<FallbackOutcome> {
    match search_fallback_chain(pool, meta, true).await {
        Ok(SearchOutcome::Found(result)) => Ok(FallbackOutcome::Found(
            cache_external_lyrics(pool, track_id, &meta.file_hash, result).await?,
        )),
        // Propagated straight through: only the search itself knows whether
        // a request was issued, so nothing here re-derives it.
        Ok(SearchOutcome::Miss { partial }) => Ok(FallbackOutcome::Miss { partial }),
        Ok(SearchOutcome::Unavailable) => Ok(FallbackOutcome::Unavailable),
        Err(err) => {
            tracing::debug!(?err, "external fallback chain failed; not caching a miss");
            Ok(FallbackOutcome::Unavailable)
        }
    }
}

/// Resolve a track once LRCLIB has given no usable lyrics (404 or an
/// empty row). Tries the external provider chain first; then, under
/// `lyrics.prefer_lrclib` (where the local tiers haven't run yet), the
/// embedded + sidecar tiers.
///
/// Returns `None` when the chain was [`FallbackOutcome::Unavailable`] and
/// no local tier covered for it: nothing is cached in that case, so the
/// next panel open retries instead of being served a miss that was really
/// just a network blip (#391). A confirmed [`FallbackOutcome::Miss`] still
/// caches the empty row, preserving the "don't re-hit the network on every
/// open" property. The two `fetch_lyrics` LRCLIB call sites share this so
/// they can't drift on the contract.
pub(super) async fn resolve_after_lrclib_miss(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
    prefer_lrclib: bool,
) -> AppResult<Option<LyricsPayload>> {
    let outcome = try_external_fallback(pool, track_id, meta).await?;
    if let FallbackOutcome::Found(payload) = outcome {
        return Ok(Some(payload));
    }

    // Under prefer-LRCLIB the local tiers were deferred, so they still owe
    // us an answer — whether the chain missed or was unreachable.
    if prefer_lrclib {
        if let Some(payload) = try_local_lyrics(pool, track_id, meta).await? {
            return Ok(Some(payload));
        }
    }

    match outcome {
        FallbackOutcome::Miss { partial } => cache_lyrics_miss(pool, track_id, meta, partial)
            .await
            .map(Some),
        // Transient: persist nothing so the next attempt can retry.
        FallbackOutcome::Unavailable => Ok(None),
        // Handled above.
        FallbackOutcome::Found(_) => unreachable!("Found returns early"),
    }
}

/// Resolve a track while LRCLIB cannot be reached (a timeout, DNS, a
/// refused connection, or the cooldown that follows one). The other
/// providers are still asked, then, under `lyrics.prefer_lrclib`, the
/// deferred local tiers: one host being down should not cost lyrics the
/// others have. When none of them has any, `reason` is returned as an
/// error and nothing is cached: LRCLIB never gave its verdict, so this is
/// not a miss, and the UI offers a retry.
pub(super) async fn resolve_while_lrclib_unreachable(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
    prefer_lrclib: bool,
    reason: String,
) -> AppResult<Option<LyricsPayload>> {
    if let FallbackOutcome::Found(payload) = try_external_fallback(pool, track_id, meta).await? {
        return Ok(Some(payload));
    }
    if prefer_lrclib {
        if let Some(payload) = try_local_lyrics(pool, track_id, meta).await? {
            return Ok(Some(payload));
        }
    }
    Err(AppError::Other(reason))
}

/// Until when LRCLIB's exact lookup (`/api/get`) sits out after it could
/// not be reached. Its own record, apart from the providers' (#720): the
/// chain's `/api/search` on the same host keeps its own, so a search that
/// answers neither lifts this one nor is held back by it.
static LRCLIB_GET_DOWN_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

pub(super) fn lrclib_get_cooling_down() -> bool {
    LRCLIB_GET_DOWN_UNTIL
        .lock()
        .map(|until| until.is_some_and(|until| Instant::now() < until))
        .unwrap_or(false)
}

/// Record how LRCLIB's exact lookup went: a failure to reach it (no
/// connection, a timeout, an error status) starts its cooldown, the way a
/// provider's does; an answer ends it. A response it could not read is
/// neither.
pub(super) fn record_lrclib_get<T>(result: &reqwest::Result<T>) {
    let Ok(mut until) = LRCLIB_GET_DOWN_UNTIL.lock() else {
        return;
    };
    match result {
        Ok(_) => *until = None,
        Err(err) if err.is_connect() || err.is_timeout() || err.is_request() || err.is_status() => {
            *until = Some(Instant::now() + lyrics_providers::COOLDOWN);
        }
        Err(_) => {}
    }
}
