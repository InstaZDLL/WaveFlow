//! The local tiers: the embedded tags, the sidecar files next to the
//! audio file, and the generic description field.

use std::path::Path;

use lofty::file::{FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::ItemKey;

use crate::error::AppResult;

use super::*;

/// The sidecar beside the track or in its `Lyrics/` folder, read off the
/// async runtime. Cheap (a couple of stat calls and at most two
/// `read_dir` scans).
async fn read_sidecar_of(meta: &TrackMeta) -> Option<String> {
    let path = meta.file_path.clone();
    tokio::task::spawn_blocking(move || read_sidecar_lyrics(Path::new(&path)))
        .await
        .ok()
        .flatten()
}

pub(super) fn sidecar_payload(track_id: i64, content: String) -> LyricsPayload {
    LyricsPayload {
        track_id,
        format: detect_format(&content),
        content,
        source: LyricsSource::LrcFile,
        provider: None,
        tag_write_skipped: None,
        associated: Vec::new(),
    }
}

/// The sidecar tier on its own, cached when found.
async fn try_sidecar_lyrics(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    meta: &TrackMeta,
) -> AppResult<Option<LyricsPayload>> {
    let Some(content) = read_sidecar_of(meta).await else {
        return Ok(None);
    };
    let payload = sidecar_payload(track_id, content);
    upsert_lyrics(
        pool,
        &meta.file_hash,
        &payload.content,
        &payload.format,
        &payload.source,
        None,
    )
    .await?;
    Ok(Some(payload))
}

/// Write a sidecar's lyrics over a cached miss, and only over a miss: the
/// row is checked again by the statement that writes it, so lyrics that a
/// lookup or an edit stored since the miss was read are left as they are.
/// `false` when the row no longer holds a miss.
pub(super) async fn replace_miss(
    pool: &sqlx::SqlitePool,
    file_hash: &str,
    payload: &LyricsPayload,
) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let replaced = sqlx::query(
        "UPDATE app.lyrics
            SET content = ?, format = ?, source = ?, provider = NULL, language = NULL,
                fetched_at = ?, retry_after = NULL
          WHERE file_hash = ? AND content = ''",
    )
    .bind(&payload.content)
    .bind(format_to_db(&payload.format))
    .bind(source_to_db(&payload.source))
    .bind(now_ms())
    .bind(file_hash)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if replaced {
        // A miss carries no bundle, but a new primary ends any bundle,
        // as in `upsert_lyrics_until`.
        sqlx::query("DELETE FROM app.lyrics_associated WHERE file_hash = ?")
            .bind(file_hash)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(replaced)
}

/// [`read_cached`], except that a cached miss is first checked against
/// the track's sidecar.
///
/// The cache is keyed by the audio's hash, and a complete miss is kept
/// for good, so a sidecar the reader could not see when the miss was
/// written would stay hidden behind it forever: one added since, or one
/// in an encoding read as UTF-8 only before #910. Fixing the reader alone
/// would only have helped tracks nobody had opened yet. The sidecar is the
/// one local source that can change without the hash changing (a tag edit
/// changes it, and with it the cache key), and it outranks a miss in
/// every order the waterfall runs, an instrumental verdict included. So a
/// miss costs one directory listing per lookup, and only for tracks
/// without lyrics. The sidecar replaces the miss only if the row still
/// holds it ([`replace_miss`]); anything written in between is served
/// instead.
pub(super) async fn read_cached_or_new_sidecar(
    pool: &sqlx::SqlitePool,
    track_id: i64,
) -> AppResult<Option<LyricsPayload>> {
    let cached = read_cached(pool, track_id).await?;
    if !cached.as_ref().is_some_and(|row| row.content.is_empty()) {
        return Ok(cached);
    }
    let Some(meta) = read_track_meta(pool, track_id).await? else {
        return Ok(cached);
    };
    let Some(content) = read_sidecar_of(&meta).await else {
        return Ok(cached);
    };
    let payload = sidecar_payload(track_id, content);
    if replace_miss(pool, &meta.file_hash, &payload).await? {
        return Ok(Some(payload));
    }
    read_cached(pool, track_id).await
}

/// Local tiers, in order: embedded lyrics tag → sidecar `.ttml`/`.lrc`/
/// `.txt` → the generic description field. Returns the first hit, already
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
pub(super) async fn try_local_lyrics(
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
            associated: Vec::new(),
        }));
    }

    if let Some(payload) = try_sidecar_lyrics(pool, track_id, meta).await? {
        return Ok(Some(payload));
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
            associated: Vec::new(),
        }));
    }

    Ok(None)
}

/// TXXX (ID3v2) / Vorbis-comment descriptions carrying **synced** lyrics,
/// most-preferred first. Antra and similar rips ship a timestamped LRC
/// body under `SYNCEDLYRICS` alongside a plain `LYRICS`/`UNSYNCEDLYRICS`
/// tag; we want the synced one to win (issue #378).
pub(super) const SYNCED_LYRICS_KEYS: &[&str] = &[
    "SYNCEDLYRICS",
    "SYNCED LYRICS",
    "SYNCED_LYRICS",
    "LYRICS_SYNCED",
];

