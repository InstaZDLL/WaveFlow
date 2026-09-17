//! The floating desktop lyrics window (issue #582).
//!
//! A third webview (label [`LABEL`]) that loads the same bundle with
//! `?lyrics=1`: transparent, undecorated, always on top, out of the
//! taskbar, independent of the main window (it stays up while the main
//! window is hidden to the tray).
//!
//! The window is owned here rather than created from JavaScript like the
//! mini-player, because it has three control surfaces that must agree —
//! the player bar's overflow menu, Settings, and the tray menu — and the
//! tray lives in Rust. One `open` / `close` / `set_locked` sequence, one
//! [`STATE_EVENT`] broadcast, and the tray's check marks follow from it.
//!
//! **Locked** means click-through: the OS stops delivering mouse input to
//! the window, so it cannot be dragged, resized or clicked, and nothing
//! inside it can offer a way back. Unlocking is therefore only ever done
//! from outside — the tray menu, the overflow menu or Settings. A new
//! window always starts unlocked, so a user who closed a locked overlay
//! is never handed back one they cannot move.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::commands::preferences::{load_desktop_lyrics_bounds, MiniPlayerBounds};
use crate::state::AppState;

/// Window label, and the query flag `main.tsx` branches on.
pub const LABEL: &str = "lyrics";

/// Broadcast to every window whenever the window opens, closes or its
/// lock changes. Payload: [`DesktopLyricsStatus`].
pub const STATE_EVENT: &str = "desktop-lyrics:state";

const DEFAULT_WIDTH: f64 = 760.0;
const DEFAULT_HEIGHT: f64 = 170.0;
const MIN_WIDTH: f64 = 320.0;
const MIN_HEIGHT: f64 = 90.0;
/// Distance from the bottom of the monitor for the first placement,
/// in logical pixels — clear of the Windows taskbar and the macOS Dock.
const BOTTOM_MARGIN: f64 = 120.0;
/// How much of a saved rectangle has to overlap a monitor, on both axes,
/// before it is trusted. Same rule as the mini-player: enough to grab,
/// so a monitor unplugged since does not strand the window off-screen.
const MIN_VISIBLE_OVERLAP: f64 = 80.0;

#[derive(Default)]
pub struct DesktopLyricsState {
    locked: AtomicBool,
    /// Held across every open / close decision. `open` awaits a database
    /// read between "is there a window?" and building one, so without it
    /// a close landing in that gap would find nothing to close and the
    /// window would appear anyway, and two opens would both try to build.
    lifecycle: tokio::sync::Mutex<()>,
    /// Set from the moment a close is asked for until `Destroyed` runs.
    /// `close()` only requests it, so the window is still there for a
    /// moment afterwards; an open landing in that moment would find it,
    /// merely show it, and then watch it disappear.
    closing: AtomicBool,
    destroyed: tokio::sync::Notify,
}

/// How long an open waits for a closing window to finish going away. Past
/// it the open goes ahead with whatever is there rather than hanging a
/// tray click forever, and the warning is what makes that visible.
const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DesktopLyricsStatus {
    pub open: bool,
    pub locked: bool,
    /// The window is a native Wayland surface, whose compositor is free to
    /// ignore "always on top" and to place it wherever it likes. Settings
    /// says so rather than letting the user think the option is broken.
    pub wayland: bool,
}

/// Running as a Wayland client. `GDK_BACKEND=x11` forces XWayland, where
/// both guarantees hold again, so it is checked first.
fn is_wayland_client() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if std::env::var("GDK_BACKEND").is_ok_and(|b| b.starts_with("x11")) {
        return false;
    }
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn status(app: &AppHandle) -> DesktopLyricsStatus {
    let open = app.get_webview_window(LABEL).is_some();
    DesktopLyricsStatus {
        open,
        wayland: is_wayland_client(),
        locked: open
            && app
                .state::<DesktopLyricsState>()
                .locked
                .load(Ordering::Acquire),
    }
}

/// Open the window, or show it if it already exists.
pub async fn open(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<DesktopLyricsState>();
    let _lifecycle = state.lifecycle.lock().await;
    open_locked(app).await
}

