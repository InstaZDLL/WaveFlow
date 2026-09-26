//! Tauri implementation of the shared playback host boundary.
pub fn spawn<F>(_: AppHandle, future: F) -> tauri::async_runtime::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tauri::async_runtime::spawn(future)
}
pub use tauri::{AppHandle, Emitter, Manager};

pub fn update_playback(
    app: &AppHandle,
    state: crate::audio::PlayerState,
    position: u64,
    discord: bool,
) {
    if let Some(controls) = app.try_state::<crate::media_controls::MediaControlsHandle>() {
        controls.update_playback(state, position);
    }
    if discord {
        if let Some(presence) = app.try_state::<crate::discord_presence::DiscordPresenceHandle>() {
            presence.update_playback(state, position);
        }
    }
}
