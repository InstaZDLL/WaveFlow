//! The lyrics payload every tier answers with, its format and source
//! codes, and the track metadata a lookup starts from.

use chrono::Utc;
use serde::Serialize;

use crate::error::AppResult;

pub(super) fn now_ms() -> i64 {
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

pub(super) fn parse_format(s: &str) -> LyricsFormat {
    match s {
        "lrc" => LyricsFormat::Lrc,
        "enhanced_lrc" => LyricsFormat::EnhancedLrc,
        "ttml" => LyricsFormat::Ttml,
        _ => LyricsFormat::Plain,
    }
}

pub(super) fn parse_source(s: &str) -> LyricsSource {
    match s {
        "lrc_file" => LyricsSource::LrcFile,
        "api" => LyricsSource::Api,
        "manual" => LyricsSource::Manual,
        _ => LyricsSource::Embedded,
    }
}

pub(super) fn format_to_db(fmt: &LyricsFormat) -> &'static str {
    match fmt {
        LyricsFormat::Plain => "plain",
        LyricsFormat::Lrc => "lrc",
        LyricsFormat::EnhancedLrc => "enhanced_lrc",
        LyricsFormat::Ttml => "ttml",
    }
}

pub(super) fn source_to_db(src: &LyricsSource) -> &'static str {
    match src {
        LyricsSource::Embedded => "embedded",
        LyricsSource::LrcFile => "lrc_file",
        LyricsSource::Api => "api",
        LyricsSource::Manual => "manual",
    }
}

/// Look up the track's metadata needed to call LRCLIB and to read the
/// embedded tag.
pub(super) async fn read_track_meta(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> AppResult<Option<TrackMeta>> {
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

pub(super) struct TrackMeta {
    pub(super) file_path: String,
    pub(super) file_hash: String,
    pub(super) title: String,
    pub(super) artist_name: Option<String>,
    pub(super) album_title: Option<String>,
    pub(super) duration_ms: i64,
}
