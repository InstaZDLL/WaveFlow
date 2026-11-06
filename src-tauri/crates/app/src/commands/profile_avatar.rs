//! A profile's picture: set from a file the user picked, or removed.
//!
//! The file is read here, bounded, and made into a square PNG by
//! [`waveflow_core::artwork::avatar::normalize_avatar`], which treats it
//! as hostile input. Only that PNG is kept, at
//! [`AppPaths::profile_avatar`]; the original's path is not stored
//! anywhere. `profile.avatar_hash` holds the BLAKE3 of the stored PNG,
//! which is what tells the webview a new picture from the one it cached
//! under the same file name.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sqlx::SqlitePool;
use waveflow_core::artwork::avatar::{normalize_avatar, AvatarRefusal, MAX_AVATAR_SOURCE_BYTES};
use waveflow_core::repository::{profile::ProfileRepository, sqlite::SqliteProfileRepository};

use crate::{
    commands::profile::ProfileView,
    error::{AppError, AppResult},
    paths::AppPaths,
    state::AppState,
};

impl From<AvatarRefusal> for AppError {
    /// `avatar: too-large` and its two siblings: the frontend matches the
    /// prefix and shows its own sentence.
    fn from(refusal: AvatarRefusal) -> Self {
        AppError::Other(refusal.to_string())
    }
}

/// Read at most [`MAX_AVATAR_SOURCE_BYTES`] of `source`. A file past the
/// cap is refused from its size, or from the one byte too many when the
/// size could not be trusted (a file still growing). The refusal is kept
/// apart from a read that failed: an import leaves a refused picture out,
/// and fails on a read that failed.
pub(crate) fn read_bounded(
    source: &mut impl Read,
) -> std::io::Result<Result<Vec<u8>, AvatarRefusal>> {
    let mut bytes = Vec::new();
    source
        .take(MAX_AVATAR_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AVATAR_SOURCE_BYTES {
        return Ok(Err(AvatarRefusal::TooLarge));
    }
    Ok(Ok(bytes))
}

/// The picture a file makes, ready to store. Blocking.
fn picture_from_file(file_path: &Path) -> AppResult<Vec<u8>> {
    let mut file = std::fs::File::open(file_path)?;
    if file.metadata()?.len() > MAX_AVATAR_SOURCE_BYTES {
        return Err(AvatarRefusal::TooLarge.into());
    }
    let bytes = read_bounded(&mut file)??;
    Ok(normalize_avatar(&bytes)?)
}

/// Write an already-normalised PNG as the profile's picture and return
/// its hash. The file is replaced whole or not at all. Used by an import,
/// whose row is rolled back whole when anything after it fails. Blocking.
pub(crate) fn store_picture(paths: &AppPaths, profile_id: i64, png: &[u8]) -> AppResult<String> {
    let target = paths.profile_avatar(profile_id);
    waveflow_core::atomic_write::write_bytes(&target, png)?;
    Ok(blake3::hash(png).to_hex().to_string())
}

/// Make `file_path` the picture of `profile_id`.
///
/// Refused with `avatar: too-large`, `avatar: unsupported` or
/// `avatar: unreadable` when the file cannot be one; the profile keeps
/// the picture it had. The file is decoded first, so a large picture does
/// not hold up a profile switch; everything that touches the profile is
/// then done under the profile-switch lock, which `delete_profile` holds
/// too, so a picture cannot be written into the directory of a profile
/// being deleted.
#[tauri::command]
pub async fn set_profile_avatar(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
    file_path: String,
) -> AppResult<ProfileView> {
    let png = tokio::task::spawn_blocking(move || picture_from_file(Path::new(&file_path)))
        .await
        .map_err(|e| AppError::Other(format!("avatar task join: {e}")))?
        .inspect_err(|e| tracing::warn!(profile_id, error = %e, "profile picture refused"))?;

    let _switching = state.profile_switch_lock.clone().lock_owned().await;
    let repo = SqliteProfileRepository::new(state.app_db.clone());
    if !repo.exists(profile_id).await? {
        return Err(AppError::ProfileNotFound(profile_id));
    }

    let previous: Option<String> =
        sqlx::query_scalar("SELECT avatar_hash FROM profile WHERE id = ?")
            .bind(profile_id)
            .fetch_one(&state.app_db)
            .await?;

    // The new picture is written beside the current one and put in place
    // only once the row names it, so a failure at any step leaves the file
    // and the row agreeing on the picture the profile had: the interface
    // shows it, and an export carries it. A run that stops between the
    // row and the rename is put back in step at the next launch
    // ([`reconcile_avatars`]).
    let paths = state.paths.clone();
    let (staged, hash) =
        tokio::task::spawn_blocking(move || stage_picture(&paths, profile_id, &png))
            .await
            .map_err(|e| AppError::Other(format!("avatar task join: {e}")))??;

    if let Err(err) = sqlx::query("UPDATE profile SET avatar_hash = ? WHERE id = ?")
        .bind(&hash)
        .bind(profile_id)
        .execute(&state.app_db)
        .await
    {
        let _ = std::fs::remove_file(&staged);
        return Err(err.into());
    }
    let target = state.paths.profile_avatar(profile_id);
    let staged_for_blocking = staged.clone();
    let placed = tokio::task::spawn_blocking(move || std::fs::rename(&staged_for_blocking, target))
        .await
        .map_err(|e| AppError::Other(format!("avatar task join: {e}")))?;
    if let Err(err) = placed {
        let _ = std::fs::remove_file(&staged);
        if let Err(revert) = sqlx::query("UPDATE profile SET avatar_hash = ? WHERE id = ?")
            .bind(&previous)
            .bind(profile_id)
            .execute(&state.app_db)
            .await
        {
            tracing::warn!(profile_id, %revert, "profile picture: the row could not be put back");
        }
        return Err(err.into());
    }
    profile_view(&state, profile_id).await
}

/// Write an already-normalised PNG beside the profile's picture, under a
/// name of its own, and return that path with the PNG's hash. Flushed to
/// disk, so the rename that puts it in place cannot expose a file the
/// disk did not keep. Blocking.
fn stage_picture(paths: &AppPaths, profile_id: i64, png: &[u8]) -> AppResult<(PathBuf, String)> {
    let dir = paths.profile_dir(profile_id);
    std::fs::create_dir_all(&dir)?;
    let staged = partial_path(paths, profile_id);
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create_new(&staged)?;
        file.write_all(png)?;
        file.sync_all()
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(err.into());
    }
    Ok((staged, blake3::hash(png).to_hex().to_string()))
}

