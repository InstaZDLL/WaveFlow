//! Lyrics fetch + cache.
//!
//! Lazy multi-tier lookup, in order:
//!   1. Local DB cache (`lyrics` table, keyed by `track_id`)
//!   2. Embedded lyrics tag inside the audio file (via lofty), preferring
//!      a synced `SYNCEDLYRICS` tag over the plain `USLT` / `LYRICS` ones
//!   3. Local sidecar file — `{stem}.lrc` / `{stem}.txt` next to the
//!      audio file, or inside a `Lyrics/` (case-insensitive) subfolder
//!      next to it. `.lrc` wins over `.txt` (timing info).
//!   4. The generic `description` field — last of the local tiers,
//!      because it is the only one that is a guess about a field meant
//!      for something else
//!   5. Musixmatch Enhanced LRC when word-level timing exists
//!   6. LRCLIB public API (matched by artist + track + album + duration)
//!   7. Query-based external providers before caching a network miss
//!
//! Whichever tier hits first becomes the cached entry. We never refetch
//! once a row exists — the user can manually overwrite by importing a
//! `.lrc` file via [`import_lrc_file`].
//!
//! One exception, and it is narrow: a cached `embedded` row that is a
//! distribution service's credit rather than lyrics is dropped on
//! read and re-resolved. Those rows were written by a bug in tier 4
//! (see below), and without this the fix would only have reached
//! tracks nobody had opened yet.
//!
//! Because of that cache-first rule, **only a confirmed negative may be
//! cached** (#391). A provider outage is `Unavailable`, not a miss: it
//! persists nothing, so the next panel open retries. Caching it would
//! freeze a one-off network blip into a permanent "no lyrics" that only a
//! manual refetch could clear.
//!
//! **Prefer-LRCLIB toggle** (`profile_setting['lyrics.prefer_lrclib']`,
//! issue #378): when on, [`fetch_lyrics`] flips tiers 2–4 (the local
//! ones) to run *after* the online providers — LRCLIB / Musixmatch /
//! the fallback chain win, and the local tiers become the fallback used
//! only when the network has nothing (so a track LRCLIB doesn't carry
//! still shows its own embedded lyrics rather than nothing). Default off
//! (local wins). Applies to on-demand fetch + refetch; the bulk
//! [`run_prefetch`] path stays local-first (a cheap gap-filler).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::Utc;
use lofty::file::{FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use waveflow_core::metadata::lrclib::LrclibClient;
use waveflow_core::plugin::runtime::{
    AssociatedKind as PluginAssociatedKind, LyricsBundle as PluginLyricsBundle,
    LyricsDocFormat as PluginLyricsFormat,
};
use waveflow_syncedlyrics::{
    LyricsFormat as ExternalLyricsFormat, LyricsResult as ExternalLyricsResult, Provider,
    SearchMode, SearchOptions, SyncedLyricsClient,
};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

/// Guards against two concurrent prefetch runs and exposes a
/// cancellation flag the user can flip from the UI. Module-local — the
/// prefetch is a single global operation, so a bare `AtomicBool` pair
/// is enough; no need to thread a token through `AppState`.
static PREFETCH_RUNNING: AtomicBool = AtomicBool::new(false);
static PREFETCH_CANCEL: AtomicBool = AtomicBool::new(false);

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
/// `EnhancedLrc` is the per-word timed variant (`[00:01.00]Hello <00:01.50>world`).
/// `Ttml` is Apple-Music-style XML with `<span begin="…" end="…">` word timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsFormat {
    Plain,
    Lrc,
    EnhancedLrc,
    Ttml,
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
    /// Sub-provider that produced this row when `source` is
    /// `LyricsSource::Api`. Matches `Provider::as_str()` from
    /// `waveflow_syncedlyrics` (snake_case identifier:
    /// `"lrclib"` / `"genius"` / `"net_ease"` / `"megalobiz"` /
    /// `"musixmatch"`). `None` for embedded / sidecar / manual rows
    /// and for pre-1.5.1 cached entries that pre-date the
    /// `lyrics.provider` column. The UI surfaces this in the source
    /// badge so the user knows whether they're looking at LRCLIB-
    /// curated lyrics or a Genius scrape — important when the latter
    /// occasionally returns junk (issue #284) and the user wants to
    /// re-fetch from a different provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Set by `save_lyrics` when destination = `tag` was requested but
    /// the audio file's tag system can't carry the chosen format (e.g.
    /// TTML in an MP3's ID3v2 where lofty has no mapping for the
    /// XML-friendly `ItemKey::Lyrics`). DB cache is still updated; the
    /// UI surfaces a toast so the user knows the file itself wasn't
    /// touched. Absent on every other return path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag_write_skipped: Option<bool>,
    /// Set by `save_lyrics` when destination = `sidecar` was requested
    /// but the chosen format can't ride a `.lrc` / `.txt` companion
    /// (TTML today — neither extension carries the XML markup the
    /// reader expects). DB cache is still updated. Absent on every
    /// other return path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidecar_write_skipped: Option<bool>,
    /// Translations and pronunciations that came with `content`, as the
    /// provider served them (issue #585). Empty for every source that
    /// only yields one document, which is all of them except a
    /// `waveflow:metadata/v2` plugin today.
    ///
    /// Cached and replaced as a unit with the primary document, so these
    /// always belong to the same fetch — a Musixmatch original can never
    /// be shown next to a leftover Apple translation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub associated: Vec<AssociatedLyrics>,
}

/// One extra document beside the primary lyrics.
///
/// No `rename_all`: [`LyricsPayload`] goes over the wire in snake_case
/// and this rides inside it, so the two have to agree. Every field here
/// happens to be one word, which is exactly how a mismatch would go
/// unnoticed until someone added a two-word one.
#[derive(Debug, Clone, Serialize)]
pub struct AssociatedLyrics {
    /// `"translation"` or `"pronunciation"`.
    pub kind: String,
    /// BCP-47 tag when the provider named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub content: String,
    pub format: LyricsFormat,
}

