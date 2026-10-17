//! Replace a track's file with a better copy of the same recording,
//! keeping the track.
//!
//! A re-rip or a lossless copy dropped into the library arrives under a
//! new path, so the scanner makes it a new track — and the old one keeps
//! the plays, the likes and the playlists. This swaps the file under the
//! existing row instead: the track id, and everything hanging off it,
//! stays.
//!
//! The new file takes the old one's place — same folder, same name, its
//! own extension — and the old file goes to the system's trash. The
//! order is chosen so that every step before the last can be undone:
//!
//! 1. copy the new file into the folder's reserved `.waveflow/` directory
//!    (same volume, so the moves below are renames; invisible to the
//!    scanner and the watcher);
//! 2. move the old file aside, into `.waveflow/` as well;
//! 3. point the row at the target — **before** the file is there, so a
//!    scan running meanwhile finds a row for it rather than an unknown
//!    file to add as a new track — then rename the copy into place;
//! 4. rescan the folder, which re-reads the new file's tags and format
//!    into the same row;
//! 5. only once the row holds the new file, send the old one to the trash.
//!
//! A failure before step 5 puts the old file back and the row with it.
//!
//! **The better copy may already be in the library**, scanned as a track
//! of its own: the usual case is a FLAC downloaded next to the MP3, under
//! the very name the replacement would take. It is then adopted where it
//! is — no copy — and its own row, a few minutes old, is removed: the
//! track with the history takes its file.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// What a file is, as the confirmation shows it.
#[derive(Debug, Clone, Serialize)]
pub struct FileFacts {
    pub path: String,
    pub codec: Option<String>,
    pub duration_ms: i64,
    pub sample_rate: Option<i64>,
    pub bit_depth: Option<i64>,
}

/// Why a replacement cannot go ahead.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ReplaceRefusal {
    /// The track is playing: its file is open, and swapping it under the
    /// player is never what was asked.
    Playing,
    /// The chosen file is the track's own.
    SameFile,
    /// Not a format the library takes, or one the player cannot decode.
    NotAudio,
    /// The lengths differ by more than a re-encode explains: probably
    /// another recording, or another edit of it.
    DurationMismatch {
        current_ms: i64,
        replacement_ms: i64,
    },
    /// Another file already has the name the replacement would take.
    TargetTaken { path: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ReplacementCheck {
    pub current: FileFacts,
    pub replacement: Option<FileFacts>,
    /// Where the new file would land: the old one's folder and name, with
    /// the new file's extension.
    pub target_path: String,
    /// The chosen file already sits at the target (it is the old file's
    /// namesake in the same folder): it is adopted in place, and the track
    /// the scanner made of it — with its own short history — is removed;
    /// this track takes its file.
    pub adopts: bool,
    pub refusal: Option<ReplaceRefusal>,
}

/// What became of the old file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum OldFileFate {
    Trashed,
    /// The trash refused it; it is still under `.waveflow/`, at `path`.
    KeptAside {
        path: String,
        message: String,
    },
}

/// What a confirmed replacement did. `Refused` when a check that passed
/// for the confirmation no longer does (the track started playing, the
/// target name appeared), with the same reasons the dialog shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ReplaceOutcome {
    Replaced {
        file_path: String,
        old_file: OldFileFate,
    },
    Refused {
        refusal: ReplaceRefusal,
    },
}

/// How far two lengths may differ and still be one recording: encoder
/// padding and a lossy format's frame rounding move a length by tens of
/// milliseconds, a different edit by seconds.
fn same_length(current_ms: i64, replacement_ms: i64) -> bool {
    let tolerance = (current_ms / 100).max(2000);
    (current_ms - replacement_ms).abs() <= tolerance
}

struct TrackFile {
    path: PathBuf,
    folder_id: i64,
    facts: FileFacts,
}

