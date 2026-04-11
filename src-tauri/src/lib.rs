//! WaveFlow Tauri backend entry point.
//!
//! Sets up tracing, resolves filesystem paths, opens the global `app.db`
//! (running any pending migrations) and exposes the initial set of Tauri
//! commands to the frontend.

mod commands;
mod db;
mod error;
mod paths;
mod state;

use tauri::Manager;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize structured logging. `RUST_LOG` overrides the default filter.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,sqlx=warn")),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();

            // Block on the async init — this runs once at startup before any
            // command can be dispatched, so blocking here is acceptable.
            let state = tauri::async_runtime::block_on(async move {
                AppState::init(&handle).await
            })?;

            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info::get_app_info,
            commands::profile::list_profiles,
            commands::profile::get_active_profile,
            commands::profile::create_profile,
            commands::profile::switch_profile,
            commands::profile::deactivate_profile,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
