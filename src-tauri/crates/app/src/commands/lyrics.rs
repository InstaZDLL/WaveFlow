//! Lyrics fetch + cache.
//!
//! Lazy multi-tier lookup, in order:
//!   1. Local DB cache (`lyrics` table, keyed by `track_id`)
//!   2. Embedded lyrics tag inside the audio file (via lofty), preferring
//!      a synced `SYNCEDLYRICS` tag over the plain `USLT` / `LYRICS` ones
//!   3. Local sidecar file — `{stem}.lrc` / `{stem}.txt` next to the
//!      audio file, or inside a `Lyrics/` (case-insensitive) subfolder
//!      next to it. `.lrc` wins over `.txt` (timing info).
//!   4. Musixmatch Enhanced LRC when word-level timing exists
//!   5. LRCLIB public API (matched by artist + track + album + duration)
//!   6. Query-based external providers before caching a network miss
//!
//! Whichever tier hits first becomes the cached entry. We never refetch
//! once a row exists — the user can manually overwrite by importing a
//! `.lrc` file via [`import_lrc_file`].

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    vec![Provider::NetEase, Provider::Megalobiz, Provider::Genius]
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

async fn external_lyrics_search(
    meta: &TrackMeta,
    providers: Vec<Provider>,
    mode: SearchMode,
    enhanced: bool,
    lang: Option<&str>,
) -> AppResult<Option<ExternalLyricsResult>> {
    // Defense in depth: every outbound HTTP path must honour offline mode
    // regardless of caller. Returning Ok(None) short-circuits before any
    // request is issued (see the process-wide offline contract).
    if crate::offline::is_offline() {
        return Ok(None);
    }
    // Apply the Musixmatch opt-in here — at a single chokepoint — so
    // call sites don't have to know whether Musixmatch is in their
    // provider list. An empty list after filtering means "nothing left
    // to query": short-circuit instead of building a no-op client.
    let providers = filter_providers(providers);
    if providers.is_empty() {
        return Ok(None);
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
    // A transient provider error surfaces as Err here (not Ok(None)) so
    // callers don't cache an empty "miss" on a network blip.
    client
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
        .map_err(|err| AppError::Other(format!("external lyrics search failed: {err}")))
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
    })
}

/// Walk the post-LRCLIB fallback chain (NetEase / Megalobiz / Genius)
/// and persist whatever comes back. When every provider misses — or one
/// errors and the rest miss — cache an empty row so the panel doesn't
/// re-hit the network on every open. Always returns a `LyricsPayload`:
/// either the freshly-cached external result or the empty miss.
///
/// Called from both `fetch_lyrics` LRCLIB branches (404 + empty-row).
/// Keeping the chain in one place stops the two call sites from drifting
/// on the error-handling contract — both must treat a provider Err as
/// non-fatal (LRCLIB has already given a verdict, the fallback is best-
/// effort enrichment), and a refactor of that policy now lives in one
/// function instead of two.
async fn try_external_fallback_or_miss(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
) -> AppResult<LyricsPayload> {
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
        Ok(Some(result)) => {
            return cache_external_lyrics(pool, track_id, &meta.file_hash, result).await;
        }
        Ok(None) => {}
        Err(err) => tracing::debug!(?err, "external fallback chain failed"),
    }

    // No provider had lyrics. Cache as an empty row so we don't re-hit
    // the network on every panel open. The user can force a re-search
    // by clicking "Refetch" in the lyrics panel (clears the row,
    // re-runs the waterfall). `provider = None` for the miss row —
    // attribution to a specific provider would be misleading when
    // every provider returned nothing.
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
    })
}

