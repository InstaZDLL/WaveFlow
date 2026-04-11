use std::sync::Arc;

use chrono::Utc;
use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::RwLock;

use crate::{
    db,
    error::{AppError, AppResult},
    paths::AppPaths,
};

/// Active per-profile database. Closed and replaced on profile switch.
pub struct ActiveProfile {
    pub profile_id: i64,
    pub pool: SqlitePool,
}

/// Application-wide state managed by Tauri.
///
/// Carries:
/// - the resolved filesystem [`AppPaths`]
/// - the always-open global `app.db` pool
/// - an optional, swappable per-profile `data.db` pool
pub struct AppState {
    pub paths: AppPaths,
    pub app_db: SqlitePool,
    pub profile: Arc<RwLock<Option<ActiveProfile>>>,
}

impl AppState {
    /// Initialize the application state during Tauri setup.
    ///
    /// Resolves filesystem paths, ensures the root directories exist, opens
    /// `app.db` (running any pending migrations) and runs a bootstrap pass
    /// so the app always starts with **exactly one active profile**:
    ///
    /// 1. If the `profile` table is empty, a "Default" profile is created
    ///    (directory layout + fresh `data.db`).
    /// 2. The `app.last_profile_id` setting is consulted; if it points to a
    ///    still-existing profile, that profile is activated. Otherwise the
    ///    most-recently-used profile is activated as a fallback.
    pub async fn init(handle: &AppHandle) -> AppResult<Self> {
        let paths = AppPaths::from_handle(handle)?;
        paths.ensure_dirs()?;

        let app_db = db::app_db::open(&paths.app_db).await?;

        let state = Self {
            paths,
            app_db,
            profile: Arc::new(RwLock::new(None)),
        };

        state.bootstrap().await?;

        Ok(state)
    }

    /// Ensure at least one profile exists, then activate the most relevant
    /// one. Called once at the end of [`Self::init`].
    async fn bootstrap(&self) -> AppResult<()> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM profile")
            .fetch_one(&self.app_db)
            .await?;

        if count == 0 {
            self.create_default_profile().await?;
        }

        if let Some(profile_id) = self.resolve_target_profile().await? {
            self.activate_profile(profile_id).await?;
        }

        Ok(())
    }

    /// Create the built-in "Default" profile: DB row, filesystem layout and
    /// a freshly migrated `data.db`. Invoked only on the very first launch.
    async fn create_default_profile(&self) -> AppResult<()> {
        let now = Utc::now().timestamp_millis();

        let insert = sqlx::query(
            "INSERT INTO profile (name, color_id, avatar_hash, data_dir, created_at, last_used_at)
             VALUES (?, 'emerald', NULL, '', ?, ?)",
        )
        .bind("Default")
        .bind(now)
        .bind(now)
        .execute(&self.app_db)
        .await?;

        let profile_id = insert.last_insert_rowid();
        let rel_dir = AppPaths::profile_rel_dir(profile_id);

        sqlx::query("UPDATE profile SET data_dir = ? WHERE id = ?")
            .bind(&rel_dir)
            .bind(profile_id)
            .execute(&self.app_db)
            .await?;

        self.paths.ensure_profile_dirs(profile_id)?;
        let pool = db::profile_db::open(&self.paths.profile_db(profile_id)).await?;
        pool.close().await;

        tracing::info!(profile_id, "created default profile");
        Ok(())
    }

    /// Pick the profile to activate on startup.
    ///
    /// Priority: the `app.last_profile_id` setting if it still exists,
    /// otherwise the most-recently-used profile. Returns `None` only if the
    /// table is genuinely empty (should not happen after `bootstrap` has run
    /// `create_default_profile`, but handled defensively).
    async fn resolve_target_profile(&self) -> AppResult<Option<i64>> {
        let last_profile_id: Option<String> = sqlx::query_scalar(
            "SELECT value FROM app_setting WHERE key = 'app.last_profile_id'",
        )
        .fetch_optional(&self.app_db)
        .await?;

        if let Some(id_str) = last_profile_id {
            if let Ok(id) = id_str.parse::<i64>() {
                let exists: Option<i64> =
                    sqlx::query_scalar("SELECT id FROM profile WHERE id = ?")
                        .bind(id)
                        .fetch_optional(&self.app_db)
                        .await?;
                if exists.is_some() {
                    return Ok(Some(id));
                }
            }
        }

        let fallback: Option<i64> =
            sqlx::query_scalar("SELECT id FROM profile ORDER BY last_used_at DESC LIMIT 1")
                .fetch_optional(&self.app_db)
                .await?;

        Ok(fallback)
    }

    /// Open (or reopen) the per-profile `data.db` for `profile_id`. If a
    /// profile is currently active, its pool is closed first so that WAL
    /// files can be cleanly checkpointed.
    pub async fn activate_profile(&self, profile_id: i64) -> AppResult<()> {
        self.paths.ensure_profile_dirs(profile_id)?;

        let db_path = self.paths.profile_db(profile_id);
        let pool = db::profile_db::open(&db_path).await?;

        let mut guard = self.profile.write().await;
        if let Some(previous) = guard.take() {
            previous.pool.close().await;
        }
        *guard = Some(ActiveProfile { profile_id, pool });

        Ok(())
    }

    /// Close the active profile pool, if any, leaving no profile active.
    pub async fn deactivate_profile(&self) {
        let mut guard = self.profile.write().await;
        if let Some(previous) = guard.take() {
            previous.pool.close().await;
        }
    }

    /// Return a clone of the active profile's pool, or an error if none is
    /// active. The pool is cheap to clone (it's an `Arc` internally).
    ///
    /// Used by upcoming library/scan/queue commands.
    #[allow(dead_code)]
    pub async fn require_profile_pool(&self) -> AppResult<SqlitePool> {
        let guard = self.profile.read().await;
        guard
            .as_ref()
            .map(|p| p.pool.clone())
            .ok_or(AppError::NoActiveProfile)
    }

    /// Return the active profile id, or an error if none is active.
    ///
    /// Used by upcoming library/scan/queue commands.
    #[allow(dead_code)]
    pub async fn require_profile_id(&self) -> AppResult<i64> {
        let guard = self.profile.read().await;
        guard
            .as_ref()
            .map(|p| p.profile_id)
            .ok_or(AppError::NoActiveProfile)
    }
}
