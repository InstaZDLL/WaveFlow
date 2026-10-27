//! Bringing the main window up: the saved bounds, the splash handoff, and
//! the tray's "show".

use tauri::{AppHandle, Manager};

use crate::state::AppState;
use crate::{commands, webview_memory};

/// Minimum logical-pixel overlap for a saved window position to be considered
/// on-screen. Mirrors `MIN_VISIBLE_OVERLAP` in `miniPlayer.ts`.
const MIN_VISIBLE_OVERLAP: f64 = 80.0;

/// Returns `true` when the saved logical bounds overlap at least
/// [`MIN_VISIBLE_OVERLAP`] logical pixels with at least one available monitor.
/// Falls back to `true` (optimistic restore) when the monitor list cannot be
/// queried. Mirrors the frontend `boundsAreVisible` check in `miniPlayer.ts`.
fn bounds_on_screen(
    window: &tauri::WebviewWindow,
    b: &commands::preferences::MiniPlayerBounds,
) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        return true; // can't query — optimistically restore the saved position
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

/// Restore the persisted main-window size + position (if any) and then
/// reveal the window + close the splash. Shared between the `app://ready`
/// event handler and the 15-second fallback timer so both paths present
/// the window at the saved bounds with no visible jump.
///
/// Returns `true` only when the reveal succeeds (same contract as
/// `reveal_main_close_splash`). The caller should reset its `done` flag
/// to `false` on a `false` return so the partner path can retry.
///
/// Bounds restoration is deliberately **best-effort**: a `set_size` /
/// `set_position` failure is logged but never blocks the reveal. The
/// splash handoff's job is to guarantee the window becomes visible, and
/// the fallback retry loop is bounded (10 attempts) before it gives up on
/// the splash — so gating the reveal on a *cosmetic* bounds failure could
/// trap the user on the splash when `set_size` fails persistently. The
/// saved size/position is a nice-to-have; escaping the splash is not.
pub(super) async fn restore_bounds_and_reveal(app: AppHandle) -> bool {
    let state = app.state::<AppState>();
    // Before the reveal, not after: the window is created hidden, so a
    // user who asked WaveFlow to draw its own frame never sees the
    // desktop's appear and vanish (#696).
    if let Err(err) = commands::preferences::apply_window_chrome(
        &app,
        commands::preferences::load_window_chrome(&state.app_db).await,
    ) {
        // The window keeps whatever it was created with, which is the
        // desktop's frame. Worth a line, never worth failing a launch --
        // but the interface must be told, or it would draw a title bar
        // over the one this window still has.
        tracing::warn!(?err, "could not apply the stored window chrome");
        commands::preferences::mark_window_chrome_unapplied();
    }
    if let Some(bounds) = commands::preferences::load_main_window_bounds(&state.app_db).await {
        if let Some(window) = app.get_webview_window("main") {
            if let Err(err) = window.set_size(tauri::LogicalSize::new(bounds.width, bounds.height))
            {
                tracing::warn!(?err, "failed to restore main-window size");
            }
            // Only restore the saved position when it still overlaps a real
            // monitor — a disconnected display would otherwise place the
            // window fully off-screen with no easy way to drag it back.
            if bounds_on_screen(&window, &bounds) {
                if let Err(err) =
                    window.set_position(tauri::LogicalPosition::new(bounds.x, bounds.y))
                {
                    tracing::warn!(?err, "failed to restore main-window position");
                }
            }
        }
    }
    reveal_main_close_splash(&app)
}

/// Bring the main window back to the front (used by the tray's left
/// click and the "Ouvrir WaveFlow" menu item). No-op if the window
/// already exists and is showing — `set_focus` handles that case.
pub(super) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        webview_memory::sync(&window);
    }
}

/// Reveal the main window and close the splash, in that order.
///
/// Same ordering rule as the old frontend version: show main first,
/// then close splash, so there is never a moment where the desktop is
/// visible between the two on a multi-monitor / compositing setup.
///
/// Returns `true` only when *both* operations succeed (main shown +
/// splash closed, or splash already absent). On any failure, returns
/// `false` so the caller can clear its "done" flag and let the other
/// path (event listener or fallback timer) retry — without this,
/// a transient failure would leave the user stuck on an eternal
/// splash.
fn reveal_main_close_splash(app: &AppHandle) -> bool {
    // Bail out *before* touching the splash if the main window isn't
    // available or refuses to show — otherwise we'd close the only
    // visible window the user has and leave them staring at the
    // desktop with no way back into the app until they re-launch.
    let Some(main) = app.get_webview_window("main") else {
        tracing::warn!("splash handoff: main window missing at reveal time");
        return false;
    };
    if let Err(err) = main.show() {
        tracing::warn!(?err, "splash handoff: main.show failed");
        return false;
    }
    let _ = main.set_focus();
    webview_memory::sync(&main);
    if let Some(splash) = app.get_webview_window("splashscreen") {
        if let Err(err) = splash.close() {
            tracing::warn!(?err, "splash handoff: splash.close failed");
            return false;
        }
    }
    true
}
