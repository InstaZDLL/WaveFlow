//! A profile's picture: set from a file the user picked, or removed.
//!
//! The file is read here, bounded, and made into a square PNG by
//! [`waveflow_core::artwork::avatar::normalize_avatar`], which treats it
//! as hostile input. Only that PNG is kept, at
//! [`AppPaths::profile_avatar`]; the original's path is not stored
//! anywhere. `profile.avatar_hash` holds the BLAKE3 of the stored PNG,
//! which is what tells the webview a new picture from the one it cached
//! under the same file name.

use std::io::Read;
use std::path::Path;

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
/// size could not be trusted (a file still growing).
pub(crate) fn read_bounded(source: &mut impl Read) -> AppResult<Vec<u8>> {
    let mut bytes = Vec::new();
    source
        .take(MAX_AVATAR_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_AVATAR_SOURCE_BYTES {
        return Err(AvatarRefusal::TooLarge.into());
    }
    Ok(bytes)
}

/// The picture a file makes, ready to store. Blocking.
fn picture_from_file(file_path: &Path) -> AppResult<Vec<u8>> {
    let mut file = std::fs::File::open(file_path)?;
    if file.metadata()?.len() > MAX_AVATAR_SOURCE_BYTES {
        return Err(AvatarRefusal::TooLarge.into());
    }
    let bytes = read_bounded(&mut file)?;
    Ok(normalize_avatar(&bytes)?)
}

/// Write an already-normalised PNG as the profile's picture and return
/// its hash. The file is replaced whole or not at all. Blocking.
pub(crate) fn store_picture(paths: &AppPaths, profile_id: i64, png: &[u8]) -> AppResult<String> {
    let target = paths.profile_avatar(profile_id);
    waveflow_core::atomic_write::write_bytes(&target, png)?;
    Ok(blake3::hash(png).to_hex().to_string())
}

/// Make `file_path` the picture of `profile_id`.
///
/// Refused with `avatar: too-large`, `avatar: unsupported` or
/// `avatar: unreadable` when the file cannot be one; the profile keeps
/// the picture it had. Held under the profile-switch lock, which
/// `delete_profile` holds too, so a picture cannot be written into the
/// directory of a profile being deleted.
#[tauri::command]
pub async fn set_profile_avatar(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
    file_path: String,
) -> AppResult<ProfileView> {
    let _switching = state.profile_switch_lock.clone().lock_owned().await;
    let repo = SqliteProfileRepository::new(state.app_db.clone());
    if !repo.exists(profile_id).await? {
        return Err(AppError::ProfileNotFound(profile_id));
    }

    let paths = state.paths.clone();
    let hash = tokio::task::spawn_blocking(move || -> AppResult<String> {
        let png = picture_from_file(Path::new(&file_path))?;
        std::fs::create_dir_all(paths.profile_dir(profile_id))?;
        store_picture(&paths, profile_id, &png)
    })
    .await
    .map_err(|e| AppError::Other(format!("avatar task join: {e}")))?
    .inspect_err(|e| tracing::warn!(profile_id, error = %e, "profile picture refused"))?;

    sqlx::query("UPDATE profile SET avatar_hash = ? WHERE id = ?")
        .bind(&hash)
        .bind(profile_id)
        .execute(&state.app_db)
        .await?;
    profile_view(&state, profile_id).await
}

/// Remove the picture of `profile_id`; its initial is drawn again.
///
/// The file goes first: a file that cannot be removed fails the command
/// with the column untouched, so an export never carries a picture the
/// interface no longer shows.
#[tauri::command]
pub async fn clear_profile_avatar(
    state: tauri::State<'_, AppState>,
    profile_id: i64,
) -> AppResult<ProfileView> {
    let _switching = state.profile_switch_lock.clone().lock_owned().await;
    let target = state.paths.profile_avatar(profile_id);
    match tokio::task::spawn_blocking(move || std::fs::remove_file(target))
        .await
        .map_err(|e| AppError::Other(format!("avatar task join: {e}")))?
    {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let cleared = sqlx::query("UPDATE profile SET avatar_hash = NULL WHERE id = ?")
        .bind(profile_id)
        .execute(&state.app_db)
        .await?;
    if cleared.rows_affected() == 0 {
        return Err(AppError::ProfileNotFound(profile_id));
    }
    profile_view(&state, profile_id).await
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
        let err = read_bounded(&mut over).unwrap_err();
        assert_eq!(err.to_string(), "avatar: too-large");

        let mut at = std::io::repeat(0).take(MAX_AVATAR_SOURCE_BYTES);
        assert_eq!(
            read_bounded(&mut at).unwrap().len() as u64,
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
    fn a_file_that_is_no_picture_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("photo.png");
        std::fs::write(&fake, b"not a picture at all").unwrap();
        let err = picture_from_file(&fake).unwrap_err();
        assert_eq!(err.to_string(), "avatar: unsupported");
    }
}
