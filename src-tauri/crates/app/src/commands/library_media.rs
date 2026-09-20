//! Keeping a track's own media next to the music (issue #695).
//!
//! A Canvas clip or an animated cover normally lives in the app's data
//! folder. A user who asks for it can have the ones they set **by hand**
//! written into the library instead, under the directory the scanner
//! reserves ([`waveflow_core::scanner::RESERVED_DIR_NAME`]) — so the clip
//! travels with the album, survives a reinstall, and is on the drive the
//! music is on rather than the system one.
//!
//! **Only the ones set by hand.** The plugin caches (`canvas_cache/`,
//! `motion_cache/`) stay in the app folder on purpose: they are LRU, and
//! an eviction pass deleting files out of somebody's music folder is not
//! a behaviour worth having. What the user chose is theirs and is never
//! evicted; what a plugin fetched is a cache and should keep behaving
//! like one.
//!
//! **Where a clip is read from is decided by the disk, not by a column.**
//! Both locations are hash-addressed, so the reader looks for
//! `<hash>.<format>` in the library first and falls back to the profile
//! directory. That keeps the setting to what it actually governs — where
//! the *next* write goes — leaves the existing rows untouched, and lets a
//! user who copies their library to another machine keep their clips
//! without anything in the database having to agree.

use std::path::{Path, PathBuf};

use tauri::Manager;

use crate::error::AppResult;
use crate::state::AppState;

/// `app_setting` key: keep hand-set clips in the library folder rather
/// than in the app's data directory. App-wide rather than per-profile,
/// like the lyrics destination and for the same reason — it drives
/// filesystem state two profiles scanning the same folder share.
pub const CLIPS_IN_LIBRARY_KEY: &str = "media.clips_in_library";

/// Which kind of media a directory holds, and the subdirectory it takes
/// inside the reserved one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// Per-track Canvas clips (issue #442).
    Canvas,
    /// Per-album animated covers (issue #408).
    Motion,
}

impl MediaKind {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Canvas => "canvas",
            Self::Motion => "motion",
        }
    }
}

/// Read the preference. A missing or unparseable row is `false`, which is
/// where every clip lived before this existed.
pub async fn clips_in_library(app_db: &sqlx::SqlitePool) -> bool {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(CLIPS_IN_LIBRARY_KEY)
        .fetch_optional(app_db)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(?err, "reading the clip location preference failed");
            None
        });
    matches!(raw.as_deref(), Some("true") | Some("1"))
}

/// The directory a track's media may live in, inside the library folder
/// that track was scanned from: `<folder>/.waveflow/<kind>/`.
///
/// `None` when the track has no folder — `track.folder_id` is nullable
/// and a row can outlive the folder it came from — in which case the
/// caller keeps the app's own directory, which always exists.
pub async fn track_media_dir(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    kind: MediaKind,
) -> Option<PathBuf> {
    let row = sqlx::query_scalar::<_, String>(
        "SELECT lf.path FROM track t
           JOIN library_folder lf ON lf.id = t.folder_id
          WHERE t.id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await;
    let folder = match row {
        Ok(found) => found?,
        Err(err) => {
            // Distinct from a track with no folder, and worth saying so:
            // one is a shape the library holds, the other is a failure.
            tracing::warn!(
                ?err,
                track_id,
                "looking up the track's library folder failed"
            );
            return None;
        }
    };
    Some(media_dir_in(Path::new(&folder), kind))
}

/// Every directory an album's media may live in: the reserved directory
/// of each library folder its tracks were scanned from.
///
/// **All of them, not the first.** An album can span folders — a box set
/// split by disc, a compilation assembled from two drives — and which
/// track comes first can change when the library is rescanned. Reading
/// from every candidate means a cover written today is still found after
/// a track is added or removed; writing takes the first, which is only a
/// question of where it lands.
pub async fn album_media_dirs(
    pool: &sqlx::SqlitePool,
    album_id: i64,
    kind: MediaKind,
) -> Vec<PathBuf> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT lf.path FROM track t
           JOIN library_folder lf ON lf.id = t.folder_id
          WHERE t.album_id = ?
          ORDER BY lf.path",
    )
    .bind(album_id)
    .fetch_all(pool)
    .await;
    match rows {
        Ok(folders) => folders
            .iter()
            .map(|folder| media_dir_in(Path::new(folder), kind))
            .collect(),
        Err(err) => {
            tracing::warn!(?err, album_id, "looking up the album's folders failed");
            Vec::new()
        }
    }
}

