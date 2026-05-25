//! Native OS toast notification on track change.
//!
//! Bridged to Windows Action Center, macOS NSUserNotification, and
//! libnotify on Linux through `tauri-plugin-notification`. Different
//! axis from the SMTC/MPRIS/MediaRemote handles owned by
//! [`crate::media_controls`]: those drive the OS *media* overlay (lock
//! screen, volume flyout, Now Playing widget), while a notification is
//! a transient toast the user sees fly in. The two can coexist — many
//! desktop music apps ship both.
//!
//! Opt-in via `app_setting['notifications.track_change']`. **Off by
//! default** because toast notifications are intrusive and trigger
//! Focus Assist / Do Not Disturb across all three platforms; we'd
//! rather have the user discover the toggle than spam everyone's
//! desktop on first launch.

use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

/// `app_setting` key controlling whether the toast fires on every
/// `player:track-changed`. Stored as a typed `bool` row.
pub const TRACK_CHANGE_KEY: &str = "notifications.track_change";

/// Read the persisted opt-in flag from `app_setting`. Defaults to
/// `false` (off) when the row is missing — opposite of Discord RPC's
/// default because toast notifications are intrusive (Focus Assist,
/// Do Not Disturb on macOS, etc.) and we want the user to opt in
/// rather than silently spam them on first launch.
pub async fn read_enabled(app_db: &SqlitePool) -> bool {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value FROM app_setting WHERE key = 'notifications.track_change'",
    )
    .fetch_optional(app_db)
    .await
    .ok()
    .flatten();
    matches!(raw.as_deref(), Some("true") | Some("1"))
}

/// Fire a single track-change toast. No-op when the user opted out or
/// the plugin failed to initialise on this platform. Always
/// best-effort: any error is logged but never bubbles up to the audio
/// engine — a missing libnotify on a headless Linux build must not
/// stall playback.
///
/// Called from [`crate::commands::player::emit_track_changed`] in a
/// dedicated tokio task so the SQLite lookup doesn't sit on the path
/// that flips the player bar metadata.
pub fn schedule(app: &AppHandle, title: String, artist: Option<String>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<crate::AppState>();
        if !read_enabled(&state.app_db).await {
            return;
        }
        // Build the body separately so we can drop the line entirely
        // when no artist is known — a blank "" body would render an
        // awkward empty second line on KDE Plasma.
        let body = artist.as_deref().unwrap_or("").to_string();
        let mut builder = app.notification().builder().title(title);
        if !body.is_empty() {
            builder = builder.body(body);
        }
        if let Err(err) = builder.show() {
            tracing::warn!(%err, "notifications: toast failed");
        }
    });
}
