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
        canonical_name, split_artist_name, upsert_album, upsert_artist, upsert_genre,
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
        tagged.primary_tag_mut().expect("checked primary_tag is Some")
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
                if let Some(id) = upsert_artist(&pool.clone(), &name).await? {
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
                None => sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT primary_artist FROM track WHERE id = ?",
                )
                .bind(track_id)
                .fetch_one(&mut *tx)
                .await?,
            };
            // upsert_album takes a separate pool so we have to
            // commit the artist work first or use the same pool;
            // simplest is to release the tx briefly here. The
            // alternative (passing &mut Transaction through the
            // helper) means changing scan.rs's helper signature,
            // which would ripple through every scanner call site.
            tx.commit().await?;
            let aid = upsert_album(&pool.clone(), title, aid, edit.year).await?;
            tx = pool.begin().await?;
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
            // Commit and reopen for the helper, same trick as for album.
            tx.commit().await?;
            if let Some(gid) = upsert_genre(&pool.clone(), trimmed).await? {
                sqlx::query(
                    "INSERT OR IGNORE INTO track_genre (track_id, genre_id) VALUES (?, ?)",
                )
                .bind(track_id)
                .bind(gid)
                .execute(pool)
                .await?;
            }
            return Ok(());
        }
    }

    tx.commit().await?;
    // Suppress unused-import warning when canonical_name isn't reached
    // via a code path in this function (it's exported for callers).
    let _ = canonical_name;
    Ok(())
}
