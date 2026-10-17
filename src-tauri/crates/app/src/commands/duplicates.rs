//! Duplicate-track detection.
//!
//! Groups byte-identical files (in different folders, regardless of
//! metadata) so the user can prune copies. The scan-time `file_hash` is
//! a *partial* blake3 (size + head + tail, for speed) and isn't
//! distinguishable from a legacy full digest, so the command prefilters
//! candidates by byte SIZE — a format-stable field every duplicate
//! shares — then re-verifies each candidate with a full-content hash
//! before returning, since the UI lets the user trash copies from a group.
//! Identity is content-only, so renames / re-tags still group correctly
//! — but two re-encodes of the same source (e.g. CBR vs VBR rips) won't
//! match because the bytes differ. That's a fingerprinting problem and
//! out of scope for this MVP.

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DuplicateTrack {
    pub id: i64,
    pub title: String,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub file_path: String,
    pub file_size: i64,
    pub bitrate: Option<i64>,
    pub sample_rate: Option<i64>,
    pub duration_ms: i64,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    pub file_hash: String,
    pub tracks: Vec<DuplicateTrack>,
}

/// Find every set of tracks that share a `file_hash` (i.e. are
/// byte-identical copies of the same file). Returns one entry per
/// duplicate group, each containing every track in the group ordered
/// by `added_at` ASC so the oldest copy renders first — usually the
/// one the user wants to keep.
#[tauri::command]
pub async fn find_duplicates(state: tauri::State<'_, AppState>) -> AppResult<Vec<DuplicateGroup>> {
    let pool = state.require_profile_pool().await?;

    // Pull every candidate row in one round-trip. Candidates are tracks
    // that share their byte SIZE with another — a format-stable
    // prefilter that catches duplicates whether their stored hash is a
    // legacy full digest or a newer partial one (both are 64-char blake3
    // hex, indistinguishable). The full-content verification below forms
    // the real groups.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
    )> = sqlx::query_as(
        r#"
        SELECT t.id,
               t.title,
               (SELECT GROUP_CONCAT(name, ', ') FROM (
                   SELECT ar.name FROM track_artist ta
                   JOIN artist ar ON ar.id = ta.artist_id
                   WHERE ta.track_id = t.id
                   ORDER BY ta.position
               )) AS artist_name,
               al.title AS album_title,
               t.file_path,
               t.file_size,
               t.bitrate,
               t.sample_rate,
               t.duration_ms,
               t.added_at
          FROM track t
          LEFT JOIN album al ON al.id = t.album_id
         WHERE t.file_size IN (
             SELECT file_size FROM track
              WHERE is_available = 1
              GROUP BY file_size
              HAVING COUNT(*) > 1
         )
           AND t.is_available = 1
         ORDER BY t.file_size, t.added_at ASC
        "#,
    )
    .fetch_all(&*pool)
    .await?;

    let candidates: Vec<DuplicateTrack> = rows
        .into_iter()
        .map(
            |(
                id,
                title,
                artist_name,
                album_title,
                file_path,
                file_size,
                bitrate,
                sample_rate,
                duration_ms,
                added_at,
            )| DuplicateTrack {
                id,
                title,
                artist_name,
                album_title,
                file_path,
                file_size,
                bitrate,
                sample_rate,
                duration_ms,
                added_at,
            },
        )
        .collect();

    // The size prefilter is only a *candidate* set, and a delete follows,
    // so confirm byte-identity with a full-content hash — computed
    // off-thread and only on these few candidate files. This both forms
    // the real groups and closes the partial-hash middle-byte blind spot.
    let groups = tokio::task::spawn_blocking(move || verify_groups_full_content(candidates))
        .await
        .map_err(|e| AppError::Other(format!("dedup verify task failed: {e}")))?;
    Ok(groups)
}