/// What ends the name of a picture set aside: one being staged, or one
/// being removed. [`reconcile_avatars`] sweeps them.
const PARTIAL_SUFFIX: &str = ".partial";

/// A name of its own beside the profile's picture.
fn partial_path(paths: &AppPaths, profile_id: i64) -> PathBuf {
    paths.profile_dir(profile_id).join(format!(
        "{}.{}{PARTIAL_SUFFIX}",
        crate::paths::PROFILE_AVATAR_FILE,
        uuid::Uuid::new_v4().simple()
    ))
}

/// Remove the picture of `profile_id`; its initial is drawn again.
///
/// The file is moved aside, the row cleared, and only then is the file
/// deleted: a row that cannot be cleared gets its file back, so the file
/// and the row never disagree, and an export never carries a picture the
/// interface no longer shows.
#[tauri::command]
pub async fn clear_profile_avatar(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
) -> AppResult<ProfileView> {
    let _switching = state.profile_switch_lock.clone().lock_owned().await;
    let repo = SqliteProfileRepository::new(state.app_db.clone());
    if !repo.exists(profile_id).await? {
        return Err(AppError::ProfileNotFound(profile_id));
    }

    let target = state.paths.profile_avatar(profile_id);
    let aside = partial_path(&state.paths, profile_id);
    let moved = {
        let (target, aside) = (target.clone(), aside.clone());
        tokio::task::spawn_blocking(move || match std::fs::rename(&target, &aside) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err),
        })
        .await
        .map_err(|e| AppError::Other(format!("avatar task join: {e}")))??
    };

    let failure = match sqlx::query("UPDATE profile SET avatar_hash = NULL WHERE id = ?")
        .bind(profile_id)
        .execute(&state.app_db)
        .await
    {
        Ok(done) if done.rows_affected() > 0 => None,
        Ok(_) => Some(AppError::ProfileNotFound(profile_id)),
        Err(err) => Some(err.into()),
    };
    if let Some(err) = failure {
        if moved {
            if let Err(back) = std::fs::rename(&aside, &target) {
                tracing::warn!(profile_id, %back, "profile picture: the file could not be put back");
            }
        }
        return Err(err);
    }
    if moved {
        if let Err(err) = std::fs::remove_file(&aside) {
            // The row already says no picture; the file goes at the next
            // launch.
            tracing::warn!(profile_id, %err, "profile picture: the removed file was left aside");
        }
    }
    profile_view(&state, profile_id).await
}