/// `<folder>/.waveflow/<kind>/`, without touching the disk.
pub fn media_dir_in(folder: &Path, kind: MediaKind) -> PathBuf {
    folder
        .join(waveflow_core::scanner::RESERVED_DIR_NAME)
        .join(kind.dir_name())
}

/// The first of `candidates` that actually holds `<hash>.<format>`.
///
/// This is the whole of the "where does this clip live" logic: a stat per
/// candidate, no bookkeeping to keep in step with the disk. The library
/// comes first so a clip the user moved there wins over a leftover copy
/// in the profile directory.
pub fn existing_media_file(candidates: &[PathBuf], hash: &str, format: &str) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|dir| dir.join(format!("{hash}.{format}")))
        .find(|path| path.exists())
}

/// [`existing_media_file`], off the async runtime.
///
/// A `stat` is a syscall, and one of these candidates can be a network
/// share or a drive that has spun down — where it costs hundreds of
/// milliseconds of a runtime worker, on a path that runs at every track
/// change. A join failure answers `None`, which is the same answer as a
/// clip that is not there: the surface falls back to the static cover.
pub async fn find_media_file(
    candidates: Vec<PathBuf>,
    hash: String,
    format: String,
) -> Option<PathBuf> {
    tokio::task::spawn_blocking(move || existing_media_file(&candidates, &hash, &format))
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(?err, "media lookup task failed");
            None
        })
}

/// Let the webview load files from `dir`.
///
/// The asset-protocol scope in `tauri.conf.json` is static and names the
/// app's own directories; a clip inside somebody's music folder is not
/// covered by it and the `<video>` would simply fail to load. The grant
/// is per directory and additive, so it is made whenever a path from
/// there is handed out rather than once at startup — library folders can
/// be added while the app runs.
pub fn allow_asset_scope(app: &tauri::AppHandle, dir: &Path) {
    if let Err(err) = app.asset_protocol_scope().allow_directory(dir, true) {
        tracing::warn!(
            ?err,
            dir = %dir.display(),
            "could not open the asset scope for a library media directory"
        );
    }
}

/// Where a hand-set clip should be written now: the track's library
/// folder when the user asked for that and the directory can be created,
/// otherwise the app's own.
///
/// **A library that refuses the write does not lose the file.** A folder
/// on a read-only mount, a share that went away, a permission the user
/// does not have: the clip lands in the app directory instead and a line
/// says why. Returning an error here would mean the user's chosen file
/// simply did not get set, which is a worse answer than "it was kept
/// somewhere else".
pub async fn write_dir_for(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    profile_dir: PathBuf,
    track_id: i64,
    kind: MediaKind,
) -> PathBuf {
    let candidate = if clips_in_library(&state.app_db).await {
        track_media_dir(pool, track_id, kind).await
    } else {
        None
    };
    prepare_or_fall_back(candidate, profile_dir).await
}

/// Same, for an album's animated cover: the first of its folders, since
/// the read looks in all of them.
pub async fn album_write_dir_for(
    state: &AppState,
    pool: &sqlx::SqlitePool,
    profile_dir: PathBuf,
    album_id: i64,
    kind: MediaKind,
) -> PathBuf {
    let candidate = if clips_in_library(&state.app_db).await {
        album_media_dirs(pool, album_id, kind)
            .await
            .into_iter()
            .next()
    } else {
        None
    };
    prepare_or_fall_back(candidate, profile_dir).await
}