/// Bucket the size-prefiltered candidates by a full-content hash so only
/// genuinely byte-identical files stay grouped — independent of whatever
/// (legacy full / new partial) digest is stored on each row. A file that
/// can't be read is dropped (we won't offer to delete what we can't
/// verify); buckets that collapse to a single track are not duplicates.
fn verify_groups_full_content(candidates: Vec<DuplicateTrack>) -> Vec<DuplicateGroup> {
    use std::collections::HashMap;

    let mut by_full: HashMap<String, Vec<DuplicateTrack>> = HashMap::new();
    for track in candidates {
        match waveflow_core::scanner::hash_file_full(std::path::Path::new(&track.file_path)) {
            Ok(full) => by_full.entry(full).or_default().push(track),
            Err(err) => tracing::warn!(
                path = %track.file_path,
                error = %err,
                "dedup full-content hash failed; excluding track"
            ),
        }
    }

    let mut out: Vec<DuplicateGroup> = by_full
        .into_iter()
        .filter(|(_, tracks)| tracks.len() > 1)
        .map(|(full_hash, mut tracks)| {
            // Oldest copy first — usually the one the user keeps.
            tracks.sort_by_key(|t| t.added_at);
            DuplicateGroup {
                file_hash: full_hash,
                tracks,
            }
        })
        .collect();
    // HashMap iteration order is non-deterministic; sort groups so the UI
    // renders the same order across calls.
    out.sort_by_key(|g| g.tracks.first().map(|t| t.added_at).unwrap_or(0));
    out
}

