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

/// Main window bounds, same shape and persistence contract as the mini-player.
const KEY_MAIN_WINDOW_BOUNDS: &str = "main_window.bounds";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniPlayerBounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Shared validity predicate for persisted bounds. Mirrors the guard in
/// `set_main_window_bounds` / `set_mini_player_bounds` so corrupted or
/// default-zero rows stored by a previous build are never applied.
fn bounds_are_valid(b: &MiniPlayerBounds) -> bool {
    b.x.is_finite()
        && b.y.is_finite()
        && b.width.is_finite()
        && b.height.is_finite()
        && b.width > 0.0
        && b.height > 0.0
}

/// Non-command helper used by the `app://ready` boot path (lib.rs) to read
/// the persisted main-window bounds before the window is made visible, so
/// the window appears at the saved size/position with no jump.
pub async fn load_main_window_bounds(app_db: &SqlitePool) -> Option<MiniPlayerBounds> {
    let raw: Option<String> =
        match sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
            .bind(KEY_MAIN_WINDOW_BOUNDS)
            .fetch_optional(app_db)
            .await
        {
            Ok(v) => v,
            Err(err) => {
                // A query failure (e.g. a transient SQLITE_BUSY at boot) is NOT the
                // same as "no saved bounds": log it so the read error is
                // diagnosable instead of silently masquerading as a first-run
                // default. We still fall back to `None` — the splash handoff must
                // reveal the window at default bounds rather than block on a
                // cosmetic bounds read (see `restore_bounds_and_reveal` in lib.rs).
                tracing::warn!(?err, "failed to read persisted main-window bounds");
                None
            }
        };
    raw.and_then(|s| serde_json::from_str::<MiniPlayerBounds>(&s).ok())
        .filter(bounds_are_valid)
}

#[tauri::command]
pub async fn get_main_window_bounds(
    state: tauri::State<'_, AppState>,
) -> AppResult<Option<MiniPlayerBounds>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_MAIN_WINDOW_BOUNDS)
        .fetch_optional(&state.app_db)
        .await?;
    Ok(raw
        .and_then(|s| serde_json::from_str::<MiniPlayerBounds>(&s).ok())
        .filter(bounds_are_valid))
}

#[tauri::command]
pub async fn set_main_window_bounds(
    state: tauri::State<'_, AppState>,
    bounds: MiniPlayerBounds,
) -> AppResult<()> {
    if !bounds_are_valid(&bounds) {
        return Ok(());
    }
    let json = serde_json::to_string(&bounds)
        .map_err(|err| crate::error::AppError::Other(format!("main_window bounds: {err}")))?;
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'json', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_MAIN_WINDOW_BOUNDS)
    .bind(json)
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Forget the persisted main-window size + position. The next launch
/// falls back to the default bounds from `tauri.conf.json`. Exposed as a
/// Settings → Appearance "Reset window position" action for users whose
/// window ended up somewhere awkward (or who just want the default size
/// back). Deleting the row rather than writing a default keeps the
/// "no saved bounds → use the manifest default" path as the single
/// source of truth.
#[tauri::command]
pub async fn clear_main_window_bounds(state: tauri::State<'_, AppState>) -> AppResult<()> {
    sqlx::query("DELETE FROM app_setting WHERE key = ?")
        .bind(KEY_MAIN_WINDOW_BOUNDS)
        .execute(&state.app_db)
        .await?;
    Ok(())
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

/// Desktop lyrics window bounds (issue #582), same shape and contract as
/// the mini-player's. Read from Rust, because the window is created there.
const KEY_DESKTOP_LYRICS_BOUNDS: &str = "desktop_lyrics.bounds";

pub async fn load_desktop_lyrics_bounds(app_db: &SqlitePool) -> Option<MiniPlayerBounds> {
    let raw: Option<String> =
        match sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
            .bind(KEY_DESKTOP_LYRICS_BOUNDS)
            .fetch_optional(app_db)
            .await
        {
            Ok(v) => v,
            Err(err) => {
                // Opening at the default place beats not opening; the log
                // keeps a read failure from passing for a first launch.
                tracing::warn!(?err, "failed to read persisted desktop lyrics bounds");
                None
            }
        };
    raw.and_then(|s| serde_json::from_str::<MiniPlayerBounds>(&s).ok())
        .filter(bounds_are_valid)
}

#[tauri::command]
pub async fn set_desktop_lyrics_bounds(
    state: tauri::State<'_, AppState>,
    bounds: MiniPlayerBounds,
) -> AppResult<()> {
    if !bounds_are_valid(&bounds) {
        return Ok(());
    }
    let json = serde_json::to_string(&bounds)
        .map_err(|err| crate::error::AppError::Other(format!("desktop_lyrics bounds: {err}")))?;
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'json', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_DESKTOP_LYRICS_BOUNDS)
    .bind(json)
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await?;
    Ok(())
}

