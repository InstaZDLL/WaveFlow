//! Single-track ID3 / Vorbis tag editor.
//!
//! Mirrors the scanner's tag → DB pipeline in reverse: the user edits a
//! handful of fields, the values are written back to the audio file via
//! lofty, and the database (track / album / artist / track_artist /
//! genre / track_genre) is updated to match. FTS5 stays in sync via the
//! existing triggers on `track`, `album.title`, and `artist.name`.
//!
//! File-lock dance: the audio engine may have the file open if the
//! edited track is currently playing. Either way the write needs the
//! real file — in place, or as the target of a rename — so on Windows
//! the engine's read handle is enough to refuse it. We pause playback
//! before writing whenever the engine reports the same
//! `current_track_id`. Resume is left to the user: silently restarting
//! after a save would be surprising, and would re-open the file while we
//! are still writing it.
//!
//! **Two ways of writing, and which one runs matters to the file's
//! identity.** [`patch_file`] tries the in-place path first (#590): the
//! new tag is laid over the one already there, which works whenever it
//! fits the padding, and keeps the inode — with it the permissions, the
//! ACL, the extended attributes and the hard links. When it does not
//! fit, the rewrite runs through
//! [`waveflow_core::tagio::rewrite_via_temp`] (#598): a sibling
//! `.wf-tmp` beside the original, replaced by a single `rename`. That
//! one cannot be interrupted into a half-written file, which is the
//! point — but it hands back a **new inode**, so hard links to the old
//! one keep the old content and anything past the permission bits does
//! not follow. DSF is neither: its tag sits after the audio, so it is
//! always written in place ([`patch_dsf`], #592).

use std::sync::Arc;

use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use waveflow_core::scanner::{
    canonical_name, split_artist_name, split_genres, upsert_album, upsert_artist, upsert_artwork,
    upsert_genre, ArtworkSource,
};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

/// Recompute the BLAKE3 hash of an on-disk audio file and persist it to
/// `track.file_hash`. Required after every command that mutates the file
/// on disk (tag write, cover write, rating, lyrics write) so the
/// scanner's `(file_modified, file_hash)` fast path keeps recognising
/// the file on the next pass and the per-hash caches (lyrics, etc.) stay
/// addressable. Errors propagate so the caller can decide whether to
/// fail the command or just warn.
pub(crate) async fn rehash_track_file(
    pool: &SqlitePool,
    track_id: i64,
    path: &std::path::Path,
) -> AppResult<String> {
    let path_owned = path.to_path_buf();
    let new_hash = tokio::task::spawn_blocking(move || -> Result<String, std::io::Error> {
        let bytes = std::fs::read(&path_owned)?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    })
    .await
    .map_err(|e| AppError::Other(format!("rehash join: {e}")))?
    .map_err(|e| AppError::Other(format!("rehash read: {e}")))?;
    sqlx::query("UPDATE track SET file_hash = ? WHERE id = ?")
        .bind(&new_hash)
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Other(format!("rehash update: {e}")))?;
    // The whole-file digest cached for the server is about these bytes, and
    // these bytes have just changed. Dropping it here rather than relying on
    // `(size, mtime)` to notice covers the one case that pair cannot: a
    // rewrite that lands on the same size and whose mtime a tool preserved.
    #[cfg(feature = "sync_v2")]
    if let Err(err) = crate::remote::hashing::forget(pool, track_id).await {
        tracing::warn!(
            track = track_id,
            ?err,
            "could not drop the cached full hash"
        );
    }
    Ok(new_hash)
}

/// Edit payload from the frontend. Every field is optional — `None`
/// means "leave this field untouched"; `Some("")` means "clear this
/// field" (where applicable). The frontend sends whatever's currently
/// in the form input on save, so we always get every field set when
/// the user explicitly hits Save.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TrackEdit {
    pub title: Option<String>,
    /// Raw multi-artist string ("Artist A; Artist B"). Split via the
    /// scanner's `split_artist_name`, on `"; "` only: a comma belongs to
    /// names like "Earth, Wind & Fire".
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    /// Every genre, `;`-separated ("Rock; Jazz"), split by the scanner's
    /// `split_genres` so the file and the database read it the same way
    /// (#729). Empty clears them.
    pub genre: Option<String>,
}

/// Refuse an edit the file and the database could not agree on.
///
/// The year is the one field with a narrower range on disk than in the
/// row: a tag carries it as a `u16`, `track.year` is an SQLite integer.
/// Clamping on the way to the file would leave the two telling
/// different stories — the same divergence the date handling in
/// [`apply_patch`] exists to avoid — so an unrepresentable year is
/// refused before either is touched. The message is user-facing.
fn validate_edit(edit: &TrackEdit) -> AppResult<()> {
    if let Some(year) = edit.year {
        if year > i64::from(u16::MAX) {
            return Err(AppError::Other(format!(
                "year {year} is out of range for a tag (0-{})",
                u16::MAX
            )));
        }
    }
    Ok(())
}

