//! Offline copies of the bound server's tracks (lot 4 of the unified library).
//!
//! ## A download is not a local track
//!
//! It lands in a managed folder the scanner never sees, and no `track` row is
//! created for it. A download describes a *remote* track that happens to be on
//! this disk — which is why it is keyed by the server's id and not by a rowid,
//! and why `remote_track_link` cannot describe it (that table keys on
//! `local_track_id REFERENCES track(id)`).
//!
//! ## Always the original bytes
//!
//! The transcode preference is deliberately ignored here. It exists to spend
//! less bandwidth on a stream that is heard once; a file kept on disk is the
//! copy someone chose to keep, and baking a lossy re-encode into it would make
//! that choice irreversible without telling them. Downloads are `raw`.
//!
//! ## The hash is the point, and it is free
//!
//! Reconciling a local file against the server is expensive precisely because
//! the two digests are incompatible by construction: the library's `file_hash`
//! covers a file the server has never seen. Here we are the ones writing the
//! bytes, so the server's own `full_hash` falls out of the write for nothing —
//! one pass, no re-read. That is what will make a later *import* into a
//! scanned folder able to claim an exact reconciliation proof without paying
//! for it twice.

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use serde::Serialize;
use sqlx::{Row, SqliteConnection, SqlitePool};
use tauri::{AppHandle, Emitter};

use crate::{
    error::{AppError, AppResult},
    state::AppState,
};

/// Emitted while a download runs so the interface can show a bar rather than
/// a spinner that says nothing about a forty-megabyte file.
#[derive(Clone, Serialize)]
pub struct DownloadProgress {
    pub track_id: String,
    pub received: u64,
    /// `None` when the server did not declare a length.
    pub total: Option<u64>,
}

/// One offline copy, as the interface sees it.
#[derive(Clone, Serialize)]
pub struct DownloadedTrack {
    pub remote_track_id: String,
    pub path: String,
    pub full_hash: String,
    pub size: u64,
    pub downloaded_at: i64,
}

/// Total bytes and file count held by the managed folder.
#[derive(Clone, Copy, Serialize)]
pub struct DownloadsInfo {
    pub bytes: u64,
    pub tracks: usize,
}

/// Track ids with a download in flight right now.
///
/// Two callers asking for the same track — a second window, a double click —
/// would otherwise both find no existing copy, both fetch the same bytes, and
/// both write them to the same place. The working name below is unique per
/// call, so nothing could corrupt anything even without this; the guard is
/// what stops the *second download* from happening at all.
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Releases its track id however the download ends, including a panic.
struct InFlightGuard(String);

impl InFlightGuard {
    /// `None` when this track is already being fetched.
    fn claim(track_id: &str) -> Option<Self> {
        let mut set = in_flight().lock().ok()?;
        if !set.insert(track_id.to_string()) {
            return None;
        }
        Some(Self(track_id.to_string()))
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = in_flight().lock() {
            set.remove(&self.0);
        }
    }
}

/// The offline copy of a remote track, if one exists **and its file is still
/// there**.
///
/// The row and the file can disagree — someone can empty a folder behind the
/// application's back — and answering from the row alone would hand playback a
/// path that is not there. A row without a file is dropped rather than
/// reported, so the next play refetches instead of failing forever.
pub async fn lookup(conn: &mut SqliteConnection, remote_track_id: &str) -> Option<PathBuf> {
    let path: Option<String> =
        sqlx::query_scalar("SELECT path FROM remote_track_download WHERE remote_track_id = ?")
            .bind(remote_track_id)
            .fetch_optional(&mut *conn)
            .await
            .ok()
            .flatten();
    let path = PathBuf::from(path?);
    if path.is_file() {
        return Some(path);
    }
    let _ = sqlx::query("DELETE FROM remote_track_download WHERE remote_track_id = ?")
        .bind(remote_track_id)
        .execute(conn)
        .await;
    None
}

/// Every offline copy, newest first.
pub async fn list(pool: &SqlitePool) -> AppResult<Vec<DownloadedTrack>> {
    let rows = sqlx::query(
        "SELECT remote_track_id, path, full_hash, size, downloaded_at
           FROM remote_track_download
          ORDER BY downloaded_at DESC",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(DownloadedTrack {
                remote_track_id: row.try_get("remote_track_id")?,
                path: row.try_get("path")?,
                full_hash: row.try_get("full_hash")?,
                size: row.try_get::<i64, _>("size")?.max(0) as u64,
                downloaded_at: row.try_get("downloaded_at")?,
            })
        })
        .collect()
}

/// Bytes and count, read from the rows rather than from the directory.
///
/// The rows are the record of what was deliberately kept; a stray file in the
/// folder is not a download and should not be reported as one.
pub async fn info(pool: &SqlitePool) -> AppResult<DownloadsInfo> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(size), 0) AS bytes, COUNT(*) AS tracks FROM remote_track_download",
    )
    .fetch_one(pool)
    .await?;
    Ok(DownloadsInfo {
        bytes: row.try_get::<i64, _>("bytes")?.max(0) as u64,
        tracks: row.try_get::<i64, _>("tracks")?.max(0) as usize,
    })
}