/// Who draws the frame around the window: the desktop (`system`, the
/// default) or WaveFlow itself (`app`).
///
/// Issue #696 asked for the appearance choice Chrome offers on Linux. That
/// option does not translate one-to-one — Chrome paints its own chrome from
/// the GTK or Qt theme, while everything inside our window is a web view we
/// already theme. What the desktop actually owns is the frame, so the frame
/// is what this switches.
///
/// **The platforms differ in kind, not in degree**, so the resolution lives
/// here rather than in three `if (platform)` branches in the interface:
///
/// - **Linux**: `app` drops the decorations and the interface draws its own
///   title bar. That is the route `docs/upstream-blockers.md` (B1) already
///   names as what we can do today about GTK3 client-side decorations
///   looking dated next to libadwaita.
/// - **macOS**: dropping the decorations would take the traffic lights with
///   them, and drawing our own would be the one thing a macOS user would
///   call *not* native. `app` makes the title bar a transparent overlay
///   instead, so the real traffic lights float over our own top bar.
/// - **Windows**: not offered. A frame we drew ourselves would lose Snap
///   Layouts and the system menu, which is a worse Windows than the one the
///   user has — and nobody asked. `supported` says so, and the resolver
///   below refuses `app` there even if the row says otherwise (a profile
///   database carried over from a Linux install).
const KEY_WINDOW_CHROME: &str = "ui.window_chrome";

/// Set when the stored choice could **not** be put on the window at
/// startup, for this session only.
///
/// What the interface draws follows what the backend reports, so reporting
/// the *stored* choice after a failed apply would have the frontend draw a
/// title bar over the system one the window in fact still has — two title
/// bars, from a preference nobody could honour. The preference itself is
/// left alone: it is the user's, the failure is this run's.
///
/// A process-wide mirror of a stored setting, like
/// [`PreferencesState::minimize_to_tray`] above — the difference being that
/// this one records what the window *accepted*, not what was asked for.
static WINDOW_CHROME_UNAPPLIED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowChrome {
    /// The desktop's own frame.
    System,
    /// WaveFlow draws it.
    App,
}

impl WindowChrome {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::App => "app",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "app" => Some(Self::App),
            _ => None,
        }
    }

    /// Whether this platform offers the choice at all.
    fn supported() -> bool {
        cfg!(any(target_os = "linux", target_os = "macos"))
    }

    /// The stored choice, narrowed to what this platform can honour.
    fn resolved(self) -> Self {
        if Self::supported() {
            self
        } else {
            Self::System
        }
    }

    /// What the interface has to draw for it — the one thing the frontend
    /// needs, already decided per platform:
    ///
    /// - `none`: the desktop's frame is there, nothing to add.
    /// - `titlebar`: no frame at all, draw one.
    /// - `overlay`: the frame is there but transparent over our content,
    ///   so leave the traffic lights room and give them a drag region.
    fn draw(self) -> &'static str {
        match self.resolved() {
            Self::System => "none",
            Self::App if cfg!(target_os = "macos") => "overlay",
            Self::App => "titlebar",
        }
    }
}