async fn track_file(pool: &sqlx::SqlitePool, track_id: i64) -> AppResult<TrackFile> {
    #[allow(clippy::type_complexity)]
    let row: Option<(String, i64, Option<String>, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT file_path, folder_id, codec, duration_ms, sample_rate, bit_depth
               FROM track WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await?;
    let (path, folder_id, codec, duration_ms, sample_rate, bit_depth) =
        row.ok_or_else(|| AppError::Other(format!("track {track_id} not found")))?;
    Ok(TrackFile {
        path: PathBuf::from(&path),
        folder_id,
        facts: FileFacts {
            path,
            codec,
            duration_ms,
            sample_rate,
            bit_depth,
        },
    })
}

/// The old file's folder and name with the new file's extension.
fn target_for(current: &Path, replacement: &Path) -> PathBuf {
    let ext = replacement
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    current.with_extension(ext)
}

/// Whether two paths name one file, case aside on the systems that
/// ignore it. Compared by identity when both exist, by spelling when not.
fn same_path(a: &Path, b: &Path) -> bool {
    if let Ok(same) = same_file::is_same_file(a, b) {
        return same;
    }
    if cfg!(any(windows, target_os = "macos")) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// Everything that can be known without touching a file, for the
/// confirmation and again right before the swap.
async fn check(
    pool: &sqlx::SqlitePool,
    engine: &crate::audio::AudioEngine,
    track_id: i64,
    source: &Path,
) -> AppResult<(TrackFile, ReplacementCheck)> {
    let current = track_file(pool, track_id).await?;
    let named = target_for(&current.path, source);
    // Everything that touches the disk, off the async runtime: on a
    // network share each of these can take seconds.
    let (probed, target_exists, target_is_current, source_is_current, adopts) = {
        let (source, target, current_path) =
            (source.to_path_buf(), named.clone(), current.path.clone());
        tokio::task::spawn_blocking(move || {
            (
                waveflow_core::scanner::extract::probe_playable_stream(&source),
                target.exists(),
                same_path(&target, &current_path),
                same_path(&source, &current_path),
                same_path(&source, &target),
            )
        })
        .await
        .map_err(|e| AppError::Other(format!("probe task failed: {e}")))?
    };
    // Adopting, the chosen file's exact path is the target: on a system
    // that ignores case, `Song.FLAC` and the `Song.flac` the name rule
    // spells are one file, and the row must keep the spelling the
    // scanner stored, or two rows would point at it.
    // The folder as the scanner stored it, with the chosen file's name: a
    // dialog may hand back another spelling of the same folder.
    let target = if adopts {
        match (current.path.parent(), source.file_name()) {
            (Some(folder), Some(name)) => folder.join(name),
            _ => source.to_path_buf(),
        }
    } else {
        named
    };
    let replacement = probed.map(|info| FileFacts {
        path: source.to_string_lossy().to_string(),
        codec: info.codec,
        duration_ms: info.duration_ms,
        sample_rate: info.sample_rate,
        bit_depth: info.bit_depth,
    });

    let ext_ok = source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            waveflow_core::scanner::extract::AUDIO_EXTENSIONS
                .contains(&e.to_ascii_lowercase().as_str())
        });
    // The adopted file's own track counts too: it is the one whose file
    // is open if that is what is playing.
    let now_playing = engine
        .shared()
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire);
    let adopted_id: Option<i64> = if adopts {
        sqlx::query_scalar("SELECT id FROM track WHERE file_path = ? AND id <> ?")
            .bind(target.to_string_lossy().to_string())
            .bind(track_id)
            .fetch_optional(pool)
            .await?
    } else {
        None
    };
    let playing = now_playing == track_id || adopted_id == Some(now_playing);
    // Taken by someone else's file; the chosen file itself sitting there
    // is the adoption case, not a collision.
    let target_taken = !target_is_current
        && !adopts
        && (target_exists || path_has_row(pool, &target, track_id).await?);

    let refusal = if playing {
        Some(ReplaceRefusal::Playing)
    } else if source_is_current {
        Some(ReplaceRefusal::SameFile)
    } else if !ext_ok || replacement.is_none() {
        Some(ReplaceRefusal::NotAudio)
    } else if let Some(r) = replacement
        .as_ref()
        .filter(|r| !same_length(current.facts.duration_ms, r.duration_ms))
    {
        Some(ReplaceRefusal::DurationMismatch {
            current_ms: current.facts.duration_ms,
            replacement_ms: r.duration_ms,
        })
    } else if target_taken {
        Some(ReplaceRefusal::TargetTaken {
            path: target.to_string_lossy().to_string(),
        })
    } else {
        None
    };
    let report = ReplacementCheck {
        current: current.facts.clone(),
        replacement,
        target_path: target.to_string_lossy().to_string(),
        adopts,
        refusal,
    };
    Ok((current, report))
}