/// TXXX (ID3v2) / Vorbis-comment descriptions carrying plain / unsynced
/// lyrics — the fallback once no synced tag is present.
pub(super) const UNSYNCED_LYRICS_KEYS: &[&str] = &[
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
pub(super) fn read_embedded_lyrics(path: &Path) -> Option<String> {
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
pub(super) fn read_local_after_embedded(path: &Path) -> Option<(String, LyricsSource)> {
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
pub(super) fn is_service_blurb(text: &str) -> bool {
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
pub(super) fn known_key_lyrics(tag: &lofty::tag::Tag) -> Option<String> {
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
pub(super) fn resolve_embedded_lyrics(candidates: [Option<String>; 3]) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
}

/// Match a sidecar lyrics file on disk for an audio track.
///
/// Looks for `{stem}.ttml` / `{stem}.lrc` / `{stem}.txt` (case-insensitive
/// on both the stem and the extension) either next to the audio file or
/// inside a sibling `Lyrics/` directory. Returns the file contents; the
/// caller sniffs the format from them.
///
/// Common K-Pop / J-Pop rip layouts ship synced lyrics as sidecars
/// rather than embedded tags, and the user may also keep them in a
/// `Lyrics/` subfolder to declutter the listing. Both layouts are
/// supported here.
///
/// When a directory holds more than one, the richest wins
/// ([`sidecar_richness`]): synced lyrics over unsynced, word timing over
/// line timing; between two as rich, the one written last, and only then
/// `.ttml` over `.lrc` over `.txt`. Judged on the contents rather than the
/// extension, so an Apple-style `.ttml` with no timing does not hide a
/// synced `.lrc`, a `.txt` that holds LRC counts as the LRC it is, and an
/// edit the editor saves as `.lrc` beside an exported `.ttml` of the same
/// richness is the one read back.
///
/// Same-folder hits always beat `Lyrics/` hits because users who
/// duplicate lyrics in both spots almost certainly want the same-
/// folder copy as the primary.
pub(super) fn read_sidecar_lyrics(audio_path: &Path) -> Option<String> {
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

/// Inner helper for [`read_sidecar_lyrics`]: scan `dir` once, read every
/// candidate, keep the richest. Stem matching is case-insensitive so a
/// Windows rip with `Song.MP3` still finds `song.lrc` cleanly on Linux.
fn read_stem_match_in_dir(dir: &Path, stem: &str) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    // Indexed by the extension's rank, the last tie-breaker.
    let mut matches: [Option<std::path::PathBuf>; 3] = [None, None, None];
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip directories early. Without this a directory named
        // `Song.lrc` would be picked as the `.lrc`, the read below
        // would fail, and a legitimate `Song.txt` in the same
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
        let rank = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref()
        {
            Some("txt") => 0,
            Some("lrc") => 1,
            Some("ttml") => 2,
            _ => continue,
        };
        matches[rank] = Some(path);
    }
    // An empty or whitespace-only file is not a candidate: a stub `Song.lrc`
    // (common in low-quality rips) must not mask a valid `Song.txt`. Nor is
    // a TTML the renderer could not show a line of.
    matches
        .iter()
        .enumerate()
        .filter_map(|(rank, path)| {
            let path = path.as_deref()?;
            let content = read_non_empty_file(path)?;
            let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            Some((rank, modified, content))
        })
        .filter_map(|(rank, modified, content)| {
            let (synced, words) = sidecar_richness(&content)?;
            Some(((synced, words, modified, rank), content))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, content)| content)
}

/// How much a sidecar's lyrics carry, judged the way the renderer will:
/// whether they are synced at all ([`lyrics_are_synced`]), and whether
/// they are timed word by word (Enhanced LRC, or TTML with a timed
/// `<span>`). `None` for a TTML document the renderer could not show a
/// line of — malformed, or with no `<p>` that holds text — which is no
/// better than no file, and must not outrank a readable `.txt`.
fn sidecar_richness(content: &str) -> Option<(bool, bool)> {
    let format = detect_format(content);
    if matches!(format, LyricsFormat::Ttml) && !ttml_has_a_line(content) {
        return None;
    }
    let synced = lyrics_are_synced(&format, content);
    let words = synced
        && match format {
            LyricsFormat::EnhancedLrc => true,
            LyricsFormat::Ttml => TTML_TIMED_SPAN.is_match(content),
            LyricsFormat::Lrc | LyricsFormat::Plain => false,
        };
    Some((synced, words))
}

/// Read a text file and return its trimmed contents, or `None` if
/// the file is missing, unreadable, or contains only whitespace. The
/// file need not be UTF-8: lyrics saved as GB18030, Big5, Shift_JIS or
/// a Windows code page are decoded too (#910).
fn read_non_empty_file(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let text = waveflow_core::text_encoding::decode(&raw);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