/// Why a copy stayed where it was.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "message")]
pub enum TrashRefusal {
    /// It is the track playing: on Windows its file is held open, and
    /// pulling it from under the player is never what was asked.
    Playing,
    /// The system would not move it (read-only, a share without a
    /// trash, held open elsewhere), with its reason.
    Failed(String),
    /// The copy kept for it is missing, changed, or is this very file:
    /// moving this one could leave no copy at all.
    KeptCopyGone,
    /// The file is not where its row says. Deleted by hand, or on a
    /// drive that is not plugged in: from here the two look the same, so
    /// the row stays and the next scan of its folder decides.
    Missing,
    /// The file went to the trash but its row could not be removed, so
    /// the library still lists it until a rescan of its folder notices.
    /// Reported apart: the file is no longer where the row says.
    RowKept(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct TrashFailure {
    pub track_id: i64,
    pub file_path: String,
    pub reason: TrashRefusal,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrashReport {
    /// Copies moved to the trash, their rows removed with them.
    pub trashed: i64,
    /// Copies left in place, rows included.
    pub failed: Vec<TrashFailure>,
}

/// Move duplicate copies to the system's trash, then remove their rows
/// (#878).
///
/// Removing the rows alone, as this used to, undid itself: the scanner
/// adds any audio file that has no row, so the next rescan (or the
/// folder watcher) brought every copy back as a new track, without its
/// playlists, likes or history. The files go first, and only a copy
/// this command moved loses its row, so a copy still on disk never
/// does. A row whose removal fails after its file moved stays listed
/// until the next scan, and is reported as [`TrashRefusal::RowKept`].
/// The trash is the operating system's, so a wrong pick can be restored
/// from there.
///
/// Cascading rows (`track_artist`, `playlist_track`, `play_event`, …)
/// go through the schema's `ON DELETE CASCADE`. The folder watcher sees
/// the files leave and rescans; whichever of it and this transaction
/// runs first removes the rows, and the other finds them gone.
#[tauri::command]
pub async fn move_tracks_to_trash(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    engine: tauri::State<'_, std::sync::Arc<crate::audio::AudioEngine>>,
    groups: Vec<TrashGroup>,
) -> AppResult<TrashReport> {
    use tauri::Emitter;
    let mut report = TrashReport {
        trashed: 0,
        failed: Vec::new(),
    };
    let pool = state.require_profile_pool().await?;

    let mut jobs: Vec<TrashJob> = Vec::new();
    for group in groups {
        // The kept copy's path, to check against right before each move:
        // `None` when its row is gone, which refuses the whole group.
        let keep_path = lookup_track(&pool, group.keep_id)
            .await?
            .map(|(_, path)| path);
        for id in group.trash_ids {
            if id == group.keep_id {
                continue;
            }
            // A row already gone (a scan, a second click) has nothing
            // left to move.
            let Some((library_id, file_path)) = lookup_track(&pool, id).await? else {
                continue;
            };
            jobs.push(TrashJob {
                id,
                library_id,
                file_path,
                keep_path: keep_path.clone(),
            });
        }
    }
    if jobs.is_empty() {
        return Ok(report);
    }

    let shared = std::sync::Arc::clone(engine.shared());
    let done = crate::trash::on_trash_thread(move || {
        jobs.into_iter()
            .map(|job| {
                let outcome = trash_one(&job, &shared);
                (job, outcome)
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(AppError::Other)?;

    // One transaction per file: the files are already in the trash, so a
    // row that cannot be removed must not take the others' with it.
    for (job, outcome) in done {
        let TrashJob {
            id,
            library_id,
            file_path,
            ..
        } = job;
        match outcome {
            Ok(()) => match remove_row(&pool, id, library_id, &file_path).await {
                Ok(()) => report.trashed += 1,
                Err(message) => {
                    tracing::warn!(path = %file_path, error = %message, "a trashed duplicate kept its row");
                    report.failed.push(TrashFailure {
                        track_id: id,
                        file_path,
                        reason: TrashRefusal::RowKept(message),
                    });
                }
            },
            Err(reason) => {
                tracing::warn!(path = %file_path, ?reason, "a duplicate was not moved to the trash");
                report.failed.push(TrashFailure {
                    track_id: id,
                    file_path,
                    reason,
                });
            }
        }
    }

    if report.trashed > 0 {
        // Ship the per-track delete ops now rather than at the drain's
        // next idle poll, like every other sync-emitting command.
        state.drain.notify();
        let _ = app.emit("library:rescanned", ());
    }
    Ok(report)
}

/// One copy the user chose to discard, and the copy that stays for it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TrashGroup {
    pub keep_id: i64,
    pub trash_ids: Vec<i64>,
}

struct TrashJob {
    id: i64,
    library_id: i64,
    file_path: String,
    keep_path: Option<String>,
}

async fn lookup_track(pool: &sqlx::SqlitePool, id: i64) -> AppResult<Option<(i64, String)>> {
    sqlx::query_as("SELECT library_id, file_path FROM track WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Other(format!("lookup track {id}: {e}")))
}

/// The checks that must hold at the moment a copy moves, not when the
/// user clicked: seconds or minutes can pass in between, and the move
/// is the only step that cannot be taken back from inside WaveFlow.
///
/// - It is not the track playing now.
/// - The copy kept for it is still there and still byte-identical, and
///   is not the same file reached by another path: otherwise this copy
///   may be the last one. The kept copy is read again for every copy it
///   vouches for, not once per group — in a group of three it can change
///   between the first move and the second.
fn trash_one(
    job: &TrashJob,
    shared: &crate::audio::state::SharedPlayback,
) -> Result<(), TrashRefusal> {
    use std::path::Path;

    if shared
        .current_track_id
        .load(std::sync::atomic::Ordering::Acquire)
        == job.id
    {
        return Err(TrashRefusal::Playing);
    }
    let path = Path::new(&job.file_path);
    if std::fs::symlink_metadata(path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return Err(TrashRefusal::Missing);
    }
    let Some(keep) = job.keep_path.as_deref() else {
        return Err(TrashRefusal::KeptCopyGone);
    };
    // One file under two paths — a mapped drive and its UNC path, a bind
    // mount, another case on a case-insensitive volume — is the kept copy
    // itself. Compared by identity (volume and file index, device and
    // inode); an error here means the kept copy cannot be opened, which
    // the hash below refuses anyway.
    if same_file::is_same_file(path, keep).unwrap_or(false) {
        return Err(TrashRefusal::KeptCopyGone);
    }
    let Ok(kept) = waveflow_core::scanner::hash_file_full(Path::new(keep)) else {
        return Err(TrashRefusal::KeptCopyGone);
    };
    match waveflow_core::scanner::hash_file_full(path) {
        Ok(hash) if hash == kept => {}
        Ok(_) => return Err(TrashRefusal::KeptCopyGone),
        Err(e) => return Err(TrashRefusal::Failed(e.to_string())),
    }
    crate::trash::move_one(path).map_err(TrashRefusal::Failed)
}

/// Remove one trashed copy's row, with its sync op, in a transaction of
/// its own. A row already gone (the watcher's rescan got there first)
/// is not an error.
async fn remove_row(
    pool: &sqlx::SqlitePool,
    id: i64,
    library_id: i64,
    file_path: &str,
) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let res = sqlx::query("DELETE FROM track WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    if res.rows_affected() > 0 {
        // User-initiated delete (not a library / folder cascade, which
        // the server replays from the parent op). In the same
        // transaction as the DELETE, so both land or neither does.
        crate::sync::track_emit::emit_track_delete_in_tx(&mut tx, library_id, file_path)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::{verify_groups_full_content, DuplicateTrack};

    fn track(path: String, added_at: i64) -> DuplicateTrack {
        DuplicateTrack {
            id: added_at,
            title: "t".into(),
            artist_name: None,
            album_title: None,
            file_path: path,
            file_size: 0,
            bitrate: None,
            sample_rate: None,
            duration_ms: 0,
            added_at,
        }
    }

    #[test]
    fn groups_identical_files_by_content_not_stored_hash() {
        // Two byte-identical files + one different. In a mixed library
        // the identical pair could carry a legacy full hash on one row
        // and a new partial hash on the other; verify reads the bytes,
        // so they still group. `DuplicateTrack` carries no stored hash —
        // that's the point: identity is content-derived here.
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.bin");
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        std::fs::write(&c, b"different bytes").unwrap();

        let groups = verify_groups_full_content(vec![
            track(a.to_string_lossy().into_owned(), 1),
            track(b.to_string_lossy().into_owned(), 2),
            track(c.to_string_lossy().into_owned(), 3),
        ]);

        assert_eq!(groups.len(), 1, "only the identical pair forms a group");
        let mut ids: Vec<i64> = groups[0].tracks.iter().map(|t| t.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    /// The checks made right before a move: each refusal keeps the file
    /// where it is, and only a copy with an identical kept twin goes.
    #[test]
    fn a_copy_moves_only_while_its_kept_twin_still_matches() {
        use super::{trash_one, TrashJob, TrashRefusal};
        use crate::audio::state::SharedPlayback;

        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p.to_string_lossy().into_owned()
        };
        let job = |id: i64, file_path: String, keep_path: Option<String>| TrashJob {
            id,
            library_id: 1,
            file_path,
            keep_path,
        };
        let shared = SharedPlayback::new();

        // The kept copy's row is gone.
        let lone = write("lone.mp3", b"same");
        assert!(matches!(
            trash_one(&job(1, lone.clone(), None), &shared),
            Err(TrashRefusal::KeptCopyGone)
        ));
        // The kept copy's file is gone.
        let missing = dir
            .path()
            .join("missing.mp3")
            .to_string_lossy()
            .into_owned();
        assert!(matches!(
            trash_one(&job(1, lone.clone(), Some(missing)), &shared),
            Err(TrashRefusal::KeptCopyGone)
        ));
        // The kept copy changed since the scan.
        let changed = write("changed.mp3", b"retagged");
        assert!(matches!(
            trash_one(&job(1, lone.clone(), Some(changed)), &shared),
            Err(TrashRefusal::KeptCopyGone)
        ));
        // The "copy" is the kept file itself.
        assert!(matches!(
            trash_one(&job(1, lone.clone(), Some(lone.clone())), &shared),
            Err(TrashRefusal::KeptCopyGone)
        ));
        // It started playing after the click.
        let twin = write("twin.mp3", b"same");
        shared
            .current_track_id
            .store(7, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            trash_one(&job(7, lone.clone(), Some(twin.clone())), &shared),
            Err(TrashRefusal::Playing)
        ));
        assert!(std::path::Path::new(&lone).exists());

        // A copy no longer on disk keeps its row.
        let vanished = dir
            .path()
            .join("vanished.mp3")
            .to_string_lossy()
            .into_owned();
        assert!(matches!(
            trash_one(&job(1, vanished, Some(twin.clone())), &shared),
            Err(TrashRefusal::Missing)
        ));

        // An identical twin still there: the copy goes.
        assert!(trash_one(&job(1, lone.clone(), Some(twin.clone())), &shared).is_ok());
        assert!(!std::path::Path::new(&lone).exists());
        assert!(std::path::Path::new(&twin).exists());
    }

    /// A group of three: the kept copy changes between the first move and
    /// the second, and the second copy stays.
    #[test]
    fn the_kept_copy_is_read_again_before_every_move() {
        use super::{trash_one, TrashJob, TrashRefusal};
        use crate::audio::state::SharedPlayback;

        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            p.to_string_lossy().into_owned()
        };
        let kept = write("kept.mp3", b"same");
        let first = write("first.mp3", b"same");
        let second = write("second.mp3", b"same");
        let job = |id: i64, file_path: String| TrashJob {
            id,
            library_id: 1,
            file_path,
            keep_path: Some(kept.clone()),
        };
        let shared = SharedPlayback::new();

        assert!(trash_one(&job(1, first.clone()), &shared).is_ok());
        std::fs::write(&kept, b"retagged in between").unwrap();
        assert!(matches!(
            trash_one(&job(2, second.clone()), &shared),
            Err(TrashRefusal::KeptCopyGone)
        ));
        assert!(std::path::Path::new(&second).exists());
    }

    #[test]
    fn unreadable_files_are_excluded() {
        let groups = verify_groups_full_content(vec![
            track("/no/such/file/x.bin".into(), 1),
            track("/no/such/file/y.bin".into(), 2),
        ]);
        assert!(groups.is_empty());
    }
}