/// Whether another track's row already holds `path` (a file gone from
/// disk keeps its row, and the path is unique).
async fn path_has_row(pool: &sqlx::SqlitePool, path: &Path, track_id: i64) -> AppResult<bool> {
    let other: Option<i64> =
        sqlx::query_scalar("SELECT id FROM track WHERE file_path = ? AND id <> ?")
            .bind(path.to_string_lossy().to_string())
            .bind(track_id)
            .fetch_optional(pool)
            .await?;
    Ok(other.is_some())
}

/// What a replacement would do, without doing it.
#[tauri::command]
pub async fn check_track_replacement(
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, std::sync::Arc<crate::audio::AudioEngine>>,
    track_id: i64,
    source_path: String,
) -> AppResult<ReplacementCheck> {
    let pool = state.require_profile_pool().await?;
    let (_, report) = check(&pool, &engine, track_id, Path::new(&source_path)).await?;
    Ok(report)
}

/// Swap the track's file for `source_path`. See the module documentation
/// for the order and why each step can be undone.
#[tauri::command]
pub async fn replace_track_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, std::sync::Arc<crate::audio::AudioEngine>>,
    track_id: i64,
    source_path: String,
) -> AppResult<ReplaceOutcome> {
    use tauri::Emitter;
    let (pool, profile_id) = state.require_profile_snapshot().await?;
    let source = PathBuf::from(&source_path);

    // Let a running scan finish first: it reads the folder and the rows
    // this is about to change. The watcher may still start one after this,
    // which step 3's order is for.
    let waited = std::time::Instant::now();
    while crate::commands::scan::scan_in_flight() {
        if waited.elapsed() > SCAN_WAIT {
            return Err(AppError::Other(
                "a library scan is running; try again when it is done".into(),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    // Checked again here, after the wait, not trusted from the
    // confirmation: the track may have started playing, or the target
    // appeared, since.
    let (current, report) = check(&pool, &engine, track_id, &source).await?;
    if let Some(refusal) = report.refusal {
        return Ok(ReplaceOutcome::Refused { refusal });
    }
    let target = PathBuf::from(&report.target_path);
    let old = current.path.clone();
    let adopts = report.adopts;
    let artwork_dir = state.paths.profile_artwork_dir(profile_id);

    // 1-2, on a blocking thread: they touch the disk, maybe a network one.
    let swap = {
        let (source, old, target) = (source.clone(), old.clone(), target.clone());
        tokio::task::spawn_blocking(move || prepare(&source, &old, &target, adopts))
            .await
            .map_err(|e| AppError::Other(format!("replace task failed: {e}")))?
    }
    .map_err(AppError::Other)?;

    // 3a. The row first. When adopting, the chosen file's own row gives way
    // in the same transaction. `file_modified = 0` makes the scan re-read
    // the file even if the copy kept the old one's timestamp.
    let pointed = point_row(&pool, track_id, &target, adopts).await;
    if let Err(err) = pointed {
        return Err(AppError::Other(with_undo(
            format!("could not point the track at the new file: {err}"),
            undo_off_thread(&swap, &old, &target, false).await,
        )));
    }
    // 3b. Then the file.
    let placed = {
        let (swap, target) = (swap.clone(), target.clone());
        tokio::task::spawn_blocking(move || place(&swap, &target))
            .await
            .map_err(|e| AppError::Other(format!("replace task failed: {e}")))?
    };
    if let Err(err) = placed {
        let repointed = repoint(&pool, track_id, &old).await;
        // Not placed: whatever is at the target is not our copy.
        return Err(AppError::Other(with_undo(
            with_repoint(err, repointed),
            undo_off_thread(&swap, &old, &target, false).await,
        )));
    }

    // 4. The scan keys rows by path, so it updates this one in place.
    let scanned = crate::commands::scan::scan_folder_inner(
        &pool,
        &artwork_dir,
        current.folder_id,
        Some(&app),
        false,
    )
    .await;
    let indexed: Option<i64> = match sqlx::query_scalar(
        // `file_modified <> 0`: the row was pointed here with 0, and only
        // a scan that actually re-read the file writes its real time back.
        // Path and availability alone would also pass for a scan that
        // skipped it.
        "SELECT id FROM track
          WHERE id = ? AND file_path = ? AND is_available = 1 AND file_modified <> 0",
    )
    .bind(track_id)
    .bind(target.to_string_lossy().to_string())
    .fetch_optional(&*pool)
    .await
    {
        Ok(found) => found,
        // Not knowing is not "not indexed": the swap may well have
        // worked, and undoing it on a failed read would be the worse
        // guess. Stop here and say where the old file waits.
        Err(err) => {
            return Err(AppError::Other(format!(
                "could not confirm the replacement ({err}); the old file is kept at {}",
                swap.aside.display()
            )));
        }
    };
    if scanned.is_err() || indexed.is_none() {
        // Put everything back: the file, the row, and a scan to re-read it.
        // An adopted file stays where the user put it, and the scan lists
        // it again as the track of its own it was.
        // The row first, as on the way in: a scan in between then finds
        // the row waiting for the old file rather than a file with none.
        let repointed = repoint(&pool, track_id, &old).await;
        let undone = undo_off_thread(&swap, &old, &target, true).await;
        let _ = crate::commands::scan::scan_folder_inner(
            &pool,
            &artwork_dir,
            current.folder_id,
            Some(&app),
            false,
        )
        .await;
        let message = with_repoint("the library did not take the new file".into(), repointed);
        return Err(AppError::Other(match undone {
            Ok(()) if !message.contains(';') => format!("{message}; the old one is back in place"),
            other => with_undo(message, other),
        }));
    }

    // The old measurements describe the old file — a fake-lossless verdict
    // above all. Dropped only now that the new file is in: a rollback
    // above keeps them. The sweep measures the new one.
    match sqlx::query("DELETE FROM track_analysis WHERE track_id = ?")
        .bind(track_id)
        .execute(&*pool)
        .await
    {
        // With automatic analysis on, the new file is measured now rather
        // than at the next startup.
        Ok(_) => crate::commands::analysis::maybe_auto_analyze(&app),
        // The swap stands: stale measurements are re-taken by the next
        // analysis of the track, a lost file would not be.
        Err(err) => {
            tracing::warn!(track_id, %err, "could not drop the replaced file's analysis");
        }
    }

    // 5. Only now does the old file leave.
    let aside = swap.aside.clone();
    let trashed = crate::trash::on_trash_thread(move || crate::trash::move_one(&aside))
        .await
        .and_then(|outcome| outcome);
    let old_file = match trashed {
        Ok(()) => OldFileFate::Trashed,
        Err(message) => {
            tracing::warn!(path = %swap.aside.display(), %message, "replaced file kept aside");
            OldFileFate::KeptAside {
                path: swap.aside.to_string_lossy().to_string(),
                message,
            }
        }
    };
    {
        let swap = swap.clone();
        let _ = tokio::task::spawn_blocking(move || swap.tidy()).await;
    }

    let _ = app.emit("track:updated", track_id);
    let _ = app.emit("library:rescanned", ());
    Ok(ReplaceOutcome::Replaced {
        file_path: target.to_string_lossy().to_string(),
        old_file,
    })
}

/// How long a replacement waits for a running scan before giving up.
const SCAN_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Step 3a: the track's row takes the target path; when adopting, the
/// row the scanner made of the chosen file goes first, in one transaction.
async fn point_row(
    pool: &sqlx::SqlitePool,
    track_id: i64,
    target: &Path,
    adopts: bool,
) -> Result<(), sqlx::Error> {
    let target = target.to_string_lossy().to_string();
    let mut tx = pool.begin().await?;
    if adopts {
        sqlx::query("DELETE FROM track WHERE file_path = ? AND id <> ?")
            .bind(&target)
            .bind(track_id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE track SET file_path = ?, file_modified = 0 WHERE id = ?")
        .bind(&target)
        .bind(track_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// The row back on the old file, for a rollback.
async fn repoint(pool: &sqlx::SqlitePool, track_id: i64, old: &Path) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE track SET file_path = ?, file_modified = 0 WHERE id = ?")
        .bind(old.to_string_lossy().to_string())
        .bind(track_id)
        .execute(pool)
        .await
        .map(|_| ())
}

fn with_repoint(message: String, repointed: Result<(), sqlx::Error>) -> String {
    match repointed {
        Ok(()) => message,
        Err(err) => format!("{message}; the track could not be pointed back: {err}"),
    }
}

fn with_undo(message: String, undone: Result<(), String>) -> String {
    match undone {
        Ok(()) => message,
        Err(undo) => format!("{message}; {undo}"),
    }
}

/// The files a swap moved, for undoing it or tidying after it.
#[derive(Clone)]
struct Swap {
    /// Where the old file waits until the new one is indexed.
    aside: PathBuf,
    /// The copy waiting to be renamed into place; `None` when the chosen
    /// file is adopted where it already is.
    staged: Option<PathBuf>,
    /// The reserved directories created on the way, removed if empty.
    dirs: Vec<PathBuf>,
}

impl Swap {
    fn tidy(&self) {
        for dir in &self.dirs {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Steps 1-2: stage the copy (unless adopting) and move the old file
/// aside. A failure leaves nothing changed.
fn prepare(source: &Path, old: &Path, target: &Path, adopts: bool) -> Result<Swap, String> {
    let folder = old
        .parent()
        .ok_or_else(|| "the track's file has no folder".to_string())?;
    let reserved = folder.join(waveflow_core::scanner::RESERVED_DIR_NAME);
    let aside_dir = reserved.join("replaced");
    std::fs::create_dir_all(&aside_dir)
        .map_err(|e| format!("create {}: {e}", aside_dir.display()))?;
    let mut dirs = vec![aside_dir.clone()];

    let id = uuid::Uuid::new_v4();
    let aside = aside_dir.join(format!(
        "{id}-{}",
        old.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    ));
    let tidy = |dirs: &[PathBuf]| {
        for dir in dirs {
            let _ = std::fs::remove_dir(dir);
        }
    };

    let staged = if adopts {
        None
    } else {
        let staging_dir = reserved.join("replacing");
        if let Err(e) = std::fs::create_dir_all(&staging_dir) {
            tidy(&dirs);
            return Err(format!("create {}: {e}", staging_dir.display()));
        }
        dirs.insert(0, staging_dir.clone());
        let staged = staging_dir.join(format!(
            "{id}.{}",
            target
                .extension()
                .map(|e| e.to_string_lossy())
                .unwrap_or_default()
        ));
        if let Err(e) = std::fs::copy(source, &staged) {
            let _ = std::fs::remove_file(&staged);
            tidy(&dirs);
            return Err(format!("copy the new file: {e}"));
        }
        Some(staged)
    };
    if let Err(e) = std::fs::rename(old, &aside) {
        if let Some(staged) = &staged {
            let _ = std::fs::remove_file(staged);
        }
        tidy(&dirs);
        return Err(format!("move the old file aside: {e}"));
    }
    Ok(Swap {
        aside,
        staged,
        dirs,
    })
}

/// Step 3b: rename the staged copy into place. Nothing to do when the
/// chosen file is adopted where it is. Leaves undoing to [`undo`].
fn place(swap: &Swap, target: &Path) -> Result<(), String> {
    let Some(staged) = &swap.staged else {
        return Ok(());
    };
    // A rename replaces what it lands on. The old file is aside by now, so
    // anything at the target is a file that appeared since the check.
    if target.exists() {
        return Err(format!("{} appeared in the meantime", target.display()));
    }
    std::fs::rename(staged, target).map_err(|e| format!("put the new file in place: {e}"))
}

/// [`undo`] on a blocking thread: it touches the disk, maybe a network one.
async fn undo_off_thread(
    swap: &Swap,
    old: &Path,
    target: &Path,
    placed: bool,
) -> Result<(), String> {
    let (swap, old, target) = (swap.clone(), old.to_path_buf(), target.to_path_buf());
    tokio::task::spawn_blocking(move || undo(&swap, &old, &target, placed))
        .await
        .unwrap_or_else(|e| Err(format!("undo task failed: {e}")))
}

/// Undo [`prepare`] and [`place`]: our copy goes, wherever it got to (an
/// adopted file is the user's and stays), and the old file comes back.
/// `placed` says whether [`place`] put the copy at the target: only then
/// is the file there ours to remove — one that appeared meanwhile is not.
///
/// Every step is tried whatever the others do, and a failure says where
/// the old file is left, so no caller reports a restore that did not
/// happen.
fn undo(swap: &Swap, old: &Path, target: &Path, placed: bool) -> Result<(), String> {
    let mut leftovers = Vec::new();
    if let Some(staged) = &swap.staged {
        // Still staged, or renamed to the target — then our copy, at the
        // old path itself when the format did not change.
        let copy = if placed { target } else { staged.as_path() };
        {
            match std::fs::remove_file(copy) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    leftovers.push(format!(
                        "the new copy could not be removed ({e}) and is at {}",
                        copy.display()
                    ));
                }
                _ => {}
            }
        }
    }
    // A rename replaces what it lands on: a file that appeared at the old
    // path meanwhile is not overwritten, and the old file stays aside.
    let restored = if old.exists() {
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} appeared in the meantime", old.display()),
        ))
    } else {
        std::fs::rename(&swap.aside, old)
    };
    swap.tidy();
    match restored {
        Ok(()) if leftovers.is_empty() => Ok(()),
        Ok(()) => Err(leftovers.join("; ")),
        Err(e) => {
            leftovers.insert(
                0,
                format!(
                    "the old file could not be put back ({e}); it is at {}",
                    swap.aside.display()
                ),
            );
            Err(leftovers.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reencode_is_the_same_length_and_an_edit_is_not() {
        assert!(same_length(240_000, 240_050));
        assert!(same_length(240_000, 238_100));
        // 1% of a long track is more than the 2 s floor.
        assert!(same_length(1_200_000, 1_211_000));
        assert!(!same_length(240_000, 250_000));
        assert!(!same_length(240_000, 236_000));
    }

    #[test]
    fn the_new_file_takes_the_old_name_with_its_own_extension() {
        let target = target_for(Path::new("/m/a/01 Song.mp3"), Path::new("/dl/x.FLAC"));
        assert_eq!(target, Path::new("/m/a/01 Song.flac"));
    }

    /// The swap puts the new bytes at the target and the old file aside,
    /// and the undo restores the old file exactly.
    #[test]
    fn a_swap_can_be_undone() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.mp3");
        let source = dir.path().join("better.flac");
        std::fs::write(&old, b"old bytes").unwrap();
        std::fs::write(&source, b"new bytes").unwrap();
        let target = target_for(&old, &source);

        let swap = prepare(&source, &old, &target, false).unwrap();
        place(&swap, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new bytes");
        assert!(!old.exists());
        assert_eq!(std::fs::read(&swap.aside).unwrap(), b"old bytes");
        assert!(source.exists(), "the user's file is copied, never moved");

        undo(&swap, &old, &target, true).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"old bytes");
        assert!(!target.exists());
        assert!(!dir.path().join(".waveflow").join("replaced").exists());
    }

    /// A file that appeared at the target after the check is not
    /// overwritten, and the old file stays where it was.
    #[test]
    fn a_target_that_appeared_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.mp3");
        let source = dir.path().join("better.flac");
        let target = target_for(&old, &source);
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&source, b"new").unwrap();
        std::fs::write(&target, b"someone else's").unwrap();

        let swap = prepare(&source, &old, &target, false).unwrap();
        assert!(place(&swap, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"someone else's");
        undo(&swap, &old, &target, false).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
        assert_eq!(std::fs::read(&target).unwrap(), b"someone else's");
    }

    /// Undoing never overwrites a file that appeared at the old path: the
    /// old file then stays aside, and the error says where.
    #[test]
    fn an_undo_does_not_overwrite_a_file_that_appeared() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.flac");
        let source = dir.path().join("rerip.flac");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&source, b"new").unwrap();
        let target = target_for(&old, &source);
        assert_eq!(target, old);

        let swap = prepare(&source, &old, &target, false).unwrap();
        std::fs::write(&old, b"someone else's").unwrap();
        let err = undo(&swap, &old, &target, false).unwrap_err();
        assert!(err.contains(&swap.aside.display().to_string()), "{err}");
        assert_eq!(std::fs::read(&old).unwrap(), b"someone else's");
        assert_eq!(std::fs::read(&swap.aside).unwrap(), b"old");
    }

    /// Same extension: the new file lands on the old one's own path.
    #[test]
    fn a_same_format_swap_reuses_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.flac");
        let source = dir.path().join("rerip.flac");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&source, b"new").unwrap();
        let target = target_for(&old, &source);
        assert_eq!(target, old);

        let swap = prepare(&source, &old, &target, false).unwrap();
        place(&swap, &target).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"new");
        undo(&swap, &old, &target, true).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
    }

    /// The better copy already at the target is adopted where it is: no
    /// copy, and an undo leaves the user's file alone.
    #[test]
    fn an_adopted_file_is_never_copied_nor_removed() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("song.mp3");
        let source = dir.path().join("song.flac");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&source, b"new").unwrap();
        let target = target_for(&old, &source);
        assert!(same_path(&source, &target));

        let swap = prepare(&source, &old, &target, true).unwrap();
        assert!(swap.staged.is_none());
        place(&swap, &target).unwrap();
        assert!(!old.exists());
        undo(&swap, &old, &target, true).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"old");
        assert_eq!(std::fs::read(&source).unwrap(), b"new");
    }
}