/// The choice narrowed twice: to what this platform can honour, and to
/// what this session's window actually accepted.
///
/// Pure, and takes the session flag rather than reading it, so the rule is
/// testable without a window to fail against.
fn effective_chrome(stored: WindowChrome, unapplied: bool) -> WindowChrome {
    if unapplied {
        WindowChrome::System
    } else {
        stored.resolved()
    }
}

/// What the frontend needs to know in one round-trip.
#[derive(Debug, Serialize)]
pub struct WindowChromeState {
    /// The stored choice, `"system"` or `"app"`.
    pub chrome: String,
    /// What to draw: `"none"`, `"titlebar"` or `"overlay"`.
    pub draw: String,
    /// Whether to offer the choice in Settings at all.
    pub supported: bool,
}

/// Record that the stored choice could not be put on the window. Called by
/// the startup path, which logs rather than fails — see
/// [`WINDOW_CHROME_UNAPPLIED`].
pub fn mark_window_chrome_unapplied() {
    WINDOW_CHROME_UNAPPLIED.store(true, Ordering::Relaxed);
}

/// Read the stored choice. A missing or unparseable row is `System`, which
/// is also what every platform looked like before this existed.
pub async fn load_window_chrome(app_db: &SqlitePool) -> WindowChrome {
    let raw: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(KEY_WINDOW_CHROME)
        .fetch_optional(app_db)
        .await
        .unwrap_or_else(|err| {
            tracing::warn!(?err, "reading the window chrome preference failed");
            None
        });
    raw.as_deref()
        .and_then(WindowChrome::parse)
        .unwrap_or(WindowChrome::System)
        .resolved()
}