/// Remove one offline copy: the file first, then the row.
///
/// That order matters. A row without a file is self-healing — [`lookup`] drops
/// it on the next miss — while a file without a row is invisible to everything
/// and would sit there forever.
pub async fn remove(conn: &mut SqliteConnection, remote_track_id: &str) -> AppResult<bool> {
    let path: Option<String> =
        sqlx::query_scalar("SELECT path FROM remote_track_download WHERE remote_track_id = ?")
            .bind(remote_track_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(path) = path else {
        return Ok(false);
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        // Already gone is the outcome asked for, not a failure.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(AppError::from(err)),
    }
    sqlx::query("DELETE FROM remote_track_download WHERE remote_track_id = ?")
        .bind(remote_track_id)
        .execute(conn)
        .await?;
    Ok(true)
}

/// Drop every offline copy. Returns how many were removed.
///
/// Files first, rows after, and one row at a time rather than a bulk delete:
/// a file that refuses to go must keep its row, or it becomes invisible
/// clutter nothing will ever offer to remove again.
pub async fn clear_all(conn: &mut SqliteConnection) -> AppResult<usize> {
    let ids: Vec<String> = sqlx::query_scalar("SELECT remote_track_id FROM remote_track_download")
        .fetch_all(&mut *conn)
        .await?;
    let mut removed = 0;
    for id in ids {
        if remove(&mut *conn, &id).await? {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Fetch a remote track's original bytes into the managed folder.
///
/// Returns the existing copy untouched when there already is one: asking twice
/// is a no-op, not a re-download.
pub async fn download(
    app: &AppHandle,
    state: &AppState,
    remote_track_id: &str,
) -> AppResult<DownloadedTrack> {
    if crate::offline::is_offline() {
        return Err(AppError::Other("offline".into()));
    }
    // Pool and profile together: everything below — the existing-copy check,
    // the destination folder, the row written at the end — belongs to one
    // profile, and resolving them separately across the download's awaits is
    // what would let a switch file one profile's audio under another's.
    let (pool, profile_id) = state.require_profile_snapshot().await?;

    if let Some(existing) = existing_row(&pool, remote_track_id).await? {
        return Ok(existing);
    }
    // Claimed before the first byte and held through the rename and the row,
    // so the window a second caller could slip into does not exist.
    let _guard = InFlightGuard::claim(remote_track_id)
        .ok_or_else(|| AppError::Other("this track is already downloading".into()))?;
    // Re-checked under the claim: the copy may have landed while we waited.
    if let Some(existing) = existing_row(&pool, remote_track_id).await? {
        return Ok(existing);
    }

    let extension = suffix_for(&pool, remote_track_id).await;
    let dir = state.paths.profile_remote_download_dir(profile_id);
    std::fs::create_dir_all(&dir)?;

    // The original bytes, whatever the streaming preference says — see the
    // module note. `ticket_url` applies the preference, so the ticket is minted
    // here instead of borrowed from it.
    let url = crate::remote::stream::raw_ticket_url(state, remote_track_id).await?;

    let final_path = dir.join(file_name(remote_track_id, &extension));
    // Unique per call, not merely per track: a shared working name is how two
    // writers end up interleaving one file. The stream cache learned this; the
    // lesson belongs here too.
    let part_path = dir.join(format!(
        "{}.{}.in-flight",
        file_name(remote_track_id, &extension),
        blake3::hash(format!("{:?}", std::time::SystemTime::now()).as_bytes())
            .to_hex()
            .to_string()
            .split_at(16)
            .0
    ));
    let outcome = stream_to_file(app, remote_track_id, &url, &part_path).await;
    let (size, full_hash) = match outcome {
        Ok(pair) => pair,
        Err(err) => {
            // Nothing partial survives: a half file in the managed folder is
            // indistinguishable from a whole one to anything that lists it.
            let _ = std::fs::remove_file(&part_path);
            return Err(err);
        }
    };
    // The server told us what these bytes hash to when it mirrored the track.
    // Comparing costs nothing here — we hashed while writing — and it is the
    // only thing standing between a truncated or substituted body and a file
    // we will later offer as an exact reconciliation proof.
    if let Some(expected) = catalogue_hash(&pool, remote_track_id).await {
        if expected != full_hash {
            let _ = std::fs::remove_file(&part_path);
            return Err(AppError::Other(format!(
                "download: hash mismatch for {remote_track_id}"
            )));
        }
    }

    if let Err(err) = std::fs::rename(&part_path, &final_path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(AppError::from(err));
    }

    let downloaded_at = chrono::Utc::now().timestamp_millis();
    let record = DownloadedTrack {
        remote_track_id: remote_track_id.to_string(),
        path: final_path.to_string_lossy().to_string(),
        full_hash,
        size,
        downloaded_at,
    };
    sqlx::query(
        "INSERT INTO remote_track_download
             (remote_track_id, path, full_hash, size, downloaded_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(remote_track_id) DO UPDATE SET
             path = excluded.path,
             full_hash = excluded.full_hash,
             size = excluded.size,
             downloaded_at = excluded.downloaded_at",
    )
    .bind(&record.remote_track_id)
    .bind(&record.path)
    .bind(&record.full_hash)
    .bind(record.size as i64)
    .bind(record.downloaded_at)
    .execute(&*pool)
    .await?;
    Ok(record)
}

async fn existing_row(
    pool: &SqlitePool,
    remote_track_id: &str,
) -> AppResult<Option<DownloadedTrack>> {
    let row = sqlx::query(
        "SELECT remote_track_id, path, full_hash, size, downloaded_at
           FROM remote_track_download WHERE remote_track_id = ?",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let path: String = row.try_get("path")?;
    if !Path::new(&path).is_file() {
        // The row outlived its file; treat it as absent so the caller
        // re-downloads rather than reporting a copy nobody can play.
        return Ok(None);
    }
    Ok(Some(DownloadedTrack {
        remote_track_id: row.try_get("remote_track_id")?,
        path,
        full_hash: row.try_get("full_hash")?,
        size: row.try_get::<i64, _>("size")?.max(0) as u64,
        downloaded_at: row.try_get("downloaded_at")?,
    }))
}

/// Stream the body to `dest`, hashing as it goes, and report progress.
///
/// The hash is fed from the same buffer that is written, in order, so it costs
/// one pass over bytes that were being copied anyway — no second read of the
/// finished file.
async fn stream_to_file(
    app: &AppHandle,
    remote_track_id: &str,
    url: &str,
    dest: &Path,
) -> AppResult<(u64, String)> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|err| AppError::Other(format!("download client: {err}")))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| AppError::Other(format!("download: {err}")))?;
    if !response.status().is_success() {
        return Err(AppError::Other(format!(
            "download refused: HTTP {}",
            response.status()
        )));
    }
    let total = response.content_length();

    let mut file = std::fs::File::create(dest)?;
    let mut hasher = blake3::Hasher::new();
    let mut received: u64 = 0;
    let mut last_emit: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| AppError::Other(format!("download: {err}")))?
    {
        file.write_all(&chunk)?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        // One event per megabyte, not per chunk: a 40 MB file would otherwise
        // cross the IPC boundary a few thousand times to move one bar.
        if received - last_emit >= 1024 * 1024 {
            last_emit = received;
            let _ = app.emit(
                "remote:download-progress",
                DownloadProgress {
                    track_id: remote_track_id.to_string(),
                    received,
                    total,
                },
            );
        }
    }
    // Durable before the caller renames it: a rename publishes the name, and
    // whatever finds that name must find the bytes.
    file.sync_all()?;
    drop(file);

    if received == 0 {
        return Err(AppError::Other("download: server sent no audio".into()));
    }
    if let Some(total) = total {
        if received != total {
            return Err(AppError::Other(format!(
                "download: {received} bytes received, {total} declared"
            )));
        }
    }
    Ok((received, hasher.finalize().to_hex().to_string()))
}

/// File name for an offline copy.
///
/// Hashed, like the stream cache's, because a server track id is opaque and
/// may hold anything a path cannot. The extension stays legible so the decoder
/// can hint its probe from it.
fn file_name(remote_track_id: &str, extension: &str) -> String {
    let key = blake3::hash(remote_track_id.as_bytes())
        .to_hex()
        .to_string();
    let ext: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    if ext.is_empty() {
        format!("{key}.bin")
    } else {
        format!("{key}.{ext}")
    }
}

/// What the catalogue says this track's bytes hash to, when it knows.
///
/// `None` for a track mirrored before the column was populated, which is a
/// reason to accept the download rather than to refuse it: an unverifiable
/// copy is still the copy the server just sent.
async fn catalogue_hash(pool: &SqlitePool, remote_track_id: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT full_hash FROM remote_track WHERE remote_id = ?",
    )
    .bind(remote_track_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .filter(|hash| hash.len() == 64)
}

/// The container the server holds this track in, for the file name.
async fn suffix_for(pool: &SqlitePool, remote_track_id: &str) -> String {
    sqlx::query_scalar::<_, Option<String>>("SELECT suffix FROM remote_track WHERE remote_id = ?")
        .bind(remote_track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_track_id_never_reaches_the_path() {
        let name = file_name("../../etc/passwd", "flac");
        assert!(!name.contains('/'), "{name}");
        assert!(name.ends_with(".flac"), "{name}");
    }

    #[test]
    fn a_hostile_extension_cannot_escape_either() {
        let name = file_name("t1", "../../sh");
        assert!(!name.contains('/'), "{name}");
        assert!(name.ends_with(".sh"), "{name}");
        assert!(file_name("t1", "").ends_with(".bin"));
    }

    #[test]
    fn two_tracks_never_share_a_file() {
        assert_ne!(file_name("t1", "flac"), file_name("t2", "flac"));
        // Same track, same name: asking twice must land on the copy that is
        // already there rather than beside it.
        assert_eq!(file_name("t1", "flac"), file_name("t1", "flac"));
    }
}
