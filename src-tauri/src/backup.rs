//! Automatic profile backup.
//!
//! Long-running tokio task that periodically writes a `.waveflow` archive
//! for every profile into a user-chosen folder, with retention. Reuses
//! the export plumbing from [`crate::commands::profile_io`] so the
//! per-profile archives produced here are bit-compatible with the
//! manual export (they restore through the same `import_profile`).
//!
//! Config lives in `app_setting` (install-wide, not per-profile) so the
//! decision to back up applies to every profile the install knows about:
//!
//! | Key                       | Type   | Default                                  |
//! |---------------------------|--------|------------------------------------------|
//! | `backup.enabled`          | bool   | `false`                                  |
//! | `backup.interval_days`    | int    | `7`                                      |
//! | `backup.folder`           | string | `<documents>/WaveFlow Backups` if empty  |
//! | `backup.retention`        | int    | `5` (per-profile; oldest pruned)         |
//! | `backup.last_run_at`      | int ms | `0`                                      |
//!
//! Wake-up model: the task loops on `tokio::select!` between a deadline
//! computed from `last_run_at + interval` and a `Notify` woken by
//! `set_backup_config` whenever the user toggles or reconfigures the
//! backup. Disabled backups park the task on a notify-only branch so
//! the loop costs nothing while the user hasn't opted in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;

use crate::{
    commands::profile_io::{write_archive, ArchiveManifest, ARCHIVE_VERSION},
    error::{AppError, AppResult},
    state::AppState,
};

/// Handle stored in Tauri state. Cheaply cloneable via `Arc`.
#[derive(Clone)]
pub struct BackupHandle {
    /// Signalled whenever the config changes — wakes the loop so the
    /// next backup deadline is recomputed without waiting for the old
    /// sleep to expire.
    pub notify: Arc<Notify>,
}

impl BackupHandle {
    pub fn new() -> Self {
        Self {
            notify: Arc::new(Notify::new()),
        }
    }
}

/// Snapshot returned to the frontend Settings card.
#[derive(Debug, Clone, Serialize)]
pub struct BackupConfig {
    pub enabled: bool,
    pub interval_days: i64,
    pub folder: String,
    pub retention: i64,
    /// Epoch ms of the last successful run; `0` if never. Frontend
    /// formats with `Date(last_run_at)` so the user can verify the
    /// task actually fired.
    pub last_run_at: i64,
    /// Default folder the frontend should suggest in the picker when
    /// the user hasn't chosen one yet. Resolved on every read so a
    /// missing `app_data_dir` (sandboxed launch) still yields a sane
    /// path the user can override.
    pub default_folder: String,
}

const DEFAULT_INTERVAL_DAYS: i64 = 7;
const DEFAULT_RETENTION: i64 = 5;
const MIN_INTERVAL_DAYS: i64 = 1;
const MAX_INTERVAL_DAYS: i64 = 90;
const MIN_RETENTION: i64 = 1;
const MAX_RETENTION: i64 = 50;

/// Resolve the default backup folder. `app_data_dir/waveflow/backups`
/// — co-located with the rest of the app data so the user can find it
/// without remembering an extra path, and survives an OS reinstall on
/// the same drive (data is preserved across most upgrade paths).
fn default_backup_folder(handle: &AppHandle) -> PathBuf {
    handle
        .path()
        .app_data_dir()
        .map(|p| p.join("waveflow").join("backups"))
        .unwrap_or_else(|_| PathBuf::from("backups"))
}

/// Read the persisted config from `app_setting`. Missing rows fall back
/// to defaults so the first read after a fresh install doesn't error.
pub async fn read_config(state: &AppState, handle: &AppHandle) -> AppResult<BackupConfig> {
    let default_folder = default_backup_folder(handle)
        .to_string_lossy()
        .to_string();

    let mut enabled = false;
    let mut interval_days = DEFAULT_INTERVAL_DAYS;
    let mut folder = String::new();
    let mut retention = DEFAULT_RETENTION;
    let mut last_run_at: i64 = 0;

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM app_setting WHERE key IN
            ('backup.enabled', 'backup.interval_days', 'backup.folder',
             'backup.retention', 'backup.last_run_at')",
    )
    .fetch_all(&state.app_db)
    .await?;
    for (k, v) in rows {
        match k.as_str() {
            "backup.enabled" => enabled = v == "true" || v == "1",
            "backup.interval_days" => {
                interval_days = v.parse().unwrap_or(DEFAULT_INTERVAL_DAYS)
            }
            "backup.folder" => folder = v,
            "backup.retention" => retention = v.parse().unwrap_or(DEFAULT_RETENTION),
            "backup.last_run_at" => last_run_at = v.parse().unwrap_or(0),
            _ => {}
        }
    }

    Ok(BackupConfig {
        enabled,
        interval_days: interval_days.clamp(MIN_INTERVAL_DAYS, MAX_INTERVAL_DAYS),
        folder,
        retention: retention.clamp(MIN_RETENTION, MAX_RETENTION),
        last_run_at,
        default_folder,
    })
}