fn parse_format(s: &str) -> LyricsFormat {
    match s {
        "lrc" => LyricsFormat::Lrc,
        "enhanced_lrc" => LyricsFormat::EnhancedLrc,
        "ttml" => LyricsFormat::Ttml,
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

/// Heuristic format sniffer.
///
/// Order matters: TTML (XML envelope) is checked first because its
/// `<p begin="...">` could otherwise look like nothing else, then
/// Enhanced LRC (LRC with inline `<mm:ss.xx>` word stamps), then
/// plain LRC, then unsynced text.
fn detect_format(content: &str) -> LyricsFormat {
    let head = content.trim_start();

    // TTML: XML declaration, root `<tt`, or the TTML namespace anywhere
    // in the first ~512 bytes. Apple Music's exported lyrics start with
    // `<?xml version="1.0"...`, LyricsX-style exports start with `<tt`.
    let head_lower_prefix: String = head
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    if head_lower_prefix.starts_with("<?xml")
        || head_lower_prefix.starts_with("<tt ")
        || head_lower_prefix.starts_with("<tt>")
        || head_lower_prefix.contains("xmlns=\"http://www.w3.org/ns/ttml\"")
        || head_lower_prefix.contains("<timedtext")
    {
        return LyricsFormat::Ttml;
    }

    // Scan up to 40 lines (first lines may be `[ar:Artist]` / `[ti:…]`
    // LRC headers before the synced body starts).
    let mut has_line_stamp = false;
    let mut has_word_stamp = false;
    for raw in content.lines().take(40) {
        let line = raw.trim_start();
        // Line stamp: `[mm:ss` with both digits present.
        if line.starts_with('[')
            && line.len() >= 7
            && line[1..].chars().take(2).all(|c| c.is_ascii_digit())
            && line.as_bytes().get(3) == Some(&b':')
        {
            has_line_stamp = true;
            // Inline word stamp: `<mm:ss(.xx)?>` somewhere after the
            // first `]`. We scan the byte string directly to keep this
            // cheap for large libraries.
            if let Some(close) = line.find(']') {
                let body = &line[close + 1..];
                if word_stamp_present(body) {
                    has_word_stamp = true;
                    break;
                }
            }
        }
    }

    if has_word_stamp {
        LyricsFormat::EnhancedLrc
    } else if has_line_stamp {
        LyricsFormat::Lrc
    } else {
        LyricsFormat::Plain
    }
}

/// Return true if `s` contains at least one `<\d+:\d+(\.\d+)?>` token —
/// the Enhanced LRC word-stamp shape. Hand-rolled (no regex dep) to
/// keep `detect_format` allocation-free on the hot prefetch path.
fn word_stamp_present(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            // Need at least one digit, then ':', then one digit, then '>'.
            let digits1 = scan_digits(bytes, j);
            if digits1 > 0 {
                j += digits1;
                if bytes.get(j) == Some(&b':') {
                    j += 1;
                    let digits2 = scan_digits(bytes, j);
                    if digits2 > 0 {
                        j += digits2;
                        // Optional fractional `.xx` or `:xx`.
                        if matches!(bytes.get(j), Some(b'.') | Some(b':')) {
                            j += 1;
                            let frac = scan_digits(bytes, j);
                            j += frac;
                        }
                        if bytes.get(j) == Some(&b'>') {
                            return true;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

fn scan_digits(bytes: &[u8], start: usize) -> usize {
    let mut n = 0;
    while let Some(&b) = bytes.get(start + n) {
        if b.is_ascii_digit() {
            n += 1;
        } else {
            break;
        }
    }
    n
}

fn format_to_db(fmt: &LyricsFormat) -> &'static str {
    match fmt {
        LyricsFormat::Plain => "plain",
        LyricsFormat::Lrc => "lrc",
        LyricsFormat::EnhancedLrc => "enhanced_lrc",
        LyricsFormat::Ttml => "ttml",
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

fn external_format_to_app(format: ExternalLyricsFormat) -> LyricsFormat {
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

/// Provider order for the query-based fallback chain that runs after
/// LRCLIB's exact-metadata match has missed.
///
/// **Musixmatch is intentionally absent.** Both [`fetch_lyrics`] and
/// [`run_prefetch`] already invoke Musixmatch on its own dedicated tier
/// (the enhanced word-level lookup, gated on `MUSIXMATCH_ENABLED`). If
/// we listed it here too, every cache miss that fell through the
/// dedicated tier would issue a second Musixmatch request — same
/// endpoint, same query, same token — for a result we already knew
/// wasn't word-level. The fallback chain stays Musixmatch-free; the
/// dedicated tier owns it.
fn external_fallback_providers() -> Vec<Provider> {
    // LRCLIB leads even though tier 5 already asked it, because the two
    // ask *differently*: tier 5 hits `/api/get`, which matches on artist
    // + track + album + duration and 404s when any of them disagrees
    // with the file's tags (a remaster, a "Deluxe" album name, a rip a
    // few seconds off). This chain goes through the provider's
    // `/api/search`, which is fuzzy.
    //
    // Without it, that 404 dropped straight to providers that answer for
    // almost anything — so a track LRCLIB *does* carry came back from
    // Genius, and picking LRCLIB by hand in the panel (fuzzy search)
    // found it immediately. That contradiction is what issue #463
    // reported.
    vec![
        Provider::Lrclib,
        Provider::NetEase,
        Provider::Megalobiz,
        Provider::Genius,
    ]
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
enum SearchOutcome {
    Found(ExternalLyricsResult),
    /// Providers were queried and none had lyrics. A real negative.
    Miss,
    /// Nothing was queried at all — offline mode, or the provider list was
    /// empty once the Musixmatch opt-in filter ran. Never a negative.
    Unavailable,
}

async fn external_lyrics_search(
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
    // A transient provider error surfaces as Err here (never as a miss) so
    // callers don't cache an empty row on a network blip.
    let found = client
        .search(SearchOptions {
            query,
            mode,
            providers,
            enhanced,
            lang,
            genius_cookie: std::env::var("SYNCEDLYRICS_GENIUS_COOKIE").ok(),
            netease_cookie: std::env::var("SYNCEDLYRICS_NETEASE_COOKIE").ok(),
        })
        .await
        .map_err(|err| AppError::Other(format!("external lyrics search failed: {err}")))?;
    // We got here only by actually querying, so `None` is a real negative.
    Ok(match found {
        Some(result) => SearchOutcome::Found(result),
        None => SearchOutcome::Miss,
    })
}

/// `profile_setting` key for the user-picked Musixmatch translation
/// target language. Empty string / absent row → no translation
/// (default). ISO 639-1 lowercase code otherwise (`"en"`, `"fr"`, …).
pub const TRANSLATION_LANG_KEY: &str = "lyrics.translation_lang";

/// Read the per-profile translation language. Returns `None` when
/// the row is absent OR when the value is blank — both surface as
/// "no translation" to the rest of the waterfall.
async fn read_translation_lang(pool: &sqlx::SqlitePool) -> AppResult<Option<String>> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(TRANSLATION_LANG_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(value.filter(|v| !v.trim().is_empty()))
}

async fn cache_external_lyrics(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    file_hash: &str,
    result: ExternalLyricsResult,
) -> AppResult<LyricsPayload> {
    let format = external_format_to_app(result.format);
    let source = LyricsSource::Api;
    let provider = result.provider.as_str();
    upsert_lyrics(
        pool,
        file_hash,
        &result.content,
        &format,
        &source,
        Some(provider),
    )
    .await?;
    Ok(LyricsPayload {
        track_id,
        content: result.content,
        format,
        source,
        provider: Some(provider.to_string()),
        tag_write_skipped: None,
        sidecar_write_skipped: None,
        associated: Vec::new(),
    })
}

/// Marks a `lyrics.provider` value as a plugin id rather than one of the
/// built-in network providers. The two namespaces share a column and
/// nothing else: `Provider::from_id` knows only its own.
const PLUGIN_PROVIDER_PREFIX: &str = "plugin:";

/// Write the primary lyrics row. The one spelling of that statement.
///
/// Takes the connection rather than the pool so both callers can run it
/// inside their own transaction — `upsert_lyrics` pairs it with clearing
/// the bundle, `cache_lyrics_bundle` with inserting a new one, and
/// neither can be allowed to commit half of that.
///
/// `language` is what the SOURCE said, not a user preference: only the
/// v2 plugin world reports one today, and every other tier passes `None`
/// exactly as this row has always been written.
#[allow(clippy::too_many_arguments)]
async fn write_primary_lyrics(
    conn: &mut sqlx::SqliteConnection,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
    provider: Option<&str>,
    language: Option<&str>,
    fetched_at: i64,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app.lyrics (file_hash, content, format, source, provider, language, fetched_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(file_hash) DO UPDATE SET
            content = excluded.content,
            format = excluded.format,
            source = excluded.source,
            provider = excluded.provider,
            language = excluded.language,
            fetched_at = excluded.fetched_at",
    )
    .bind(file_hash)
    .bind(content)
    .bind(format_to_db(format))
    .bind(source_to_db(source))
    .bind(provider)
    .bind(language)
    .bind(fetched_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Map the `waveflow:metadata/v2` format enum onto the host's.
fn plugin_format_to_app(f: PluginLyricsFormat) -> LyricsFormat {
    match f {
        PluginLyricsFormat::Plain => LyricsFormat::Plain,
        PluginLyricsFormat::Lrc => LyricsFormat::Lrc,
        PluginLyricsFormat::EnhancedLrc => LyricsFormat::EnhancedLrc,
        PluginLyricsFormat::Ttml => LyricsFormat::Ttml,
    }
}

/// The whole of the host's trust in a plugin's lyrics document.
///
/// A plugin hands over bytes and a label; nothing parses the document
/// into lines and words on this side, because the renderer already does
/// that and a second model in Rust would only be a second thing to keep
/// correct. So the check is the one that can be made cheaply and here:
/// re-sniff the content and refuse it unless the answer matches what was
/// declared. `detect_format` is the same routine the embedded-tag and
/// sidecar tiers already trust, and it is deliberately conservative —
/// brackets with no timestamp stay `plain` rather than being promoted to
/// `lrc`.
///
/// What this catches: a document truncated in transit, a provider that
/// changed format without saying, an `ttml` label on an HTML error page.
/// What it does not: TTML that parses as XML and means nothing. That
/// failure surfaces in the renderer, which is the only place that could
/// have judged it anyway.
fn document_is_what_it_claims(content: &str, declared: LyricsFormat) -> bool {
    !content.trim().is_empty() && detect_format(content) == declared
}

/// Persist one fetch result — the primary document and everything that
/// came with it — as a single unit.
///
/// The bundle replaces whatever was cached for this file: the associated
/// rows are deleted before the new ones land, inside the same
/// transaction as the primary upsert. Without that, switching providers
/// would leave the previous one's translation sitting under the new
/// original, and the panel would show two documents that were never
/// published together.
///
/// Documents that fail [`document_is_what_it_claims`] are dropped, not
/// fatal: a provider getting one translation wrong should cost that
/// translation, not the lyrics. Each rejection is logged with the label
/// it claimed and the one it sniffed as, because that pair is what
/// identifies the broken provider.
async fn cache_lyrics_bundle(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    file_hash: &str,
    bundle: PluginLyricsBundle,
    plugin_id: &str,
) -> AppResult<Option<LyricsPayload>> {
    let primary_format = plugin_format_to_app(bundle.primary.format);
    if !document_is_what_it_claims(&bundle.primary.content, primary_format) {
        tracing::warn!(
            plugin = %plugin_id,
            declared = ?primary_format,
            sniffed = ?detect_format(&bundle.primary.content),
            "plugin lyrics rejected: primary document is not the format it declared"
        );
        return Ok(None);
    }

    // One document per (kind, language), decided HERE rather than left to
    // the unique index. The insert below is `OR IGNORE`, so the database
    // would keep the first and drop the rest in silence — but the payload
    // returned to the caller is built from this list, so without the same
    // rule the panel would show two French translations now and one after
    // the next reload, with the cache and the response disagreeing about
    // what was fetched.
    //
    // NULL and "no language" are one slot, matching `COALESCE(language,
    // '')` in the index; otherwise two untagged pronunciations would both
    // pass here and only one would survive the write.
    let mut claimed: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let accepted: Vec<AssociatedLyrics> = bundle
        .associated
        .into_iter()
        .filter_map(|a| {
            let format = plugin_format_to_app(a.document.format);
            if !document_is_what_it_claims(&a.document.content, format) {
                tracing::warn!(
                    plugin = %plugin_id,
                    kind = ?a.kind,
                    declared = ?format,
                    sniffed = ?detect_format(&a.document.content),
                    "plugin lyrics: dropping an associated document that is not the format it declared"
                );
                return None;
            }
            let kind = match a.kind {
                PluginAssociatedKind::Translation => "translation".to_string(),
                PluginAssociatedKind::Pronunciation => "pronunciation".to_string(),
            };
            let slot = (
                kind.clone(),
                a.document.language.clone().unwrap_or_default(),
            );
            if !claimed.insert(slot) {
                tracing::warn!(
                    plugin = %plugin_id,
                    %kind,
                    language = ?a.document.language,
                    "plugin lyrics: dropping a second document for a slot already filled"
                );
                return None;
            }
            Some(AssociatedLyrics {
                kind,
                language: a.document.language,
                content: a.document.content,
                format,
            })
        })
        .collect();

    // A plugin id is not a `Provider` id, and both land in the same
    // column. Namespacing it keeps the two apart: `Provider::from_id`
    // can never accidentally match one, the badge can tell the user a
    // plugin answered, and `refetch_lyrics` knows to re-run the plugin
    // tier rather than reject the value as an unknown network provider.
    let provider_id = format!("{PLUGIN_PROVIDER_PREFIX}{plugin_id}");

    let now = now_ms();
    let mut tx = pool.begin().await?;

    write_primary_lyrics(
        &mut tx,
        file_hash,
        &bundle.primary.content,
        &primary_format,
        &LyricsSource::Api,
        Some(&provider_id),
        bundle.primary.language.as_deref(),
        now,
    )
    .await?;

    sqlx::query("DELETE FROM app.lyrics_associated WHERE file_hash = ?")
        .bind(file_hash)
        .execute(&mut *tx)
        .await?;

    for doc in &accepted {
        // A provider returning two documents for the same slot would
        // violate the unique index; the first wins and the rest are
        // skipped rather than failing the whole bundle. Inventing a
        // variant identity for rival versions can wait for a provider
        // that actually produces them.
        sqlx::query(
            "INSERT OR IGNORE INTO app.lyrics_associated
                (file_hash, kind, language, content, format, fetched_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(file_hash)
        .bind(&doc.kind)
        .bind(doc.language.as_deref())
        .bind(&doc.content)
        .bind(format_to_db(&doc.format))
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    // This path writes its primary document itself rather than through
    // `upsert_lyrics`, so it carries the export too (#695).
    export_fetched_sidecar(
        pool,
        file_hash,
        &bundle.primary.content,
        &primary_format,
        &LyricsSource::Api,
    )
    .await;

    Ok(Some(LyricsPayload {
        track_id,
        content: bundle.primary.content,
        format: primary_format,
        source: LyricsSource::Api,
        provider: Some(provider_id),
        tag_write_skipped: None,
        sidecar_write_skipped: None,
        associated: accepted,
    }))
}

/// Read a cached bundle's associated documents.
async fn read_associated(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> AppResult<Vec<AssociatedLyrics>> {
    let rows: Vec<(String, Option<String>, String, String)> = sqlx::query_as(
        "SELECT a.kind, a.language, a.content, a.format
           FROM track t
           JOIN app.lyrics_associated a ON a.file_hash = t.file_hash
          WHERE t.id = ?
          ORDER BY a.kind, a.language",
    )
    .bind(track_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(kind, language, content, format)| AssociatedLyrics {
            kind,
            language,
            content,
            format: parse_format(&format),
        })
        .collect())
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
    /// Every provider answered and none had lyrics. A real negative, safe
    /// to remember so the panel stops re-hitting the network on each open.
    Miss,
    /// The chain could not be consulted at all. NOT a negative — nothing
    /// may be cached, so the next attempt retries.
    Unavailable,
}

/// Walk the post-LRCLIB fallback chain (NetEase / Megalobiz / Genius) and
/// persist the first hit.
///
/// A provider `Err` stays non-fatal for the waterfall — LRCLIB has already
/// given its verdict and this chain is best-effort enrichment, so we don't
/// fail the whole lookup. But it now surfaces as [`FallbackOutcome::Unavailable`]
/// rather than masquerading as a miss, so the caller knows not to persist
/// anything.
async fn try_external_fallback(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
) -> AppResult<FallbackOutcome> {
    match external_lyrics_search(
        meta,
        external_fallback_providers(),
        SearchMode::PreferSynced,
        true,
        // Fallback chain excludes Musixmatch — `external_lyrics_search`
        // would drop the lang anyway. Keep it `None` to flag intent.
        None,
    )
    .await
    {
        Ok(SearchOutcome::Found(result)) => Ok(FallbackOutcome::Found(
            cache_external_lyrics(pool, track_id, &meta.file_hash, result).await?,
        )),
        // Propagated straight through: only the search itself knows whether
        // a request was issued, so nothing here re-derives it.
        Ok(SearchOutcome::Miss) => Ok(FallbackOutcome::Miss),
        Ok(SearchOutcome::Unavailable) => Ok(FallbackOutcome::Unavailable),
        Err(err) => {
            tracing::debug!(?err, "external fallback chain failed; not caching a miss");
            Ok(FallbackOutcome::Unavailable)
        }
    }
}

/// Cache an empty "miss" row so the panel doesn't re-hit the network on
/// every open, and return it as a payload. The user can force a
/// re-search via "Refetch" in the lyrics panel (clears the row, re-runs
/// the waterfall). `provider = None` — attribution to a specific provider
/// would be misleading when every provider returned nothing.
async fn cache_lyrics_miss(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
) -> AppResult<LyricsPayload> {
    let empty = String::new();
    upsert_lyrics(
        pool,
        &meta.file_hash,
        &empty,
        &LyricsFormat::Plain,
        &LyricsSource::Api,
        None,
    )
    .await?;
    Ok(LyricsPayload {
        track_id,
        content: empty,
        format: LyricsFormat::Plain,
        source: LyricsSource::Api,
        provider: None,
        tag_write_skipped: None,
        sidecar_write_skipped: None,
        associated: Vec::new(),
    })
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
async fn resolve_after_lrclib_miss(
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
        FallbackOutcome::Miss => cache_lyrics_miss(pool, track_id, meta).await.map(Some),
        // Transient: persist nothing so the next attempt can retry.
        FallbackOutcome::Unavailable => Ok(None),
        // Handled above.
        FallbackOutcome::Found(_) => unreachable!("Found returns early"),
    }
}

/// `profile_setting` key: prefer the online providers (LRCLIB / Musixmatch
/// / fallback chain) over the track's own embedded + sidecar lyrics.
/// Default off — the local tiers win, as they historically have.
pub const PREFER_LRCLIB_KEY: &str = "lyrics.prefer_lrclib";

/// Read the per-profile prefer-LRCLIB flag. A missing row / blank / any
/// non-truthy value → `false` (local-first, the historical default). A
/// SQL error propagates rather than masquerading as "disabled" so the
/// caller (and the Settings toggle) sees a real read failure.
async fn read_prefer_lrclib(pool: &sqlx::SqlitePool) -> AppResult<bool> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(PREFER_LRCLIB_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(value.map(|v| v == "true" || v == "1").unwrap_or(false))
}

/// Local tiers, in order: embedded lyrics tag → sidecar `.lrc`/`.txt`
/// → the generic description field. Returns the first hit, already
/// cached, or `None` when none is present. Shared by the default
/// (local-first) order and the `lyrics.prefer_lrclib`
/// (local-as-fallback) order so the two never drift on how a local hit
/// is read + persisted.
///
/// The description comes last, and that ordering is the fix for a
/// reported bug rather than a detail: it is the one tier that is a
/// guess about a field meant for something else, and it used to sit
/// inside the embedded tier ahead of the sidecar — so a `yt-dlp` rip
/// showed its YouTube blurb and never read the `.lrc` sitting beside
/// it.
async fn try_local_lyrics(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
) -> AppResult<Option<LyricsPayload>> {
    // Embedded tag. Lofty I/O is blocking — push to spawn_blocking.
    let path_clone = meta.file_path.clone();
    let embedded =
        tokio::task::spawn_blocking(move || read_embedded_lyrics(Path::new(&path_clone)))
            .await
            .ok()
            .flatten();
    if let Some(content) = embedded {
        let format = detect_format(&content);
        let source = LyricsSource::Embedded;
        upsert_lyrics(pool, &meta.file_hash, &content, &format, &source, None).await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format,
            source,
            provider: None,
            tag_write_skipped: None,
            sidecar_write_skipped: None,
            associated: Vec::new(),
        }));
    }

    // Local sidecar `.lrc` / `.txt`. Cheap (a couple of stat calls + at
    // most two `read_dir` scans).
    let path_for_sidecar = meta.file_path.clone();
    let sidecar =
        tokio::task::spawn_blocking(move || read_sidecar_lyrics(Path::new(&path_for_sidecar)))
            .await
            .ok()
            .flatten();
    if let Some(content) = sidecar {
        let format = detect_format(&content);
        let source = LyricsSource::LrcFile;
        upsert_lyrics(pool, &meta.file_hash, &content, &format, &source, None).await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format,
            source,
            provider: None,
            tag_write_skipped: None,
            sidecar_write_skipped: None,
            associated: Vec::new(),
        }));
    }

    // Last: the generic description field, which is a guess about a
    // field meant for something else. Reached only when the track
    // carries no real lyrics tag and the user has placed no sidecar.
    let path_for_description = meta.file_path.clone();
    let description = tokio::task::spawn_blocking(move || {
        read_description_lyrics(Path::new(&path_for_description))
    })
    .await
    .ok()
    .flatten();
    if let Some(content) = description {
        let format = detect_format(&content);
        let source = LyricsSource::Embedded;
        upsert_lyrics(pool, &meta.file_hash, &content, &format, &source, None).await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format,
            source,
            provider: None,
            tag_write_skipped: None,
            sidecar_write_skipped: None,
            associated: Vec::new(),
        }));
    }

    Ok(None)
}

/// TXXX (ID3v2) / Vorbis-comment descriptions carrying **synced** lyrics,
/// most-preferred first. Antra and similar rips ship a timestamped LRC
/// body under `SYNCEDLYRICS` alongside a plain `LYRICS`/`UNSYNCEDLYRICS`
/// tag; we want the synced one to win (issue #378).
const SYNCED_LYRICS_KEYS: &[&str] = &[
    "SYNCEDLYRICS",
    "SYNCED LYRICS",
    "SYNCED_LYRICS",
    "LYRICS_SYNCED",
];

/// TXXX (ID3v2) / Vorbis-comment descriptions carrying plain / unsynced
/// lyrics — the fallback once no synced tag is present.
const UNSYNCED_LYRICS_KEYS: &[&str] = &[
    "UNSYNCEDLYRICS",
    "UNSYNCED LYRICS",
    "UNSYNCED_LYRICS",
    "LYRICS_UNSYNCED",
    "LYRICS",
];

/// Re-open an MP3 as a typed `Id3v2Tag` and pull the lyrics out of the
/// first TXXX user-defined frame whose description matches one of
/// `descriptions` (in order).
///
/// Required because the generic `Tag` interface returned by
/// `read_from_path` doesn't expose unmapped TXXX frames.
fn read_id3v2_txxx(path: &Path, descriptions: &[&str]) -> Option<String> {
    use lofty::config::ParseOptions;
    use lofty::id3::v2::Id3v2Tag;
    use lofty::mpeg::MpegFile;

    let mut file = std::fs::File::open(path).ok()?;
    let mpeg =
        <MpegFile as lofty::file::AudioFile>::read_from(&mut file, ParseOptions::new()).ok()?;
    let tag: &Id3v2Tag = mpeg.id3v2()?;

    for description in descriptions {
        if let Some(s) = tag.get_user_text(description) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Pull lyrics from a Vorbis-comment field (`SYNCEDLYRICS`, …) on the
/// container types that use them — FLAC, Ogg Vorbis, Opus, Speex.
/// `VorbisComments::get` matches keys case-insensitively. The generic
/// `Tag` drops these because lofty has no `ItemKey` for them, so we read
/// the concrete tag directly, mirroring the ID3v2 TXXX path.
fn read_vorbis_comment(path: &Path, file_type: FileType, keys: &[&str]) -> Option<String> {
    use lofty::config::ParseOptions;
    use lofty::file::AudioFile;

    fn pick(comments: &lofty::ogg::tag::VorbisComments, keys: &[&str]) -> Option<String> {
        for key in keys {
            if let Some(s) = comments.get(key) {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    let mut file = std::fs::File::open(path).ok()?;
    match file_type {
        FileType::Flac => {
            use lofty::flac::FlacFile;
            let f = FlacFile::read_from(&mut file, ParseOptions::new()).ok()?;
            pick(f.vorbis_comments()?, keys)
        }
        FileType::Vorbis => {
            use lofty::ogg::VorbisFile;
            let f = VorbisFile::read_from(&mut file, ParseOptions::new()).ok()?;
            pick(f.vorbis_comments(), keys)
        }
        FileType::Opus => {
            use lofty::ogg::OpusFile;
            let f = OpusFile::read_from(&mut file, ParseOptions::new()).ok()?;
            pick(f.vorbis_comments(), keys)
        }
        FileType::Speex => {
            use lofty::ogg::SpeexFile;
            let f = SpeexFile::read_from(&mut file, ParseOptions::new()).ok()?;
            pick(f.vorbis_comments(), keys)
        }
        _ => None,
    }
}

/// Read a custom (non-`ItemKey`) lyrics tag by `keys` across every
/// container we support: ID3v2 TXXX for MP3, Vorbis comments for
/// FLAC/Ogg/Opus/Speex. Used for the `SYNCEDLYRICS` priority tag and the
/// TXXX unsynced fallback.
fn read_custom_lyrics_tag(
    path: &Path,
    file_type: Option<FileType>,
    keys: &[&str],
) -> Option<String> {
    match file_type {
        Some(FileType::Mpeg) => read_id3v2_txxx(path, keys),
        Some(ft @ (FileType::Flac | FileType::Vorbis | FileType::Opus | FileType::Speex)) => {
            read_vorbis_comment(path, ft, keys)
        }
        _ => None,
    }
}

/// Read the embedded lyrics tag. Lookup order:
///   1. `SYNCEDLYRICS` custom tag — TXXX (ID3v2) / Vorbis comment. Timed
///      LRC body written by Antra & similar; wins because it carries the
///      timestamps a plain lyrics tag doesn't (issue #378).
///   2. `ItemKey::UnsyncLyrics` — `USLT` (ID3v2), `UNSYNCEDLYRICS`
///      (Vorbis), `©lyr` (MP4)
///   3. `ItemKey::Lyrics` — `LYRICS` (Vorbis), `©lyr` (MP4). Not
///      supported by ID3v2 in lofty.
///   4. TXXX / Vorbis custom `LYRICS` / `UNSYNCEDLYRICS` frames (legacy
///      Mp3tag / foobar2000 / lame --tg output common on K-Pop / J-Pop
///      rips) that the generic tag doesn't surface.
///
/// The generic `Description` field used to be a fifth tier here. It is
/// not a lyrics field, and reading it as one cost a real user their
/// lyrics: a `.m4a` pulled with `yt-dlp` carries the "Provided to
/// YouTube by…" blurb in `description`, which is comfortably more than
/// three lines, so it won — and because the embedded tier runs before
/// the sidecar, the `.lrc` they had put next to the file was never
/// read. It now lives in [`read_description_lyrics`], behind the
/// sidecar, so a file the user placed deliberately always beats a
/// field we are guessing about.
fn read_embedded_lyrics(path: &Path) -> Option<String> {
    let probe = Probe::open(path).ok()?.guess_file_type().ok()?;
    let file_type = probe.file_type();
    let tagged = probe.read().ok()?;

    // Synced tag takes priority over every plain-lyrics source, including
    // the standard USLT/Lyrics keys.
    let from_synced = read_custom_lyrics_tag(path, file_type, SYNCED_LYRICS_KEYS);

    // Each standard key is validated non-blank at its source so a
    // whitespace-only USLT falls through to a valid LYRICS (and, when both
    // are blank, doesn't mask the custom-tag / Description fallback below).
    let from_known_key = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(known_key_lyrics);

    // Generic Tag wraps the underlying Id3v2Tag for MP3s, but the
    // SplitAndMergeTag conversion drops unknown TXXX frames (and lofty
    // has no `ItemKey` for these Vorbis keys either). Re-read the concrete
    // tag for `TXXX:LYRICS` / `TXXX:UNSYNCEDLYRICS` only when the standard
    // frames came up empty.
    let from_custom_unsynced = if from_synced.is_none() && from_known_key.is_none() {
        read_custom_lyrics_tag(path, file_type, UNSYNCED_LYRICS_KEYS)
    } else {
        None
    };

    resolve_embedded_lyrics([from_synced, from_known_key, from_custom_unsynced])
}

/// The local tiers below the embedded tag, in order: sidecar first,
/// then the generic description field.
///
/// Exists because [`run_prefetch`] walks the same waterfall as
/// [`try_local_lyrics`] but spells it out itself, and the description
/// tier was silently lost from it when it moved out of
/// [`read_embedded_lyrics`] — a second caller of a function whose
/// contract had changed. One helper now holds the order, so the two
/// cannot disagree about it again.
fn read_local_after_embedded(path: &Path) -> Option<(String, LyricsSource)> {
    if let Some(content) = read_sidecar_lyrics(path) {
        return Some((content, LyricsSource::LrcFile));
    }
    read_description_lyrics(path).map(|content| (content, LyricsSource::Embedded))
}

/// Last-resort lyrics from the generic `Description` field, read only
/// once the real lyrics tags and the sidecar have both come up empty.
///
/// Some rips really do put lyrics there, which is why this survives at
/// all. But a description field is a description field: the length
/// check below is the whole of what separates "someone pasted lyrics
/// into it" from "this is a sleeve note", and it is a guess. Ranking
/// it under the sidecar is what makes the guess safe — a `.lrc` the
/// user put next to the track is a statement, not a guess.
fn read_description_lyrics(path: &Path) -> Option<String> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let text = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(|tag| {
            #[allow(deprecated)]
            tag.get_string(ItemKey::Description)
                .filter(|s| s.lines().count() > 3)
                .map(|s| s.to_string())
        })?;
    if is_service_blurb(&text) {
        return None;
    }
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Whether a description is a distribution service's boilerplate
/// rather than anything a listener wants to read along to.
///
/// Narrow on purpose. This is not an attempt to tell prose from verse
/// — it recognises the one blurb that reliably fills this field on
/// files people actually have, the auto-generated YouTube credit that
/// `yt-dlp` copies into `description`. Anything it does not recognise
/// still gets through, because the cost of a false positive here is
/// losing real lyrics.
fn is_service_blurb(text: &str) -> bool {
    // Only the two YouTube lines identify the blurb on their own.
    // "Released on:" was in this list and should not have been: it
    // appears in perfectly ordinary sleeve notes, and on its own it
    // would throw away lyrics someone had written into the field.
    const MARKERS: &[&str] = &["provided to youtube by", "auto-generated by youtube"];
    let head: String = text.chars().take(400).collect::<String>().to_lowercase();
    MARKERS.iter().any(|marker| head.contains(marker))
}

/// Pick the first non-blank value among the standard lyric keys on a tag:
/// `UnsyncLyrics` (USLT), then `Lyrics`. Each is validated at its source
/// so a whitespace-only `UnsyncLyrics` falls through to a valid `Lyrics`
/// rather than winning and collapsing to nothing.
fn known_key_lyrics(tag: &lofty::tag::Tag) -> Option<String> {
    tag.get_string(ItemKey::UnsyncLyrics)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            tag.get_string(ItemKey::Lyrics)
                .filter(|s| !s.trim().is_empty())
        })
        .map(|s| s.to_string())
}

/// Choose the embedded lyrics body from the candidate sources in priority
/// order (synced → standard USLT/Lyrics → custom TXXX/Vorbis),
/// trimming each and treating blank/whitespace-only values as absent so a
/// stale empty tag can never win over a later valid one.
fn resolve_embedded_lyrics(candidates: [Option<String>; 3]) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

/// Match a sidecar lyrics file on disk for an audio track.
///
/// Looks for `{stem}.lrc` / `{stem}.txt` (case-insensitive on both
/// the stem and the extension) either next to the audio file or inside
/// a sibling `Lyrics/` directory. Returns the file contents and which
/// flavour matched so the caller can pick a sensible format default.
///
/// Common K-Pop / J-Pop rip layouts ship synced lyrics as sidecars
/// rather than embedded tags, and the user may also keep them in a
/// `Lyrics/` subfolder to declutter the listing. Both layouts are
/// supported here.
///
/// Preference order at every directory we probe:
///   1. `.lrc` (carries line-level timing)
///   2. `.txt` (plain text fallback)
///
/// Same-folder hits always beat `Lyrics/` hits because users who
/// duplicate lyrics in both spots almost certainly want the same-
/// folder copy as the primary.
fn read_sidecar_lyrics(audio_path: &Path) -> Option<String> {
    let stem = audio_path.file_stem()?.to_str()?;
    let parent = audio_path.parent()?;

    if let Some(content) = read_stem_match_in_dir(parent, stem) {
        return Some(content);
    }

    // Sibling `Lyrics/` (or any case variant). Iterate the parent
    // directory once and probe the first directory whose name
    // case-insensitively matches "lyrics".
    for entry in std::fs::read_dir(parent).ok()?.flatten() {
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.eq_ignore_ascii_case("lyrics") {
            continue;
        }
        if let Some(content) = read_stem_match_in_dir(&entry.path(), stem) {
            return Some(content);
        }
    }

    None
}

/// Inner helper for [`read_sidecar_lyrics`]: scan `dir` once, prefer
/// `.lrc` over `.txt`. Stem matching is case-insensitive so a Windows
/// rip with `Song.MP3` still finds `song.lrc` cleanly on Linux.
fn read_stem_match_in_dir(dir: &Path, stem: &str) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut lrc_match: Option<std::path::PathBuf> = None;
    let mut txt_match: Option<std::path::PathBuf> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip directories early. Without this a directory named
        // `Song.lrc` would be picked into `lrc_match`, `read_to_string`
        // below would fail, and a legitimate `Song.txt` in the same
        // directory would be silently masked. `is_file` follows
        // symlinks, so a symlinked sidecar still works.
        if !path.is_file() {
            continue;
        }
        let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !file_stem.eq_ignore_ascii_case(stem) {
            continue;
        }
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        {
            Some(ref ext) if ext == "lrc" => lrc_match = Some(path),
            Some(ref ext) if ext == "txt" => txt_match = Some(path),
            _ => {}
        }
    }
    // Try .lrc first (synced wins), then .txt — but skip whichever
    // candidate turns out to be empty / whitespace-only on disk.
    // Without this fallback an empty `Song.lrc` (common in low-quality
    // rips that ship a stub file) would silently mask a valid
    // `Song.txt` next to it.
    lrc_match
        .as_deref()
        .and_then(read_non_empty_file)
        .or_else(|| txt_match.as_deref().and_then(read_non_empty_file))
}

/// Read a text file and return its trimmed contents, or `None` if
/// the file is missing, unreadable, or contains only whitespace.
fn read_non_empty_file(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Insert (or replace) the lyrics row, keyed by file content hash so the
/// cache is shared across profiles that contain the same audio file.
///
/// `provider` carries the sub-source identifier when `source` is
/// `LyricsSource::Api` (e.g. `"lrclib"`, `"genius"`) and `None` for
/// embedded / sidecar / manual writes where the broad `source` is
/// the only meaningful attribution. The DB column allows NULL so
/// pre-1.5.1 rows + non-API tiers store cleanly.
async fn upsert_lyrics(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
    provider: Option<&str>,
) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    write_primary_lyrics(
        &mut tx,
        file_hash,
        content,
        format,
        source,
        provider,
        None,
        now_ms(),
    )
    .await?;
    // Replacing the primary document ends the bundle it belonged to.
    // Translations and pronunciations are cached as part of ONE fetch
    // result (issue #585), so leaving them behind would pair new lyrics
    // with the previous provider's companions — and nothing downstream
    // could tell they were never published together. Every tier that
    // writes a primary comes through here, so the invariant holds
    // without each of them having to remember it.
    sqlx::query("DELETE FROM app.lyrics_associated WHERE file_hash = ?")
        .bind(file_hash)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    // After the commit, never inside the transaction: a sidecar that
    // could not be written must not cost the user the lyrics themselves
    // (#695).
    export_fetched_sidecar(pool, file_hash, content, format, source).await;
    Ok(())
}

/// Also write freshly fetched lyrics next to the audio file, when the
/// user asked for sidecars (issue #695).
///
/// Until now the destination chosen in onboarding / Settings governed
/// lyrics the user **typed**; anything fetched from LRCLIB, Musixmatch or
/// a plugin stayed in `app.lyrics` alone, so a library carefully set to
/// "sidecar file" still ended up with its fetched lyrics locked inside
/// WaveFlow's database. Now they travel with the music: a reinstall, a
/// copy to another machine, another player reading the same folder.
///
/// Four rules, each of them a refusal:
///
/// - **Only what came off the network.** `Embedded` and `LrcFile` are
///   already on disk, and `Manual` goes through [`save_lyrics`], which
///   asks where that one edit should land and honours all three answers.
/// - **`Tag` is deliberately not honoured here.** Writing a tag rewrites
///   the audio file, and the file a fetch is about is usually the one
///   playing — which is why the tag editor pauses playback and re-hashes
///   ([`invariants.md`](../../../../../docs/architecture/invariants.md#file-write-safety-on-windows)).
///   A fetch that happened on its own must not do that behind the user's
///   back, so the cache row carries it and the editor stays the way
///   lyrics reach a tag.
/// - **Never overwrite.** A sidecar already there is the user's, or an
///   earlier export's. In the usual order this cannot even arise — the
///   local tier would have found it and the source would be `LrcFile` —
///   but under `lyrics.prefer_lrclib` the network runs first.
/// - **A failure is not an error.** A read-only library folder, a NAS
///   that went away, a filename the filesystem refuses: the database row
///   is the source of truth and lyrics must still show. Logged, swallowed.
async fn export_fetched_sidecar(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    content: &str,
    format: &LyricsFormat,
    source: &LyricsSource,
) {
    if !matches!(source, LyricsSource::Api) {
        return;
    }
    // An empty row is the instrumental marker, not lyrics to carry.
    if content.trim().is_empty() {
        return;
    }
    // TTML's XML rides neither extension the waterfall reader accepts,
    // the same reason `save_lyrics` reports a skip for it.
    if matches!(format, LyricsFormat::Ttml) {
        return;
    }
    match read_default_destination(pool).await {
        Ok(LyricsDestination::Sidecar) => {}
        Ok(_) => return,
        Err(err) => {
            tracing::debug!(?err, "lyrics sidecar export: destination unreadable");
            return;
        }
    }

    // The cache is keyed by content hash, which can name several files —
    // the same track twice in one library, or a copy the user keeps
    // elsewhere. Each copy is a file the user may open in another player,
    // so each gets its own sidecar; in practice there is one row.
    let paths: Vec<String> =
        match sqlx::query_scalar("SELECT file_path FROM track WHERE file_hash = ?")
            .bind(file_hash)
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::debug!(?err, "lyrics sidecar export: track lookup failed");
                return;
            }
        };

    let plain = matches!(format, LyricsFormat::Plain);
    for path in paths {
        let content = content.to_string();
        let _ = tokio::task::spawn_blocking(move || {
            let audio = Path::new(&path);
            let Some(sidecar) = sidecar_path(audio, plain) else {
                return;
            };
            // Ask the reader the waterfall itself uses, rather than probing
            // one exact path: it matches the stem case-insensitively and
            // looks in a sibling `Lyrics/` folder too. Probing
            // `Song.lrc` alone would write a second file beside a
            // `song.LRC` the user made, on any case-sensitive filesystem --
            // and a file that supersedes theirs is the thing this refuses
            // to do, whatever it is called.
            if read_sidecar_lyrics(audio).is_some() {
                return;
            }
            // `create_new` rather than `write`, for the two files that
            // reader says nothing about: one that is empty or all
            // whitespace (it reports those as misses, and truncating one
            // would still be writing over something we did not create),
            // and one that appears between the check above and this line.
            // The kernel decides, so there is no window left to lose.
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&sidecar);
            let mut file = match created {
                Ok(file) => file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return,
                Err(err) => {
                    tracing::debug!(
                        %err,
                        path = %sidecar.display(),
                        "lyrics sidecar export failed"
                    );
                    return;
                }
            };
            if let Err(err) = std::io::Write::write_all(&mut file, content.as_bytes()) {
                tracing::debug!(
                    %err,
                    path = %sidecar.display(),
                    "lyrics sidecar export failed"
                );
            } else {
                tracing::debug!(path = %sidecar.display(), "exported fetched lyrics");
            }
        })
        .await;
    }
}

/// Read the cached lyrics row, if any. The frontend identifies tracks by
/// numeric `track_id` so we look up the file hash first, then key into the
/// shared `app.lyrics` cache.
async fn read_cached(pool: &sqlx::SqlitePool, track_id: i64) -> AppResult<Option<LyricsPayload>> {
    let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT l.content, l.format, l.source, l.provider
           FROM track t
           JOIN app.lyrics l ON l.file_hash = t.file_hash
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;

    // A cached row that is a distribution service's credit rather than
    // lyrics is dropped and re-resolved, rather than served forever.
    //
    // The predicate keys on `source = 'embedded'`, which is also what
    // the description tier stores under — `lyrics.source` is
    // CHECK-constrained to four values, and giving the description its
    // own would mean rebuilding a shared cache table for a
    // classification nicety. The practical consequence is one absurd
    // edge: a genuine `USLT` tag whose opening really is a YouTube
    // credit gets dropped and re-read from the file on every panel
    // open. The listener sees the same text either way; it costs one
    // file read.
    //
    // Only here, and deliberately: `run_prefetch` picks its work with
    // `WHERE l.file_hash IS NULL`, so it still skips a track holding
    // one of these rows. Teaching it otherwise would mean spelling the
    // recogniser a second time in SQL, and two spellings of one
    // predicate drift. The healing happens when the panel is opened,
    // which is the moment the wrong text would have been read.
    //
    // Fixing `read_description_lyrics` alone would only have helped
    // tracks nobody had opened yet: the waterfall never refetches once
    // a row exists, so the person who reported this would have seen
    // nothing change. The row is *provably* not lyrics — it is the
    // auto-generated YouTube credit — which is what makes deleting it
    // safe rather than presumptuous, and it costs one string scan on a
    // path that already does a join.
    if let Some((content, _, source, _)) = row.as_ref() {
        if source == "embedded" && is_service_blurb(content) {
            let _ = sqlx::query(
                "DELETE FROM app.lyrics
                  WHERE file_hash = (SELECT file_hash FROM track WHERE id = ?)",
            )
            .bind(track_id)
            .execute(pool)
            .await;
            tracing::info!(
                track_id,
                "dropped a cached service credit that had been stored as lyrics"
            );
            return Ok(None);
        }
    }

    let Some((content, fmt, src, provider)) = row else {
        return Ok(None);
    };

    // The companions are read separately rather than joined in above: a
    // bundle has zero rows in the common case, and a LEFT JOIN would
    // multiply the primary row by however many documents came with it
    // only to collapse it again here.
    //
    // Nothing needs to filter out stale ones. They cascade with the
    // primary — including on the service-credit delete just above — and
    // every write of a primary clears them, so a row that exists here
    // was cached by the same fetch as the content beside it.
    let associated = read_associated(pool, track_id).await?;

    Ok(Some(LyricsPayload {
        track_id,
        content,
        format: parse_format(&fmt),
        source: parse_source(&src),
        provider,
        tag_write_skipped: None,
        sidecar_write_skipped: None,
        associated,
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

/// Multi-tier lookup: cache → embedded tag → sidecar → enhanced/API
/// providers. Caches the first hit and returns it. Returns `None` if
/// local tiers fail and offline mode prevents network lookup.
/// How long one plugin gets to answer a lyrics lookup.
///
/// Matches the motion-artwork budget: the call is a network round-trip
/// inside a wasm guest, and the panel is waiting on it.
const LYRICS_PLUGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// What the plugin tier cached: the payload it returned, and the bundle
/// and plugin id it came from, kept so the same answer can be cached again.
struct PluginAnswer {
    payload: LyricsPayload,
    bundle: PluginLyricsBundle,
    plugin_id: String,
}

/// Lyrics the renderer can follow: LRC or Enhanced LRC with at least one
/// complete line stamp, or a well-formed TTML document with at least one
/// `<p>` whose `begin` it can read. Each rule mirrors the frontend parser,
/// because an answer judged synced here ends the waterfall, and one the
/// renderer then cannot follow is static text that beat synced lyrics.
/// Apple serves lyrics it has no timing for as TTML too, with
/// `itunes:timing="None"` and bare lines.
fn lyrics_are_synced(format: &LyricsFormat, content: &str) -> bool {
    match format {
        LyricsFormat::Lrc | LyricsFormat::EnhancedLrc => lrc_has_line_stamp(content),
        LyricsFormat::Plain => false,
        LyricsFormat::Ttml => ttml_has_timed_line(content),
    }
}

/// `LRC_LINE_STAMP_RE` in `src/lib/tauri/lyrics.ts`: the renderer keeps a
/// line only when it carries a complete stamp of this shape.
static LRC_LINE_STAMP: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]").expect("static pattern")
});

fn lrc_has_line_stamp(content: &str) -> bool {
    LRC_LINE_STAMP.is_match(content)
}

fn ttml_has_timed_line(content: &str) -> bool {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(content);
    let mut depth: usize = 0;
    let mut timed = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                match is_timed_line(&e) {
                    Some(t) => timed |= t,
                    None => return false,
                }
            }
            Ok(Event::Empty(e)) => match is_timed_line(&e) {
                Some(t) => timed |= t,
                None => return false,
            },
            Ok(Event::End(_)) => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            Ok(Event::Eof) => return timed && depth == 0,
            Err(_) => return false,
            _ => {}
        }
    }
}

/// Whether this element is a `<p>` with a `begin` the renderer can read,
/// or `None` when one of its attributes is malformed (a duplicate, a
/// missing quote). Every element's attributes are checked, not only the
/// lines': `DOMParser` rejects the whole document for any of them.
fn is_timed_line(e: &quick_xml::events::BytesStart<'_>) -> Option<bool> {
    let is_line = e.local_name().into_inner() == "p";
    let mut timed = false;
    for attr in e.attributes() {
        let attr = attr.ok()?;
        if is_line
            && attr.key.local_name().into_inner() == "begin"
            && ttml_time_ms(&attr.value).is_some_and(|ms| ms >= -0.5)
        {
            timed = true;
        }
    }
    Some(timed)
}

/// `parseTtmlTime` in `src/lib/tauri/lyrics.ts`, which drops a line whose
/// `begin` it cannot read or that rounds below zero.
fn ttml_time_ms(value: &str) -> Option<f64> {
    // `Number("")` is 0 in JavaScript, which the renderer inherits.
    fn number(s: &str) -> Option<f64> {
        let t = s.trim();
        if t.is_empty() {
            return Some(0.0);
        }
        t.parse::<f64>().ok().filter(|n| n.is_finite())
    }
    let s = value.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(n) = s.strip_suffix("ms") {
        return number(n);
    }
    if let Some(n) = s.strip_suffix('s') {
        return number(n).map(|v| v * 1000.0);
    }
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        return match parts.as_slice() {
            [m, sec] => Some(number(m)? * 60_000.0 + number(sec)? * 1000.0),
            [h, m, sec] => {
                Some(number(h)? * 3_600_000.0 + number(m)? * 60_000.0 + number(sec)? * 1000.0)
            }
            _ => None,
        };
    }
    number(s).map(|v| v * 1000.0)
}

/// Ask every enabled `waveflow:metadata/v2` plugin for this track, and
/// cache the first bundle that survives validation.
///
/// Fan-out rather than a chain: plugins are independent, so they are all
/// asked at once and the first usable answer wins. A plugin that traps,
/// times out, or returns a document that is not the format it claimed is
/// skipped — one bad provider must not deny the tier.
///
/// `Ok(None)` means nothing was cached and the caller should carry on
/// down the waterfall. Nothing here writes a miss: a plugin having no
/// lyrics says nothing about whether LRCLIB does.
///
/// The answer carries the bundle it cached as well as the payload, so
/// `fetch_lyrics` can write it back after trying the network tiers for
/// something better (#668).
async fn try_plugin_lyrics(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
    only: Option<&str>,
) -> AppResult<Option<PluginAnswer>> {
    let mut plugin_ids = super::plugins::enabled_plugin_ids_for_world(
        state,
        waveflow_core::plugin::worlds::METADATA_V2,
    )
    .await?;
    // `refetch_lyrics` pins the plugin that answered last time. Filtering
    // the enumeration rather than taking the id on trust means a plugin
    // that has since been disabled or uninstalled simply drops out, and
    // the caller gets the same "nothing found" it would get for any other
    // silent provider — no separate not-installed path to keep correct.
    if let Some(wanted) = only {
        plugin_ids.retain(|id| id == wanted);
    }
    if plugin_ids.is_empty() {
        return Ok(None);
    }

    let artist = meta.artist_name.clone().unwrap_or_default();
    let title = meta.title.clone();

    let mut set = tokio::task::JoinSet::new();
    for plugin_id in plugin_ids {
        // Take the lock HANDLE here (a fast map op) and acquire the guard
        // inside the blocking closure, so it spans the real work: the
        // guest call is uncancellable, and an early drop on timeout would
        // otherwise let an enable/uninstall race a call still running.
        let lock_arc = super::plugins::plugin_lock_arc(state, &plugin_id).await;
        let runtime = state.plugins.clone();
        let paths = state.paths.plugin_paths();
        let id_owned = plugin_id.clone();
        let artist_owned = artist.clone();
        let title_owned = title.clone();

        set.spawn(async move {
            let outcome = tokio::time::timeout(
                LYRICS_PLUGIN_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    let _guard = lock_arc.blocking_lock_owned();
                    waveflow_core::plugin::runtime::metadata_v2_lyrics(
                        &runtime,
                        &paths,
                        &id_owned,
                        &artist_owned,
                        &title_owned,
                    )
                }),
            )
            .await;
            (plugin_id, outcome)
        });
    }

    while let Some(joined) = set.join_next().await {
        let (plugin_id, outcome) = match joined {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(%e, "plugin lyrics task join failed; skipping");
                continue;
            }
        };
        let bundle = match outcome {
            Ok(Ok(Ok(Some(bundle)))) => bundle,
            // The plugin answered and has nothing for this track.
            Ok(Ok(Ok(None))) => continue,
            Ok(Ok(Err(err))) if err.is_load_failure() => {
                tracing::warn!(
                    plugin = %plugin_id,
                    err = %err.detail(),
                    "plugin lyrics: the plugin could not be loaded"
                );
                continue;
            }
            Ok(Ok(Err(err))) => {
                tracing::debug!(
                    plugin = %plugin_id,
                    err = %err.detail(),
                    "plugin lyrics lookup failed"
                );
                continue;
            }
            Ok(Err(e)) => {
                tracing::warn!(plugin = %plugin_id, %e, "plugin lyrics task panicked");
                continue;
            }
            Err(_) => {
                tracing::warn!(plugin = %plugin_id, "plugin lyrics lookup timed out");
                continue;
            }
        };

        if let Some(payload) =
            cache_lyrics_bundle(pool, track_id, &meta.file_hash, bundle.clone(), &plugin_id).await?
        {
            return Ok(Some(PluginAnswer {
                payload,
                bundle,
                plugin_id,
            }));
        }
        // Validation rejected it; `cache_lyrics_bundle` has already said
        // why. Keep asking the others.
    }

    Ok(None)
}

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
        match external_lyrics_search(
            meta,
            vec![Provider::Musixmatch],
            SearchMode::SyncedOnly,
            true,
            translation_lang.as_deref(),
        )
        .await
        {
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
            // LRCLIB 404 — try the broader provider chain (and, under
            // prefer-LRCLIB, the deferred local tiers) before caching a
            // miss. These sources are query-based and less strict than
            // LRCLIB's metadata endpoint, so they only run after the exact
            // lookup fails. See `resolve_after_lrclib_miss` for the
            // error-handling contract (provider Err is non-fatal here).
            return resolve_after_lrclib_miss(pool, track_id, meta, prefer_lrclib).await;
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
            sidecar_write_skipped: None,
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
        sidecar_write_skipped: None,
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

    // No provider pinned → identical to a fresh `fetch_lyrics` call,
    // since the cache row is gone and the waterfall starts at the
    // embedded tier.
    let Some(provider_str) = provider.as_deref() else {
        return fetch_lyrics(state, track_id).await;
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
        Ok(SearchOutcome::Miss) => {
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
                sidecar_write_skipped: None,
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
    upsert_lyrics(&pool, &file_hash, trimmed, &format, &source, None).await?;
    Ok(LyricsPayload {
        track_id,
        content: trimmed.to_string(),
        format,
        source,
        provider: None,
        tag_write_skipped: None,
        sidecar_write_skipped: None,
        associated: Vec::new(),
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
    .fetch_all(&*pool)
    .await?;

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

        // 2. Local sidecar `.lrc` / `.txt`. Cheap; runs before the
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
        match external_lyrics_search(
            &meta,
            vec![Provider::Musixmatch],
            SearchMode::SyncedOnly,
            true,
            None,
        )
        .await
        {
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
        // actually attempted — when the toggle is off,
        // `filter_providers` returns an empty list and
        // `external_lyrics_search` short-circuits to Ok(None) without
        // touching the network.
        if musixmatch_enabled() {
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
                        match external_lyrics_search(
                            &meta,
                            external_fallback_providers(),
                            SearchMode::PreferSynced,
                            true,
                            None,
                        )
                        .await
                        {
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
                            Ok(SearchOutcome::Miss) => {
                                let _ = upsert_lyrics(
                                    &pool,
                                    &file_hash,
                                    "",
                                    &LyricsFormat::Plain,
                                    &LyricsSource::Api,
                                    None,
                                )
                                .await;
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
                match external_lyrics_search(
                    &meta,
                    external_fallback_providers(),
                    SearchMode::PreferSynced,
                    true,
                    None,
                )
                .await
                {
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
                    Ok(SearchOutcome::Miss) => {
                        // No provider had lyrics. Cache as empty so re-runs
                        // of the prefetch and re-opens of the lyrics panel
                        // skip this track. User can force a re-search
                        // per-track via the "Refetch" button.
                        let _ = upsert_lyrics(
                            &pool,
                            &file_hash,
                            "",
                            &LyricsFormat::Plain,
                            &LyricsSource::Api,
                            None,
                        )
                        .await;
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

/// Format hint coming from the in-app editor. The frontend can pass
/// "plain", "lrc", "enhanced_lrc" or "ttml" — the backend re-runs
/// `detect_format` on the content as a safety net so a mistyped header
/// still ends up in the right bucket.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSaveFormat {
    Plain,
    Lrc,
    EnhancedLrc,
    Ttml,
}

/// Where the user wants the editor's output to land.
///
/// Drives both the per-edit choice in [`save_lyrics`] and the global
/// default the editor pre-fills the segmented control with (read
/// from `app_setting['lyrics.default_destination']`). The setting is
/// app-wide on purpose — a per-profile choice would create a false
/// sense of isolation since two profiles scanning the same folder
/// share the underlying files (a `Tag` write from profile A is
/// immediately visible to profile B's `Sidecar`-preferring scan
/// because the waterfall reads tag before sidecar).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricsDestination {
    /// Embed in the audio file's USLT/SYLT/©lyr/LYRICS frame
    /// (current default — keeps round-trip with foobar2000 / MusicBee
    /// / iTunes).
    Tag,
    /// Write a sibling `.lrc` (synced) or `.txt` (plain) next to the
    /// audio file. The waterfall reader picks it up at next play.
    /// Audio file tags are left untouched — issue #201 use case.
    Sidecar,
    /// DB cache only. The audio file is untouched and no sidecar is
    /// emitted; lyrics live inside WaveFlow's per-profile DB only.
    /// Useful for privacy-conscious users and for one-shot tweaks.
    DbOnly,
}

impl LyricsDestination {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Sidecar => "sidecar",
            Self::DbOnly => "db_only",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "tag" => Some(Self::Tag),
            "sidecar" => Some(Self::Sidecar),
            "db_only" => Some(Self::DbOnly),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SaveLyricsPayload {
    pub content: String,
    pub format: LyricsSaveFormat,
    /// Per-edit choice; the frontend pre-fills it with the app-wide
    /// default and lets the user override per save.
    #[serde(default = "default_destination")]
    pub destination: LyricsDestination,
}

fn default_destination() -> LyricsDestination {
    LyricsDestination::Tag
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
            .fetch_optional(&*pool)
            .await?;
    let (file_path, mut file_hash) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let trimmed = payload
        .content
        .trim_end_matches(['\n', '\r', ' '])
        .to_string();
    // Re-detect from content so a "plain" payload with [mm:ss] stamps
    // is correctly stored as lrc, and vice versa. The frontend hint is
    // the user's intent, but content is the source of truth — except
    // when the user explicitly picked Plain (we never auto-promote to
    // a synced format) or Ttml (which the detector also catches but we
    // honour the explicit choice).
    let detected = detect_format(&trimmed);
    let format = match &payload.format {
        LyricsSaveFormat::Plain => LyricsFormat::Plain,
        LyricsSaveFormat::Ttml => LyricsFormat::Ttml,
        // For Lrc / EnhancedLrc the detector picks between Lrc,
        // EnhancedLrc and Plain (if the user cleared every stamp).
        LyricsSaveFormat::Lrc | LyricsSaveFormat::EnhancedLrc => detected,
    };

    let mut tag_write_skipped = false;
    let mut sidecar_write_skipped = false;
    match payload.destination {
        LyricsDestination::Tag => {
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
            let format_for_write = format;
            let written = tokio::task::spawn_blocking(move || {
                write_lyrics_to_file(&path, &content_for_write, &format_for_write)
            })
            .await
            .map_err(|e| AppError::Other(format!("lyrics write panicked: {e}")))?
            .map_err(|e| AppError::Other(format!("lyrics tag write failed: {e}")))?;

            if written {
                // The file changed — recompute its blake3 hash so the
                // cache row stays addressable. We update the track row +
                // the lyrics row in the same transaction below.
                let path_for_hash = file_path.clone();
                let new_hash =
                    tokio::task::spawn_blocking(move || hash_file_blake3(&path_for_hash))
                        .await
                        .map_err(|e| AppError::Other(format!("rehash panicked: {e}")))??;

                let mut tx = pool.begin().await?;
                sqlx::query("UPDATE track SET file_hash = ? WHERE id = ?")
                    .bind(&new_hash)
                    .bind(track_id)
                    .execute(&mut *tx)
                    .await?;
                // Drop any cache row keyed on the old hash so we don't
                // end up with a stale embedded payload pointing at the
                // previous content.
                sqlx::query("DELETE FROM app.lyrics WHERE file_hash = ?")
                    .bind(&file_hash)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                file_hash = new_hash;
            } else {
                tag_write_skipped = true;
            }
        }
        LyricsDestination::Sidecar => {
            // The sidecar reader supports .lrc + .txt only — TTML's
            // XML payload doesn't ride either extension cleanly, so we
            // surface the skip the same way the tag path does and let
            // the DB cache carry the edit forward.
            if matches!(format, LyricsFormat::Ttml) {
                sidecar_write_skipped = true;
            } else {
                let path = std::path::PathBuf::from(&file_path);
                let content_for_write = trimmed.clone();
                let plain = matches!(format, LyricsFormat::Plain);
                tokio::task::spawn_blocking(move || {
                    write_lyrics_sidecar(&path, &content_for_write, plain)
                })
                .await
                .map_err(|e| AppError::Other(format!("sidecar write panicked: {e}")))?
                .map_err(|e| AppError::Other(format!("sidecar write failed: {e}")))?;
            }
        }
        LyricsDestination::DbOnly => {
            // Nothing on disk. The DB upsert below is the whole job.
        }
    }

    let source = LyricsSource::Manual;
    upsert_lyrics(&pool, &file_hash, &trimmed, &format, &source, None).await?;

    let _ = app.emit("lyrics:updated", track_id);
    Ok(LyricsPayload {
        track_id,
        content: trimmed,
        format,
        source,
        provider: None,
        tag_write_skipped: if tag_write_skipped { Some(true) } else { None },
        sidecar_write_skipped: if sidecar_write_skipped {
            Some(true)
        } else {
            None
        },
        associated: Vec::new(),
    })
}

/// Write a sibling `.lrc` (synced output) or `.txt` (plain) next to
/// the audio file, replacing any existing matching-extension sidecar.
///
/// We deliberately don't try to drop the OPPOSITE extension: if the
/// user had a hand-rolled `Song.txt` and is now saving synced LRC,
/// the `.txt` linger is intentional (the waterfall reader prefers
/// `.lrc` over `.txt` so the new file wins anyway, and silently
/// deleting an unrelated user-managed sidecar is more surprising
/// than the lingering file). UTF-8 without BOM matches the format
/// every LRC reader expects.
fn write_lyrics_sidecar(audio_path: &Path, content: &str, plain: bool) -> AppResult<()> {
    let sidecar = sidecar_path(audio_path, plain)
        .ok_or_else(|| AppError::Other("audio file has no stem or parent dir".to_string()))?;
    std::fs::write(&sidecar, content)
        .map_err(|e| AppError::Other(format!("write {}: {e}", sidecar.display())))?;
    Ok(())
}

/// Where a track's sidecar goes: `{stem}.lrc` for synced output,
/// `{stem}.txt` for plain, in the track's own directory. `None` for a
/// path with no stem or no parent, which is not a file we can sit beside.
///
/// Shared by the editor's write and the export of fetched lyrics (#695),
/// so the two cannot drift into disagreeing about where a sidecar lives —
/// the export checks for an existing one at exactly the path the editor
/// would have written.
fn sidecar_path(audio_path: &Path, plain: bool) -> Option<std::path::PathBuf> {
    let stem = audio_path.file_stem().and_then(|s| s.to_str())?;
    let parent = audio_path.parent()?;
    let ext = if plain { "txt" } else { "lrc" };
    Some(parent.join(format!("{stem}.{ext}")))
}

fn hash_file_blake3(path: &str) -> AppResult<String> {
    let bytes = std::fs::read(path).map_err(|e| AppError::Other(format!("read for hash: {e}")))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// The tag writer a file's container leads to, for the two questions
/// this module asks about one.
///
/// DSD is the reason it exists. A `.dsf` keeps a plain ID3v2 tag at an
/// offset its header declares, which lofty cannot open at all — so
/// asking lofty what the file is answered "unrecognised container" for a
/// file we can in fact write, and choosing "save to tag" on a DSD track
/// failed with a message about the format rather than about the
/// situation (#644).
#[derive(Clone, Copy)]
enum LyricsContainer {
    /// Written through `edit::with_dsf_tag`, on the `id3` crate.
    Dsf,
    /// Anything lofty recognises, named by its file type.
    Lofty(FileType),
}

impl LyricsContainer {
    /// `None` when the container could not be identified, which is not
    /// an error here: the authority on "can this file be tagged at all"
    /// is `patch_file`, and it answers in words the dialog can show.
    /// This only decides where lyrics may go, and a container we could
    /// not name is not one to put XML into.
    fn of(path: &Path) -> Option<Self> {
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("dsf"))
        {
            return Some(Self::Dsf);
        }
        lofty::probe::Probe::open(path)
            .ok()?
            .guess_file_type()
            .ok()?
            .file_type()
            .map(Self::Lofty)
    }

    /// Whether the tag has a key that accepts an arbitrary string, which
    /// is what TTML needs.
    ///
    /// An allow list on purpose. lofty states it outright —
    /// "`ItemKey::Lyrics` is **not** supported in ID3v2, you must use
    /// `ItemKey::UnsyncLyrics`" — so on an ID3v2 container the item is
    /// dropped on the way to the file: the save reported success and
    /// wrote nothing. A deny list that forgot a container would go on
    /// doing that silently; forgetting one here costs an honest
    /// "stays in-app only" instead.
    fn carries_arbitrary_lyrics(self) -> bool {
        matches!(
            self,
            Self::Lofty(
                FileType::Flac
                    | FileType::Vorbis
                    | FileType::Opus
                    | FileType::Speex
                    | FileType::Mp4
            )
        )
    }
}

/// Write the lyrics back into the audio file's tag.
///
/// - Plain / LRC / Enhanced LRC → `ItemKey::UnsyncLyrics` (USLT for
///   ID3v2, UNSYNCEDLYRICS for Vorbis, `©lyr` for MP4), for cross-player
///   compatibility. LRC / Enhanced LRC additionally re-stamp the canonical
///   `SYNCEDLYRICS` custom tag (issue #378) so a synced edit round-trips
///   into the tag the reader prefers instead of being silently downgraded.
/// - TTML → `ItemKey::Lyrics`, which only the containers in
///   [`LyricsContainer::carries_arbitrary_lyrics`] have. Everywhere else
///   the file write is skipped and `Ok(false)` returned — the DB cache
///   still gets updated and the UI surfaces a toast, so the user knows
///   their TTML stays in-app only.
///
/// The write itself goes through [`crate::commands::edit::patch_file`],
/// the writer the properties dialog uses, rather than through
/// `lofty::read_from_path` + `save_to_path` as it did. That is what
/// gives a `.dsf` its lyrics, and it also stops a save from dropping
/// every non-standard comment on a Vorbis-family file — the generic tag
/// throws that remainder away, which is the whole reason `patch_file`
/// exists (#644).
///
/// Returns `Ok(true)` when the tag was rewritten on disk, `Ok(false)`
/// when the write was intentionally skipped (TTML on a format that
/// can't carry it).
fn write_lyrics_to_file(
    path: &Path,
    content: &str,
    format: &LyricsFormat,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use crate::commands::edit::{patch_file, LyricsSlot, TagPatch};

    let container = LyricsContainer::of(path);
    let empty = content.trim().is_empty();
    let is_ttml = matches!(format, LyricsFormat::Ttml);

    // Decided before anything is written: a TTML save that cannot land
    // must leave the file exactly as it was, including whatever lyrics
    // it already carries.
    //
    // **A clear is not such a save.** The format is the tab the user
    // happens to be on, and emptying the editor there still means
    // "remove the lyrics from this file" — refusing it left an MP3
    // holding the words a previous plain save wrote, with the database
    // saying there were none.
    if is_ttml && !empty && !container.is_some_and(LyricsContainer::carries_arbitrary_lyrics) {
        return Ok(false);
    }
    patch_file(
        path,
        &TagPatch::Lyrics(if empty {
            LyricsSlot::Cleared
        } else if is_ttml {
            LyricsSlot::Arbitrary(content)
        } else {
            LyricsSlot::Unsynchronised(content)
        }),
    )?;

    // The canonical `SYNCEDLYRICS` custom tag, which
    // `read_embedded_lyrics` prefers over every standard key (#378). It
    // cannot ride along with the write above — lofty's generic tag has
    // no key for a custom one — so it takes a second pass over the
    // concrete tag.
    //
    // **Every save, not only the synced ones.** The pass used to run
    // for a non-empty LRC and nothing else, which left the old synced
    // tag in place when the user replaced those lyrics with plain ones,
    // saved TTML, or cleared them outright — and the reader prefers it,
    // so the edit appeared not to take and clearing brought the lyrics
    // back. What the file says under the preferred key has to follow
    // what was just written, including to nothing.
    //
    // Best-effort: the standard key already landed, so a failure here
    // leaves the lyrics readable. It is logged loudly, because the
    // shape it fails into is exactly the shadowing above.
    let synced = match format {
        LyricsFormat::Lrc | LyricsFormat::EnhancedLrc if !empty => Some(content),
        _ => None,
    };
    // Always `Some` this far in — `patch_file` refused anything whose
    // container it could not name — so this is the shape of the type
    // rather than a case being handled.
    if let Some(container) = container {
        if let Err(err) = stamp_synced_lyrics(path, container, synced) {
            tracing::warn!(
                ?err,
                "failed to update the SYNCEDLYRICS tag; the standard key is correct but a stale synced tag may still shadow it"
            );
        }
    }

    Ok(true)
}

/// Put the `SYNCEDLYRICS` custom tag in step with what was just
/// written: `Some` stamps the canonical key, `None` takes it away.
///
/// A second pass over the **concrete** tag, because lofty's generic one
/// has no key for a custom frame — TXXX for ID3v2 (MP3 and DSF), a
/// Vorbis comment for FLAC / Ogg / Opus / Speex. MP4 and the rest have
/// no comparable key and are skipped.
///
/// **Every alias goes, not just the canonical key.** The reader accepts
/// four spellings of it and takes the first it finds, so removing one
/// would leave the others shadowing the lyrics the user just saved.
/// Writing goes to the canonical one, which the reader prefers.
///
/// The file is rewritten through a copy that replaces the original in
/// one rename, like every other tag write here (#598): lofty's own
/// rewrite truncates the file it is rewriting, so an interruption — a
/// crash, a lost USB drive, an antivirus taking the handle — between
/// the truncate and the write left the audio gone, not just the tag.
/// This path spent years calling `save_to_path` directly.
///
/// Nothing is written when nothing changed, and nothing is **copied**
/// either — the rewrite starts by copying the file, so the question is
/// asked read-only first. A plain-lyrics save on a file that never
/// carried a synced tag is the common case, and it should not cost a
/// copy of the record to find that out.
fn stamp_synced_lyrics(
    path: &Path,
    container: LyricsContainer,
    content: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile;

    let key = SYNCED_LYRICS_KEYS[0]; // canonical "SYNCEDLYRICS"

    let file_type = match container {
        // DSF carries a plain ID3v2 tag; it is only kept somewhere else
        // in the file, and `with_dsf_tag` already writes it the safe
        // way — through the header's own pointer, without moving one
        // audio byte.
        LyricsContainer::Dsf => {
            return crate::commands::edit::with_dsf_tag(path, |tag, _version| {
                use id3::TagLike;
                // `remove_extended_text` says nothing about what it
                // removed, so the presence is read first — this is what
                // decides whether the file is rewritten at all.
                let mut changed = false;
                for alias in SYNCED_LYRICS_KEYS {
                    if tag.extended_texts().any(|txxx| txxx.description == *alias) {
                        tag.remove_extended_text(Some(alias), None);
                        changed = true;
                    }
                }
                if let Some(content) = content {
                    tag.add_frame(id3::frame::ExtendedText {
                        description: key.to_string(),
                        value: content.to_string(),
                    });
                    changed = true;
                }
                changed
            });
        }
        LyricsContainer::Lofty(file_type) => file_type,
    };

    /// Remove every spelling of the key, then write the canonical one if
    /// there is something to write. Answers whether the tag changed.
    macro_rules! restamp_id3v2 {
        ($tag:expr) => {{
            let mut changed = false;
            for alias in SYNCED_LYRICS_KEYS {
                changed |= $tag.remove_user_text(alias).is_some();
            }
            if let Some(content) = content {
                $tag.insert_user_text(key.to_string(), content.to_string());
                changed = true;
            }
            changed
        }};
    }

    macro_rules! restamp_vorbis {
        ($comments:expr) => {{
            let mut changed = false;
            for alias in SYNCED_LYRICS_KEYS {
                changed |= $comments.remove(alias).count() > 0;
            }
            if let Some(content) = content {
                $comments.insert(key.to_string(), content.to_string());
                changed = true;
            }
            changed
        }};
    }

    /// Read the concrete file, restamp its tag, save it back — through
    /// the temporary the rename replaces the original with.
    ///
    /// Asked read-only first, and that is the point of the two passes:
    /// `rewrite_via_temp` copies the whole file before the body runs,
    /// so deciding inside it would copy a 40 MB record to find out
    /// there was nothing to change. Re-parsing a tag is the cheap half
    /// of that trade by a wide margin.
    macro_rules! rewrite {
        ($ty:ty, |$f:ident| $restamp:block) => {{
            let needed = {
                let mut handle = std::fs::File::open(path)?;
                let mut $f = <$ty>::read_from(&mut handle, ParseOptions::new())?;
                let changed: bool = $restamp;
                changed
            };
            if needed {
                waveflow_core::tagio::rewrite_via_temp(
                    path,
                    |handle| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                        let mut $f = <$ty>::read_from(handle, ParseOptions::new())?;
                        let changed: bool = $restamp;
                        if changed {
                            $f.save_to(handle, WriteOptions::default())?;
                        }
                        Ok(())
                    },
                )?;
            }
        }};
    }

    match file_type {
        FileType::Mpeg => rewrite!(lofty::mpeg::MpegFile, |mpeg| {
            match mpeg.id3v2_mut() {
                Some(tag) => restamp_id3v2!(tag),
                // No tag at all: nothing to remove, and a file with no
                // lyrics under any key needs no synced one either — the
                // standard write above has already refused or created
                // whatever the file should hold.
                None => false,
            }
        }),
        // The same ID3v2 tag as MP3, and `patch_file` writes the
        // standard lyrics key there too — so this pass has the same
        // stale stamp to clear. (Our own reader looks for the custom key
        // in MP3 and the Vorbis families only, so what it removes here
        // is for the other players that do read it.)
        FileType::Aac => rewrite!(lofty::aac::AacFile, |aac| {
            match aac.id3v2_mut() {
                Some(tag) => restamp_id3v2!(tag),
                None => false,
            }
        }),
        FileType::Flac => rewrite!(lofty::flac::FlacFile, |flac| {
            match flac.vorbis_comments_mut() {
                Some(comments) => restamp_vorbis!(comments),
                None => false,
            }
        }),
        FileType::Vorbis => rewrite!(lofty::ogg::VorbisFile, |vorbis| {
            restamp_vorbis!(vorbis.vorbis_comments_mut())
        }),
        FileType::Opus => rewrite!(lofty::ogg::OpusFile, |opus| {
            restamp_vorbis!(opus.vorbis_comments_mut())
        }),
        FileType::Speex => rewrite!(lofty::ogg::SpeexFile, |speex| {
            restamp_vorbis!(speex.vorbis_comments_mut())
        }),
        _ => {}
    }
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
    .execute(&*pool)
    .await?;
    Ok(())
}

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
            sidecar_write_skipped: None,
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
    // LRCLIB first (best-curated, often synced), then the query-based
    // fallback chain. No album / duration context, so this goes through
    // the query search rather than LRCLIB's exact-match `get`.
    let providers = vec![
        Provider::Lrclib,
        Provider::NetEase,
        Provider::Megalobiz,
        Provider::Genius,
    ];
    let result =
        match external_lyrics_search(&meta, providers, SearchMode::PreferSynced, false, None).await
        {
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
                sidecar_write_skipped: None,
                associated: Vec::new(),
            }))
        }
        SearchOutcome::Miss => {
            // Cache the miss (empty content) so a recurring song on the
            // station's rotation doesn't re-hit the network every time.
            upsert_radio_lyrics(&pool, &key, artist, title, "", &LyricsFormat::Plain, None).await?;
            Ok(None)
        }
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
            sidecar_write_skipped: None,
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
    let providers = vec![
        Provider::Lrclib,
        Provider::NetEase,
        Provider::Megalobiz,
        Provider::Genius,
    ];
    match external_lyrics_search(&meta, providers, SearchMode::PreferSynced, false, None).await {
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
                sidecar_write_skipped: None,
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

/// Whitelist of language codes the Musixmatch translation endpoint
/// accepts. Mirrors the union of locale codes WaveFlow ships UI for
/// (CLAUDE.md "Language" section) — the 17 i18n locales — minus the
/// `pt-BR` regional variant since Musixmatch only knows the base
/// `pt` code, and minus `kr` (legacy alias for `ko`). Any value
/// outside this set is rejected with a clear error so a typoed
/// IPC payload can't land a row that silently disables translation
/// across every fetch.
const SUPPORTED_TRANSLATION_LANGS: &[&str] = &[
    "en", "es", "de", "it", "nl", "pt", "ru", "tr", "id", "ja", "ko", "zh-CN", "zh-TW", "ar", "hi",
    "fr",
];

/// Read the per-profile translation language flag the lyrics UI
/// pre-fills its dropdown with. Empty / absent → no translation.
#[tauri::command]
pub async fn get_lyrics_translation_lang(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    read_translation_lang(&pool).await
}

/// Persist the per-profile translation language flag. Passing
/// `None` clears the row → translation disabled. The whitelist
/// guards against an unknown / typoed code silently breaking the
/// fetch (Musixmatch would return an empty body, which our merge
/// pass would treat as "nothing to inject" — wasted hop).
#[tauri::command]
pub async fn set_lyrics_translation_lang(
    state: tauri::State<'_, AppState>,
    lang: Option<String>,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    let normalized: Option<String> = match lang.as_deref() {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else if !SUPPORTED_TRANSLATION_LANGS.contains(&trimmed) {
                return Err(AppError::Other(format!(
                    "set_lyrics_translation_lang: unsupported language '{trimmed}'",
                )));
            } else {
                Some(trimmed.to_string())
            }
        }
        None => None,
    };
    match &normalized {
        Some(code) => {
            let now = chrono::Utc::now().timestamp_millis();
            sqlx::query(
                "INSERT INTO profile_setting (key, value, value_type, updated_at)
                 VALUES (?, ?, 'string', ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, value_type = excluded.value_type, updated_at = excluded.updated_at",
            )
            .bind(TRANSLATION_LANG_KEY)
            .bind(code)
            .bind(now)
            .execute(&*pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM profile_setting WHERE key = ?")
                .bind(TRANSLATION_LANG_KEY)
                .execute(&*pool)
                .await?;
        }
    }
    Ok(normalized)
}

/// Read the per-profile prefer-LRCLIB flag for the Settings toggle.
/// Absent / blank → `false` (local lyrics win, the historical default).
#[tauri::command]
pub async fn get_prefer_lrclib(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;
    read_prefer_lrclib(&pool).await
}

/// Persist the per-profile prefer-LRCLIB flag. `true` makes the on-demand
/// lyrics waterfall try the online providers before the track's own
/// embedded + sidecar lyrics (issue #378); `false` clears the row and
/// restores the local-first default.
#[tauri::command]
pub async fn set_prefer_lrclib(state: tauri::State<'_, AppState>, enabled: bool) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    if enabled {
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "INSERT INTO profile_setting (key, value, value_type, updated_at)
             VALUES (?, 'true', 'bool', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, value_type = excluded.value_type, updated_at = excluded.updated_at",
        )
        .bind(PREFER_LRCLIB_KEY)
        .bind(now)
        .execute(&*pool)
        .await?;
    } else {
        sqlx::query("DELETE FROM profile_setting WHERE key = ?")
            .bind(PREFER_LRCLIB_KEY)
            .execute(&*pool)
            .await?;
    }
    Ok(())
}

/// `app_setting` key holding the global default lyrics destination
/// the editor pre-fills the segmented control with. App-wide rather
/// than per-profile because the destination drives filesystem state
/// (tag bytes, sidecar files) that is shared between profiles whose
/// libraries touch the same audio files — a per-profile choice
/// would create a false sense of isolation.
pub const LYRICS_DEFAULT_DESTINATION_KEY: &str = "lyrics.default_destination";

/// Read the app-wide destination through a **profile** pool, which has
/// `app.db` attached as `app` — so the fetch paths, which hold a profile
/// pool and no `AppState`, can ask without threading one through.
///
/// Missing or unparseable row falls back to `Tag`, like the command below.
async fn read_default_destination(pool: &sqlx::SqlitePool) -> AppResult<LyricsDestination> {
    let row: Option<String> = sqlx::query_scalar("SELECT value FROM app.app_setting WHERE key = ?")
        .bind(LYRICS_DEFAULT_DESTINATION_KEY)
        .fetch_optional(pool)
        .await?;
    Ok(row
        .as_deref()
        .and_then(LyricsDestination::parse)
        .unwrap_or(LyricsDestination::Tag))
}

/// Read the app-wide default. Missing row falls back to `Tag` so
/// existing installs keep the pre-#201 behaviour until the user opts
/// into the new flow (either from the onboarding step or the
/// Settings → Images and lyrics card).
#[tauri::command]
pub async fn get_lyrics_default_destination(
    state: tauri::State<'_, AppState>,
) -> AppResult<String> {
    let row: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(LYRICS_DEFAULT_DESTINATION_KEY)
        .fetch_optional(&state.app_db)
        .await?;
    let resolved = row
        .as_deref()
        .and_then(LyricsDestination::parse)
        .unwrap_or(LyricsDestination::Tag);
    Ok(resolved.as_str().to_string())
}

/// Persist the user's pick. Rejects unknown values with an explicit
/// error so a typo from the frontend doesn't silently land an
/// unparseable row that the next read would discard back to default.
#[tauri::command]
pub async fn set_lyrics_default_destination(
    state: tauri::State<'_, AppState>,
    destination: String,
) -> AppResult<String> {
    let parsed = LyricsDestination::parse(&destination).ok_or_else(|| {
        AppError::Other(format!(
            "set_lyrics_default_destination: unsupported value '{destination}' (expected tag, sidecar, db_only)"
        ))
    })?;
    let value = parsed.as_str();
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            value_type = excluded.value_type,
            updated_at = excluded.updated_at",
    )
    .bind(LYRICS_DEFAULT_DESTINATION_KEY)
    .bind(value)
    .bind(now)
    .execute(&state.app_db)
    .await?;
    Ok(value.to_string())
}

/// Write a lyrics payload to an arbitrary on-disk path. Used by the
/// Lyrics Editor "Save to file…" affordance (issue #201) so the user
/// can ship the LRC/TXT they just crafted as a sidecar next to the
/// audio file or anywhere else on their filesystem — leaving the
/// audio file's tag block untouched, complementing the existing
/// "embed in tag" + "cache only" options.
///
/// The frontend resolves `target_path` via the Tauri save dialog
/// (`@tauri-apps/plugin-dialog`'s `save()`), so the user explicitly
/// picked it. We still defend against a malformed IPC payload by
/// requiring the parent directory to exist before writing — the
/// dialog already enforces that, but a stray call with a bogus path
/// would otherwise surface as an opaque `os error 3`. UTF-8 without
/// BOM because LRCLIB / Musicolet / Spotify-style consumers all read
/// BOM-less files fine and a BOM trips up some smaller offline
/// players.
///
/// The `state` parameter exists for the
/// `state.require_profile_pool().await?` sentry: without an active
/// profile the lyrics editor isn't reachable from the UI, and the
/// call refuses to mint a sidecar file from an orphaned IPC payload.
/// The pool itself isn't touched — this command does not read or
/// write the per-profile DB, the bytes come straight from the
/// frontend's `buildPayload`. Mirrors the same gate every sibling
/// `commands::lyrics::*` handler applies (CLAUDE.md cross-cutting
/// rule).
#[tauri::command]
pub async fn export_lyrics_to_path(
    state: tauri::State<'_, AppState>,
    target_path: String,
    content: String,
) -> AppResult<()> {
    let _pool = state.require_profile_pool().await?;
    let path = std::path::PathBuf::from(&target_path);
    let parent = path.parent().ok_or_else(|| {
        AppError::Other(format!(
            "export_lyrics_to_path: target has no parent: {target_path}"
        ))
    })?;
    if !parent.as_os_str().is_empty() && !parent.exists() {
        return Err(AppError::Other(format!(
            "export_lyrics_to_path: parent directory does not exist: {}",
            parent.display()
        )));
    }
    tokio::task::spawn_blocking(move || std::fs::write(&path, content.as_bytes()))
        .await
        .map_err(|e| AppError::Other(format!("export_lyrics_to_path write panicked: {e}")))?
        .map_err(|e| AppError::Other(format!("export_lyrics_to_path write failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document is accepted only if it looks like what it says it is.
    ///
    /// This is the whole of the host's trust in a plugin's lyrics, so
    /// both directions matter: a correct document must not be refused,
    /// and a mislabelled one must not get through.
    #[test]
    fn a_document_must_look_like_the_format_it_declares() {
        let lrc = "[00:12.00]Hello\n[00:15.00]World";
        let enhanced = "[00:12.00]<00:12.00>Hello <00:13.50>world";
        let ttml = "<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div><p>Hi</p></div></body></tt>";

        assert!(document_is_what_it_claims(lrc, LyricsFormat::Lrc));
        assert!(document_is_what_it_claims(
            enhanced,
            LyricsFormat::EnhancedLrc
        ));
        assert!(document_is_what_it_claims(ttml, LyricsFormat::Ttml));
        assert!(document_is_what_it_claims(
            "just words",
            LyricsFormat::Plain
        ));

        // The mislabelling that matters: a provider that served an error
        // page, or changed format without saying.
        assert!(!document_is_what_it_claims(ttml, LyricsFormat::Lrc));
        assert!(!document_is_what_it_claims(
            "just words",
            LyricsFormat::Ttml
        ));
        assert!(!document_is_what_it_claims(lrc, LyricsFormat::EnhancedLrc));

        // Empty is never a document, whatever it claims.
        assert!(!document_is_what_it_claims("", LyricsFormat::Plain));
        assert!(!document_is_what_it_claims("   \n  ", LyricsFormat::Lrc));
    }

    /// A profile pool with the real `app` schema attached, the way the
    /// application attaches it.
    ///
    /// The point of the tests below is the foreign key and its cascade,
    /// so fixtures that recreate the tables by hand would prove nothing
    /// — they would omit the very constraint under test. Hence the real
    /// migrations, and two details copied from `db::profile_db::open`
    /// rather than invented here:
    ///
    /// `ATTACH` is per-CONNECTION, so it belongs in `after_connect`. Run
    /// once through the pool it lands on whichever connection served it
    /// and the next query gets `no such table: app.lyrics` — which is
    /// exactly how the first version of this harness failed.
    ///
    /// Both databases are files. A `sqlite::memory:` pool gives each
    /// connection its own empty database, so the profile schema would
    /// have been just as absent, one connection later.
    ///
    /// `foreign_keys` is set explicitly: it is per-connection and off by
    /// default, and without it every assertion below passes for the
    /// wrong reason.
    async fn pool_with_app_schema(dir: &std::path::Path) -> sqlx::SqlitePool {
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
        use std::str::FromStr;

        let opts_for = |path: &std::path::Path| {
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
                .foreign_keys(true)
        };

        let app_path = dir.join("app.db");
        let app_pool = sqlx::SqlitePool::connect_with(opts_for(&app_path))
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/app")
            .run(&app_pool)
            .await
            .unwrap();
        app_pool.close().await;

        let attach = format!(
            "ATTACH DATABASE '{}' AS app",
            app_path.display().to_string().replace('\'', "''")
        );
        let pool = SqlitePoolOptions::new()
            .after_connect(move |conn, _meta| {
                let attach = attach.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(attach))
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(opts_for(&dir.join("profile.db")))
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        pool
    }

    async fn seed_bundle(pool: &sqlx::SqlitePool, file_hash: &str) {
        upsert_lyrics(
            pool,
            file_hash,
            "[00:12.00]Original",
            &LyricsFormat::Lrc,
            &LyricsSource::Api,
            Some("apple-lyrics"),
        )
        .await
        .unwrap();
        for (kind, lang) in [("translation", Some("fr")), ("pronunciation", None)] {
            sqlx::query(
                "INSERT INTO app.lyrics_associated
                    (file_hash, kind, language, content, format, fetched_at)
                 VALUES (?, ?, ?, '[00:12.00]Companion', 'lrc', 0)",
            )
            .bind(file_hash)
            .bind(kind)
            .bind(lang)
            .execute(pool)
            .await
            .unwrap();
        }
    }

    /// One library, one artist, one album, one track pointing at `path`.
    ///
    /// Built from the real migrations by `pool_with_app_schema`, so the
    /// NOT NULL columns and the foreign keys are the ones the app runs
    /// with — a looser fixture would let a broken insert pass.
    async fn seed_track(pool: &sqlx::SqlitePool, path: &std::path::Path, file_hash: &str) {
        sqlx::query(
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, duration_ms, added_at, is_available,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, ?, ?, 1, 0, 'T', 300000, 0, 1, 0, 0, 0, 0)",
        )
        .bind(path.display().to_string())
        .bind(file_hash)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn set_destination(pool: &sqlx::SqlitePool, value: &str) {
        // `value_type` and `updated_at` are NOT NULL, and `value_type`
        // carries a CHECK -- the same shape `set_lyrics_default_destination`
        // writes. A fixture that omitted them would fail only at runtime.
        sqlx::query(
            "INSERT INTO app.app_setting (key, value, value_type, updated_at)
             VALUES (?, ?, 'string', 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(LYRICS_DEFAULT_DESTINATION_KEY)
        .bind(value)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Lyrics fetched from a provider land next to the music when that is
    /// where the user said lyrics go (#695). Before this, the destination
    /// governed only what the user typed, so a library set to "sidecar
    /// file" still kept everything fetched inside WaveFlow's database.
    #[tokio::test]
    async fn fetched_lyrics_land_next_to_the_music() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "sidecar").await;

        upsert_lyrics(
            &pool,
            "h1",
            "[00:12.00]Hello",
            &LyricsFormat::Lrc,
            &LyricsSource::Api,
            Some("lrclib"),
        )
        .await
        .unwrap();

        let sidecar = dir.path().join("Song.lrc");
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            "[00:12.00]Hello"
        );
    }

    /// The refusal that matters most: `tag` must NOT make a background
    /// fetch rewrite the audio file. That write pauses playback and
    /// re-hashes, which is the tag editor's job, on the user's command --
    /// never a side effect of a track starting.
    #[tokio::test]
    async fn a_fetch_never_rewrites_the_audio_file() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "tag").await;

        upsert_lyrics(
            &pool,
            "h1",
            "[00:12.00]Hello",
            &LyricsFormat::Lrc,
            &LyricsSource::Api,
            Some("lrclib"),
        )
        .await
        .unwrap();

        assert!(!dir.path().join("Song.lrc").exists());
        assert_eq!(std::fs::read(&audio).unwrap(), b"audio");
    }

    /// An empty sidecar is a file nobody else wrote, even though the
    /// reader reports it as a miss. Truncating it would be the overwrite
    /// this code refuses to do.
    #[tokio::test]
    async fn an_empty_sidecar_is_still_someone_else_s_file() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        let sidecar = dir.path().join("Song.lrc");
        std::fs::write(&sidecar, "").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "sidecar").await;

        upsert_lyrics(
            &pool,
            "h1",
            "[00:12.00]Theirs",
            &LyricsFormat::Lrc,
            &LyricsSource::Api,
            Some("lrclib"),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "");
    }

    /// A sidecar the user spelled differently is still the user's file.
    /// The guard asks the waterfall's own reader, which matches the stem
    /// case-insensitively, so this only has something to prove on a
    /// case-sensitive filesystem -- the Linux CI runner, where writing
    /// `Song.lrc` beside `song.LRC` would leave two files and let ours
    /// supersede theirs.
    #[tokio::test]
    async fn a_sidecar_spelled_differently_still_counts() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        std::fs::write(dir.path().join("song.LRC"), "[00:01.00]Mine").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "sidecar").await;

        upsert_lyrics(
            &pool,
            "h1",
            "[00:12.00]Theirs",
            &LyricsFormat::Lrc,
            &LyricsSource::Api,
            Some("lrclib"),
        )
        .await
        .unwrap();

        assert!(!dir.path().join("Song.lrc").exists());
    }

    /// A sidecar already on disk is the user's file. The export adds one
    /// where there was none; it never edits one that is already there.
    #[tokio::test]
    async fn an_existing_sidecar_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        let sidecar = dir.path().join("Song.lrc");
        std::fs::write(&sidecar, "[00:01.00]Mine").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "sidecar").await;

        upsert_lyrics(
            &pool,
            "h1",
            "[00:12.00]Theirs",
            &LyricsFormat::Lrc,
            &LyricsSource::Api,
            Some("lrclib"),
        )
        .await
        .unwrap();

        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "[00:01.00]Mine");
    }

    /// Lyrics read out of the file itself have nowhere to go: writing a
    /// sidecar from an embedded tag would fill the library with copies of
    /// what is already in the music.
    #[tokio::test]
    async fn lyrics_that_came_off_the_disk_are_not_written_back() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "sidecar").await;

        upsert_lyrics(
            &pool,
            "h1",
            "Hello",
            &LyricsFormat::Plain,
            &LyricsSource::Embedded,
            None,
        )
        .await
        .unwrap();

        assert!(!dir.path().join("Song.txt").exists());
        assert!(!dir.path().join("Song.lrc").exists());
    }

    /// An instrumental verdict is an empty row, not lyrics: it must not
    /// leave an empty file beside the track.
    #[tokio::test]
    async fn an_instrumental_verdict_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"audio").unwrap();
        seed_track(&pool, &audio, "h1").await;
        set_destination(&pool, "sidecar").await;

        upsert_lyrics(
            &pool,
            "h1",
            "",
            &LyricsFormat::Plain,
            &LyricsSource::Api,
            Some("lrclib"),
        )
        .await
        .unwrap();

        assert!(!dir.path().join("Song.txt").exists());
    }

    async fn associated_count(pool: &sqlx::SqlitePool, file_hash: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM app.lyrics_associated WHERE file_hash = ?")
            .bind(file_hash)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Writing a new primary document ends the bundle the old one
    /// belonged to.
    ///
    /// Without this, switching provider leaves the previous one's
    /// translation under the new lyrics — two documents that were never
    /// published together, with nothing downstream able to tell. The
    /// invariant lives in `upsert_lyrics` precisely so every tier gets
    /// it; this test is what stops someone adding a tier that writes
    /// the row directly.
    #[tokio::test]
    async fn replacing_the_primary_document_clears_its_companions() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        seed_bundle(&pool, "hash-a").await;
        assert_eq!(associated_count(&pool, "hash-a").await, 2);

        // Any other tier answering for the same file — here a manual save.
        upsert_lyrics(
            &pool,
            "hash-a",
            "Typed by hand",
            &LyricsFormat::Plain,
            &LyricsSource::Manual,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            associated_count(&pool, "hash-a").await,
            0,
            "a new primary document must not inherit the previous bundle's companions"
        );
    }

    /// Deleting the lyrics row takes its companions with it.
    ///
    /// `read_cached` deletes a cached service credit in place, and the
    /// bundle has to follow. That is the foreign key's job rather than
    /// the caller's — which only holds while `foreign_keys` is on, so
    /// this asserts the behaviour rather than the schema text.
    #[tokio::test]
    async fn companions_cascade_when_the_lyrics_row_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        seed_bundle(&pool, "hash-b").await;

        sqlx::query("DELETE FROM app.lyrics WHERE file_hash = ?")
            .bind("hash-b")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            associated_count(&pool, "hash-b").await,
            0,
            "companions must not outlive the lyrics they belong to"
        );
    }

    fn plugin_doc(
        content: &str,
        language: Option<&str>,
    ) -> waveflow_core::plugin::runtime::LyricsDocument {
        waveflow_core::plugin::runtime::LyricsDocument {
            content: content.to_string(),
            format: PluginLyricsFormat::Lrc,
            language: language.map(str::to_string),
        }
    }

    /// What `cache_lyrics_bundle` returns must be what it stored.
    ///
    /// The insert is `OR IGNORE`, so a provider sending two documents for
    /// one slot has the second dropped by the database whatever the host
    /// does. If the payload were built from the unfiltered list, the
    /// panel would show both until the next reload and one after it, and
    /// nothing would explain the difference. This pins the two together.
    #[tokio::test]
    async fn a_returned_bundle_matches_what_was_cached() {
        use waveflow_core::plugin::runtime::{AssociatedDocument, LyricsBundle};

        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;

        let bundle = LyricsBundle {
            primary: plugin_doc("[00:12.00]Original", Some("ja")),
            associated: vec![
                AssociatedDocument {
                    kind: PluginAssociatedKind::Translation,
                    document: plugin_doc("[00:12.00]Premiere", Some("fr")),
                },
                // Same slot as the one above — the provider contradicting
                // itself, which is the case the two rules have to agree on.
                AssociatedDocument {
                    kind: PluginAssociatedKind::Translation,
                    document: plugin_doc("[00:12.00]Seconde", Some("fr")),
                },
                AssociatedDocument {
                    kind: PluginAssociatedKind::Pronunciation,
                    document: plugin_doc("[00:12.00]Romaji", None),
                },
            ],
        };

        let payload = cache_lyrics_bundle(&pool, 1, "hash-d", bundle, "apple-lyrics")
            .await
            .unwrap()
            .expect("the primary document is valid LRC");

        assert_eq!(
            payload.associated.len(),
            2,
            "the duplicate slot must be dropped before the payload is built"
        );
        assert_eq!(
            payload.associated.len() as i64,
            associated_count(&pool, "hash-d").await,
            "the payload and the cache must agree on what was stored"
        );
        assert_eq!(
            payload.associated[0].content, "[00:12.00]Premiere",
            "the first document for a slot is the one kept"
        );

        // Provenance is namespaced so `Provider::from_id` can never take a
        // plugin id for one of its own.
        assert_eq!(payload.provider.as_deref(), Some("plugin:apple-lyrics"));
    }

    /// One document per (kind, language) — and NULL is one slot, not a
    /// fresh one each time.
    ///
    /// SQLite treats NULLs as distinct in a UNIQUE index, so the index
    /// is written over `COALESCE(language, '')`. Without that, a
    /// provider returning two untagged pronunciations would store both
    /// and the panel would have no way to choose.
    #[tokio::test]
    async fn a_bundle_holds_one_document_per_kind_and_language() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_app_schema(dir.path()).await;
        seed_bundle(&pool, "hash-c").await;

        let second_untagged = sqlx::query(
            "INSERT INTO app.lyrics_associated
                (file_hash, kind, language, content, format, fetched_at)
             VALUES (?, 'pronunciation', NULL, 'Another', 'plain', 0)",
        )
        .bind("hash-c")
        .execute(&pool)
        .await;
        assert!(
            second_untagged.is_err(),
            "a second untagged pronunciation must collide with the first"
        );

        // A different language is a different slot, and allowed.
        sqlx::query(
            "INSERT INTO app.lyrics_associated
                (file_hash, kind, language, content, format, fetched_at)
             VALUES (?, 'translation', 'ja', '[00:12.00]Companion', 'lrc', 0)",
        )
        .bind("hash-c")
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(associated_count(&pool, "hash-c").await, 3);
    }

    /// The chain must consult LRCLIB before providers that answer for
    /// almost any query. Tier 5 already asked LRCLIB, but through the
    /// exact-match `/api/get`; this pass is the fuzzy one, and dropping
    /// it is what let Genius answer for tracks LRCLIB actually carries
    /// (issue #463).
    #[test]
    fn fallback_chain_asks_lrclib_before_the_catch_all_providers() {
        let chain = external_fallback_providers();
        let pos = |p: Provider| chain.iter().position(|c| *c == p);

        let lrclib = pos(Provider::Lrclib).expect("lrclib is in the chain");
        for catch_all in [Provider::NetEase, Provider::Megalobiz, Provider::Genius] {
            let other = pos(catch_all).expect("provider is in the chain");
            assert!(
                lrclib < other,
                "lrclib must precede {catch_all:?} in the fallback chain"
            );
        }

        // Musixmatch stays out: the dedicated enhanced tier owns it, and
        // listing it here would re-issue the identical request.
        assert!(
            pos(Provider::Musixmatch).is_none(),
            "musixmatch belongs to its own tier, not this chain"
        );
    }

    /// The cached half of the same bug. The waterfall never refetches
    /// once a row exists, so correcting the reader alone would have
    /// left every already-affected track showing the credit forever —
    /// including the reporter's.
    ///
    /// The recogniser is the same one the reader uses, which is what
    /// keeps "what we refuse to store" and "what we refuse to serve"
    /// from drifting apart.
    #[test]
    fn a_cached_blurb_is_recognised_by_the_same_test_that_refuses_it() {
        let blurb = "Provided to YouTube by Some Label\n\nSong · Artist\n\nAlbum";
        assert!(is_service_blurb(blurb), "refused on the way in");
        // …and therefore recognised on the way out, by definition.
        // The deletion itself needs a database and is covered by the
        // app-crate suite; what matters here is that one predicate
        // governs both directions.
        assert!(!is_service_blurb("Real lyrics\nsecond line"));
    }

    /// Reported on discussion #519: a `.m4a` pulled with `yt-dlp`
    /// carries the auto-generated YouTube credit in `description`,
    /// which is several lines long and was therefore read as lyrics —
    /// ahead of the `.lrc` the user had placed next to the file.
    ///
    /// Two things had to be wrong at once: a description field counted
    /// as a lyrics source, and it was consulted before the sidecar.
    /// The blurb is now refused outright, and the tier that remains
    /// sits behind the sidecar.
    #[test]
    fn a_youtube_credit_is_not_lyrics() {
        let blurb = "Provided to YouTube by RCA Records Label\n\n\
             Sorry · Nothing But Thieves\n\n\
             Broken Machine (Deluxe)\n\n\
             ℗ 2017 Sony Music Entertainment UK Limited\n\n\
             Released on: 2017-09-08";
        assert!(is_service_blurb(blurb));
        assert!(is_service_blurb(
            "Auto-generated by YouTube.\nfoo\nbar\nbaz"
        ));

        // And the check stays narrow: anything it does not recognise
        // goes through, because a false positive here costs someone
        // their real lyrics.
        assert!(!is_service_blurb(
            "I found a love for me\nDarling just dive right in\nAnd follow my lead"
        ));
        assert!(!is_service_blurb(""));

        // A song that happens to say the words much later is not
        // boilerplate: the markers are looked for near the top, where
        // the credit always sits.
        let long_song = "la la la\n".repeat(200) + "provided to youtube by nobody";
        assert!(!is_service_blurb(&long_song));
    }

    #[test]
    fn embedded_lyrics_resolution_prioritizes_synced_and_skips_blanks() {
        let s = |x: &str| Some(x.to_string());

        // Synced wins over an unsynced standard tag (the Antra case, #378).
        assert_eq!(
            resolve_embedded_lyrics([s("[00:01.00]timed"), s("plain body"), None]),
            Some("[00:01.00]timed".to_string())
        );
        // A whitespace-only standard tag is treated as absent, so a valid
        // custom TXXX/Vorbis value is selected instead of returning nothing
        // (the whitespace-masks-fallback bug from the CR).
        assert_eq!(
            resolve_embedded_lyrics([None, s("   \n\t  "), s("from txxx")]),
            Some("from txxx".to_string())
        );
        // Unsynced-only falls back to the standard key.
        assert_eq!(
            resolve_embedded_lyrics([None, s("just words"), None]),
            Some("just words".to_string())
        );
        // Every candidate blank/absent → None.
        assert_eq!(resolve_embedded_lyrics([None, s("  "), s("\t")]), None);
        // The synced body is detected as timed LRC downstream.
        assert_eq!(detect_format("[00:01.00]timed"), LyricsFormat::Lrc);
    }

    #[test]
    fn known_key_lyrics_falls_through_blank_uslt_to_lyrics() {
        use lofty::tag::{Tag, TagType};

        // A whitespace-only USLT must not mask a valid LYRICS on the same
        // tag (the deeper case of the blank-tag bug).
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::UnsyncLyrics, "   \n\t ".to_string());
        tag.insert_text(ItemKey::Lyrics, "real body".to_string());
        assert_eq!(known_key_lyrics(&tag), Some("real body".to_string()));

        // USLT present and non-blank wins over LYRICS.
        let mut primary = Tag::new(TagType::VorbisComments);
        primary.insert_text(ItemKey::UnsyncLyrics, "from uslt".to_string());
        primary.insert_text(ItemKey::Lyrics, "from lyrics".to_string());
        assert_eq!(known_key_lyrics(&primary), Some("from uslt".to_string()));

        // Both blank → None.
        let mut blank = Tag::new(TagType::VorbisComments);
        blank.insert_text(ItemKey::UnsyncLyrics, "  ".to_string());
        blank.insert_text(ItemKey::Lyrics, "\t".to_string());
        assert_eq!(known_key_lyrics(&blank), None);
    }

    #[test]
    fn synced_keys_match_vorbis_comments_case_insensitively() {
        use lofty::ogg::tag::VorbisComments;

        // Validates the reader mechanism: our key constants resolve a real
        // VorbisComments case-insensitively, and a plain-only file yields
        // nothing for the synced keys but hits the unsynced set.
        let mut synced = VorbisComments::default();
        synced.push("SyncedLyrics".to_string(), "[00:01.00]timed".to_string());
        assert_eq!(
            SYNCED_LYRICS_KEYS.iter().find_map(|k| synced.get(k)),
            Some("[00:01.00]timed")
        );

        let mut plain = VorbisComments::default();
        plain.push("UNSYNCEDLYRICS".to_string(), "just words".to_string());
        assert!(SYNCED_LYRICS_KEYS
            .iter()
            .find_map(|k| plain.get(k))
            .is_none());
        assert_eq!(
            UNSYNCED_LYRICS_KEYS.iter().find_map(|k| plain.get(k)),
            Some("just words")
        );
    }

    #[test]
    fn detect_format_plain() {
        let sample = "This is just\nsome text without any timestamps.";
        assert_eq!(detect_format(sample), LyricsFormat::Plain);
    }

    #[test]
    fn detect_format_lrc() {
        let sample =
            "[ar:Some Artist]\n[ti:Some Title]\n[00:01.00]First line\n[00:05.50]Second line";
        assert_eq!(detect_format(sample), LyricsFormat::Lrc);
    }

    #[test]
    fn detect_format_enhanced_lrc() {
        let sample =
            "[00:01.00]<00:01.00>Hello <00:01.50>world\n[00:03.00]<00:03.00>Another <00:03.40>line";
        assert_eq!(detect_format(sample), LyricsFormat::EnhancedLrc);
    }

    #[test]
    fn detect_format_enhanced_lrc_no_colon_frac() {
        let sample = "[00:01.00]<00:01>plain stamps still count";
        assert_eq!(detect_format(sample), LyricsFormat::EnhancedLrc);
    }

    #[test]
    fn detect_format_ttml_xml_decl() {
        let sample = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml">
  <body>
    <div>
      <p begin="00:00:01.000" end="00:00:03.000">
        <span begin="00:00:01.000" end="00:00:01.500">Hello</span>
        <span begin="00:00:01.500" end="00:00:03.000">world</span>
      </p>
    </div>
  </body>
</tt>"#;
        assert_eq!(detect_format(sample), LyricsFormat::Ttml);
    }

    #[test]
    fn detect_format_ttml_no_decl() {
        let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="0s">x</p></div></body></tt>"#;
        assert_eq!(detect_format(sample), LyricsFormat::Ttml);
    }

    // #668: whether an answer can be followed decides if it ends the
    // waterfall, so these mirror the renderer's own rules. The untimed
    // shape is Apple's; the text is placeholder.
    #[test]
    fn untimed_apple_ttml_is_not_synced() {
        let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="None" xml:lang="en"><head><metadata/></head><body><div><p>line one</p><p>line two</p></div></body></tt>"#;
        assert!(!lyrics_are_synced(&LyricsFormat::Ttml, sample));
    }
    #[test]
    fn ttml_with_a_timed_line_is_synced() {
        let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="00:01.000" end="00:02.000">line</p></div></body></tt>"#;
        assert!(lyrics_are_synced(&LyricsFormat::Ttml, sample));
    }
    #[test]
    fn ttml_timed_only_on_spans_is_not_synced() {
        let sample = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p><span begin="1s">word</span></p></div></body></tt>"#;
        assert!(!lyrics_are_synced(&LyricsFormat::Ttml, sample));
    }
    #[test]
    fn prefixed_ttml_line_with_begin_is_synced() {
        let sample = r#"<tt:tt xmlns:tt="http://www.w3.org/ns/ttml"><tt:body><tt:div><tt:p begin="1s">line</tt:p></tt:div></tt:body></tt:tt>"#;
        assert!(lyrics_are_synced(&LyricsFormat::Ttml, sample));
    }
    #[test]
    fn unparseable_ttml_is_not_synced() {
        assert!(!lyrics_are_synced(
            &LyricsFormat::Ttml,
            "<tt><p begin=\"1s\">"
        ));
    }
    #[test]
    fn ttml_begin_the_renderer_cannot_read_is_untimed() {
        for begin in ["", "   ", "soon", "1:2:3:4", "-5s"] {
            let sample = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{begin}">line</p></div></body></tt>"#
            );
            assert!(
                !lyrics_are_synced(&LyricsFormat::Ttml, &sample),
                "{begin:?}"
            );
        }
        for begin in ["12.5s", "1500ms", "01:02.5", "01:02:03.4", "5"] {
            let sample = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="{begin}">line</p></div></body></tt>"#
            );
            assert!(lyrics_are_synced(&LyricsFormat::Ttml, &sample), "{begin:?}");
        }
    }
    #[test]
    fn ttml_with_a_malformed_attribute_is_not_synced() {
        // `DOMParser` rejects the document, so the renderer shows nothing
        // timed however valid the rest of it is.
        let duplicate = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" begin="2s">line</p></div></body></tt>"#;
        assert!(!lyrics_are_synced(&LyricsFormat::Ttml, duplicate));
        let elsewhere = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div lang="en" lang="fr"><p begin="1s">line</p></div></body></tt>"#;
        assert!(!lyrics_are_synced(&LyricsFormat::Ttml, elsewhere));
    }

    #[test]
    fn lrc_needs_a_complete_line_stamp() {
        assert!(lyrics_are_synced(&LyricsFormat::Lrc, "[00:01.00]x"));
        assert!(lyrics_are_synced(
            &LyricsFormat::Lrc,
            "[ar:Someone]\n[01:02]x"
        ));
        assert!(lyrics_are_synced(
            &LyricsFormat::EnhancedLrc,
            "[00:01.00]<00:01.00>x"
        ));
        assert!(!lyrics_are_synced(&LyricsFormat::Lrc, "[00:01x\nline"));
        assert!(!lyrics_are_synced(&LyricsFormat::Lrc, "[ar:Someone]\nline"));
        assert!(!lyrics_are_synced(&LyricsFormat::Plain, "x"));
    }

    #[test]
    fn detect_format_brackets_but_no_timestamp_stays_plain() {
        // A line starting with `[foo]` (LRC metadata header) without
        // any actual time-stamped line should NOT be classified as
        // synchronized.
        let sample = "[ar:Artist]\n[ti:Title]\nVerse without timestamps.";
        assert_eq!(detect_format(sample), LyricsFormat::Plain);
    }

    #[test]
    fn word_stamp_present_basic() {
        assert!(word_stamp_present("<00:01.50>word"));
        assert!(word_stamp_present("plain<5:00>more"));
        assert!(!word_stamp_present("nothing here"));
        assert!(!word_stamp_present("<not:a:stamp>"));
    }

    #[test]
    fn sidecar_finds_same_folder_lrc() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("01 Track.mp3");
        std::fs::write(&audio, b"fake audio").unwrap();
        std::fs::write(dir.path().join("01 Track.lrc"), "[00:01.00]Hello world").unwrap();
        let content = read_sidecar_lyrics(&audio).expect("sidecar should be found");
        assert!(content.contains("Hello world"));
    }

    #[test]
    fn sidecar_prefers_lrc_over_txt_in_same_folder() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"").unwrap();
        std::fs::write(dir.path().join("Song.txt"), "plain content").unwrap();
        std::fs::write(dir.path().join("Song.lrc"), "[00:01.00]synced").unwrap();
        let content = read_sidecar_lyrics(&audio).unwrap();
        assert!(content.contains("synced"), "got: {content}");
        assert!(!content.contains("plain content"));
    }

    #[test]
    fn sidecar_falls_back_to_txt() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.flac");
        std::fs::write(&audio, b"").unwrap();
        std::fs::write(dir.path().join("Song.txt"), "plain content").unwrap();
        let content = read_sidecar_lyrics(&audio).unwrap();
        assert_eq!(content, "plain content");
    }

    #[test]
    fn sidecar_finds_lyrics_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Track.mp3");
        std::fs::write(&audio, b"").unwrap();
        let sub = dir.path().join("Lyrics");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Track.lrc"), "[00:00.00]from subfolder").unwrap();
        let content = read_sidecar_lyrics(&audio).unwrap();
        assert!(content.contains("from subfolder"));
    }

    #[test]
    fn sidecar_subfolder_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Track.mp3");
        std::fs::write(&audio, b"").unwrap();
        // lowercase variant — common on Linux rips.
        let sub = dir.path().join("lyrics");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Track.lrc"), "[00:00.00]lower").unwrap();
        let content = read_sidecar_lyrics(&audio).unwrap();
        assert!(content.contains("lower"));
    }

    #[test]
    fn sidecar_stem_match_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.MP3");
        std::fs::write(&audio, b"").unwrap();
        // Stem differs in casing — should still match.
        std::fs::write(dir.path().join("song.LRC"), "[00:00.00]ok").unwrap();
        let content = read_sidecar_lyrics(&audio).unwrap();
        assert!(content.contains("ok"));
    }

    #[test]
    fn sidecar_same_folder_beats_lyrics_subfolder() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Track.mp3");
        std::fs::write(&audio, b"").unwrap();
        std::fs::write(dir.path().join("Track.lrc"), "primary").unwrap();
        let sub = dir.path().join("Lyrics");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("Track.lrc"), "secondary").unwrap();
        let content = read_sidecar_lyrics(&audio).unwrap();
        assert_eq!(content, "primary");
    }

    #[test]
    fn sidecar_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Lonely.mp3");
        std::fs::write(&audio, b"").unwrap();
        assert!(read_sidecar_lyrics(&audio).is_none());
    }

    #[test]
    fn sidecar_empty_lrc_falls_back_to_txt() {
        // Some low-quality rips ship a stub empty `.lrc` alongside a
        // valid plain `.txt`. The empty `.lrc` must NOT short-circuit
        // the waterfall — the `.txt` should win.
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.mp3");
        std::fs::write(&audio, b"").unwrap();
        std::fs::write(dir.path().join("Song.lrc"), "   \n  \n").unwrap();
        std::fs::write(dir.path().join("Song.txt"), "plain backup").unwrap();
        let content = read_sidecar_lyrics(&audio).expect("should fall back to txt");
        assert_eq!(content, "plain backup");
    }

    #[test]
    fn sidecar_skips_directory_named_like_a_sidecar() {
        // A directory named `Song.lrc` must NOT shadow a real
        // `Song.txt` sidecar in the same folder. Before the fix,
        // the directory was selected into `lrc_match`,
        // `read_to_string` failed, and the txt was silently lost.
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.mp3");
        std::fs::write(&audio, b"").unwrap();
        std::fs::create_dir(dir.path().join("Song.lrc")).unwrap();
        std::fs::write(dir.path().join("Song.txt"), "fallback ok").unwrap();
        let content = read_sidecar_lyrics(&audio).expect("should fall back to txt");
        assert_eq!(content, "fallback ok");
    }

    #[test]
    fn sidecar_returns_none_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("Song.mp3");
        std::fs::write(&audio, b"").unwrap();
        std::fs::write(dir.path().join("Song.lrc"), "   \n  \n").unwrap();
        // Whitespace-only payload is treated as a miss so we fall
        // through to the next tier instead of caching an empty hit.
        assert!(read_sidecar_lyrics(&audio).is_none());
    }
}