/// The file's stamp ([`waveflow_core::tagio::file_stamp`]) as the tag
/// editor opens, for it to hand back with its save. `None` when the file
/// cannot be reached, in which case the save is not checked against
/// anything and fails on its own when it tries to write.
#[tauri::command]
pub async fn get_track_file_stamp(
    state: tauri::State<'_, AppState>,
    track_id: i64,
) -> AppResult<Option<String>> {
    let pool = state.require_profile_pool().await?;
    let path: Option<String> = sqlx::query_scalar("SELECT file_path FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&*pool)
        .await?;
    let path = path.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    tokio::task::spawn_blocking(move || {
        waveflow_core::tagio::file_stamp(std::path::Path::new(&path)).ok()
    })
    .await
    .map_err(|e| AppError::Other(format!("file stamp join: {e}")))
}

/// Refuse to write over a file that changed since the editor read it.
///
/// The dialog's form holds the values the library had when it opened,
/// and its save writes the title, the numbers and the year back whether
/// or not the user touched them — so if another tagger rewrote the file
/// meanwhile, saving would undo that program's change without a word.
fn ensure_unchanged(path: &std::path::Path, opened_at: &str) -> AppResult<()> {
    if waveflow_core::tagio::file_stamp(path)? == opened_at {
        Ok(())
    } else {
        Err(AppError::FileChangedOnDisk)
    }
}

/// Re-read the folder holding `track_id`, so the library catches up with
/// a file another program changed — what the tag editor's "reload"
/// does after a save was refused by [`AppError::FileChangedOnDisk`].
///
/// The whole folder, through the ordinary scan, rather than a one-file
/// path of its own: the scanner is the only code that turns a file into
/// rows (albums, credits, genres, artwork), and its fast path skips every
/// file whose size and time have not moved, so the cost is a walk of the
/// folder plus the files that did change — the same scan the folder
/// watcher would have run.
#[tauri::command]
pub async fn reread_track_folder(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    track_id: i64,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let folder_id: Option<Option<i64>> =
        sqlx::query_scalar("SELECT folder_id FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
    let folder_id = folder_id
        .flatten()
        .ok_or_else(|| AppError::Other(format!("track {track_id} is in no library folder")))?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    crate::commands::scan::scan_folder_inner(&pool, &artwork_dir, folder_id, Some(&app), false)
        .await?;
    // Same follow-ups as a manual folder scan: ship the sync ops it
    // queued, and let every view refetch.
    state.drain.notify();
    let _ = app.emit("track:updated", track_id);
    let _ = app.emit("library:rescanned", ());
    Ok(())
}

/// Write `edit` into the track's file, then bring the database in line.
///
/// `file_stamp` is what [`get_track_file_stamp`] returned when the
/// editor opened. When given, the write is refused with
/// [`AppError::FileChangedOnDisk`] if the file no longer matches it.
/// Callers that send only the fields the user changed (the tag fetcher)
/// leave it out. Returns the file's stamp after the write, so the editor
/// can save again without being refused by its own change.
#[tauri::command]
pub async fn update_track_tags(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    app: AppHandle,
    track_id: i64,
    edit: TrackEdit,
    file_stamp: Option<String>,
) -> AppResult<Option<String>> {
    validate_edit(&edit)?;
    let pool = state.require_profile_pool().await?;

    // 1. Pull the current track row so we know the file path AND can
    //    fall back to existing values for fields the user didn't
    //    touch (matters for the album/artist relink which needs the
    //    full new context).
    let row: Option<TrackRow> = sqlx::query_as::<_, TrackRow>(
        "SELECT id, file_path, primary_artist, album_id FROM track WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(&*pool)
    .await?;
    let row = row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    let path = std::path::PathBuf::from(&row.file_path);

    // 1b. A save that will be refused should not pause the music first,
    //     so the stamp is checked before step 2 as well as at the write.
    if let Some(opened_at) = file_stamp.clone() {
        let path = path.clone();
        tokio::task::spawn_blocking(move || ensure_unchanged(&path, &opened_at))
            .await
            .map_err(|e| AppError::Other(format!("file stamp join: {e}")))??;
    }

    // 2. If the engine is playing this track, pause before opening.
    //    Releases the read handle that would otherwise refuse our write
    //    open on Windows. Resume is the user's call — see module doc.
    let active = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if active == track_id {
        let _ = engine.send(crate::audio::AudioCmd::Pause);
        // Give the audio thread a moment to drop its handles before
        // we touch the file. 100 ms is overkill for the channel
        // round-trip but cheap insurance against a 0-byte race.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // 3. Write the new tags to the file. If lofty can't read the
    //    container at all (corrupt header), surface the error — we
    //    don't want to update the DB to values the file doesn't
    //    actually carry.
    // On a blocking thread, not here: this reaches the disk, and on the
    // network share the whole of #590 is about it can take seconds. An
    // async worker parked on it is one fewer for every other command.
    // The stamp is checked again here, as close to the write as it gets:
    // the pause above can take its time, and a tagger saving during it
    // is exactly the case being guarded.
    {
        let path = path.clone();
        let edit = edit.clone();
        let file_stamp = file_stamp.clone();
        tokio::task::spawn_blocking(move || -> AppResult<()> {
            if let Some(opened_at) = &file_stamp {
                ensure_unchanged(&path, opened_at)?;
            }
            write_tags_to_file(&path, &edit)
                .map_err(|e| AppError::Other(format!("tag write failed: {e}")))
        })
        .await
        .map_err(|e| AppError::Other(format!("tag write join: {e}")))??;
    }

    // 3b. The file has changed on disk; recompute its hash so the
    //     scanner's (mtime, size, hash) fast path keeps matching and
    //     the shared lyrics cache (keyed on file_hash) doesn't drift.
    //     Propagate the error — if we can't even read the file we
    //     just wrote, something serious is wrong (disk full, perms,
    //     file deleted mid-write) and continuing would silently leave
    //     `track.file_hash` pointing at a stale digest.
    rehash_track_file(&pool, track_id, &path).await?;

    // 4. DB sync. Run inside a transaction so a partial failure
    //    (artist resolved but album INSERT racing) doesn't leave the
    //    track in a weird intermediate state.
    sync_db(&pool, track_id, &edit).await?;

    // 5. Emit a typed event so any open view can refresh the affected
    //    track without reloading everything. Payload is the track ID
    //    so the frontend doesn't need a hash to know what changed.
    //
    // Also emit `library:rescanned` (which the LibraryContext already
    // listens to for filesystem-watcher-driven refreshes) and
    // `player:queue-changed` so the QueuePanel / PlayerBar reflect
    // the new title / artist / album immediately. Reusing existing
    // events spares every consumer view from wiring a new listener.
    let _ = app.emit("track:updated", track_id);
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());

    // The file as this write left it, which is what the next save from
    // the same dialog has to match. Best-effort: the tags are written
    // and the database is in line, so failing the command here would
    // report a save that happened as one that did not.
    let stamp = tokio::task::spawn_blocking(move || waveflow_core::tagio::file_stamp(&path).ok())
        .await
        .unwrap_or(None);
    Ok(stamp)
}

/// Per-track result of a batch update. Surfaced to the frontend so it
/// can render "N updated, M failed (Track X: <reason>)" instead of
/// silently swallowing partial failures.
#[derive(Debug, serde::Serialize)]
pub struct BatchUpdateSummary {
    pub updated: u32,
    /// `(track_id, error_message)` for each failure. Kept short — the
    /// frontend shows them inline so we don't want stack traces.
    pub errors: Vec<(i64, String)>,
}

/// Apply the same `TrackEdit` to every track in `track_ids`. Used by
/// the batch tag editor when the user multi-selects rows and edits
/// shared fields (artist, album, year, genre…).
///
/// Each track is processed independently: a file-write error on one
/// track logs an entry in `errors` and the loop continues. This is the
/// opposite of `update_track_tags` which aborts on the first failure —
/// for batch UX, the user explicitly opted into a multi-track save
/// and a single corrupt header shouldn't block the others.
///
/// Caller-supplied `edit`: every field is optional. `None` means
/// "leave this field untouched on every track" — the batch UI uses
/// per-field toggles to materialise which fields the user actually
/// wants to propagate. Title / track_number / disc_number are
/// rejected at the frontend level (they're per-track unique), but the
/// backend still accepts them — useful for future scripted batch ops.
#[tauri::command]
pub async fn update_tracks_batch(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    app: AppHandle,
    track_ids: Vec<i64>,
    edit: TrackEdit,
) -> AppResult<BatchUpdateSummary> {
    // One edit for the whole batch, so one check for the whole batch.
    validate_edit(&edit)?;
    let pool = state.require_profile_pool().await?;
    let mut summary = BatchUpdateSummary {
        updated: 0,
        errors: Vec::new(),
    };

    // Pause once up front if the currently-playing track is in the
    // batch. Saves the per-track sleep + re-pause cycle when the user
    // batch-edits a queue that's playing in the background.
    let active = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if active > 0 && track_ids.contains(&active) {
        let _ = engine.send(crate::audio::AudioCmd::Pause);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    for track_id in &track_ids {
        let row: Option<TrackRow> = sqlx::query_as::<_, TrackRow>(
            "SELECT id, file_path, primary_artist, album_id FROM track WHERE id = ?",
        )
        .bind(track_id)
        .fetch_optional(&*pool)
        .await?;
        let Some(row) = row else {
            summary.errors.push((*track_id, "track not found".into()));
            continue;
        };
        let path = std::path::PathBuf::from(&row.file_path);

        let written = {
            let path = path.clone();
            let edit = edit.clone();
            tokio::task::spawn_blocking(move || write_tags_to_file(&path, &edit)).await
        };
        match written {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                summary
                    .errors
                    .push((*track_id, format!("tag write failed: {err}")));
                continue;
            }
            Err(err) => {
                summary
                    .errors
                    .push((*track_id, format!("tag write join: {err}")));
                continue;
            }
        }

        // Rehash the file before the DB sync so the new hash lands
        // alongside the metadata update. Batch mode: record the
        // failure on this row's slot in `errors` and skip the DB
        // sync — without a fresh hash the DB would point at a
        // file we can't address anymore, and the scanner would
        // either pick it up as "new" or never reconcile.
        if let Err(err) = rehash_track_file(&pool, *track_id, &path).await {
            summary
                .errors
                .push((*track_id, format!("rehash failed: {err}")));
            continue;
        }

        if let Err(err) = sync_db(&pool, *track_id, &edit).await {
            summary
                .errors
                .push((*track_id, format!("db sync failed: {err}")));
            continue;
        }

        summary.updated += 1;
        let _ = app.emit("track:updated", *track_id);
    }

    // Single library + queue refresh at the end — every consumer view
    // already coalesces these so emitting one bulk signal beats one
    // per track on a 50-row batch.
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());

    Ok(summary)
}

#[derive(sqlx::FromRow)]
struct TrackRow {
    #[allow(dead_code)]
    id: i64,
    file_path: String,
    #[allow(dead_code)]
    primary_artist: Option<i64>,
    #[allow(dead_code)]
    album_id: Option<i64>,
}

/// Containers the library indexes but nothing here can tag.
///
/// lofty 0.25's `FileType` carries no DSD variant, so asking it about a
/// `.dsf` or a `.dff` fails with a generic "unknown format" — accurate,
/// but it reads as a corrupt file rather than as a format we never
/// supported writing. `.dsf` no longer needs lofty: its tag is a plain
/// ID3v2 block after the audio and [`patch_dsf`] writes it (#592).
///
/// `.dff` stays here. It carries no ID3 by convention, its metadata
/// lives in its own chunk structure, and some taggers append an ID3
/// chunk anyway — so which shape to write is a real question with no
/// obvious answer, and half an answer would be worse than a clear no.
const UNTAGGABLE_EXTENSIONS: &[&str] = &["dff"];

/// `Err` when `path` names a container this build can index but not write
/// tags into. The message is user-facing — it reaches the properties
/// dialog through `AppError::Other`.
fn reject_untaggable(
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
    else {
        return Ok(());
    };
    if UNTAGGABLE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(format!(
            "{} files have no writable tag block in this build",
            ext.to_uppercase()
        )
        .into());
    }
    Ok(())
}

/// Where a set of lyrics goes in the tag.
///
/// The key is decided by the caller, which is the only place that knows
/// the format: plain text and LRC go where every player looks for
/// lyrics, TTML needs a slot that accepts arbitrary text. Expressed as
/// one choice rather than two optional strings so "both at once" cannot
/// be written down.
pub(crate) enum LyricsSlot<'a> {
    /// Both keys go and nothing replaces them.
    Cleared,
    /// `USLT` for ID3v2, `UNSYNCEDLYRICS` for Vorbis, `©lyr` for MP4 —
    /// what every other player reads.
    Unsynchronised(&'a str),
    /// `ItemKey::Lyrics`, the arbitrary-string slot TTML needs. Vorbis
    /// and MP4 only; ID3v2 has nothing equivalent.
    Arbitrary(&'a str),
}

/// One edit to a file's tags, applied through [`patch_file`].
pub(crate) enum TagPatch<'a> {
    /// The metadata fields the properties dialog exposes.
    Fields(&'a TrackEdit),
    /// A new front cover. Every other embedded image survives.
    Cover {
        bytes: &'a [u8],
        mime: &'a lofty::picture::MimeType,
    },
    /// The star rating as the raw 0-255 POPM byte, or `None` to clear
    /// it. Comes through here rather than through lofty directly so a
    /// `.dsf` gets its rating like any other container (#644).
    Rating(Option<u8>),
    /// The lyrics, under the key [`LyricsSlot`] names. Same reason: the
    /// route through `patch_file` is what gives DSD a lyrics write at
    /// all, and what keeps the non-standard comments of every other
    /// container from being dropped on the way (#644).
    Lyrics(LyricsSlot<'a>),
}

/// Apply a patch to a generic [`lofty::tag::Tag`].
///
/// The edit semantics live here, in one place, whatever the container:
/// [`patch_file`] converts each concrete tag into this shape and back.
fn apply_patch(tag: &mut lofty::tag::Tag, patch: &TagPatch<'_>) {
    use lofty::prelude::*;
    use lofty::tag::ItemKey;

    let edit = match patch {
        TagPatch::Fields(edit) => edit,
        TagPatch::Rating(rating) => {
            // Cleared first whatever comes next, so a rating written
            // earlier — possibly by another tagger, under a different
            // email — cannot shadow the new one.
            tag.remove_key(ItemKey::Popularimeter);
            if let Some(r) = rating {
                // Vorbis / MP4 / APE / WavPack: text on the 0-100
                // scale, the shape the scanner reads back.
                //
                // ID3v2 does not come through here at all. lofty's
                // generic tag carries a rating as a **whole star**
                // (`<provider>|<stars>|<counter>`), so a value on this
                // 0-255 scale either fails to convert — which is
                // silent, the frame simply never reaches the file — or
                // is rounded to a star and re-encoded on the provider's
                // own scale. Neither is the byte the user set, so that
                // container is written as the POPM frame it is, by
                // [`apply_rating_id3v2`].
                let as_100 = ((*r as u16) * 100 / 255) as u8;
                tag.insert_text(ItemKey::Popularimeter, as_100.to_string());
            }
            return;
        }
        TagPatch::Lyrics(slot) => {
            // Both keys go before either is written. Switching format
            // (LRC → TTML, say) moves the lyrics from one key to the
            // other, and the reader checks `UnsyncLyrics` first — left
            // in place, the old content would shadow the new.
            // ID3v2 has no arbitrary-string lyrics slot — lofty says so
            // outright — so an `Arbitrary` write there would be dropped
            // on the way to the file, and the clear above would have
            // spent the file's lyrics to make room for it.
            // `write_lyrics_to_file` refuses TTML on those containers
            // before this is called; this is the same answer the `id3`
            // applier gives, so the two cannot disagree about it.
            if matches!(
                (slot, tag.tag_type()),
                (LyricsSlot::Arbitrary(_), lofty::tag::TagType::Id3v2)
            ) {
                return;
            }
            tag.remove_key(ItemKey::UnsyncLyrics);
            tag.remove_key(ItemKey::Lyrics);
            match slot {
                LyricsSlot::Cleared => {}
                LyricsSlot::Unsynchronised(text) => {
                    tag.insert_text(ItemKey::UnsyncLyrics, (*text).to_string());
                }
                LyricsSlot::Arbitrary(text) => {
                    tag.insert_text(ItemKey::Lyrics, (*text).to_string());
                }
            }
            return;
        }
        TagPatch::Cover { bytes, mime } => {
            use lofty::picture::{Picture, PictureType};
            // Replace the cover, not the artwork. A release with a
            // booklet, a back cover or an artist shot carries several
            // pictures, and clearing the list to make room for one of
            // them threw the rest away — nothing brings them back.
            //
            // `Other` goes with `CoverFront` because taggers that don't
            // set a type write the cover there; keeping it would leave
            // the previous cover in the file next to the new one.
            let mut i = 0;
            while i < tag.pictures().len() {
                match tag.pictures()[i].pic_type() {
                    PictureType::CoverFront | PictureType::Other => {
                        tag.remove_picture(i);
                    }
                    _ => i += 1,
                }
            }
            tag.push_picture(
                Picture::unchecked(bytes.to_vec())
                    .pic_type(PictureType::CoverFront)
                    .mime_type((*mime).clone())
                    .build(),
            );
            return;
        }
    };

    if let Some(t) = edit.title.as_ref() {
        if t.trim().is_empty() {
            tag.remove_title();
        } else {
            tag.set_title(t.trim().to_string());
        }
    }
    if let Some(a) = edit.artist.as_ref() {
        if a.trim().is_empty() {
            tag.remove_artist();
        } else {
            // Multi-artist files store the comma-joined string in the
            // tag — the DB-side split is what materialises the many-
            // to-many. Keeping the raw string in the file is also what
            // the scanner reads back, so this round-trips cleanly.
            tag.set_artist(a.trim().to_string());
        }
    }
    if let Some(al) = edit.album.as_ref() {
        if al.trim().is_empty() {
            tag.remove_album();
        } else {
            tag.set_album(al.trim().to_string());
        }
    }
    if let Some(y) = edit.year {
        // The year rides on the *date* item (`TDRC` / `DATE` / `©day` /
        // `ICRD`), which is what the scanner reads back through
        // `Accessor::date`. `ItemKey::Year` has no ID3v2 mapping at all,
        // so writing it there dropped the value on the way to the file
        // and left the database claiming a year no player could see.
        if y <= 0 {
            tag.remove_date();
            // Legacy files may still carry a bare `YEAR` next to the
            // date; clearing one without the other leaves the value
            // visible to every other player.
            tag.remove_key(ItemKey::Year);
        } else if let Ok(year) = u16::try_from(y) {
            match tag.date() {
                // The dialog sends the year on every save, so a save
                // that changed only the title must not truncate a full
                // release date to its year.
                Some(existing) if existing.year == year => {}
                _ => tag.set_date(lofty::tag::items::Timestamp {
                    year,
                    ..Default::default()
                }),
            }
        }
        // A year `u16` cannot hold never reaches here — `validate_edit`
        // refuses the whole edit first, because clamping it would write
        // one value to the file and another to the row.
    }
    if let Some(n) = edit.track_number {
        if n > 0 {
            tag.set_track(n as u32);
        } else {
            tag.remove_track();
        }
    }
    if let Some(n) = edit.disc_number {
        if n > 0 {
            tag.set_disk(n as u32);
        } else {
            tag.remove_disk();
        }
    }
    if let Some(g) = edit.genre.as_ref() {
        // One item per genre, never `set_genre`, which keeps one. Merged
        // into the concrete tag they become a multi-value TCON, repeated
        // Vorbis GENRE comments or several MP4 `©gen`: the forms the
        // scanner reads back as separate genres.
        tag.remove_genre();
        for name in split_genres([g.as_str()]) {
            tag.push(lofty::tag::TagItem::new(
                ItemKey::Genre,
                lofty::tag::ItemValue::Text(name),
            ));
        }
    }
}

/// Write the rating into an ID3v2 tag as the frame it is.
///
/// The star rating is a **0-255 POPM byte**, in half-star steps, and
/// lofty's generic tag cannot carry one: since 0.25 it converts a POPM
/// into `<provider>|<stars>|<counter>` and back through a whole-star
/// `StarRating`, so a raw byte put into the generic tag is dropped on
/// the way back to the file — measured: the frame count is zero, and
/// nothing says so. That is why this is applied to the concrete tag,
/// beside the reasons the rest of `patch_file` already exists for.
///
/// Removing by frame id takes every POPM the file holds. They are keyed
/// by email, so a file can carry several — one per tagger — and a
/// reader takes the first. Leaving the others would let a rating the
/// user replaced go on being the one another player shows.
fn apply_rating_id3v2(tag: &mut lofty::id3::v2::Id3v2Tag, rating: Option<u8>) {
    use lofty::id3::v2::{Frame, FrameId, PopularimeterFrame};

    let popm = FrameId::Valid(std::borrow::Cow::Borrowed("POPM"));
    // Consumed, not dropped: `remove` hands back a lazy iterator over
    // what it takes, so ignoring it removes nothing at all.
    tag.remove(&popm).for_each(drop);
    if let Some(rating) = rating {
        // An empty email is the anonymous user the spec allows, and
        // what every reader checked (foobar2000, Mp3tag, MusicBee)
        // accepts. The counter stays 0 — we do not track plays here.
        tag.insert(Frame::Popularimeter(PopularimeterFrame::new("", rating, 0)));
    }
}

/// Apply a patch to an [`id3::Tag`], for the one container lofty cannot
/// write (#592).
///
/// The mirror of [`apply_patch`], deliberately kept beside it. DSF keeps
/// its metadata as a plain ID3v2 tag at an offset its header declares,
/// and lofty has no `FileType` for it at all — so the tag has to be
/// parsed and re-encoded by the `id3` crate the DSD reader already uses.
/// Two tag models, one set of edit semantics: a test asserts the two
/// appliers agree field by field, because a divergence here would mean
/// the same edit meaning different things depending on the container.
///
/// Frames this does not name are carried through untouched, which is
/// what `id3` does by holding the frames it parsed.
fn apply_patch_id3(tag: &mut id3::Tag, patch: &TagPatch<'_>) {
    use id3::TagLike;

    let edit = match patch {
        TagPatch::Fields(edit) => edit,
        TagPatch::Rating(rating) => {
            // `id3` models POPM, so the frame is built from its fields
            // rather than from a hand-packed body — same bytes, and the
            // crate owns the encoding. Removing first covers the frames
            // another tagger wrote under its own email: POPM is keyed by
            // user, so several can sit in one tag and the reader takes
            // the first.
            tag.remove("POPM");
            if let Some(r) = rating {
                tag.add_frame(id3::Frame::with_content(
                    "POPM",
                    id3::Content::Popularimeter(id3::frame::Popularimeter {
                        user: String::new(),
                        rating: *r,
                        counter: 0,
                    }),
                ));
            }
            return;
        }
        TagPatch::Lyrics(slot) => {
            match slot {
                LyricsSlot::Cleared => tag.remove_all_lyrics(),
                LyricsSlot::Unsynchronised(text) => {
                    tag.remove_all_lyrics();
                    tag.add_frame(id3::frame::Lyrics {
                        // `und` — undetermined. USLT keys on the
                        // language, and claiming one we have not
                        // detected would let a reader hide the lyrics
                        // from someone whose player filters by it.
                        lang: "und".to_string(),
                        description: String::new(),
                        text: (*text).to_string(),
                    });
                }
                // ID3v2 has no arbitrary-string lyrics slot, so
                // `write_lyrics_to_file` refuses TTML on every ID3v2
                // container before it reaches an applier. This arm is
                // what happens if that guard is ever moved: leave the
                // tag exactly as it is. Clearing it would spend the
                // lyrics already in the file on a write we cannot make.
                LyricsSlot::Arbitrary(_) => {}
            }
            return;
        }
        TagPatch::Cover { bytes, mime } => {
            // Replace the cover, not the artwork — the same rule, and
            // the same reason: a release with a booklet, a back cover or
            // an artist shot carries several pictures and nothing brings
            // them back. `id3` removes pictures all at once, so the ones
            // that survive are put back rather than left alone.
            let kept: Vec<id3::frame::Picture> = tag
                .pictures()
                .filter(|picture| {
                    !matches!(
                        picture.picture_type,
                        id3::frame::PictureType::CoverFront | id3::frame::PictureType::Other
                    )
                })
                .cloned()
                .collect();
            tag.remove_all_pictures();
            for picture in kept {
                tag.add_frame(picture);
            }
            tag.add_frame(id3::frame::Picture {
                mime_type: mime.to_string(),
                picture_type: id3::frame::PictureType::CoverFront,
                description: String::new(),
                data: bytes.to_vec(),
            });
            return;
        }
    };

    if let Some(t) = edit.title.as_ref() {
        if t.trim().is_empty() {
            tag.remove_title();
        } else {
            tag.set_title(t.trim());
        }
    }
    if let Some(a) = edit.artist.as_ref() {
        if a.trim().is_empty() {
            tag.remove_artist();
        } else {
            tag.set_artist(a.trim());
        }
    }
    if let Some(al) = edit.album.as_ref() {
        if al.trim().is_empty() {
            tag.remove_album();
        } else {
            tag.set_album(al.trim());
        }
    }
    if let Some(y) = edit.year {
        // The date item, not the bare year, for the same reason the
        // lofty side uses it: that is what the scanner reads back. The
        // legacy `TYER` is cleared alongside so a stale value cannot
        // outlive the one the user just removed.
        if y <= 0 {
            tag.remove_date_recorded();
            tag.remove_year();
        } else if let Ok(year) = i32::try_from(y) {
            match tag.date_recorded() {
                // A save that changed only the title must not truncate a
                // full release date to its year.
                Some(existing) if existing.year == year => {}
                _ => tag.set_date_recorded(id3::Timestamp {
                    year,
                    ..Default::default()
                }),
            }
        }
    }
    if let Some(n) = edit.track_number {
        if n > 0 {
            tag.set_track(n as u32);
        } else {
            tag.remove_track();
        }
    }
    if let Some(n) = edit.disc_number {
        if n > 0 {
            tag.set_disc(n as u32);
        } else {
            tag.remove_disc();
        }
    }
    if let Some(g) = edit.genre.as_ref() {
        let names = split_genres([g.as_str()]);
        if names.is_empty() {
            tag.remove_genre();
        } else {
            // One TCON, its values NUL-separated, as the lofty side writes.
            tag.set_text_values("TCON", names);
        }
    }
}

/// Put the year where a pre-2.4 reader will look for it.
///
/// `TDRC` is a 2.4 frame. The `id3` crate writes whatever frames the tag
/// holds, version target or not, so a 2.3 tag built from a `TDRC` comes
/// back out carrying `TDRC` — and a reader that only knows 2.3 finds no
/// year at all (measured: `year()` answers `None` after that round
/// trip). Since a DSF now keeps the version it arrived with, the year
/// has to be mirrored into `TYER`, which is the frame the older
/// versions define — 2.2 among them, where the writer shortens it to
/// `TYE` on the way out.
///
/// A cleared year clears both, so a stale `TYER` cannot outlive the
/// date it duplicated.
///
/// **Only when the edit touched the year.** A genuine 2.3 tag carries
/// `TYER` and no `TDRC` at all — that is the normal shape, not an odd
/// one — so a mirror that ran on every save read "no date" from a file
/// that had a perfectly good year and removed it. Editing a title, or
/// setting a cover, would have quietly erased the year of every 2.3 DSF
/// it touched.
/// Keep several genres readable as several in an ID3v2.2 / 2.3 tag
/// (#729).
///
/// Only 2.4 has multi-value text frames. Asked to write one into an older
/// version, the `id3` crate joins the values with `/` (measured: "Jazz" and
/// "Trip Hop" come back as "Jazz/Trip Hop"), and `split_genres` does not
/// split on `/`, which belongs to names like "Hip-Hop/Rap". So an older
/// tag gets the one value the scanner does split: the genres joined with
/// `"; "`.
fn join_genres_for_legacy(tag: &mut id3::Tag, version: id3::Version) {
    use id3::TagLike;

    if !matches!(version, id3::Version::Id3v22 | id3::Version::Id3v23) {
        return;
    }
    let Some(values) = tag.genres() else {
        return;
    };
    if values.len() < 2 {
        return;
    }
    let joined = values.join("; ");
    tag.set_genre(joined);
}

fn mirror_year_for_legacy(tag: &mut id3::Tag, patch: &TagPatch<'_>, version: id3::Version) {
    use id3::TagLike;

    if !matches!(version, id3::Version::Id3v22 | id3::Version::Id3v23) {
        return;
    }
    // Only a field edit can carry a year at all — a cover, a rating and
    // a set of lyrics do not — and a field edit that left `year` at
    // `None` is asking for the year to stay exactly as it is.
    let TagPatch::Fields(edit) = patch else {
        return;
    };
    if edit.year.is_none() {
        return;
    }
    match tag.date_recorded() {
        Some(date) => {
            tag.set_year(date.year);
            // 2.2 frame IDs are three characters, and `TDRC` has no
            // three-character form — so the writer does not drop it, it
            // refuses the whole tag: "Unable to downgrade frame ID to
            // ID3v2.2" (measured). Editing the year of a 2.2 file failed
            // outright because of a frame we had just added. `TYER`
            // carries the year now, so the frame goes back out.
            if version == id3::Version::Id3v22 {
                tag.remove_date_recorded();
            }
        }
        None => tag.remove_year(),
    }
}

/// Write a patch into a DSF file (#592).
///
/// The easy container, once someone writes the twenty lines for it: the
/// tag lives *after* the audio at an offset the header declares, so it
/// can grow or shrink without a single audio byte moving. That is why
/// this path does not need the in-place/rewrite split the others do —
/// every DSF write is already the cheap kind.
///
/// `.dff` is still refused. It has no ID3 by convention, its metadata
/// lives in its own chunk structure, and some taggers append an ID3
/// chunk anyway — so "which shape do we write" is a real question with
/// no obvious answer, and half an answer would be worse than today's
/// clear no.
fn patch_dsf(
    path: &std::path::Path,
    patch: &TagPatch<'_>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    with_dsf_tag(path, |tag, version| {
        apply_patch_id3(tag, patch);
        mirror_year_for_legacy(tag, patch, version);
        join_genres_for_legacy(tag, version);
        // An edit the dialog sent is an edit to write, even when it
        // happens to set what the file already said: the alternative is
        // comparing two encodings of the same tag to save a write of a
        // block that sits after the audio and costs nothing to move.
        true
    })
}

/// Read a DSF file's ID3v2 tag, let `edit` change it, write it back.
///
/// The read-modify-write half of [`patch_dsf`], on its own because it
/// has a second caller: the canonical `SYNCEDLYRICS` stamp
/// (`commands::lyrics`) is a different payload over the same container,
/// and a second copy of this would be a second place to get the
/// preservation rules wrong.
///
/// `edit` is handed the version the file arrived with, for the one
/// decision that depends on it (`TYER` versus `TDRC`), and answers
/// whether the tag should be written back. `false` leaves the file
/// untouched — for the callers whose edit may turn out to be no edit at
/// all, where rewriting would cost the user a file modification, and a
/// re-hash, for nothing.
pub(crate) fn with_dsf_tag<F>(
    path: &std::path::Path,
    edit: F,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: FnOnce(&mut id3::Tag, id3::Version) -> bool,
{
    waveflow_core::tagio::with_writable_file(
        path,
        |handle| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let layout = waveflow_core::tagio::read_dsf_layout(handle)?
                .ok_or("this DSF file's header does not describe itself")?;
            let existing = if layout.metadata_offset != 0 {
                use std::io::{Seek, SeekFrom};
                handle.seek(SeekFrom::Start(layout.metadata_offset))?;
                // A tag we cannot parse stops the edit. Carrying on with
                // a fresh one looks like resilience and is the opposite:
                // the write replaces the whole tag, so every frame we
                // failed to read — the cover, the ratings, the
                // MusicBrainz identifiers — would be dropped to save a
                // title. Refusing leaves the file exactly as it is, and
                // says why.
                //
                // `NoTag` is the exception, and the only one: a pointer
                // leading to no tag at all describes a file with no
                // metadata, not one whose metadata we failed to read.
                // There is nothing there to lose, so it starts fresh.
                match id3::Tag::read_from2(&mut *handle) {
                    Ok(tag) => Some(tag),
                    Err(err) if matches!(err.kind, id3::ErrorKind::NoTag) => None,
                    Err(err) => return Err(err.into()),
                }
            } else {
                None
            };
            // Write back the version the file already used. A DSF tagged
            // as ID3v2.3 rewritten as 2.4 is a file some hardware DSD
            // players stop reading, and an edit to a title is not the
            // place to make that decision for the user. Only a tag we
            // are creating gets our own choice.
            let version = existing
                .as_ref()
                .map_or(id3::Version::Id3v24, id3::Tag::version);
            let mut tag = existing.unwrap_or_default();
            if !edit(&mut tag, version) {
                return Ok(());
            }
            let mut bytes = Vec::new();
            tag.write_to(&mut bytes, version)?;
            waveflow_core::tagio::write_dsf_id3v2(handle, &bytes)?;
            Ok(())
        },
    )
}

/// Apply `patch` to the file at `path`, keeping every field the generic
/// [`lofty::tag::Tag`] shape cannot model.
///
/// `lofty::read_from_path` hands back a `TaggedFile`, whose tags are
/// generic `Tag`s produced by *splitting* each concrete tag: what has an
/// `ItemKey` mapping goes into the `Tag`, the rest stays behind in a
/// remainder. What happens to that remainder is not uniform, and that is
/// the whole reason this function exists:
///
/// * `Id3v2Tag` stashes it in the `Tag`'s companion slot, so a `TXXX`
///   lofty doesn't model survives a save.
/// * `VorbisComments` has no such slot — `From<VorbisComments> for Tag`
///   is `split_tag().1` and throws the remainder away. Every
///   non-standard comment on a FLAC / Ogg / Opus / Speex file therefore
///   disappeared the first time the user pressed Save: our own
///   `SYNCEDLYRICS` (written by `commands::lyrics`), `REPLAYGAIN_*`
///   values another tagger left, anything a different player stores.
///
/// Reading the concrete file, splitting it here and merging the
/// remainder back covers both cases the same way, so the guarantee no
/// longer depends on which container the user happens to own.
///
/// Only the containers the scanner indexes are handled; anything else is
/// refused rather than written through a path we haven't checked.
pub(crate) fn patch_file(
    path: &std::path::Path,
    patch: &TagPatch<'_>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::{AudioFile, FileType};
    use lofty::prelude::*;
    use lofty::probe::Probe;

    reject_untaggable(path)?;

    // DSF before the probe: lofty has no FileType for it, so asking it
    // to guess would fail with "unrecognised container" on a file we
    // can in fact write (#592).
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("dsf"))
    {
        return patch_dsf(path, patch);
    }

    let file_type = Probe::open(path)?
        .guess_file_type()?
        .file_type()
        .ok_or("unrecognised audio container")?;

    // Round-trip one tag slot: take it out of the file, split it into a
    // generic tag plus the frames lofty can't model, apply the patch to
    // the generic half, then merge the two back together. An absent slot
    // starts from an empty tag, which is what gives a never-tagged file
    // its first tag.
    macro_rules! patch_slot {
        ($file:expr, $take:ident, $set:ident) => {{
            let (remainder, mut generic) = $file.$take().unwrap_or_default().split_tag();
            apply_patch(&mut generic, patch);
            $file.$set(remainder.merge_tag(generic));
        }};
    }

    // The ID3v2 slot, which takes one payload off the generic path: a
    // rating is written as the POPM frame it is, because lofty's
    // generic tag cannot carry the byte. Everything else is the round
    // trip above.
    macro_rules! patch_id3v2_slot {
        ($file:expr) => {{
            let mut tag = $file.remove_id3v2().unwrap_or_default();
            match patch {
                TagPatch::Rating(rating) => apply_rating_id3v2(&mut tag, *rating),
                _ => {
                    let (remainder, mut generic) = tag.split_tag();
                    apply_patch(&mut generic, patch);
                    tag = remainder.merge_tag(generic);
                }
            }
            $file.set_id3v2(tag);
        }};
    }

    // Same, for a slot the container always has (the Ogg families carry
    // their Vorbis comments by spec, so lofty models them unwrapped).
    macro_rules! patch_required_slot {
        ($file:expr, $take:ident, $set:ident) => {{
            let (remainder, mut generic) = $file.$take().split_tag();
            apply_patch(&mut generic, patch);
            $file.$set(remainder.merge_tag(generic));
        }};
    }

    // The rewrite: read the concrete file, run the body over it, save it
    // back — through a copy that replaces the original in one `rename`
    // (#598). lofty's own rewrite truncates the file it is rewriting, so
    // the original has to stay out of its reach until the new bytes are
    // on the disk. lofty rewinds the handle itself at the top of its
    // writer, so reading and writing through the same one is expected.
    macro_rules! with_file {
        ($ty:ty, |$f:ident, $h:ident| $body:block) => {{
            waveflow_core::tagio::rewrite_via_temp(
                path,
                |$h| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    let mut $f = <$ty>::read_from($h, ParseOptions::new())?;
                    $body
                    $f.save_to($h, WriteOptions::default())?;
                    Ok(())
                },
            )?;
        }};
    }

    // First pass: lay the new tag over the one already in the file,
    // which works whenever the edit did not outgrow the padding there
    // (#590). That is the common case — a corrected title is a few bytes
    // either way and taggers leave kilobytes of slack — and it is the
    // only path that touches neither the audio nor the file's length.
    // `false` means "could not", and the rewrite below takes over.
    //
    // An **error** here fails the edit rather than falling through, and
    // that is deliberate. The rewrite opens the same container with the
    // same parser on a copy of the same bytes, so a parse or encode
    // failure would simply happen again one file later. The one error
    // that would genuinely behave differently is the open: `rewrite_via
    // _temp` never opens the original for writing, only for reading, so
    // it would succeed on a file the user has deliberately made
    // read-only on Unix — replacing it through a rename the directory
    // permits. Turning "you may not write this file" into a silent
    // replacement is not a fallback, it is a way around the answer.
    // Windows is the exception the guard already handles: there
    // read-only is a single attribute we lift and put back.
    macro_rules! try_in_place_id3v2 {
        ($ty:ty) => {{
            waveflow_core::tagio::with_writable_file(
                path,
                |handle| -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                    let mut f = <$ty>::read_from(handle, ParseOptions::new())?;
                    patch_id3v2_slot!(f);
                    match f.id3v2() {
                        Some(tag) => waveflow_core::tagio::try_id3v2_in_place(handle, tag),
                        None => Ok(false),
                    }
                },
            )?
        }};
    }

    let written_in_place = match file_type {
        FileType::Mpeg => try_in_place_id3v2!(lofty::mpeg::MpegFile),
        FileType::Aac => try_in_place_id3v2!(lofty::aac::AacFile),
        // Anything but a cover. A cover edit regenerates picture
        // blocks, which the FLAC fast path deliberately copies through
        // rather than rebuilds, so it has nothing to offer there — a
        // rating and a set of lyrics are comment values like any other
        // and take the same cheap route the fields do.
        FileType::Flac if !matches!(patch, TagPatch::Cover { .. }) => {
            waveflow_core::tagio::with_writable_file(
                path,
                |handle| -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
                    let mut f = lofty::flac::FlacFile::read_from(handle, ParseOptions::new())?;
                    patch_slot!(f, remove_vorbis_comments, set_vorbis_comments);
                    match f.vorbis_comments() {
                        Some(comments) => waveflow_core::tagio::try_flac_in_place(handle, comments),
                        None => Ok(false),
                    }
                },
            )?
        }
        _ => false,
    };
    if written_in_place {
        return Ok(());
    }

    match file_type {
        FileType::Mpeg => with_file!(lofty::mpeg::MpegFile, |f, handle| {
            patch_id3v2_slot!(f);
        }),
        FileType::Aac => with_file!(lofty::aac::AacFile, |f, handle| {
            patch_id3v2_slot!(f);
        }),
        FileType::Mp4 => with_file!(lofty::mp4::Mp4File, |f, handle| {
            patch_slot!(f, remove_ilst, set_ilst);
        }),
        // WAV and AIFF can carry both an ID3v2 chunk and their native
        // list, and players disagree on which one they read. Patching
        // only the primary would leave the other one contradicting it,
        // so both get the edit whenever both exist; a file with neither
        // gets the ID3v2 chunk lofty treats as primary.
        FileType::Wav => with_file!(lofty::iff::wav::WavFile, |f, handle| {
            if f.riff_info().is_some() {
                patch_slot!(f, remove_riff_info, set_riff_info);
            }
            patch_id3v2_slot!(f);
        }),
        FileType::Aiff => with_file!(lofty::iff::aiff::AiffFile, |f, handle| {
            if f.text_chunks().is_some() {
                patch_slot!(f, remove_text_chunks, set_text_chunks);
            }
            patch_id3v2_slot!(f);
        }),
        FileType::Vorbis => with_file!(lofty::ogg::VorbisFile, |f, handle| {
            patch_required_slot!(f, remove_vorbis_comments, set_vorbis_comments);
        }),
        FileType::Opus => with_file!(lofty::ogg::OpusFile, |f, handle| {
            patch_required_slot!(f, remove_vorbis_comments, set_vorbis_comments);
        }),
        FileType::Speex => with_file!(lofty::ogg::SpeexFile, |f, handle| {
            patch_required_slot!(f, remove_vorbis_comments, set_vorbis_comments);
        }),
        // FLAC keeps its pictures in their own metadata blocks, which
        // lofty exposes on the file and not on the tag — a cover pushed
        // into the Vorbis comments here would be written *alongside* the
        // blocks already there rather than replacing the front cover.
        FileType::Flac => with_file!(lofty::flac::FlacFile, |f, handle| {
            match patch {
                TagPatch::Cover { bytes, mime } => {
                    use lofty::ogg::OggPictureStorage;
                    use lofty::picture::{Picture, PictureInformation, PictureType};
                    f.remove_picture_type(PictureType::CoverFront);
                    f.remove_picture_type(PictureType::Other);
                    let picture = Picture::unchecked(bytes.to_vec())
                        .pic_type(PictureType::CoverFront)
                        .mime_type((*mime).clone())
                        .build();
                    // `from_picture` decodes the header for width /
                    // height / colour depth. A format it can't read (our
                    // WebP path) is no reason to refuse the cover — the
                    // fields are informational and readers fall back to
                    // the image itself.
                    let info = PictureInformation::from_picture(&picture).unwrap_or_default();
                    f.insert_picture(picture, Some(info))?;
                }
                // Fields, rating, lyrics: all of them are comment
                // values, and the pictures stay where they are.
                _ => {
                    patch_slot!(f, remove_vorbis_comments, set_vorbis_comments);
                }
            }
        }),
        other => {
            return Err(format!("{other:?} files are not editable in this build").into());
        }
    }

    Ok(())
}