/// Persist a config update + wake the backup loop so the new schedule
/// takes effect immediately (e.g. user enables backup → run within
/// seconds, not "at next 7-day tick").
pub async fn write_config(
    state: &AppState,
    handle: &BackupHandle,
    enabled: bool,
    interval_days: i64,
    folder: String,
    retention: i64,
) -> AppResult<()> {
    let interval = interval_days.clamp(MIN_INTERVAL_DAYS, MAX_INTERVAL_DAYS);
    let retention = retention.clamp(MIN_RETENTION, MAX_RETENTION);
    let now = Utc::now().timestamp_millis();

    let mut tx = state.app_db.begin().await?;
    for (key, value, kind) in [
        ("backup.enabled", if enabled { "true" } else { "false" }.to_string(), "bool"),
        ("backup.interval_days", interval.to_string(), "int"),
        ("backup.folder", folder, "string"),
        ("backup.retention", retention.to_string(), "int"),
    ] {
        sqlx::query(
            "INSERT INTO app_setting (key, value, value_type, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                value_type = excluded.value_type,
                updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(kind)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    handle.notify.notify_one();
    Ok(())
}

/// Stamp `backup.last_run_at` with the current epoch ms. Used by the
/// loop AND by `run_backup_now` so the next deadline reset happens in
/// one place.
async fn stamp_last_run(state: &AppState) -> AppResult<()> {
    let now = Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES ('backup.last_run_at', ?, 'int', ?)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at = excluded.updated_at",
    )
    .bind(now.to_string())
    .bind(now)
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Sanitize a profile name so it's safe to embed in a filename across
/// Windows / macOS / Linux. Replaces every character outside
/// `[A-Za-z0-9._-]` with `_` and trims to 60 chars so the resulting
/// filename + timestamp + extension stay under typical 255-byte limits.
fn sanitize_for_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    let limited: String = trimmed.chars().take(60).collect();
    if limited.is_empty() {
        "profile".to_string()
    } else {
        limited
    }
}

/// Run a single backup pass over every profile in the install.
///
/// Returns the list of archive paths created (one per profile that
/// succeeded). Failures on individual profiles are logged but don't
/// abort the pass — a corrupt or detached profile shouldn't stop the
/// healthy ones from being saved.
pub async fn run_one_backup(
    state: &AppState,
    handle: &AppHandle,
    config: &BackupConfig,
) -> AppResult<Vec<String>> {
    let folder = if config.folder.is_empty() {
        default_backup_folder(handle)
    } else {
        PathBuf::from(&config.folder)
    };
    std::fs::create_dir_all(&folder)
        .map_err(|e| AppError::Other(format!("create backup folder: {e}")))?;

    // Active profile gets a WAL checkpoint so the bundled DB captures
    // every committed page. Inactive profiles are cold on disk — their
    // last checkpoint already ran at switch/shutdown.
    let active_id = {
        let guard = state.profile.read().await;
        guard.as_ref().map(|p| p.profile_id)
    };
    if let Some(_id) = active_id {
        if let Ok(pool) = state.require_profile_pool().await {
            let _ = crate::commands::profile_io::checkpoint_wal(&pool).await;
        }
    }

    let profiles: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, name FROM profile ORDER BY id")
            .fetch_all(&state.app_db)
            .await?;

    let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let app_version = env!("CARGO_PKG_VERSION").to_string();
    let mut created = Vec::with_capacity(profiles.len());

    for (profile_id, profile_name) in profiles {
        let safe = sanitize_for_filename(&profile_name);
        let target = folder.join(format!("{safe}-{ts}.waveflow"));

        let manifest = ArchiveManifest {
            archive_version: ARCHIVE_VERSION,
            app_version: app_version.clone(),
            profile_name: profile_name.clone(),
            source_profile_id: profile_id,
            exported_at: Utc::now().to_rfc3339(),
        };

        let profile_dir = state.paths.profile_dir(profile_id);
        let db_path = state.paths.profile_db(profile_id);
        let artwork_dir = state.paths.profile_artwork_dir(profile_id);
        let target_owned = target.clone();

        let result = tokio::task::spawn_blocking(move || -> AppResult<()> {
            write_archive(&target_owned, &profile_dir, &db_path, &artwork_dir, &manifest)
        })
        .await
        .map_err(|e| AppError::Other(format!("backup task join: {e}")))?;

        match result {
            Ok(()) => {
                tracing::info!(profile_id, target = %target.display(), "auto backup ok");
                created.push(target.to_string_lossy().to_string());
            }
            Err(err) => {
                tracing::warn!(profile_id, ?err, "auto backup failed");
            }
        }

        // Prune old archives for this profile. List `<safe>-*.waveflow`
        // in the folder, sort by mtime descending, keep the first
        // `retention` entries.
        if let Err(err) =
            prune_old_backups(&folder, &safe, config.retention as usize).await
        {
            tracing::warn!(?err, "auto backup retention sweep failed");
        }
    }

    stamp_last_run(state).await?;
    Ok(created)
}

async fn prune_old_backups(folder: &std::path::Path, name_prefix: &str, keep: usize) -> AppResult<()> {
    let prefix = format!("{name_prefix}-");
    let folder = folder.to_path_buf();
    let prefix_owned = prefix.clone();

    // `read_dir` + metadata lookups can stall on slow disks; run on the
    // blocking pool to keep the tokio runtime responsive.
    let pruned = tokio::task::spawn_blocking(move || -> AppResult<usize> {
        let mut entries: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        let read_dir = match std::fs::read_dir(&folder) {
            Ok(r) => r,
            // Folder doesn't exist anymore — caller deleted it manually,
            // nothing to prune. Not an error from the user's perspective.
            Err(_) => return Ok(0),
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            let Some(stem) = path
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| s.starts_with(&prefix_owned) && s.ends_with(".waveflow"))
            else {
                continue;
            };
            let _ = stem;
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.push((path, mtime));
        }
        entries.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
        let mut removed = 0;
        for (path, _) in entries.into_iter().skip(keep) {
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    })
    .await
    .map_err(|e| AppError::Other(format!("prune task join: {e}")))??;

    if pruned > 0 {
        tracing::debug!(pruned, "auto backup retention pruned old archives");
    }
    Ok(())
}

/// Spawn the long-running backup loop. Idempotent in the sense that
/// dropping the returned `BackupHandle` doesn't tear the task down —
/// it lives for the app's lifetime and exits cleanly on `Notify` drop
/// (which only happens when the entire state is destroyed at shutdown).
pub fn spawn_backup_loop(handle: AppHandle, backup_handle: BackupHandle) {
    tauri::async_runtime::spawn(async move {
        // Small initial delay so we don't compete with the heavy boot
        // path (audio engine init, watchers, scanner re-snapshots).
        tokio::time::sleep(Duration::from_secs(15)).await;

        loop {
            let state = match handle.try_state::<AppState>() {
                Some(s) => s,
                None => {
                    tracing::warn!("backup loop: AppState unavailable, retrying");
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    continue;
                }
            };

            let config = match read_config(&state, &handle).await {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(?err, "backup loop: read_config failed");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    continue;
                }
            };

            if !config.enabled {
                // Park on the notify channel until the user enables it.
                // No timeout — there's nothing to wake up for.
                backup_handle.notify.notified().await;
                continue;
            }

            // Compute the next wakeup. `last_run_at == 0` (never run)
            // means the first tick fires immediately; otherwise wait
            // until `last_run_at + interval_days * 86_400_000`.
            let interval_ms = config.interval_days * 86_400_000;
            let now = Utc::now().timestamp_millis();
            let next_run_at = if config.last_run_at == 0 {
                now
            } else {
                config.last_run_at + interval_ms
            };
            let wait_ms = (next_run_at - now).max(0) as u64;

            if wait_ms > 0 {
                // Either the deadline expires OR the user reconfigures —
                // whichever comes first.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
                    _ = backup_handle.notify.notified() => continue,
                }
            }

            // Re-read in case the user toggled OFF while we were
            // sleeping (notify branch is only one way to learn that).
            let config = match read_config(&state, &handle).await {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(?err, "backup loop: re-read failed");
                    continue;
                }
            };
            if !config.enabled {
                continue;
            }

            match run_one_backup(&state, &handle, &config).await {
                Ok(paths) => {
                    tracing::info!(count = paths.len(), "auto backup run finished");
                    let _ = handle.emit("backup:completed", paths);
                }
                Err(err) => {
                    tracing::warn!(?err, "auto backup run failed");
                }
            }
        }
    });
}
