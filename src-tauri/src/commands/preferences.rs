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
use serde::{Deserialize, Serialize};
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

/// UI zoom level (1.0 = 100 %). Stored in `app_setting` because it's a
/// machine-level preference: a 4K user and a 1080p user on the same
/// box would never share a comfortable zoom, but switching profiles
/// on the same screen shouldn't reset the choice.
///
/// The frontend reads this on boot via `getUiZoom`, applies it through
/// `getCurrentWebviewWindow().setZoom(level)`, and rewrites the row
/// whenever the user nudges the level via the Settings card or the
/// `Ctrl+=` / `Ctrl+-` / `Ctrl+0` shortcuts. The backend keeps it
/// stateless — no atomic mirror because nothing in the hot path needs
/// to read it.
const KEY_UI_ZOOM: &str = "ui.zoom_level";

/// Bounds shared with the frontend. The Settings UI clamps to the
/// same range; this is a server-side safety net so a stray
/// `set_ui_zoom(50)` from a future caller can't blow the layout away
/// (Tauri's `set_zoom` would accept it silently).
const UI_ZOOM_MIN: f64 = 0.5;
const UI_ZOOM_MAX: f64 = 2.0;

#[tauri::command]
pub async fn get_ui_zoom(state: tauri::State<'_, AppState>) -> AppResult<f64> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_UI_ZOOM)
        .fetch_optional(&state.app_db)
        .await?;
    let zoom = raw
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(UI_ZOOM_MIN, UI_ZOOM_MAX))
        .unwrap_or(1.0);
    Ok(zoom)
}

#[tauri::command]
pub async fn set_ui_zoom(state: tauri::State<'_, AppState>, zoom: f64) -> AppResult<()> {
    let clamped = if zoom.is_finite() {
        zoom.clamp(UI_ZOOM_MIN, UI_ZOOM_MAX)
    } else {
        1.0
    };
    // `app_setting.value_type` CHECK constraint only accepts
    // `'string' | 'int' | 'bool' | 'json'` (initial migration). We
    // serialize the zoom as a stringified float anyway, so
    // `'string'` is the honest tag — adding `'real'` would require
    // a migration that none of the persisted keys actually need.
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_UI_ZOOM)
    .bind(format!("{clamped}"))
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Mini-player window bounds in logical pixels. Persisted as a JSON blob
/// under `app_setting['mini_player.bounds']` so the four fields move as
/// one row — restoring half a position is worse than restoring none of
/// it. Position is machine-level (same reason as the zoom level above):
/// a 4K and a 1080p monitor would never share a sensible corner.
const KEY_MINI_PLAYER_BOUNDS: &str = "mini_player.bounds";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniPlayerBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[tauri::command]
pub async fn get_mini_player_bounds(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<MiniPlayerBounds>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_MINI_PLAYER_BOUNDS)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(raw.and_then(|s| serde_json::from_str::<MiniPlayerBounds>(&s).ok()))
}

#[tauri::command]
pub async fn set_mini_player_bounds(
    state: tauri::State<'_, AppState>,
    bounds: MiniPlayerBounds,
) -> AppResult<()> {
    // Drop non-finite or non-positive sizes silently — the frontend can
    // fire a save in the middle of the window being destroyed, where
    // outerSize / outerPosition briefly return junk on some platforms.
    if !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
        || bounds.width <= 0.0
        || bounds.height <= 0.0
    {
        return Ok(());
    }
    let json = serde_json::to_string(&bounds)
        .map_err(|err| crate::error::AppError::Other(format!("mini_player bounds: {err}")))?;
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'json', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_MINI_PLAYER_BOUNDS)
    .bind(json)
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}
