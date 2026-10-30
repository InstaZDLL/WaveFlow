//! The edit commands, for one track and for a batch, and the guards
//! around a write: validation, the file stamp, the re-hash and the folder
//! re-read.

use std::sync::Arc;

use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::{
    audio::AudioEngine,
    error::{AppError, AppResult},
    state::AppState,
};

use super::*;

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
    /// The Album Artist (`TPE2` / `ALBUMARTIST` / `aART`). Kept whole,
    /// as the scanner keeps it — `resolve_album_artist` upserts the name
    /// verbatim — because it names the album's artist, not a list of
    /// credits. It decides which album the track is grouped into, so a
    /// change moves the track (see [`sync_db`]). Empty clears it.
    pub album_artist: Option<String>,
    /// The composer (`TCOM` / `COMPOSER` / `©wrt`), stored in `track_tag`
    /// under [`COMPOSER_KEY`] — the row the scanner fills from the same
    /// frame. Empty clears it.
    pub composer: Option<String>,
    /// The size of the track's disc (`TRCK` "4/12", `TRACKTOTAL`, `trkn`).
    /// `0` clears it.
    pub track_total: Option<i64>,
    /// The number of discs (`TPOS` "1/2", `DISCTOTAL`, `disk`). `0` clears it.
    pub disc_total: Option<i64>,
    /// The MusicBrainz identifiers, each under its format's own key. Only
    /// the tag workshop sends them; the properties dialog leaves them be.
    pub musicbrainz: MusicBrainzEdit,
}

/// The six MusicBrainz identifiers a file can carry, as the tag workshop
/// writes them. Same convention as every other field: `None` leaves the
/// identifier alone, `Some("")` removes it. The two artist lists are
/// `"; "`-joined, one identifier per credited artist.
///
/// Where each one goes is [`super::musicbrainz_slots`].
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct MusicBrainzEdit {
    pub release_id: Option<String>,
    pub release_group_id: Option<String>,
    pub recording_id: Option<String>,
    pub release_track_id: Option<String>,
    pub artist_id: Option<String>,
    pub album_artist_id: Option<String>,
}

/// Refuse an edit the file and the database could not agree on.
///
/// The year is the one field with a narrower range on disk than in the
/// row: a tag carries it as a `u16`, `track.year` is an SQLite integer.
/// Clamping on the way to the file would leave the two telling
/// different stories — the same divergence the date handling in
/// [`apply_patch`] exists to avoid — so an unrepresentable year is
/// refused before either is touched. The message is user-facing.
pub(crate) fn validate_edit(edit: &TrackEdit) -> AppResult<()> {
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
    let (pool, profile_id) = state.require_profile_snapshot().await?;
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
