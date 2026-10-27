//! The system tray: its icon and menu, and what each entry does.

use std::sync::atomic::Ordering;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::QuitGate;
use crate::{commands, desktop_lyrics, player_actions};

use super::window::show_main_window;

const TRAY_ID: &str = "waveflow";

/// Build the tray icon and its menu, and manage the item handles
/// `set_tray_labels` retitles.
pub(super) fn build(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    // System tray (status icon).
    //
    // Labels are seeded in English because Rust runs before the
    // frontend has had a chance to load i18next; the React layer
    // pushes a localised set via `set_tray_labels` once
    // `i18nReady` resolves, and again on every `languageChanged`
    // event. The `MenuItem` handles are stashed in
    // `TrayMenuItems` so retitling doesn't rebuild the menu.
    // Left-click on the icon mirrors "Open WaveFlow" for the
    // common case where the window was hidden via the
    // close-to-tray path. Tooltip is updated to "Title — Artist"
    // by `commands::player::emit_track_changed` whenever a new
    // track starts.
    let play_pause_item = MenuItem::with_id(app, "play_pause", "Play / Pause", true, None::<&str>)?;
    let previous_item = MenuItem::with_id(app, "previous", "Previous", true, None::<&str>)?;
    let next_item = MenuItem::with_id(app, "next", "Next", true, None::<&str>)?;
    // Desktop lyrics (#582). Both start unchecked and "lock"
    // disabled: the window never survives a restart, and
    // `tray::sync_desktop_lyrics` keeps the marks in step after.
    let desktop_lyrics_item = CheckMenuItem::with_id(
        app,
        "desktop_lyrics",
        "Desktop lyrics",
        true,
        false,
        None::<&str>,
    )?;
    let desktop_lyrics_lock_item = CheckMenuItem::with_id(
        app,
        "desktop_lyrics_lock",
        "Lock desktop lyrics",
        false,
        false,
        None::<&str>,
    )?;
    let show_item = MenuItem::with_id(app, "show", "Open WaveFlow", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &play_pause_item,
            &previous_item,
            &next_item,
            &PredefinedMenuItem::separator(app)?,
            &desktop_lyrics_item,
            &desktop_lyrics_lock_item,
            &PredefinedMenuItem::separator(app)?,
            &show_item,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    app.manage(commands::tray::TrayMenuItems {
        play_pause: play_pause_item,
        previous: previous_item,
        next: next_item,
        desktop_lyrics: desktop_lyrics_item,
        desktop_lyrics_lock: desktop_lyrics_lock_item,
        show: show_item,
        quit: quit_item,
    });

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or("default window icon missing")?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("WaveFlow")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "play_pause" => player_actions::toggle_play_pause(app, "tray"),
            "previous" => spawn_previous(app),
            "next" => spawn_next(app),
            "desktop_lyrics" => desktop_lyrics::toggle_from_tray(app),
            "desktop_lyrics_lock" => desktop_lyrics::toggle_lock_from_tray(app),
            "show" => show_main_window(app),
            "quit" => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Tray "Suivant" — advance the queue and play what lands.
///
/// Delegates to [`player_actions`], shared with the OS media controls
/// and the MPD server so all three emit the same events; the tray menu
/// handler is sync, hence the spawn.
fn spawn_next(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        player_actions::next(&app, "tray").await;
    });
}

/// Tray "Précédent" — same "seek to 0 if past 3 s, else
/// jump back" rule the in-app previous button uses, implemented once in
/// [`player_actions::previous`].
fn spawn_previous(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        player_actions::previous(&app, "tray").await;
    });
}

/// Tray "Quitter" — arms the QuitGate so `WindowEvent::CloseRequested`
/// stops intercepting close, then asks the app to exit. The window
/// closes, fires `Destroyed`, and the existing teardown logic
/// persists the resume point and shuts the audio engine down.
fn request_quit(app: &AppHandle) {
    app.state::<QuitGate>().0.store(true, Ordering::Release);
    app.exit(0);
}
