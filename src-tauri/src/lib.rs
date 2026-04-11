//! WaveFlow Tauri backend entry point.
//!
//! Sets up tracing, resolves filesystem paths, opens the global `app.db`
//! (running any pending migrations) and exposes the initial set of Tauri
//! commands to the frontend.

mod audio;
mod commands;
mod db;
mod error;
mod paths;
mod state;

use std::sync::Arc;

use tauri::Manager;

use audio::AudioEngine;
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize structured logging. `RUST_LOG` overrides the default filter.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // Keep the default filter terse — lofty is noisy on malformed
                // MP4 atoms and sqlx logs every query at info level.
                tracing_subscriber::EnvFilter::new("info,sqlx=warn,lofty=error")
            }),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // Block on the async init — this runs once at startup before any
            // command can be dispatched, so blocking here is acceptable.
            let state = tauri::async_runtime::block_on(async move {
                AppState::init(&handle).await
            })?;

            app.manage(state);

            // Audio engine lives alongside AppState. Later checkpoints will
            // spawn the decoder thread and open the cpal output stream from
            // inside `AudioEngine::new`; for now it's a no-op placeholder so
            // player_* commands can type-check against the managed handle.
            let engine: Arc<AudioEngine> = AudioEngine::new();
            app.manage(engine);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info::get_app_info,
            commands::profile::list_profiles,
            commands::profile::get_active_profile,
            commands::profile::create_profile,
            commands::profile::switch_profile,
            commands::profile::deactivate_profile,
            commands::library::list_libraries,
            commands::library::create_library,
            commands::library::update_library,
            commands::library::delete_library,
            commands::library::rescan_library,
            commands::library::add_folder_to_library,
            commands::scan::scan_folder,
            commands::track::list_tracks,
            commands::browse::list_albums,
            commands::browse::list_artists,
            commands::browse::list_genres,
            commands::browse::list_folders,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
