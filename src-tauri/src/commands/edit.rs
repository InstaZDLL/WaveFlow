//! Single-track ID3 / Vorbis tag editor.
//!
//! Mirrors the scanner's tag → DB pipeline in reverse: the user edits a
//! handful of fields, the values are written back to the audio file via
//! lofty, and the database (track / album / artist / track_artist /
//! genre / track_genre) is updated to match. FTS5 stays in sync via the
//! existing triggers on `track`, `album.title`, and `artist.name`.
//!
//! File-lock dance: the audio engine may have the file open if the
//! edited track is currently playing. lofty's `save_to_path` uses
//! atomic rename on POSIX but needs an exclusive handle on Windows, so
//! we pause playback before writing whenever the engine reports the
//! same `current_track_id`. Resume is left to the user — silently
//! restarting after a save would be surprising and could re-lock the
//! file before the rename completed.

use std::sync::Arc;

use serde::Deserialize;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter};

use crate::{
    audio::AudioEngine,
    commands::scan::{
        canonical_name, split_artist_name, upsert_album, upsert_artist, upsert_artwork,
        upsert_genre,
    },
    error::{AppError, AppResult},
    state::AppState,
};

/// Edit payload from the frontend. Every field is optional — `None`
/// means "leave this field untouched"; `Some("")` means "clear this
/// field" (where applicable). The frontend sends whatever's currently
/// in the form input on save, so we always get every field set when
/// the user explicitly hits Save.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct TrackEdit {
    pub title: Option<String>,
    /// Raw multi-artist string ("Artist A, Artist B"). Split via the
    /// scanner's `split_artist_name` so a comma-separated input gets
    /// normalised to the same many-to-many shape.
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i64>,
    pub track_number: Option<i64>,
    pub disc_number: Option<i64>,
    pub genre: Option<String>,
}