async fn open_locked(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<DesktopLyricsState>();
    // Registered before the flag is read: `notify_waiters` reaches a
    // `Notified` from the moment it exists, so a `Destroyed` landing
    // between the check and the await is not missed.
    let destroyed = state.destroyed.notified();
    let still_closing =
        state.closing.load(Ordering::Acquire) && app.get_webview_window(LABEL).is_some();
    if still_closing && tokio::time::timeout(CLOSE_WAIT, destroyed).await.is_err() {
        tracing::warn!("desktop lyrics: previous window still closing after the wait");
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show()?;
        publish(app, status(app));
        return Ok(());
    }

    let saved = load_desktop_lyrics_bounds(&app.state::<AppState>().app_db)
        .await
        .filter(|b| overlaps_a_monitor(app, b));
    let (position, width, height) = match saved {
        Some(b) => (
            Some((b.x, b.y)),
            b.width.max(MIN_WIDTH),
            b.height.max(MIN_HEIGHT),
        ),
        None => (default_position(app), DEFAULT_WIDTH, DEFAULT_HEIGHT),
    };

    let url = WebviewUrl::App("index.html?lyrics=1".into());
    let mut builder = WebviewWindowBuilder::new(app, LABEL, url)
        .title("WaveFlow")
        .inner_size(width, height)
        .min_inner_size(MIN_WIDTH, MIN_HEIGHT)
        .decorations(false)
        .transparent(true)
        // An undecorated transparent window still gets a drop shadow
        // on Windows and macOS, which draws a grey box around text
        // that is meant to float on the desktop.
        .shadow(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(true)
        // Opening it from the tray or the player bar must not pull
        // focus away from whatever the user is working in.
        .focused(false);
    builder = match position {
        Some((x, y)) => builder.position(x, y),
        None => builder.center(),
    };
    builder.build()?;

    app.state::<DesktopLyricsState>()
        .locked
        .store(false, Ordering::Release);
    publish(app, status(app));
    Ok(())
}

pub async fn close(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<DesktopLyricsState>();
    let _lifecycle = state.lifecycle.lock().await;
    close_locked(app)
}

fn close_locked(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(LABEL) {
        mark_closing(app);
        window.close()?;
    }
    Ok(())
}

/// Also called from `WindowEvent::CloseRequested`, which covers a close
/// that did not come through here (Alt+F4, the overlay's own button
/// before it reaches the command).
pub fn mark_closing(app: &AppHandle) {
    app.state::<DesktopLyricsState>()
        .closing
        .store(true, Ordering::Release);
}

/// Turn click-through on or off. A no-op (that still republishes) when
/// the window is not open, so a stale "unlock" from a menu opened before
/// the window closed cannot fail.
pub fn set_locked(app: &AppHandle, locked: bool) -> tauri::Result<()> {
    let state = app.state::<DesktopLyricsState>();
    match app.get_webview_window(LABEL) {
        Some(window) => {
            window.set_ignore_cursor_events(locked)?;
            state.locked.store(locked, Ordering::Release);
        }
        None => state.locked.store(false, Ordering::Release),
    }
    publish(app, status(app));
    Ok(())
}

/// Called from `WindowEvent::Destroyed` for this window. The manager may
/// still hold the window while the event runs, so the status is stated
/// rather than read back.
pub fn on_destroyed(app: &AppHandle) {
    let state = app.state::<DesktopLyricsState>();
    state.locked.store(false, Ordering::Release);
    state.closing.store(false, Ordering::Release);
    state.destroyed.notify_waiters();
    publish(
        app,
        DesktopLyricsStatus {
            open: false,
            locked: false,
            wayland: is_wayland_client(),
        },
    );
}

/// Tray entry: open when closed, close when open. The existence check
/// happens under the lifecycle lock, so it decides on the state the
/// previous open or close left rather than on one still in flight.
pub fn toggle_from_tray(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<DesktopLyricsState>();
        let _lifecycle = state.lifecycle.lock().await;
        let result = if app.get_webview_window(LABEL).is_some() {
            close_locked(&app)
        } else {
            open_locked(&app).await
        };
        if let Err(err) = result {
            tracing::warn!(%err, "desktop lyrics: toggle from tray failed");
        }
    });
}

pub fn toggle_lock_from_tray(app: &AppHandle) {
    let next = !status(app).locked;
    if let Err(err) = set_locked(app, next) {
        tracing::warn!(%err, "desktop lyrics: lock toggle from tray failed");
    }
}

fn publish(app: &AppHandle, status: DesktopLyricsStatus) {
    crate::commands::tray::sync_desktop_lyrics(app, status);
    if let Err(err) = app.emit(STATE_EVENT, status) {
        tracing::warn!(%err, "desktop lyrics: state broadcast failed");
    }
}

/// Bottom-centre of the primary monitor, in logical pixels.
fn default_position(app: &AppHandle) -> Option<(f64, f64)> {
    let monitor = app.primary_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let x = monitor.position().x as f64 / scale;
    let y = monitor.position().y as f64 / scale;
    let w = monitor.size().width as f64 / scale;
    let h = monitor.size().height as f64 / scale;
    Some((
        x + ((w - DEFAULT_WIDTH) / 2.0).max(0.0),
        y + (h - DEFAULT_HEIGHT - BOTTOM_MARGIN).max(0.0),
    ))
}

fn overlaps_a_monitor(app: &AppHandle, b: &MiniPlayerBounds) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return false;
    };
    monitors.iter().any(|m| {
        let scale = m.scale_factor();
        let mx = m.position().x as f64 / scale;
        let my = m.position().y as f64 / scale;
        let mw = m.size().width as f64 / scale;
        let mh = m.size().height as f64 / scale;
        let overlap_x = (b.x + b.width).min(mx + mw) - b.x.max(mx);
        let overlap_y = (b.y + b.height).min(my + mh) - b.y.max(my);
        overlap_x >= MIN_VISIBLE_OVERLAP && overlap_y >= MIN_VISIBLE_OVERLAP
    })
}