/// Put every profile's row back in step with its picture, once at launch.
///
/// A picture is set by naming it in the row, then renaming it into place,
/// and removed by moving it aside, then clearing the row: a run that stops
/// between the two leaves a row naming another file than the one on the
/// disk, and a file aside. The file is what is shown and exported, so the
/// row follows it: the hash of the `avatar.png` that is there, or none
/// when there is none. Every file set aside is deleted. A picture that
/// cannot be read is left as it is, row and all.
pub(crate) async fn reconcile_avatars(app_db: &SqlitePool, paths: &AppPaths) -> AppResult<()> {
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as("SELECT id, avatar_hash FROM profile")
        .fetch_all(app_db)
        .await?;
    let paths = paths.clone();
    let fixes = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .filter_map(|(id, named)| {
                sweep_partials(&paths.profile_dir(id));
                let on_disk = picture_hash(&paths.profile_avatar(id))
                    .inspect_err(|err| {
                        tracing::warn!(profile_id = id, %err, "profile picture: unreadable, its row left as it is");
                    })
                    .ok()?;
                (on_disk != named).then_some((id, on_disk))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| AppError::Other(format!("avatar task join: {e}")))?;

    for (id, hash) in fixes {
        sqlx::query("UPDATE profile SET avatar_hash = ? WHERE id = ?")
            .bind(&hash)
            .bind(id)
            .execute(app_db)
            .await?;
        tracing::info!(
            profile_id = id,
            "profile picture: row put back in step with its file"
        );
    }
    Ok(())
}

/// The hash of the picture at `path`, or `None` when there is none.
/// Blocking.
fn picture_hash(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(blake3::hash(&bytes).to_hex().to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Delete the pictures a stopped run set aside in `dir`. Blocking.
fn sweep_partials(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{}.", crate::paths::PROFILE_AVATAR_FILE);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(&prefix) && name.ends_with(PARTIAL_SUFFIX) {
            if let Err(err) = std::fs::remove_file(entry.path()) {
                tracing::warn!(%err, "profile picture: a file set aside could not be deleted");
            }
        }
    }
}

async fn profile_view(state: &AppState, profile_id: i64) -> AppResult<ProfileView> {
    let profile = SqliteProfileRepository::new(state.app_db.clone())
        .get(profile_id)
        .await?
        .ok_or(AppError::ProfileNotFound(profile_id))?;
    Ok(ProfileView::new(&state.paths, profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_past_the_cap_is_refused() {
        let mut over = std::io::repeat(0).take(MAX_AVATAR_SOURCE_BYTES + 10);
        let refusal = read_bounded(&mut over).unwrap().unwrap_err();
        assert_eq!(refusal, AvatarRefusal::TooLarge);

        let mut at = std::io::repeat(0).take(MAX_AVATAR_SOURCE_BYTES);
        assert_eq!(
            read_bounded(&mut at).unwrap().unwrap().len() as u64,
            MAX_AVATAR_SOURCE_BYTES
        );
    }

    #[test]
    fn a_stored_picture_is_named_by_its_hash_and_replaced_whole() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().to_path_buf(), None);
        std::fs::create_dir_all(paths.profile_dir(7)).unwrap();

        let first = store_picture(&paths, 7, b"first").unwrap();
        let second = store_picture(&paths, 7, b"second picture").unwrap();
        assert_ne!(first, second);
        assert_eq!(second, blake3::hash(b"second picture").to_hex().to_string());
        assert_eq!(
            std::fs::read(paths.profile_avatar(7)).unwrap(),
            b"second picture"
        );
        // Nothing but the picture beside the database: no partial left.
        let names: Vec<_> = std::fs::read_dir(paths.profile_dir(7))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("avatar.png")]);
    }

    #[test]
    fn a_staged_picture_leaves_the_current_one_in_place() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().to_path_buf(), None);
        std::fs::create_dir_all(paths.profile_dir(3)).unwrap();
        std::fs::write(paths.profile_avatar(3), b"current").unwrap();

        let (staged, hash) = stage_picture(&paths, 3, b"next").unwrap();
        assert_eq!(std::fs::read(paths.profile_avatar(3)).unwrap(), b"current");
        assert_eq!(std::fs::read(&staged).unwrap(), b"next");
        assert_eq!(staged.parent(), Some(paths.profile_dir(3).as_path()));
        assert_eq!(hash, blake3::hash(b"next").to_hex().to_string());

        std::fs::rename(&staged, paths.profile_avatar(3)).unwrap();
        assert_eq!(std::fs::read(paths.profile_avatar(3)).unwrap(), b"next");
    }

    /// A run stopped between the row and the file: one row names a picture
    /// that never got into place, another names one already moved aside,
    /// a third has a picture its row does not name. At the next launch each
    /// row follows its file, and nothing set aside is left.
    #[tokio::test]
    async fn a_stopped_run_is_put_back_in_step_at_launch() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::from_root(root.path().to_path_buf(), None);
        let app_db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(root.path().join("app.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("../../migrations/app")
            .run(&app_db)
            .await
            .unwrap();
        for (id, named) in [(1, Some("new")), (2, Some("removed")), (3, None)] {
            sqlx::query(
                "INSERT INTO profile (id, name, avatar_hash, data_dir, created_at, last_used_at) \
                 VALUES (?, ?, ?, '', 0, 0)",
            )
            .bind(id)
            .bind(format!("p{id}"))
            .bind(named)
            .execute(&app_db)
            .await
            .unwrap();
            std::fs::create_dir_all(paths.profile_dir(id)).unwrap();
        }
        std::fs::write(paths.profile_avatar(1), b"old").unwrap();
        std::fs::write(partial_path(&paths, 1), b"new").unwrap();
        std::fs::write(partial_path(&paths, 2), b"removed").unwrap();
        std::fs::write(paths.profile_avatar(3), b"imported").unwrap();

        reconcile_avatars(&app_db, &paths).await.unwrap();

        let rows: Vec<(i64, Option<String>)> =
            sqlx::query_as("SELECT id, avatar_hash FROM profile ORDER BY id")
                .fetch_all(&app_db)
                .await
                .unwrap();
        let hash = |b: &[u8]| Some(blake3::hash(b).to_hex().to_string());
        assert_eq!(
            rows,
            vec![(1, hash(b"old")), (2, None), (3, hash(b"imported"))]
        );
        for id in 1..=3 {
            let left: Vec<_> = std::fs::read_dir(paths.profile_dir(id))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .filter(|n| n.to_string_lossy().ends_with(PARTIAL_SUFFIX))
                .collect();
            assert!(left.is_empty(), "profile {id}: {left:?}");
        }
    }

    #[test]
    fn a_file_that_is_no_picture_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("photo.png");
        std::fs::write(&fake, b"not a picture at all").unwrap();
        let err = picture_from_file(&fake).unwrap_err();
        assert_eq!(err.to_string(), "avatar: unsupported");
    }
}