#[tauri::command]
pub async fn update_track_tags(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, Arc<AudioEngine>>,
    app: AppHandle,
    track_id: i64,
    edit: TrackEdit,
) -> AppResult<()> {
    let pool = state.require_profile_pool().await?;

    // 1. Pull the current track row so we know the file path AND can
    //    fall back to existing values for fields the user didn't
    //    touch (matters for the album/artist relink which needs the
    //    full new context).
    let row: Option<TrackRow> = sqlx::query_as::<_, TrackRow>(
        "SELECT id, file_path, primary_artist, album_id FROM track WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(&pool)
    .await?;
    let row = row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    let path = std::path::PathBuf::from(&row.file_path);

    // 2. If the engine is playing this track, pause before opening.
    //    Releases the file handle so lofty's atomic rename succeeds
    //    on Windows. Resume is the user's call — see module doc.
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
    write_tags_to_file(&path, &edit)
        .map_err(|e| AppError::Other(format!("tag write failed: {e}")))?;

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
    Ok(())
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
        .fetch_optional(&pool)
        .await?;
        let Some(row) = row else {
            summary
                .errors
                .push((*track_id, "track not found".into()));
            continue;
        };
        let path = std::path::PathBuf::from(&row.file_path);

        if let Err(err) = write_tags_to_file(&path, &edit) {
            summary
                .errors
                .push((*track_id, format!("tag write failed: {err}")));
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

/// Apply the edit to the file's primary tag. Creates a fresh tag of
/// the file's preferred type when none exists (rare — every supported
/// format ships with at least empty headers). Returns the boxed lofty
/// error untouched so the caller can surface a useful message.
fn write_tags_to_file(
    path: &std::path::Path,
    edit: &TrackEdit,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::prelude::*;
    use lofty::tag::{ItemKey, Tag};

    let mut tagged = lofty::read_from_path(path)?;

    // Pick the existing tag if any, otherwise create one of the
    // file's preferred type so a previously-untagged file gets
    // properly tagged on first edit.
    if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
        let preferred = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(preferred));
    }
    // Two-step borrow so the borrow checker doesn't see a chained
    // primary_tag_mut() / first_tag_mut() on the same tagged file.
    let tag = if tagged.primary_tag().is_some() {
        tagged
            .primary_tag_mut()
            .expect("checked primary_tag is Some")
    } else {
        tagged
            .first_tag_mut()
            .ok_or("no tag available after insert")?
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
        // Year isn't on the Accessor trait in lofty 0.24 — it's
        // exposed as a generic ItemKey because the underlying
        // representation differs across formats (TDRC vs DATE vs
        // ©day). insert_text overwrites the existing value.
        if y > 0 {
            tag.insert_text(ItemKey::Year, y.to_string());
        } else {
            tag.remove_key(ItemKey::Year);
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
            tag.set_disk(n as u32);
        } else {
            tag.remove_disk();
        }
    }
    if let Some(g) = edit.genre.as_ref() {
        if g.trim().is_empty() {
            tag.remove_genre();
        } else {
            tag.set_genre(g.trim().to_string());
        }
    }

    tagged.save_to_path(path, lofty::config::WriteOptions::default())?;
    Ok(())
}

/// Mirror the file write into the database. Order matters because
/// `track.album_id` depends on the resolved `primary_artist` (album
/// is keyed on `(canonical_title, artist_id)`), and `track_artist`
/// needs the new track row state.
async fn sync_db(pool: &SqlitePool, track_id: i64, edit: &TrackEdit) -> AppResult<()> {
    let mut tx = pool.begin().await?;

    // Resolve the new artist list (and the new primary). When the
    // user didn't touch the artist field, leave the existing rows
    // alone — re-upserting the same names would no-op but the DELETE
    // + INSERT churn isn't free on a hot library.
    let (artist_ids, primary_artist_id): (Option<Vec<i64>>, Option<i64>) =
        if let Some(raw) = edit.artist.as_ref() {
            let mut ids: Vec<i64> = Vec::new();
            for name in split_artist_name(raw) {
                if let Some(id) = upsert_artist(&mut tx, &name).await? {
                    if !ids.contains(&id) {
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
            // upsert_album now takes &mut SqliteConnection, so we
            // can call it directly inside the open transaction —
            // no commit/reopen dance needed.
            let aid = upsert_album(&mut tx, title, aid, edit.year).await?;
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
        let mut q = sqlx::query(&sql);
        if let Some(t) = edit.title.as_ref() {
            q = q.bind(t.trim());
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

    // Genre — single value for v1. Keep the current shape (track_genre
    // is a many-to-many table) by clearing then optionally re-inserting.
    if let Some(g) = edit.genre.as_ref() {
        sqlx::query("DELETE FROM track_genre WHERE track_id = ?")
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
        let trimmed = g.trim();
        if !trimmed.is_empty() {
            if let Some(gid) = upsert_genre(&mut tx, trimmed).await? {
                sqlx::query("INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?, ?)")
                    .bind(track_id)
                    .bind(gid)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    tx.commit().await?;
    // Suppress unused-import warning when canonical_name isn't reached
    // via a code path in this function (it's exported for callers).
    let _ = canonical_name;
    Ok(())
}

/// Replace the embedded cover for a track. The new image is written
/// into the audio file's tag (replacing every existing picture so the
/// thumbnail-thieving "20-cover ID3 spam" tracks get cleaned up too)
/// AND copied into the per-profile artwork cache, then the track's
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
            .fetch_optional(&pool)
            .await?;
    let (file_path, album_id) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;

    let bytes = std::fs::read(&image_path)
        .map_err(|e| AppError::Other(format!("cover read failed: {e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Other("cover file is empty".into()));
    }
    let (mime, ext) = sniff_image_mime(&bytes, &image_path);

    // Pause if the engine has the file open — same Windows-rename
    // dance as the tag-edit path.
    let active = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    if active == track_id {
        let _ = engine.send(crate::audio::AudioCmd::Pause);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    write_cover_to_file(std::path::Path::new(&file_path), &bytes, &mime)
        .map_err(|e| AppError::Other(format!("cover tag write failed: {e}")))?;

    // Hash + persist the bytes in the shared artwork cache. blake3
    // makes "same image, different file" deduplicate naturally.
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let out_path = artwork_dir.join(format!("{hash}.{ext}"));
    if !out_path.exists() {
        std::fs::write(&out_path, &bytes)
            .map_err(|e| AppError::Other(format!("artwork cache write failed: {e}")))?;
    }
    crate::thumbnails::spawn_thumbnail_job(out_path.clone(), artwork_dir.clone(), hash.clone());

    let mut conn = pool.acquire().await?;
    let artwork_id = upsert_artwork(&mut conn, &hash, ext, "manual").await?;
    drop(conn);
    if let Some(aid) = album_id {
        sqlx::query("UPDATE album SET artwork_id = ? WHERE id = ?")
            .bind(artwork_id)
            .bind(aid)
            .execute(&pool)
            .await?;
    }

    let _ = app.emit("track:updated", track_id);
    let _ = app.emit("library:rescanned", ());
    let _ = app.emit("player:queue-changed", ());
    Ok(())
}

/// Write `bytes` as the only embedded picture in the audio file at
/// `path`. Removes every existing picture first so we don't end up
/// with a chimera tag holding both the new cover AND the previous
/// one(s).
fn write_cover_to_file(
    path: &std::path::Path,
    bytes: &[u8],
    mime: &lofty::picture::MimeType,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::picture::{Picture, PictureType};
    use lofty::tag::Tag;

    let mut tagged = lofty::read_from_path(path)?;
    if tagged.primary_tag().is_none() && tagged.first_tag().is_none() {
        let preferred = tagged.primary_tag_type();
        tagged.insert_tag(Tag::new(preferred));
    }
    let tag = if tagged.primary_tag().is_some() {
        tagged.primary_tag_mut().expect("checked")
    } else {
        tagged.first_tag_mut().ok_or("no tag")?
    };

    // Drop existing pictures. `remove_picture` takes an index so we
    // pop from the end backwards to avoid invalidating positions.
    while !tag.pictures().is_empty() {
        tag.remove_picture(tag.pictures().len() - 1);
    }
    // Lofty 0.24 swapped the constructor for a builder.
    let picture = Picture::unchecked(bytes.to_vec())
        .pic_type(PictureType::CoverFront)
        .mime_type(mime.clone())
        .build();
    tag.push_picture(picture);
    tagged.save_to_path(path, lofty::config::WriteOptions::default())?;
    Ok(())
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
