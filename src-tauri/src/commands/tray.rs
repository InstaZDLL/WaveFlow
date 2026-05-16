//! Tray menu localisation bridge.
//!
//! The system tray menu (Play/Pause, Previous, Next, Open WaveFlow, Quit)
//! is created in Rust at startup before the frontend has loaded i18next,
//! so the labels are seeded in English and the frontend pushes a
//! localised set once `i18nReady` resolves — and again on every
//! `languageChanged`. The `MenuItem` handles are stashed in
//! [`TrayMenuItems`] so this command can call `set_text` without
//! rebuilding the menu.

use tauri::{menu::MenuItem, AppHandle, Manager, Runtime, State};

/// Holds the five user-facing tray `MenuItem`s so their labels can be
/// retitled at runtime when the UI language changes.
pub struct TrayMenuItems<R: Runtime> {
    pub play_pause: MenuItem<R>,
    pub previous: MenuItem<R>,
    pub next: MenuItem<R>,
    pub show: MenuItem<R>,
    pub quit: MenuItem<R>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayLabels {
    pub play_pause: String,
    pub previous: String,
    pub next: String,
    pub show: String,
    pub quit: String,
}

#[tauri::command]
pub fn set_tray_labels<R: Runtime>(
    app: AppHandle<R>,
    labels: TrayLabels,
) -> Result<(), String> {
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
    items.show.set_text(&labels.show)?;
    items.quit.set_text(&labels.quit)?;
    Ok(())
}
