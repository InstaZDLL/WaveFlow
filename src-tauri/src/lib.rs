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
mod queue;
mod state;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{Manager, WindowEvent};

use audio::{AudioCmd, AudioEngine};
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
            let init_handle = app.handle().clone();
            let engine_handle = app.handle().clone();

            // Block on the async init — this runs once at startup before any
            // command can be dispatched, so blocking here is acceptable.
            let state = tauri::async_runtime::block_on(async move {
                AppState::init(&init_handle).await
            })?;

            app.manage(state);

            // Audio engine lives alongside AppState. `new` spawns the cpal
            // output thread (silence callback) and the decoder thread, both
            // receiving a clone of the AppHandle so they can emit Tauri
            // events (player:position, player:state, player:track-ended,
            // player:error) directly.
            let engine: Arc<AudioEngine> = AudioEngine::new(engine_handle);
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
            commands::player::player_get_state,
            commands::player::player_pause,
            commands::player::player_resume,
            commands::player::player_stop,
            commands::player::player_seek,
            commands::player::player_set_volume,
            commands::player::player_play_tracks,
            commands::player::player_next,
            commands::player::player_previous,
            commands::player::player_toggle_shuffle,
            commands::player::player_cycle_repeat,
            commands::player::player_resume_last,
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                // Persist the resume point before the app tears down
                // so the next launch can pick up where the user left
                // off. Block_on is acceptable here — the window is
                // already gone and we're on the shutdown path.
                let app = window.app_handle().clone();
                let _ = tauri::async_runtime::block_on(async move {
                    let state = app.state::<AppState>();
                    let engine = app.state::<Arc<AudioEngine>>();
                    let track_id = engine
                        .shared()
                        .current_track_id
                        .load(Ordering::Acquire);
                    let position_ms = engine.shared().current_position_ms();
                    if track_id > 0 {
                        if let Ok(pool) = state.require_profile_pool().await {
                            let _ = queue::persist_resume_point(
                                &pool,
                                track_id,
                                position_ms,
                            )
                            .await;
                        }
                    }
                    // Tell the decoder thread to stop and drop the
                    // cpal stream cleanly.
                    let _ = engine.send(AudioCmd::Shutdown);
                    Ok::<_, error::AppError>(())
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
