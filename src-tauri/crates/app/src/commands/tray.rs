//! Tray menu localisation bridge.
//!
//! The system tray menu (Play/Pause, Previous, Next, Desktop lyrics, Lock
//! desktop lyrics, Open WaveFlow, Quit)
//! is created in Rust at startup before the frontend has loaded i18next,
//! so the labels are seeded in English and the frontend pushes a
//! localised set once `i18nReady` resolves — and again on every
//! `languageChanged`. The `MenuItem` handles are stashed in
//! [`TrayMenuItems`] so this command can call `set_text` without
//! rebuilding the menu.
//!
//! The same push carries the tooltips of the playback buttons under the
//! taskbar thumbnail on Windows (`crate::taskbar_buttons`), which are
//! built at startup for the same reason.

use tauri::{
    menu::{CheckMenuItem, MenuItem},
    AppHandle, Manager, Runtime, State,
};

use crate::desktop_lyrics::DesktopLyricsStatus;

/// Holds the user-facing tray items so their labels can be retitled at
/// runtime when the UI language changes, and the two desktop lyrics check
/// marks kept in step with the window.
pub struct TrayMenuItems<R: Runtime> {
    pub play_pause: MenuItem<R>,
    pub previous: MenuItem<R>,
    pub next: MenuItem<R>,
    pub desktop_lyrics: CheckMenuItem<R>,
    pub desktop_lyrics_lock: CheckMenuItem<R>,
    pub show: MenuItem<R>,
    pub quit: MenuItem<R>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    pub play_pause: String,
    pub previous: String,
    pub next: String,
    pub desktop_lyrics: String,
    pub desktop_lyrics_lock: String,
    pub show: String,
    pub quit: String,
    /// The taskbar play/pause button shows one or the other, following
    /// the player state, where the tray menu has a single entry. Only
    /// Windows has that button; elsewhere the frontend still sends both.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub play: String,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub pause: String,
}

#[tauri::command]
pub fn set_tray_labels<R: Runtime>(app: AppHandle<R>, labels: TrayLabels) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    if let Some(buttons) = app.try_state::<crate::taskbar_buttons::TaskbarButtons>() {
        buttons.set_labels(crate::taskbar_buttons::Labels {
            previous: labels.previous.clone(),
            play: labels.play.clone(),
            pause: labels.pause.clone(),
            next: labels.next.clone(),
        });
    }
    let Some(items) = app.try_state::<TrayMenuItems<R>>() else {
        return Ok(());
    };
    apply(&items, &labels).map_err(|e| e.to_string())
}

fn apply<R: Runtime>(
    items: &State<'_, TrayMenuItems<R>>,
    labels: &TrayLabels,
) -> tauri::Result<()> {
    items.play_pause.set_text(&labels.play_pause)?;
    items.previous.set_text(&labels.previous)?;
    items.next.set_text(&labels.next)?;
    items.desktop_lyrics.set_text(&labels.desktop_lyrics)?;
    items
        .desktop_lyrics_lock
        .set_text(&labels.desktop_lyrics_lock)?;
    items.show.set_text(&labels.show)?;
    items.quit.set_text(&labels.quit)?;
    Ok(())
}

/// Put the desktop lyrics check marks in step with the window (issue
/// #582). A check item flips its own mark when clicked on some platforms,
/// before the click has done anything, so the marks are always set from
/// the real state rather than left to the menu. "Lock" is only offered
/// while the window is open: locking nothing would be a promise the next
/// window, which always opens unlocked, does not keep.
pub fn sync_desktop_lyrics(app: &AppHandle, status: DesktopLyricsStatus) {
    let Some(items) = app.try_state::<TrayMenuItems<tauri::Wry>>() else {
        return;
    };
    let result = items
        .desktop_lyrics
        .set_checked(status.open)
        .and_then(|()| items.desktop_lyrics_lock.set_enabled(status.open))
        .and_then(|()| items.desktop_lyrics_lock.set_checked(status.locked));
    if let Err(err) = result {
        tracing::warn!(%err, "tray: desktop lyrics check marks not updated");
    }
}