/// Apply the edit to the file's tags. Creates a tag when the file has
/// none, so a previously-untagged file gets properly tagged on first
/// edit. Returns the boxed lofty error untouched so the caller can
/// surface a useful message.
fn write_tags_to_file(
    path: &std::path::Path,
    edit: &TrackEdit,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    patch_file(path, &TagPatch::Fields(edit))
}

/// Mirror the file write into the database. Order matters because
/// `track.album_id` depends on the resolved `primary_artist` (album
/// is keyed on `(canonical_title, artist_id)`), and `track_artist`
/// needs the new track row state.
async fn sync_db(pool: &SqlitePool, track_id: i64, edit: &TrackEdit) -> AppResult<()> {
    let mut tx = pool.begin().await?;

    // The album the track leaves, if it does: its year may have come
    // from this track.
    let old_album_id: Option<i64> = sqlx::query_scalar("SELECT album_id FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();

    // Resolve the new artist list (and the new primary). When the
    // user didn't touch the artist field, leave the existing rows
    // alone — re-upserting the same names would no-op but the DELETE
    // + INSERT churn isn't free on a hot library.
    let (artist_ids, primary_artist_id): (Option<Vec<i64>>, Option<i64>) =
        if let Some(raw) = edit.artist.as_ref() {
            let mut ids: Vec<i64> = Vec::new();
            for name in split_artist_name(raw) {
                if let Some(id) = upsert_artist(&mut tx, &name).await? {
                    // First spelling wins when one credit lists an artist twice.
                    if !ids.contains(&id) {
                        respell(&mut tx, Respell::Artist, id, &name).await?;
                        ids.push(id);
                    }
                }
            }
            let primary = ids.first().copied();
            (Some(ids), primary)
        } else {
            (None, None)
        };

    // Resolve album similarly. Needs the (possibly new) primary
    // artist; falls back to the existing one when the user didn't
    // edit the artist field.
    let new_album_id: Option<Option<i64>> = if let Some(album_title) = edit.album.as_ref() {
        let title = album_title.trim();
        if title.is_empty() {
            Some(None)
        } else {
            // Use the new primary artist when the user changed it,
            // otherwise read the current one back so the album
            // dedup still keys on a stable artist.
            let aid = match primary_artist_id {
                Some(_) => primary_artist_id,
                None => {
                    sqlx::query_scalar::<_, Option<i64>>(
                        "SELECT primary_artist FROM track WHERE id = ?",
                    )
                    .bind(track_id)
                    .fetch_one(&mut *tx)
                    .await?
                }
            };
            // Preserve the existing album's album_artist + compilation
            // flag when the user renames an album without re-tagging
            // the source file. Without this, a rename routed through
            // upsert_album would fall back to the track's primary
            // artist for grouping, re-introducing the v1.0 split-on-
            // featuring bug for the renamed row.
            let (carried_album_artist, carried_is_compilation) =
                sqlx::query_as::<_, (Option<String>, i64)>(
                    "SELECT al.album_artist, al.is_compilation
                   FROM track t
                   LEFT JOIN album al ON al.id = t.album_id
                  WHERE t.id = ?",
                )
                .bind(track_id)
                .fetch_optional(&mut *tx)
                .await?
                .map(|(aa, cmp)| (aa, cmp == 1))
                .unwrap_or((None, false));
            // upsert_album now takes &mut SqliteConnection, so we
            // can call it directly inside the open transaction —
            // no commit/reopen dance needed.
            let aid = upsert_album(
                &mut tx,
                title,
                carried_album_artist.as_deref(),
                carried_is_compilation,
                aid,
                edit.year,
            )
            .await?;
            if let Some(id) = aid {
                respell(&mut tx, Respell::Album, id, title).await?;
            }
            Some(aid)
        }
    } else {
        None
    };

    // Patch the track row. We build the SET clause dynamically so
    // unset fields keep their current values instead of getting
    // clobbered to NULL.
    let mut sets: Vec<&str> = Vec::new();
    if edit.title.is_some() {
        sets.push("title = ?");
        // A renamed title has a different romanisation, and the backfill
        // only ever looks at NULLs -- so a stale blob would stay stale
        // for good if it were not rewritten here (#579).
        sets.push("pinyin = ?");
    }
    if edit.year.is_some() {
        sets.push("year = ?");
    }
    if edit.track_number.is_some() {
        sets.push("track_number = ?");
    }
    if edit.disc_number.is_some() {
        sets.push("disc_number = ?");
    }
    if let Some(pid) = primary_artist_id {
        let _ = pid; // keep variable readable in the binding loop below
        sets.push("primary_artist = ?");
    }
    if new_album_id.is_some() {
        sets.push("album_id = ?");
    }
    // Bumping a `last_modified_ms`-style column would be ideal but
    // the schema doesn't carry one yet — skipping for v1.

    if !sets.is_empty() {
        let sql = format!("UPDATE track SET {} WHERE id = ?", sets.join(", "));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        if let Some(t) = edit.title.as_ref() {
            q = q.bind(t.trim());
            q = q.bind(waveflow_core::scanner::pinyin_blob(t.trim()).unwrap_or_default());
        }
        if let Some(y) = edit.year {
            q = q.bind(if y > 0 { Some(y) } else { None });
        }
        if let Some(n) = edit.track_number {
            q = q.bind(if n > 0 { Some(n) } else { None });
        }
        if let Some(n) = edit.disc_number {
            q = q.bind(if n > 0 { Some(n) } else { None });
        }
        if let Some(pid) = primary_artist_id {
            q = q.bind(pid);
        }
        if let Some(aid) = new_album_id {
            q = q.bind(aid);
        }
        q = q.bind(track_id);
        q.execute(&mut *tx).await?;
    }

    // Replace track_artist links when the user touched the artist
    // field. Empty list → no rows (the track stays available, just
    // with no linked artists which the UI will render as "—").
    if let Some(ids) = artist_ids {
        sqlx::query("DELETE FROM track_artist WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        for (pos, aid) in ids.iter().enumerate() {
            sqlx::query(
                "INSERT INTO track_artist (track_id, artist_id, position, role)
                 VALUES (?, ?, ?, 'main')",
            )
            .bind(track_id)
            .bind(aid)
            .bind(pos as i64)
            .execute(&mut *tx)
            .await?;
        }
    }

    // Genres: every one the edit names, split as the file was written so
    // the next scan finds the same set (#729).
    if let Some(g) = edit.genre.as_ref() {
        sqlx::query("DELETE FROM track_genre WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        for name in split_genres([g.as_str()]) {
            if let Some(gid) = upsert_genre(&mut tx, &name).await? {
                respell(&mut tx, Respell::Genre, gid, &name).await?;
                sqlx::query("INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                    .bind(track_id)
                    .bind(gid)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    // The album year is the first track's (disc, then track order),
    // recomputed by the scan only: an edited year, a renumbered track, or
    // a track moved between albums left the header on the old one until
    // the next scan of that folder.
    if edit.year.is_some()
        || edit.track_number.is_some()
        || edit.disc_number.is_some()
        || new_album_id.is_some()
    {
        let mut albums: Vec<i64> = old_album_id.into_iter().collect();
        if let Some(Some(aid)) = new_album_id {
            albums.push(aid);
        }
        waveflow_core::repository::sqlite::album::refresh_album_years(&mut tx, &albums).await?;
    }

    tx.commit().await?;
    // Suppress unused-import warning when canonical_name isn't reached
    // via a code path in this function (it's exported for callers).
    let _ = canonical_name;
    Ok(())
}

/// Which entity an edit names, for [`respell`].
#[derive(Clone, Copy)]
enum Respell {
    Artist,
    Album,
    Genre,
}

/// Give an artist, album or genre the spelling the user just typed.
///
/// They are found by a canonical key that ignores case, punctuation and
/// spacing, so "The Wall" typed over "the wall" resolves to the very same
/// row — and nothing then renamed it: the file carried the fix, every
/// view kept the old spelling, and no rescan could change it either.
///
/// Only here, on an explicit edit. The scanner must not do it: two files
/// spelling one artist differently would rename it back and forth on
/// every scan. It renames the entity for every track that shares it,
/// which is what correcting a name means. Pinyin travels with the name.
async fn respell(
    conn: &mut sqlx::SqliteConnection,
    kind: Respell,
    id: i64,
    spelled: &str,
) -> AppResult<()> {
    let pinyin = waveflow_core::scanner::pinyin_blob(spelled).unwrap_or_default();
    let query = match kind {
        Respell::Artist => {
            sqlx::query("UPDATE artist SET name = ?, pinyin = ? WHERE id = ? AND name IS NOT ?")
                .bind(spelled)
                .bind(pinyin)
                .bind(id)
                .bind(spelled)
        }
        Respell::Album => {
            sqlx::query("UPDATE album SET title = ?, pinyin = ? WHERE id = ? AND title IS NOT ?")
                .bind(spelled)
                .bind(pinyin)
                .bind(id)
                .bind(spelled)
        }
        Respell::Genre => sqlx::query("UPDATE genre SET name = ? WHERE id = ? AND name IS NOT ?")
            .bind(spelled)
            .bind(id)
            .bind(spelled),
    };
    query.execute(&mut *conn).await?;
    Ok(())
}

/// Replace the embedded cover for a track. The new image is written
/// into the audio file's tag (replacing the front cover, and only the
/// front cover — a release that ships a booklet or a back cover keeps
/// them) AND copied into the per-profile artwork cache, then the track's
/// album.artwork_id is repointed at the new row. Cover is per-album
/// in WaveFlow's data model, so editing one track repaints every
/// sibling on the same album — matching the behaviour every other
/// music player ships.
#[tauri::command]
pub async fn update_track_cover(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    app: AppHandle,
    track_id: i64,
    image_path: String,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;
    let profile_id = state.require_profile_id().await?;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);
    std::fs::create_dir_all(&artwork_dir)?;

    let row: Option<(String, Option<i64>)> =
        sqlx::query_as("SELECT file_path, album_id FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&*pool)
            .await?;
    let (file_path, album_id) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let bytes = std::fs::read(&image_path)
        .map_err(|e| AppError::Other(format!("cover read failed: {e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Other("cover file is empty".into()));
    }
    let (mime, ext) = sniff_image_mime(&bytes, &image_path);

    // Pause if the engine has the file open — same reason as the
    // tag-edit path: the write opens the real file.
    let active = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if active == track_id {
        let _ = engine.send(crate::audio::AudioCmd::Pause);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // A cover is the largest thing we ever push into a tag, so this is
    // the write least suited to an async worker.
    {
        let path = std::path::PathBuf::from(&file_path);
        let bytes = bytes.clone();
        let mime = mime.clone();
        tokio::task::spawn_blocking(move || write_cover_to_file(&path, &bytes, &mime))
            .await
            .map_err(|e| AppError::Other(format!("cover tag write join: {e}")))?
            .map_err(|e| AppError::Other(format!("cover tag write failed: {e}")))?;
    }

    // The audio file itself just changed (a new picture frame was
    // embedded), so its blake3 hash drifted. Recompute and persist so
    // the scanner's fast-path and the lyrics cache (keyed on file_hash)
    // stay valid. Propagate the error — leaving `track.file_hash`
    // pointing at the pre-write digest would silently invalidate the
    // lyrics cache for this row and confuse the next scan pass.
    rehash_track_file(&pool, track_id, std::path::Path::new(&file_path)).await?;

    // Hash + persist the bytes in the shared artwork cache. blake3
    // makes "same image, different file" deduplicate naturally.
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let out_path = artwork_dir.join(format!("{hash}.{ext}"));
    if !out_path.exists() {
        std::fs::write(&out_path, &bytes)
            .map_err(|e| AppError::Other(format!("artwork cache write failed: {e}")))?;
    }
    crate::thumbnails::spawn_thumbnail_job(out_path.clone(), artwork_dir.clone(), hash.clone());

    // One transaction rather than two pooled connections: the artwork
    // row and the album link belong together, and the previous shape
    // could leave the row behind if the update failed.
    let mut tx = pool.begin().await?;
    let artwork_id = upsert_artwork(&mut tx, &hash, ext, ArtworkSource::Manual).await?;
    // Only guarded when a track actually belongs to an album: `album_id`
    // is legitimately `None` for a loose track, and that case must still
    // commit — the cover has already been written into the audio file
    // itself, which is the point of this command.
    if let Some(aid) = album_id {
        let res =
            sqlx::query("UPDATE album SET artwork_id = ?, artwork_source = 'manual' WHERE id = ?")
                .bind(artwork_id)
                .bind(aid)
                .execute(&mut *tx)
                .await?;
        if res.rows_affected() == 0 {
            return Err(AppError::Other(format!("album {aid} not found")));
        }
    }
    tx.commit().await?;

    let _ = app.emit("track:updated", track_id);
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());
    Ok(())
}

/// Write `bytes` as the file's front cover, leaving every other
/// embedded picture (back cover, booklet, artist shot) in place.
fn write_cover_to_file(
    path: &std::path::Path,
    bytes: &[u8],
    mime: &lofty::picture::MimeType,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    patch_file(path, &TagPatch::Cover { bytes, mime })
}

/// Pick the MIME type + filename extension for the user-supplied
/// image. Magic-byte first, fall back to the path extension when the
/// header is unrecognised. Lofty stores WebP under `Unknown` because
/// the enum doesn't have a first-class variant for it.
fn sniff_image_mime(bytes: &[u8], path: &str) -> (lofty::picture::MimeType, &'static str) {
    use lofty::picture::MimeType;
    if bytes.len() >= 4 {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return (MimeType::Jpeg, "jpg");
        }
        if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
            return (MimeType::Png, "png");
        }
        if bytes.starts_with(b"GIF8") {
            return (MimeType::Gif, "gif");
        }
        if bytes.starts_with(&[0x42, 0x4D]) {
            return (MimeType::Bmp, "bmp");
        }
        if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return (MimeType::Unknown("image/webp".into()), "webp");
        }
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        (MimeType::Jpeg, "jpg")
    } else if lower.ends_with(".png") {
        (MimeType::Png, "png")
    } else if lower.ends_with(".webp") {
        (MimeType::Unknown("image/webp".into()), "webp")
    } else {
        (MimeType::Jpeg, "jpg")
    }
}

#[cfg(test)]
mod patch_agreement_tests {
    use super::*;

    /// Every field the dialog can send, set to something distinctive.
    fn full_edit() -> TrackEdit {
        TrackEdit {
            title: Some("  A Title  ".into()),
            artist: Some("Artist A, Artist B".into()),
            album: Some("An Album".into()),
            year: Some(1997),
            track_number: Some(4),
            disc_number: Some(2),
            genre: Some(" Jazz ; Trip Hop".into()),
        }
    }

    /// The same fields, all asking to be cleared.
    fn clearing_edit() -> TrackEdit {
        TrackEdit {
            title: Some("   ".into()),
            artist: Some(String::new()),
            album: Some(String::new()),
            year: Some(0),
            track_number: Some(0),
            disc_number: Some(0),
            genre: Some("  ".into()),
        }
    }

    /// What a tag says, in the only terms both models share.
    #[derive(Debug, PartialEq, Eq)]
    struct Fields {
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
        year: Option<i32>,
        track: Option<u32>,
        disc: Option<u32>,
        genres: Vec<String>,
    }

    fn through_lofty(edit: &TrackEdit) -> Fields {
        use lofty::prelude::*;
        let mut tag = lofty::tag::Tag::new(lofty::tag::TagType::Id3v2);
        apply_patch(&mut tag, &TagPatch::Fields(edit));
        Fields {
            title: tag.title().map(|v| v.to_string()),
            artist: tag.artist().map(|v| v.to_string()),
            album: tag.album().map(|v| v.to_string()),
            year: tag.date().map(|d| i32::from(d.year)),
            track: tag.track(),
            disc: tag.disk(),
            genres: tag
                .get_strings(lofty::tag::ItemKey::Genre)
                .map(str::to_string)
                .collect(),
        }
    }

    fn through_id3(edit: &TrackEdit) -> Fields {
        use id3::TagLike;
        let mut tag = id3::Tag::new();
        apply_patch_id3(&mut tag, &TagPatch::Fields(edit));
        Fields {
            title: tag.title().map(|v| v.to_string()),
            artist: tag.artist().map(|v| v.to_string()),
            album: tag.album().map(|v| v.to_string()),
            year: tag.date_recorded().map(|d| d.year),
            track: tag.track(),
            disc: tag.disc(),
            genres: tag
                .genres()
                .unwrap_or_default()
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
    }

    #[test]
    fn both_appliers_agree_on_a_full_edit() {
        // Two tag models, one set of edit semantics. A divergence would
        // mean the same edit meaning different things depending on
        // whether the file is an MP3 or a DSF — which nobody would look
        // for, because the dialog is the same one.
        let edit = full_edit();
        let written = through_lofty(&edit);
        assert_eq!(written, through_id3(&edit));
        // Two genres, each its own value, trimmed (#729).
        assert_eq!(written.genres, ["Jazz", "Trip Hop"]);
    }

    #[test]
    fn both_appliers_agree_on_clearing_every_field() {
        // The half that is easy to get wrong: "empty means clear" has to
        // be the same decision on both sides, including the year, where
        // 0 means remove rather than write a year zero.
        let edit = clearing_edit();
        let cleared = through_lofty(&edit);
        assert_eq!(cleared, through_id3(&edit));
        assert_eq!(cleared.title, None);
        assert_eq!(cleared.year, None);
        assert_eq!(cleared.track, None);
        assert!(cleared.genres.is_empty());
    }

    #[test]
    fn a_title_only_save_does_not_truncate_a_full_release_date() {
        // The dialog sends the year on every save, so both appliers have
        // to leave an existing day-precision date alone when the year
        // they were handed is the one already there.
        use id3::TagLike;
        let mut tag = id3::Tag::new();
        tag.set_date_recorded(id3::Timestamp {
            year: 1997,
            month: Some(6),
            day: Some(14),
            ..Default::default()
        });
        let edit = TrackEdit {
            title: Some("New Title".into()),
            year: Some(1997),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        let date = tag.date_recorded().expect("a date");
        assert_eq!((date.year, date.month, date.day), (1997, Some(6), Some(14)));
    }

    /// A DSF keeps the ID3 version it had, so both versions must carry
    /// several genres to the scanner, which reads TCON whole and splits
    /// it with `split_genres` (#729).
    #[test]
    fn several_genres_survive_an_id3_round_trip_in_both_versions() {
        use id3::TagLike;

        for version in [id3::Version::Id3v23, id3::Version::Id3v24] {
            let mut tag = id3::Tag::new();
            let edit = TrackEdit {
                genre: Some("Jazz; Trip Hop".into()),
                ..Default::default()
            };
            apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
            join_genres_for_legacy(&mut tag, version);
            let mut bytes = Vec::new();
            tag.write_to(&mut bytes, version).expect("write");
            let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");
            assert_eq!(
                split_genres(back.genre()),
                ["Jazz", "Trip Hop"],
                "{version:?}"
            );
        }
    }

    #[test]
    fn an_id3v23_tag_keeps_its_year_where_a_v23_reader_looks() {
        // TDRC is a 2.4 frame. The `id3` crate writes the frames the tag
        // holds whatever version it is told to target, so a 2.3 tag built
        // from a TDRC comes back out carrying TDRC — and `year()`, which
        // reads TYER, answers None. Measured, not assumed: that is what
        // the round trip below asserted before the mirror existed.
        use id3::TagLike;

        let mut tag = id3::Tag::new();
        tag.set_title("Before");
        let edit = TrackEdit {
            year: Some(1997),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v23);

        let mut bytes = Vec::new();
        tag.write_to(&mut bytes, id3::Version::Id3v23)
            .expect("write");
        let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");

        assert_eq!(back.year(), Some(1997), "a 2.3 reader finds the year");
        assert_eq!(
            back.date_recorded().map(|d| d.year),
            Some(1997),
            "and the 2.4 frame still agrees with it"
        );
    }

    #[test]
    fn clearing_the_year_clears_both_frames_in_a_legacy_tag() {
        // A stale TYER outliving the date it duplicated would show the
        // old year to exactly the readers the mirror exists for.
        use id3::TagLike;

        let mut tag = id3::Tag::new();
        tag.set_year(1997);
        tag.set_date_recorded(id3::Timestamp {
            year: 1997,
            ..Default::default()
        });
        let edit = TrackEdit {
            year: Some(0),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v23);

        assert_eq!(tag.year(), None);
        assert_eq!(tag.date_recorded(), None);
    }

    #[test]
    fn a_v24_tag_is_left_with_the_frame_its_version_defines() {
        // The mirror is a 2.3 accommodation and must not add a legacy
        // frame to a tag that has no use for one.
        use id3::TagLike;

        let mut tag = id3::Tag::new();
        let edit = TrackEdit {
            year: Some(2011),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v24);

        assert_eq!(tag.date_recorded().map(|d| d.year), Some(2011));
        assert_eq!(tag.year(), None, "no TYER in a 2.4 tag");
    }

    #[test]
    fn an_id3v22_tag_can_actually_be_written() {
        // Not a conformance nicety: the writer refuses a tag carrying a
        // frame it cannot downgrade, so a TDRC we added ourselves failed
        // the whole edit on a 2.2 file.
        use id3::TagLike;

        let mut tag = id3::Tag::new();
        tag.set_title("Before");
        let edit = TrackEdit {
            year: Some(1997),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v22);

        let mut bytes = Vec::new();
        tag.write_to(&mut bytes, id3::Version::Id3v22)
            .expect("a 2.2 tag we built must be writable as 2.2");
        let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");
        assert_eq!(back.year(), Some(1997));
        assert_eq!(back.title(), Some("Before"));
    }

    #[test]
    fn a_cover_edit_does_not_erase_a_v23_tag_s_year() {
        // The shape that matters: a genuine 2.3 tag carries TYER and no
        // TDRC. A mirror that ran on every save would read "no date"
        // from it and remove the year — so setting a cover, or fixing a
        // title, silently erased the year of the file it was helping.
        use id3::TagLike;

        let mut tag = id3::Tag::new();
        tag.set_year(1997);
        let mime = lofty::picture::MimeType::Jpeg;
        let patch = TagPatch::Cover {
            bytes: &[1, 2, 3],
            mime: &mime,
        };
        apply_patch_id3(&mut tag, &patch);
        mirror_year_for_legacy(&mut tag, &patch, id3::Version::Id3v23);

        assert_eq!(tag.year(), Some(1997), "the year the edit never mentioned");
    }

    #[test]
    fn a_title_only_edit_does_not_erase_a_v23_tag_s_year() {
        use id3::TagLike;

        let mut tag = id3::Tag::new();
        tag.set_year(1997);
        let edit = TrackEdit {
            title: Some("New Title".into()),
            ..Default::default()
        };
        apply_patch_id3(&mut tag, &TagPatch::Fields(&edit));
        mirror_year_for_legacy(&mut tag, &TagPatch::Fields(&edit), id3::Version::Id3v23);

        assert_eq!(tag.year(), Some(1997));
        assert_eq!(tag.title(), Some("New Title"));
    }

    #[test]
    fn a_dsf_cover_replaces_the_front_and_keeps_the_rest() {
        // Same rule as the lofty side: a booklet or a back cover is not
        // the front cover, and nothing brings it back.
        use id3::TagLike;
        let mut tag = id3::Tag::new();
        tag.add_frame(id3::frame::Picture {
            mime_type: "image/jpeg".into(),
            picture_type: id3::frame::PictureType::CoverBack,
            description: String::new(),
            data: vec![1, 2, 3],
        });
        tag.add_frame(id3::frame::Picture {
            mime_type: "image/jpeg".into(),
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: vec![9, 9, 9],
        });

        let mime = lofty::picture::MimeType::Jpeg;
        apply_patch_id3(
            &mut tag,
            &TagPatch::Cover {
                bytes: &[4, 5, 6],
                mime: &mime,
            },
        );

        let pictures: Vec<_> = tag.pictures().collect();
        assert_eq!(pictures.len(), 2);
        assert!(
            pictures
                .iter()
                .any(|p| p.picture_type == id3::frame::PictureType::CoverBack
                    && p.data == vec![1, 2, 3]),
            "the back cover survived"
        );
        assert!(
            pictures
                .iter()
                .any(|p| p.picture_type == id3::frame::PictureType::CoverFront
                    && p.data == vec![4, 5, 6]),
            "the front cover is the new one"
        );
    }

    /// The byte the database holds is the byte another player reads, on
    /// the container where the rating used to stop at the database (#644).
    #[test]
    fn a_rating_reaches_an_id3_tag_as_the_byte_it_was_given() {
        use id3::TagLike;
        let mut tag = id3::Tag::new();
        tag.set_title("Keep me");

        apply_patch_id3(&mut tag, &TagPatch::Rating(Some(196)));

        // Through an encode/decode, because a frame the writer refuses is
        // a frame the file never gets.
        let mut bytes = Vec::new();
        tag.write_to(&mut bytes, id3::Version::Id3v24)
            .expect("write");
        let back = id3::Tag::read_from2(std::io::Cursor::new(&bytes)).expect("read");

        let popm = back
            .frames()
            .find_map(|frame| match frame.content() {
                id3::Content::Popularimeter(p) => Some(p),
                _ => None,
            })
            .expect("a POPM frame");
        assert_eq!(popm.rating, 196);
        assert_eq!(
            back.title(),
            Some("Keep me"),
            "a rating is one frame, not a new tag"
        );
    }

    /// POPM is keyed by user, so a tag can hold several and the reader
    /// takes the first. Clearing one star out of five has to take every
    /// one of them, or the old rating simply comes back.
    #[test]
    fn clearing_a_rating_takes_every_popm_frame_with_it() {
        use id3::TagLike;
        let mut tag = id3::Tag::new();
        for user in ["", "someone@else"] {
            tag.add_frame(id3::Frame::with_content(
                "POPM",
                id3::Content::Popularimeter(id3::frame::Popularimeter {
                    user: user.to_string(),
                    rating: 255,
                    counter: 0,
                }),
            ));
        }

        apply_patch_id3(&mut tag, &TagPatch::Rating(None));

        assert_eq!(tag.frames().filter(|f| f.id() == "POPM").count(), 0);
    }

    /// The generic side of the same edit — the containers that keep a
    /// rating as plain text on the 0-100 scale, which is the shape the
    /// scanner reads back from them. ID3v2 does not come through here:
    /// see [`apply_rating_id3v2`].
    #[test]
    fn the_generic_applier_writes_the_text_scale() {
        use lofty::tag::{ItemKey, Tag, TagType};

        let mut vorbis = Tag::new(TagType::VorbisComments);
        apply_patch(&mut vorbis, &TagPatch::Rating(Some(196)));
        assert_eq!(
            vorbis.get_string(ItemKey::Popularimeter),
            Some("76"),
            "196 of 255 on the 0-100 scale"
        );

        apply_patch(&mut vorbis, &TagPatch::Rating(None));
        assert_eq!(vorbis.get_string(ItemKey::Popularimeter), None);
    }

    /// The byte the user set is the byte the frame carries.
    ///
    /// Measured on lofty 0.25: a rating put into the **generic** tag
    /// does not survive the merge back — `Binary` and a plain number
    /// both leave zero frames, and only `<provider>|<stars>|<counter>`
    /// with a whole star between the pipes produces one, re-encoded on
    /// that provider's scale. None of those is a 0-255 value in
    /// half-star steps, which is what the database holds, so the frame
    /// is built directly.
    #[test]
    fn a_rating_is_written_as_the_frame_it_is() {
        use lofty::id3::v2::{Frame, Id3v2Tag};

        let mut tag = Id3v2Tag::default();
        apply_rating_id3v2(&mut tag, Some(196));

        let ratings: Vec<u8> = tag
            .iter()
            .filter_map(|frame| match frame {
                Frame::Popularimeter(popm) => Some(popm.rating),
                _ => None,
            })
            .collect();
        assert_eq!(ratings, vec![196]);
    }

    /// POPM is keyed by email, so a file can hold several and a reader
    /// takes the first. A rating the user replaced has to stop being
    /// the one another player shows.
    #[test]
    fn writing_a_rating_takes_every_popm_the_file_held() {
        use lofty::id3::v2::{Frame, Id3v2Tag, PopularimeterFrame};

        let mut tag = Id3v2Tag::default();
        tag.insert(Frame::Popularimeter(PopularimeterFrame::new(
            "someone@else",
            255,
            12,
        )));
        tag.insert(Frame::Popularimeter(PopularimeterFrame::new("", 51, 0)));

        apply_rating_id3v2(&mut tag, Some(153));
        assert_eq!(
            tag.iter()
                .filter(|f| matches!(f, Frame::Popularimeter(_)))
                .count(),
            1
        );

        apply_rating_id3v2(&mut tag, None);
        assert_eq!(
            tag.iter()
                .filter(|f| matches!(f, Frame::Popularimeter(_)))
                .count(),
            0,
            "clearing the stars clears the frame"
        );
    }

    /// The same round trip on the container that loses what it is not
    /// asked about: `From<VorbisComments> for Tag` throws the remainder
    /// away, which is the whole reason `patch_file` merges it back.
    #[test]
    fn lyrics_survive_it_on_a_vorbis_tag_and_take_nothing_else_with_them() {
        use lofty::ogg::tag::VorbisComments;
        use lofty::prelude::{ItemKey, SplitTag};
        use lofty::tag::MergeTag;

        let mut comments = VorbisComments::default();
        // A comment lofty does not model, of exactly the kind a save
        // used to drop.
        comments.push("REPLAYGAIN_TRACK_GAIN".to_string(), "-6.5 dB".to_string());

        let (remainder, mut generic) = comments.split_tag();
        apply_patch(
            &mut generic,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("the words")),
        );
        let concrete: VorbisComments = remainder.merge_tag(generic);

        assert_eq!(
            concrete.get("REPLAYGAIN_TRACK_GAIN"),
            Some("-6.5 dB"),
            "the remainder came back"
        );
        let (_, as_the_reader_sees_it) = concrete.split_tag();
        assert_eq!(
            as_the_reader_sees_it.get_string(ItemKey::UnsyncLyrics),
            Some("the words")
        );
    }

    /// Saving lyrics twice leaves one set, not two. USLT is keyed by
    /// language and description, and a second frame under the same pair
    /// is what a reader shows instead of the edit.
    #[test]
    fn lyrics_replace_what_the_tag_already_held() {
        let mut tag = id3::Tag::new();

        apply_patch_id3(
            &mut tag,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("first take")),
        );
        apply_patch_id3(
            &mut tag,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("second take")),
        );

        let all: Vec<_> = tag.lyrics().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "second take");
    }

    #[test]
    fn clearing_lyrics_empties_the_frame_rather_than_blanking_it() {
        let mut tag = id3::Tag::new();
        apply_patch_id3(
            &mut tag,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("words")),
        );

        apply_patch_id3(&mut tag, &TagPatch::Lyrics(LyricsSlot::Cleared));

        assert_eq!(tag.lyrics().count(), 0);
    }

    /// The generic applier answers the same way its `id3` sibling does,
    /// on the container where the two could have disagreed: a write
    /// that ID3v2 silently drops must not be preceded by a clear that
    /// it does not.
    #[test]
    fn an_arbitrary_slot_leaves_a_generic_id3v2_tag_alone_too() {
        use lofty::tag::{ItemKey, Tag, TagType};

        let mut id3 = Tag::new(TagType::Id3v2);
        apply_patch(
            &mut id3,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("words already there")),
        );

        apply_patch(&mut id3, &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt/>")));
        assert_eq!(
            id3.get_string(ItemKey::UnsyncLyrics),
            Some("words already there")
        );

        // A container that does have the slot takes the write.
        let mut vorbis = Tag::new(TagType::VorbisComments);
        apply_patch(
            &mut vorbis,
            &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt/>")),
        );
        assert_eq!(vorbis.get_string(ItemKey::Lyrics), Some("<tt/>"));
    }

    /// The arm that cannot be reached today, asserted anyway: ID3v2 has
    /// no arbitrary-string lyrics slot, and the day the guard upstream
    /// moves, the wrong answer here is to clear a file's lyrics to make
    /// room for a write that never happens.
    #[test]
    fn an_arbitrary_slot_leaves_an_id3_tag_exactly_as_it_was() {
        let mut tag = id3::Tag::new();
        apply_patch_id3(
            &mut tag,
            &TagPatch::Lyrics(LyricsSlot::Unsynchronised("words already there")),
        );

        apply_patch_id3(
            &mut tag,
            &TagPatch::Lyrics(LyricsSlot::Arbitrary("<tt>…</tt>")),
        );

        let all: Vec<_> = tag.lyrics().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "words already there");
    }
}

