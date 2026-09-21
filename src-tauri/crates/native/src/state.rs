pub(crate) use crate::profile_selection::resolve_target_profile;
use crate::{
    db, error::AppResult, paths::AppPaths, profile_pool::ActiveProfile,
    remote_playback::RemotePlayback,
};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Same profile lifecycle implementation as Tauri, with no frontend state.
pub struct AppState {
    pub paths: AppPaths,
    pub app_db: SqlitePool,
    pub profile: Arc<RwLock<Option<ActiveProfile>>>,
    pub remote_playback: RemotePlayback,
}

impl AppState {
    pub async fn open(root: std::path::PathBuf) -> AppResult<Arc<Self>> {
        let mut paths = AppPaths::from_root(root, None);
        let app_db = db::app_db::open(&paths.app_db).await?;
        let cache: Option<String> =
            sqlx::query_scalar("SELECT value FROM app_setting WHERE key = 'storage.cache_root'")
                .fetch_optional(&app_db)
                .await?;
        if let Some(cache) = cache.filter(|s| !s.is_empty()) {
            paths = paths.with_cache_root(cache.into());
        }
        paths.ensure_dirs()?;
        let offline: Option<String> =
            sqlx::query_scalar("SELECT value FROM app_setting WHERE key = 'network.offline_mode'")
                .fetch_optional(&app_db)
                .await?;
        crate::offline::set(offline.as_deref() == Some("true"));
        let state = Arc::new(Self {
            paths,
            app_db,
            profile: Arc::new(RwLock::new(None)),
            remote_playback: RemotePlayback::default(),
        });
        state.bootstrap().await?;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::AppState;

    #[tokio::test]
    async fn fresh_root_creates_and_selects_the_default_profile() {
        let root = tempfile::tempdir().expect("temporary data root");
        let state = AppState::open(root.path().to_path_buf())
            .await
            .expect("native state opens");
        assert_eq!(state.require_profile_id().await.expect("active profile"), 1);
        let name: String = sqlx::query_scalar("SELECT name FROM profile WHERE id = 1")
            .fetch_one(&state.app_db)
            .await
            .expect("default profile row");
        assert_eq!(name, "Default");
    }
}
