//! Lyrics the user edited: caching them, writing them to a sidecar or
//! into the file's tags, clearing them, and exporting them to a path.

use std::path::Path;
use std::sync::Arc;

use lofty::file::FileType;
use serde::Deserialize;
use tauri::{AppHandle, Emitter};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

// ── User-edited lyrics ──────────────────────────────────────────────

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
    /// Write a sibling `.ttml` (TTML), `.lrc` (other synced formats) or
    /// `.txt` (plain) next to the audio file. The waterfall reader picks
    /// it up at next play.
    /// Audio file tags are left untouched — issue #201 use case.
    Sidecar,
    /// DB cache only. The audio file is untouched and no sidecar is
    /// emitted; lyrics live inside WaveFlow's per-profile DB only.
    /// Useful for privacy-conscious users and for one-shot tweaks.
    DbOnly,
}

impl LyricsDestination {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Sidecar => "sidecar",
            Self::DbOnly => "db_only",
        }
    }

    pub(super) fn parse(s: &str) -> Option<Self> {
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
    /// Format hint coming from the in-app editor. The backend re-runs
    /// `detect_format` on the content as a safety net so a mistyped
    /// header still ends up in the right bucket.
    pub format: LyricsFormat,
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
    let format = match payload.format {
        LyricsFormat::Plain => LyricsFormat::Plain,
        LyricsFormat::Ttml => LyricsFormat::Ttml,
        // For Lrc / EnhancedLrc the detector picks between Lrc,
        // EnhancedLrc and Plain (if the user cleared every stamp).
        LyricsFormat::Lrc | LyricsFormat::EnhancedLrc => detected,
    };

    let mut tag_write_skipped = false;
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
            // TTML goes to `{stem}.ttml`, which the sidecar reader takes
            // as it takes `.lrc` and `.txt`.
            let path = std::path::PathBuf::from(&file_path);
            let content_for_write = trimmed.clone();
            let sidecar_format = format;
            tokio::task::spawn_blocking(move || {
                write_lyrics_sidecar(&path, &content_for_write, &sidecar_format)
            })
            .await
            .map_err(|e| AppError::Other(format!("sidecar write panicked: {e}")))?
            .map_err(|e| AppError::Other(format!("sidecar write failed: {e}")))?;
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
        associated: Vec::new(),
    })
}

/// Write a sibling `.ttml`, `.lrc` or `.txt` ([`sidecar_path`]) next to
/// the audio file, replacing any existing matching-extension sidecar.
///
/// We deliberately don't try to drop the OTHER extensions: silently
/// deleting a user-managed sidecar is more surprising than the lingering
/// file. The reader prefers the richer file and, between two as rich, the
/// one written last, so what is saved here is read back unless a richer
/// sidecar of another extension sits beside it — a word-timed `.ttml`
/// beside an edit saved line by line. The cache row keeps the edit either
/// way; only a cleared cache would bring the other file back.
/// UTF-8 without BOM matches the format every LRC and TTML reader
/// expects.
pub(super) fn write_lyrics_sidecar(
    audio_path: &Path,
    content: &str,
    format: &LyricsFormat,
) -> AppResult<()> {
    let sidecar = sidecar_path(audio_path, format)
        .ok_or_else(|| AppError::Other("audio file has no stem or parent dir".to_string()))?;
    // Through a partial renamed into place: re-saving lyrics the user
    // already had must not leave a truncated `.lrc` if it fails halfway.
    waveflow_core::atomic_write::write_bytes(&sidecar, content.as_bytes())
        .map_err(|e| AppError::Other(format!("write {}: {e}", sidecar.display())))?;
    Ok(())
}

/// Where a track's sidecar goes: `{stem}.ttml` for TTML, `{stem}.txt` for
/// plain text and `{stem}.lrc` for every other synced format, in the
/// track's own directory. `None` for a path with no stem or no parent,
/// which is not a file we can sit beside.
///
/// Shared by the editor's write and the export of fetched lyrics (#695),
/// so the two cannot drift into disagreeing about where a sidecar lives —
/// the export checks for an existing one at exactly the path the editor
/// would have written.
pub(super) fn sidecar_path(audio_path: &Path, format: &LyricsFormat) -> Option<std::path::PathBuf> {
    let stem = audio_path.file_stem().and_then(|s| s.to_str())?;
    let parent = audio_path.parent()?;
    let ext = match format {
        LyricsFormat::Ttml => "ttml",
        LyricsFormat::Plain => "txt",
        LyricsFormat::Lrc | LyricsFormat::EnhancedLrc => "lrc",
    };
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
    // Through a partial renamed into place, so an export over an
    // earlier file that fails halfway leaves that file whole.
    tokio::task::spawn_blocking(move || {
        waveflow_core::atomic_write::write_bytes(&path, content.as_bytes())
    })
    .await
    .map_err(|e| AppError::Other(format!("export_lyrics_to_path write panicked: {e}")))?
    .map_err(|e| AppError::Other(format!("export_lyrics_to_path write failed: {e}")))?;
    Ok(())
}
