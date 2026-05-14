//! App-wide user preferences (close-to-tray, scan-on-start, autostart).
//!
//! These three settings were exposed as toggles in Settings → Général but
//! the original first release never wired the UI to the backend, so the
//! values reset to the React state's default on every restart and none of
//! the side effects (registry write for autostart, scan trigger at boot,
//! close-handler branch) actually happened. This module owns the backend
//! half of the fix:
//!
//! - **Minimize to tray** — `app_setting['app.minimize_to_tray']` (process-
//!   wide, default `true`). Mirrored on [`PreferencesState::minimize_to_tray`]
//!   so the `WindowEvent::CloseRequested` handler in `lib.rs` is a single
//!   atomic load. When OFF, closing the window arms the `QuitGate` and
//!   lets the destroy event run the normal shutdown path.
//! - **Scan on start** — `profile_setting['library.scan_on_start']` (per
//!   profile, default `false`). Consulted once at the end of [`AppState::init`]
//!   so the rescan happens before the frontend has time to query the
//!   library — feels like the app "noticed" the new files on its own.
//! - **Auto start** — delegated to [`tauri-plugin-autostart`][autostart]
//!   which writes the OS-level entry (registry key / LaunchAgent /
//!   xdg autostart .desktop). We expose thin wrappers so the frontend can
//!   stay in a single command vocabulary instead of mixing plugin calls
//!   and our own.
//!
//! [autostart]: https://v2.tauri.app/plugin/autostart/

use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Utc;
use sqlx::SqlitePool;
use tauri_plugin_autostart::ManagerExt;

use crate::{error::AppResult, state::AppState};

/// Process-wide mirror of the `app.minimize_to_tray` setting.
///
/// Default is `true` so the historical close-to-tray behaviour stays
/// unchanged for users who never open the new toggle.
pub struct PreferencesState {
    pub minimize_to_tray: AtomicBool,
}

impl PreferencesState {
    pub fn new(minimize_to_tray: bool) -> Self {
        Self {
            minimize_to_tray: AtomicBool::new(minimize_to_tray),
        }
    }
}

const KEY_MINIMIZE: &str = "app.minimize_to_tray";

/// Hydrate the close-to-tray flag from `app_setting` once at boot. Missing
/// key → `true` (preserve the v1.0 default).
pub async fn load_minimize_to_tray(app_db: &SqlitePool) -> bool {
    sqlx::query_scalar::<_, String>("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_MINIMIZE)
        .fetch_optional(app_db)
        .await
        .ok()
        .flatten()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(true)
}

#[tauri::command]
pub async fn get_minimize_to_tray(
    state: tauri::State<'_, AppState>,
    prefs: tauri::State<'_, PreferencesState>,
) -> AppResult<bool> {
    // Trust the atomic — it was hydrated from app_setting at boot and is
    // the source of truth for the close handler, so the UI must read the
    // same value. The `state` handle isn't strictly needed for the read,
    // but accepting it keeps the signature uniform with `set_*`.
    let _ = state;
    Ok(prefs.minimize_to_tray.load(Ordering::Acquire))
}

#[tauri::command]
pub async fn set_minimize_to_tray(
    state: tauri::State<'_, AppState>,
    prefs: tauri::State<'_, PreferencesState>,
    enabled: bool,
) -> AppResult<()> {
    prefs.minimize_to_tray.store(enabled, Ordering::Release);
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'bool', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_MINIMIZE)
    .bind(if enabled { "true" } else { "false" })
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

#[tauri::command]
pub async fn get_auto_start(app: tauri::AppHandle) -> AppResult<bool> {
    Ok(app.autolaunch().is_enabled().unwrap_or(false))
}

#[tauri::command]
pub async fn set_auto_start(app: tauri::AppHandle, enabled: bool) -> AppResult<()> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|err| crate::error::AppError::Other(format!("autostart: {err}")))?;
    Ok(())
}
