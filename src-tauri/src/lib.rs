//! WaveFlow Tauri backend entry point.
//!
//! Sets up tracing, resolves filesystem paths, opens the global `app.db`
//! (running any pending migrations) and exposes the initial set of Tauri
//! commands to the frontend.

mod audio;
mod commands;
mod db;
mod deezer;
mod error;
mod lastfm;
mod lrclib;
mod metadata_artwork;
mod paths;
mod queue;
mod state;
mod watcher;

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{Manager, WindowEvent};

use audio::{AudioCmd, AudioEngine};
use state::AppState;
use watcher::WatcherManager;

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

            // Filesystem watcher manager. Holds one notify watcher per
            // `library_folder.is_watched=1` row in the active profile;
            // the boot-time hydration walks the DB and arms each one
            // so users don't need to re-toggle after a restart.
            let watcher_handle = app.handle().clone();
            let watcher = Arc::new(WatcherManager::new(watcher_handle));
            let watcher_for_init = watcher.clone();
            let restore_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = restore_handle.state::<AppState>();
                if let Ok(pool) = state.require_profile_pool().await {
                    if let Err(err) =
                        watcher_for_init.restore_from_db(&pool).await
                    {
                        tracing::warn!(%err, "watcher boot restore failed");
                    }
                }
            });
            app.manage(watcher);

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
            commands::library::list_library_folders,
            commands::library::set_folder_watched,
            commands::playlist::list_playlists,
            commands::playlist::get_playlist,
            commands::playlist::create_playlist,
            commands::playlist::update_playlist,
            commands::playlist::delete_playlist,
            commands::playlist::list_playlist_tracks,
            commands::playlist::add_track_to_playlist,
            commands::playlist::add_tracks_to_playlist,
            commands::playlist::remove_track_from_playlist,
            commands::playlist::reorder_playlist_track,
            commands::playlist::add_source_to_playlist,
            commands::scan::scan_folder,
            commands::track::list_tracks,
            commands::track::search_tracks,
            commands::track::toggle_like_track,
            commands::track::list_liked_track_ids,
            commands::track::list_liked_tracks,
            commands::browse::list_albums,
            commands::browse::list_artists,
            commands::browse::list_genres,
            commands::browse::list_folders,
            commands::browse::list_recent_plays,
            commands::browse::get_profile_stats,
            commands::browse::get_album_detail,
            commands::browse::get_artist_detail,
            commands::deezer::enrich_album_deezer,
            commands::deezer::enrich_artist_deezer,
            commands::integration::get_lastfm_api_key,
            commands::integration::set_lastfm_api_key,
            commands::lyrics::get_lyrics,
            commands::lyrics::fetch_lyrics,
            commands::lyrics::import_lrc_file,
            commands::lyrics::clear_lyrics,
            commands::player::player_get_state,
            commands::player::player_pause,
            commands::player::player_resume,
            commands::player::player_stop,
            commands::player::player_seek,
            commands::player::player_set_volume,
            commands::player::player_play_tracks,
            commands::player::player_add_to_queue,
            commands::player::player_play_next,
            commands::player::player_next,
            commands::player::player_previous,
            commands::player::player_toggle_shuffle,
            commands::player::player_cycle_repeat,
            commands::player::player_resume_last,
            commands::player::player_get_queue,
            commands::player::player_jump_to_index,
            commands::player::player_set_normalize,
            commands::player::player_set_mono,
            commands::player::player_set_crossfade,
            commands::player::player_get_audio_settings,
            commands::stats::stats_overview,
            commands::stats::stats_top_tracks,
            commands::stats::stats_top_artists,
            commands::stats::stats_top_albums,
            commands::stats::stats_listening_by_day,
            commands::stats::stats_listening_by_hour,
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

                    // Silence the cpal output IMMEDIATELY. The rtrb
                    // ring still holds a few hundred ms of decoded
                    // samples from before the user paused; without
                    // this flag, those samples flush to the device
                    // while we persist resume state and the stream
                    // tears down, producing a jarring ~2 s of audio
                    // at shutdown.
                    engine
                        .shared()
                        .paused_output
                        .store(true, Ordering::Release);

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