/// Creating a directory is a syscall too, and the directory in question
/// can be on the far end of a mount that is no longer there — where it
/// does not fail quickly. A task that could not run at all is treated
/// like a folder that refused: the clip goes to the app directory, which
/// is the whole point of the fallback.
async fn prepare_or_fall_back(candidate: Option<PathBuf>, profile_dir: PathBuf) -> PathBuf {
    let Some(dir) = candidate else {
        return profile_dir;
    };
    let created = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || std::fs::create_dir_all(&dir)
    })
    .await;
    match created {
        Ok(Ok(())) => dir,
        Ok(Err(err)) => {
            tracing::warn!(
                ?err,
                dir = %dir.display(),
                "library folder refused the media directory; keeping the clip in the app folder"
            );
            profile_dir
        }
        Err(err) => {
            tracing::warn!(?err, "media directory task failed");
            profile_dir
        }
    }
}

/// Store a clip, falling back to the app's own directory when the
/// library folder will not take it.
///
/// Creating the directory succeeding says nothing about the write: a full
/// drive, a share that drops in between, a folder that allows `mkdir` and
/// refuses a file. Without this the user's pick would simply not take,
/// which is the outcome the fallback exists to prevent — and the doc
/// comment on [`write_dir_for`] promised otherwise.
///
/// **A source-side failure is attempted twice**, deliberately: the helper
/// reports "could not read it", "not an mp4" and "too big" the same way it
/// reports a failed write, and telling them apart would mean a richer
/// error type for one caller. Retrying costs a second read of a file that
/// is about to be refused anyway, and the second error is the one that
/// surfaces — the same message either way.
pub async fn store_mp4_with_fallback(
    primary_dir: &Path,
    profile_dir: &Path,
    file_path: &str,
    max_bytes: u64,
) -> AppResult<String> {
    let first =
        super::media_file::store_hash_addressed_mp4(primary_dir, file_path, max_bytes).await;
    match first {
        Ok(hash) => Ok(hash),
        Err(err) if primary_dir != profile_dir => {
            tracing::warn!(
                ?err,
                dir = %primary_dir.display(),
                "library folder would not take the clip; keeping it in the app folder"
            );
            super::media_file::store_hash_addressed_mp4(profile_dir, file_path, max_bytes).await
        }
        Err(err) => Err(err),
    }
}

/// Read the preference for the interface.
#[tauri::command]
pub async fn get_clips_in_library(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    Ok(clips_in_library(&state.app_db).await)
}

/// Persist the preference.
///
/// **Applies to writes from here on, and moves nothing.** It does not have
/// to: the reader looks in both places, so a clip set before the switch
/// keeps playing from where it is, and one set after lands in the new
/// place. Copying the old ones over is then something the user can do with
/// a file manager, or not at all — no half-moved state to recover from,
/// which is the failure mode a migration would have brought.
#[tauri::command]
pub async fn set_clips_in_library(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(CLIPS_IN_LIBRARY_KEY)
    .bind(if enabled { "true" } else { "false" })
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_lives_under_the_reserved_directory() {
        let dir = media_dir_in(Path::new("/music"), MediaKind::Canvas);
        assert!(dir.ends_with(Path::new(".waveflow/canvas")), "{dir:?}");
        let dir = media_dir_in(Path::new("/music"), MediaKind::Motion);
        assert!(dir.ends_with(Path::new(".waveflow/motion")), "{dir:?}");
    }

    /// The library wins over a leftover copy in the profile directory, so
    /// a user who moved their clips does not keep reading the old ones.
    #[test]
    fn the_first_candidate_that_exists_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let library = tmp.path().join("library");
        let profile = tmp.path().join("profile");
        std::fs::create_dir_all(&library).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("abc.mp4"), b"x").unwrap();

        let candidates = vec![library.clone(), profile.clone()];
        assert_eq!(
            existing_media_file(&candidates, "abc", "mp4"),
            Some(profile.join("abc.mp4"))
        );

        std::fs::write(library.join("abc.mp4"), b"x").unwrap();
        assert_eq!(
            existing_media_file(&candidates, "abc", "mp4"),
            Some(library.join("abc.mp4"))
        );
        assert_eq!(existing_media_file(&candidates, "nope", "mp4"), None);
    }
}