/// Put the choice on the main window.
///
/// Called at startup **before the window is revealed** (the main window is
/// created hidden and shown by `restore_bounds_and_reveal`), so the user
/// never sees the frame they turned off appear and vanish; and again from
/// [`set_window_chrome`] when they change their mind, where the flicker is
/// the point — they asked to see the difference.
///
/// **Failure is returned, not swallowed.** What the interface draws is
/// decided by what is stored, so a stored `app` whose `set_decorations`
/// never landed would take the in-app title bar away from a window that
/// still has no frame of its own — leaving no way to move, resize or close
/// it. The caller that persists checks this first; the caller at startup
/// logs it, where the window is whatever it was created as and the
/// alternative would be to fail the launch over a frame.
pub fn apply_window_chrome(app: &tauri::AppHandle, chrome: WindowChrome) -> AppResult<()> {
    use tauri::Manager;

    let window = app.get_webview_window("main").ok_or_else(|| {
        crate::error::AppError::Other("window chrome: no main window".to_string())
    })?;
    let chrome = chrome.resolved();

    #[cfg(target_os = "macos")]
    {
        // The title first, deliberately: it is invisible either way while
        // the style is what the user sees, so a failure here stops before
        // anything has moved rather than halfway through.
        //
        // An overlay title bar draws the window's title over our own
        // content, and `hiddenTitle` is a creation-time option with no
        // runtime switch — so the title is what we empty, and restore with
        // the frame. The app is named by the menu bar either way.
        let previous_title = window.title().ok();
        let title = match chrome {
            WindowChrome::App => "",
            WindowChrome::System => "WaveFlow",
        };
        window.set_title(title).map_err(|err| {
            crate::error::AppError::Other(format!("window chrome: set_title: {err}"))
        })?;
        let style = match chrome {
            WindowChrome::App => tauri::utils::TitleBarStyle::Overlay,
            WindowChrome::System => tauri::utils::TitleBarStyle::Visible,
        };
        if let Err(err) = window.set_title_bar_style(style) {
            // Put the title back. Going first was what kept a failure from
            // being visible; leaving the window nameless because the style
            // refused would make it visible after all.
            if let Some(previous) = previous_title {
                let _ = window.set_title(&previous);
            }
            return Err(crate::error::AppError::Other(format!(
                "window chrome: set_title_bar_style: {err}"
            )));
        }
    }

    #[cfg(target_os = "linux")]
    window
        .set_decorations(matches!(chrome, WindowChrome::System))
        .map_err(|err| {
            crate::error::AppError::Other(format!("window chrome: set_decorations: {err}"))
        })?;

    // Windows keeps its frame; see `WindowChrome::supported`.
    #[cfg(target_os = "windows")]
    let _ = (&window, chrome);

    // Here, not at the caller that persists: the startup path RETRIES (ten
    // attempts, `restore_bounds_and_reveal`), so a first attempt that
    // failed and a second that worked would otherwise leave the flag
    // armed -- and the interface would stop drawing a title bar for a
    // window that really has no frame, which is the first defect with its
    // sign flipped.
    WINDOW_CHROME_UNAPPLIED.store(false, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn get_window_chrome(state: tauri::State<'_, AppState>) -> AppResult<WindowChromeState> {
    let chrome = effective_chrome(
        load_window_chrome(&state.app_db).await,
        WINDOW_CHROME_UNAPPLIED.load(Ordering::Relaxed),
    );
    Ok(WindowChromeState {
        chrome: chrome.as_str().to_string(),
        draw: chrome.draw().to_string(),
        supported: WindowChrome::supported(),
    })
}

/// Put the choice on the window, then persist it — in that order, so a
/// frame that refused to change is never recorded as the one in use. The
/// frontend has nothing to apply itself, which on macOS it could not do
/// anyway (`setTitle` and the title-bar style are separate calls that must
/// move together).
#[tauri::command]
pub async fn set_window_chrome(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    chrome: String,
) -> AppResult<WindowChromeState> {
    let parsed = WindowChrome::parse(&chrome).ok_or_else(|| {
        crate::error::AppError::Other(format!(
            "set_window_chrome: unsupported value '{chrome}' (expected system, app)"
        ))
    })?;
    // What the window has right now, for the rollback below.
    let previous = effective_chrome(
        load_window_chrome(&state.app_db).await,
        WINDOW_CHROME_UNAPPLIED.load(Ordering::Relaxed),
    );
    // Clears WINDOW_CHROME_UNAPPLIED on success, so a startup failure stops
    // describing a window that has since been changed.
    apply_window_chrome(&app, parsed)?;

    let persisted = sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'string', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(KEY_WINDOW_CHROME)
    .bind(parsed.as_str())
    .bind(Utc::now().timestamp_millis())
    .execute(&state.app_db)
    .await;
    if let Err(err) = persisted {
        // Put the frame back. A window wearing a frame the store knows
        // nothing about is the disagreement this whole path exists to
        // avoid: the caller is about to be told the change failed, and it
        // must be able to believe that. If even the rollback refuses, say
        // so -- the two states really have parted, and only a restart
        // (which reads the store) settles it.
        if let Err(revert) = apply_window_chrome(&app, previous) {
            tracing::error!(
                ?revert,
                "window chrome: could not put the previous frame back after a failed write"
            );
        }
        return Err(err.into());
    }

    let resolved = parsed.resolved();
    Ok(WindowChromeState {
        chrome: resolved.as_str().to_string(),
        draw: resolved.draw().to_string(),
        supported: WindowChrome::supported(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stored choice this session could not put on the window must not
    /// be reported as the one in use: the interface draws what it is told,
    /// so `app` after a failed apply is a title bar over the system one
    /// the window still has.
    #[test]
    fn a_chrome_that_could_not_be_applied_reports_as_system() {
        assert_eq!(
            effective_chrome(WindowChrome::App, true),
            WindowChrome::System
        );
        assert_eq!(
            effective_chrome(WindowChrome::System, true),
            WindowChrome::System
        );
    }

    /// With nothing to correct, the platform rule is the only one left --
    /// and it already refuses `app` where the choice is not offered.
    #[test]
    fn an_applied_chrome_is_reported_as_stored() {
        let expected = if WindowChrome::supported() {
            WindowChrome::App
        } else {
            WindowChrome::System
        };
        assert_eq!(effective_chrome(WindowChrome::App, false), expected);
        assert_eq!(
            effective_chrome(WindowChrome::System, false),
            WindowChrome::System
        );
    }
}