/// TXXX (ID3v2) / Vorbis-comment descriptions carrying **synced** lyrics,
/// most-preferred first. Antra and similar rips ship a timestamped LRC
/// body under `SYNCEDLYRICS` alongside a plain `LYRICS`/`UNSYNCEDLYRICS`
/// tag; we want the synced one to win (issue #378).
const SYNCED_LYRICS_KEYS: &[&str] =
    &["SYNCEDLYRICS", "SYNCED LYRICS", "SYNCED_LYRICS", "LYRICS_SYNCED"];

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

    fn pick(comments: &lofty::ogg::VorbisComments, keys: &[&str]) -> Option<String> {
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
fn read_custom_lyrics_tag(path: &Path, file_type: Option<FileType>, keys: &[&str]) -> Option<String> {
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
///   5. Generic `Description` field as last resort.
fn read_embedded_lyrics(path: &Path) -> Option<String> {
    let probe = Probe::open(path).ok()?.guess_file_type().ok()?;
    let file_type = probe.file_type();
    let tagged = probe.read().ok()?;

    // Synced tag takes priority over every plain-lyrics source, including
    // the standard USLT/Lyrics keys.
    let from_synced = read_custom_lyrics_tag(path, file_type, SYNCED_LYRICS_KEYS);

    let from_known_key = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(|tag| {
            tag.get_string(ItemKey::UnsyncLyrics)
                .or_else(|| tag.get_string(ItemKey::Lyrics))
                .map(|s| s.to_string())
        });

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

    let from_description = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag())
        .and_then(|tag| {
            #[allow(deprecated)]
            tag.get_string(ItemKey::Description)
                .filter(|s| s.lines().count() > 3)
                .map(|s| s.to_string())
        });

    let raw = from_synced
        .or(from_known_key)
        .or(from_custom_unsynced)
        .or(from_description)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
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
    sqlx::query(
        "INSERT INTO app.lyrics (file_hash, content, format, source, provider, language, fetched_at)
         VALUES (?, ?, ?, ?, ?, NULL, ?)
         ON CONFLICT(file_hash) DO UPDATE SET
            content = excluded.content,
            format = excluded.format,
            source = excluded.source,
            provider = excluded.provider,
            fetched_at = excluded.fetched_at",
    )
    .bind(file_hash)
    .bind(content)
    .bind(format_to_db(format))
    .bind(source_to_db(source))
    .bind(provider)
    .bind(now_ms())
    .execute(pool)
    .await?;
    Ok(())
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
    Ok(row.map(|(content, fmt, src, provider)| LyricsPayload {
        track_id,
        content,
        format: parse_format(&fmt),
        source: parse_source(&src),
        provider,
        tag_write_skipped: None,
        sidecar_write_skipped: None,
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
        upsert_lyrics(&pool, &meta.file_hash, &content, &format, &source, None).await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format,
            source,
            provider: None,
            tag_write_skipped: None,
            sidecar_write_skipped: None,
        }));
    }

    // 3. Local sidecar `.lrc` / `.txt`. Cheap (a couple of stat calls
    //    + at most two `read_dir` scans), runs before the network so
    //    a user with bundled lyrics never pays the LRCLIB latency.
    let path_for_sidecar = meta.file_path.clone();
    let sidecar =
        tokio::task::spawn_blocking(move || read_sidecar_lyrics(Path::new(&path_for_sidecar)))
            .await
            .ok()
            .flatten();

    if let Some(content) = sidecar {
        let format = detect_format(&content);
        let source = LyricsSource::LrcFile;
        upsert_lyrics(&pool, &meta.file_hash, &content, &format, &source, None).await?;
        return Ok(Some(LyricsPayload {
            track_id,
            content,
            format,
            source,
            provider: None,
            tag_write_skipped: None,
            sidecar_write_skipped: None,
        }));
    }

    // 4. Musixmatch enhanced fallback. This runs before LRCLIB only
    //    when it returns true word-level LRC; regular line-level LRC
    //    still lets the stricter metadata LRCLIB lookup below win.
    if !crate::offline::is_offline() {
        // User-driven path: opt into the per-profile translation
        // language so a hit comes back with each line followed by
        // its `(translation)` companion (issue #208). Prefetch +
        // scanner paths stay `None` to avoid the extra Musixmatch
        // hop per track on bulk operations.
        let translation_lang = read_translation_lang(&pool).await.unwrap_or_else(|err| {
            tracing::warn!(
                ?err,
                "read_translation_lang failed; serving untranslated lyrics"
            );
            None
        });
        // A Musixmatch failure here is non-fatal: fall through to LRCLIB
        // rather than aborting the whole lookup or caching a miss.
        match external_lyrics_search(
            &meta,
            vec![Provider::Musixmatch],
            SearchMode::SyncedOnly,
            true,
            translation_lang.as_deref(),
        )
        .await
        {
            Ok(Some(result)) if matches!(result.format, ExternalLyricsFormat::EnhancedLrc) => {
                return cache_external_lyrics(&pool, track_id, &meta.file_hash, result)
                    .await
                    .map(Some);
            }
            Ok(_) => {}
            Err(err) => tracing::debug!(?err, "Musixmatch enhanced lookup failed"),
        }
    }

    // 5. LRCLIB fallback. Skip if we have no artist (matching is
    //    useless without one) or if offline mode is on (the cache +
    //    embedded tiers above already ran).
    if crate::offline::is_offline() {
        return Ok(None);
    }
    let Some(artist_name) = meta.artist_name.as_deref() else {
        return Ok(None);
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
            // LRCLIB 404 — try the broader provider chain before
            // caching a miss. These sources are query-based and less
            // strict than LRCLIB's metadata endpoint, so they only run
            // after the exact lookup fails. See
            // `try_external_fallback_or_miss` for the error-handling
            // contract (provider Err is non-fatal at this tier).
            return try_external_fallback_or_miss(&pool, track_id, &meta)
                .await
                .map(Some);
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
        // Attribute the row to LRCLIB so the UI badge reflects who told
        // us this track is instrumental — the user can still try a
        // different provider via `refetch_lyrics` if they disagree.
        let empty = String::new();
        upsert_lyrics(
            &pool,
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
            // via the shared helper.
            return try_external_fallback_or_miss(&pool, track_id, &meta)
                .await
                .map(Some);
        }
    };

    let source = LyricsSource::Api;
    let provider = Provider::Lrclib.as_str();
    upsert_lyrics(
        &pool,
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
        .fetch_optional(&pool)
        .await?;
    let Some(file_hash) = file_hash else {
        return Ok(None);
    };
    sqlx::query("DELETE FROM app.lyrics WHERE file_hash = ?")
        .bind(&file_hash)
        .execute(&pool)
        .await?;

    // No provider pinned → identical to a fresh `fetch_lyrics` call,
    // since the cache row is gone and the waterfall starts at the
    // embedded tier.
    let Some(provider_str) = provider.as_deref() else {
        return fetch_lyrics(state, track_id).await;
    };

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
        Ok(Some(result)) => cache_external_lyrics(&pool, track_id, &meta.file_hash, result)
            .await
            .map(Some),
        Ok(None) => {
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
    upsert_lyrics(&pool, &file_hash, trimmed, &format, &source, None).await?;
    Ok(LyricsPayload {
        track_id,
        content: trimmed.to_string(),
        format,
        source,
        provider: None,
        tag_write_skipped: None,
        sidecar_write_skipped: None,
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
        let sidecar =
            tokio::task::spawn_blocking(move || read_sidecar_lyrics(Path::new(&path_for_sidecar)))
                .await
                .ok()
                .flatten();
        if let Some(content) = sidecar {
            let format = detect_format(&content);
            let source = LyricsSource::LrcFile;
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
            Ok(Some(result)) if matches!(result.format, ExternalLyricsFormat::EnhancedLrc) => {
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
                            Ok(Some(result)) => {
                                if let Err(e) =
                                    cache_external_lyrics(&pool, track_id, &file_hash, result).await
                                {
                                    tracing::warn!(track_id, ?e, "persist external lyrics failed");
                                    failed += 1;
                                } else {
                                    hits += 1;
                                }
                            }
                            Ok(None) => {
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
                    Ok(Some(result)) => {
                        if let Err(e) =
                            cache_external_lyrics(&pool, track_id, &file_hash, result).await
                        {
                            tracing::warn!(track_id, ?e, "persist external lyrics failed");
                            failed += 1;
                        } else {
                            hits += 1;
                        }
                    }
                    Ok(None) => {
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
            let format_for_write = format.clone();
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
    let stem = audio_path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| AppError::Other("audio file has no stem".to_string()))?;
    let parent = audio_path
        .parent()
        .ok_or_else(|| AppError::Other("audio file has no parent dir".to_string()))?;
    let ext = if plain { "txt" } else { "lrc" };
    let sidecar = parent.join(format!("{stem}.{ext}"));
    std::fs::write(&sidecar, content)
        .map_err(|e| AppError::Other(format!("write {}: {e}", sidecar.display())))?;
    Ok(())
}

fn hash_file_blake3(path: &str) -> AppResult<String> {
    let bytes = std::fs::read(path).map_err(|e| AppError::Other(format!("read for hash: {e}")))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Write the lyrics back into the audio file's tag.
///
/// - Plain / LRC / Enhanced LRC → `ItemKey::UnsyncLyrics` (USLT for
///   ID3v2, UNSYNCEDLYRICS for Vorbis, `©lyr` for MP4). All three are
///   plain ASCII-safe text formats.
/// - TTML → `ItemKey::Lyrics` for tag systems that accept arbitrary
///   strings (Vorbis comments, MP4 `©lyr`). ID3v2 has no clean mapping
///   for XML lyrics in lofty, so for MP3 we skip the file write and
///   return `Ok(false)` — the DB cache still gets updated and the UI
///   surfaces a toast so the user knows their TTML stays in-app only.
///
/// Returns `Ok(true)` when the tag was rewritten on disk, `Ok(false)`
/// when the write was intentionally skipped (TTML on a format that
/// can't carry it).
fn write_lyrics_to_file(
    path: &Path,
    content: &str,
    format: &LyricsFormat,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use lofty::file::{AudioFile, FileType, TaggedFileExt};
    use lofty::tag::Tag;

    let mut tagged = lofty::read_from_path(path)?;
    let file_type = tagged.file_type();

    // Bail before touching tags when TTML hits an ID3v2-only container.
    if matches!(format, LyricsFormat::Ttml) && file_type == FileType::Mpeg {
        return Ok(false);
    }

    if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
        let preferred = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(preferred));
    }
    let tag = if tagged.primary_tag().is_some() {
        tagged.primary_tag_mut().expect("checked")
    } else {
        tagged.first_tag_mut().ok_or("no tag")?
    };

    // Always purge both keys before writing so that switching format
    // (e.g. plain LRC → TTML) doesn't leave a stale entry under the
    // other key. `read_embedded_lyrics` checks UnsyncLyrics first and
    // Lyrics second — without this clear the old content would shadow
    // the new format on the next fetch.
    tag.remove_key(ItemKey::UnsyncLyrics);
    tag.remove_key(ItemKey::Lyrics);

    if !content.trim().is_empty() {
        // TTML on a container that supports `ItemKey::Lyrics` (Vorbis /
        // MP4 / FLAC). Other formats stay in USLT, which is what every
        // other player expects.
        let key = if matches!(format, LyricsFormat::Ttml) {
            ItemKey::Lyrics
        } else {
            ItemKey::UnsyncLyrics
        };
        tag.insert_text(key, content.to_string());
    }

    tagged.save_to_path(path, lofty::config::WriteOptions::default())?;
    Ok(true)
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
    .execute(pool)
    .await?;
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
        Some(r) => {
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
            }))
        }
        None => {
            // Cache the miss (empty content) so a recurring song on the
            // station's rotation doesn't re-hit the network every time.
            upsert_radio_lyrics(&pool, &key, artist, title, "", &LyricsFormat::Plain, None).await?;
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
            .execute(&pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM profile_setting WHERE key = ?")
                .bind(TRANSLATION_LANG_KEY)
                .execute(&pool)
                .await?;
        }
    }
    Ok(normalized)
}

/// `app_setting` key holding the global default lyrics destination
/// the editor pre-fills the segmented control with. App-wide rather
/// than per-profile because the destination drives filesystem state
/// (tag bytes, sidecar files) that is shared between profiles whose
/// libraries touch the same audio files — a per-profile choice
/// would create a false sense of isolation.
pub const LYRICS_DEFAULT_DESTINATION_KEY: &str = "lyrics.default_destination";

/// Read the app-wide default. Missing row falls back to `Tag` so
/// existing installs keep the pre-#201 behaviour until the user opts
/// into the new flow (either from the onboarding step or the
/// Settings → Playback card).
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

    #[test]
    fn synced_keys_win_over_unsynced_in_vorbis_comments() {
        use lofty::ogg::VorbisComments;

        // A file carrying both tags (the Antra case, #378): the synced key
        // must resolve, case-insensitively, before we ever consult the
        // plain lyrics key.
        let mut both = VorbisComments::default();
        both.push("LYRICS".to_string(), "plain body".to_string());
        both.push("SyncedLyrics".to_string(), "[00:01.00]timed".to_string());
        assert_eq!(
            SYNCED_LYRICS_KEYS.iter().find_map(|k| both.get(k)),
            Some("[00:01.00]timed"),
            "synced key should match regardless of stored case"
        );

        // A plain-only file yields nothing for the synced keys and falls
        // through to the unsynced set.
        let mut plain = VorbisComments::default();
        plain.push("UNSYNCEDLYRICS".to_string(), "just words".to_string());
        assert!(SYNCED_LYRICS_KEYS.iter().find_map(|k| plain.get(k)).is_none());
        assert_eq!(
            UNSYNCED_LYRICS_KEYS.iter().find_map(|k| plain.get(k)),
            Some("just words")
        );

        // A synced LRC body is detected as timed lyrics downstream.
        assert_eq!(detect_format("[00:01.00]timed"), LyricsFormat::Lrc);
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