#[cfg(test)]
mod sync_db_tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    /// The real migrator, `foreign_keys` on: what `sync_db` writes must
    /// hold against the schema the app actually has.
    async fn pool() -> SqlitePool {
        let options = waveflow_core::repository::sqlite::collation::register(
            SqliteConnectOptions::from_str(":memory:")
                .unwrap()
                .foreign_keys(true),
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/profile")
            .run(&pool)
            .await
            .unwrap();
        for statement in [
            "INSERT INTO library (id, name, color_id, icon_id, created_at, updated_at,
                                  hlc_wall, hlc_logical)
             VALUES (1, 'L', 1, 1, 0, 0, 0, 0)",
            "INSERT INTO artist (id, name, canonical_name) VALUES (1, 'pink floyd', 'pink floyd')",
            "INSERT INTO album (id, title, canonical_title, artist_id, year, is_compilation)
             VALUES (1, 'the wall', 'the wall', 1, 1979, 0)",
            "INSERT INTO genre (id, name, canonical_name) VALUES (1, 'prog rock', 'prog rock')",
            "INSERT INTO track (id, library_id, file_path, file_hash, file_size, file_modified,
                                title, album_id, duration_ms, added_at, is_available, year,
                                hlc_wall, hlc_logical, rating_hlc_wall, rating_hlc_logical)
             VALUES (1, 1, '/m/1.flac', 'h1', 1, 0, 'In the Flesh?', 1, 1000, 0, 1, 1979,
                     0, 0, 0, 0)",
            "UPDATE track SET primary_artist = 1 WHERE id = 1",
            "INSERT INTO track_artist (track_id, artist_id, position) VALUES (1, 1, 0)",
            "INSERT INTO track_genre (track_id, genre_id) VALUES (1, 1)",
        ] {
            sqlx::raw_sql(statement).execute(&pool).await.unwrap();
        }
        pool
    }

    fn edit() -> TrackEdit {
        TrackEdit {
            title: None,
            artist: None,
            album: None,
            year: None,
            track_number: None,
            disc_number: None,
            genre: None,
        }
    }

    /// Case and punctuation fixes resolve to the same rows through the
    /// canonical key; they used to leave the old spelling on display.
    #[tokio::test]
    async fn a_respelled_name_reaches_the_existing_row() {
        let pool = pool().await;
        let fix = TrackEdit {
            artist: Some("Pink Floyd".into()),
            album: Some("The Wall".into()),
            genre: Some("Prog Rock".into()),
            ..edit()
        };
        sync_db(&pool, 1, &fix).await.unwrap();

        let artist: (i64, String) =
            sqlx::query_as("SELECT ar.id, ar.name FROM track t JOIN artist ar ON ar.id = t.primary_artist WHERE t.id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(artist, (1, "Pink Floyd".to_string()));
        let album: (i64, String) =
            sqlx::query_as("SELECT al.id, al.title FROM track t JOIN album al ON al.id = t.album_id WHERE t.id = 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(album, (1, "The Wall".to_string()));
        let genre: (i64, String) = sqlx::query_as(
            "SELECT g.id, g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id WHERE tg.track_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(genre, (1, "Prog Rock".to_string()));
    }

    /// Several genres become several rows, the existing one reused through
    /// its canonical key; clearing removes them all (#729).
    #[tokio::test]
    async fn several_genres_become_several_rows() {
        let pool = pool().await;
        let genres = |pool: SqlitePool| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT g.name FROM track_genre tg JOIN genre g ON g.id = tg.genre_id
                  WHERE tg.track_id = 1 ORDER BY g.name",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };

        let tagged = TrackEdit {
            genre: Some("Prog Rock; Art Rock;prog rock".into()),
            ..edit()
        };
        sync_db(&pool, 1, &tagged).await.unwrap();
        assert_eq!(genres(pool.clone()).await, ["Art Rock", "Prog Rock"]);
        let reused: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM track_genre WHERE track_id = 1 AND genre_id = 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reused, 1, "the existing genre row is the one linked");

        let cleared = TrackEdit {
            genre: Some(" ; ".into()),
            ..edit()
        };
        sync_db(&pool, 1, &cleared).await.unwrap();
        assert!(genres(pool.clone()).await.is_empty());
    }

    /// `"; "` separates artists; a comma stays inside a name.
    #[tokio::test]
    async fn artists_split_on_the_semicolon_only() {
        let pool = pool().await;
        let credits = TrackEdit {
            artist: Some("Earth, Wind & Fire; The Emotions".into()),
            ..edit()
        };
        sync_db(&pool, 1, &credits).await.unwrap();

        let names: Vec<String> = sqlx::query_scalar(
            "SELECT ar.name FROM track_artist ta JOIN artist ar ON ar.id = ta.artist_id
              WHERE ta.track_id = 1 ORDER BY ta.position",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(names, ["Earth, Wind & Fire", "The Emotions"]);
    }

    /// The album header took its year from the scan alone.
    #[tokio::test]
    async fn an_edited_year_reaches_the_album() {
        let pool = pool().await;
        let year = TrackEdit {
            year: Some(1980),
            ..edit()
        };
        sync_db(&pool, 1, &year).await.unwrap();

        let album_year: Option<i64> = sqlx::query_scalar("SELECT year FROM album WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(album_year, Some(1980));
    }
}
