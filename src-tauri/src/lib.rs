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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};

use audio::{AudioCmd, AudioEngine};
use queue::Direction;
use state::AppState;
use watcher::WatcherManager;

/// Set to `true` by the tray "Quitter" menu before calling `app.exit()`.
/// `WindowEvent::CloseRequested` checks the flag: if armed, the close
/// proceeds to actual shutdown; otherwise the close is intercepted and
/// the window is hidden instead (close-to-tray default).
struct QuitGate(AtomicBool);

const TRAY_ID: &str = "waveflow";

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
        .manage(QuitGate(AtomicBool::new(false)))
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

            // System tray (status icon).
            //
            // Menu: Lecture/Pause, Précédent, Suivant, Ouvrir WaveFlow,
            // Quitter. Left-click on the icon mirrors "Ouvrir WaveFlow"
            // for the common case where the window was hidden via the
            // close-to-tray path. Tooltip is updated to "Title — Artist"
            // by `commands::player::emit_track_changed` whenever a new
            // track starts.
            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(
                        app,
                        "play_pause",
                        "Lecture / Pause",
                        true,
                        None::<&str>,
                    )?,
                    &MenuItem::with_id(app, "previous", "Précédent", true, None::<&str>)?,
                    &MenuItem::with_id(app, "next", "Suivant", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "show", "Ouvrir WaveFlow", true, None::<&str>)?,
                    &PredefinedMenuItem::separator(app)?,
                    &MenuItem::with_id(app, "quit", "Quitter", true, None::<&str>)?,
                ],
            )?;

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
                    "play_pause" => toggle_play_pause(app),
                    "previous" => spawn_previous(app),
                    "next" => spawn_next(app),
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
            commands::player::player_reorder_queue,
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
        .on_window_event(|window, event| match event {
            // Close-to-tray: when the user clicks the window's "X" we
            // intercept the close, hide the window, and leave the
            // backend running so the tray menu can keep controlling
            // playback. The `QuitGate` is flipped to `true` by the
            // tray's "Quitter" menu before calling `app.exit()`, at
            // which point we let the close proceed normally.
            WindowEvent::CloseRequested { api, .. } => {
                let app = window.app_handle();
                let quitting = app.state::<QuitGate>().0.load(Ordering::Acquire);
                if !quitting {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            // Real shutdown path: fired only after the QuitGate has
            // been armed, so we can safely persist the resume point and
            // shut the audio engine down.
            WindowEvent::Destroyed => {
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
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Bring the main window back to the front (used by the tray's left
/// click and the "Ouvrir WaveFlow" menu item). No-op if the window
/// already exists and is showing — `set_focus` handles that case.
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Toggle Pause / Resume from the tray. Looks at the engine's current
/// state (atomic, no async needed) so the menu item works as a single
/// "Lecture / Pause" entry instead of two stateful labels we'd have to
/// keep in sync.
fn toggle_play_pause(app: &AppHandle) {
    let engine = app.state::<Arc<AudioEngine>>();
    let cmd = match engine.shared().state() {
        audio::PlayerState::Playing => AudioCmd::Pause,
        audio::PlayerState::Paused | audio::PlayerState::Idle => AudioCmd::Resume,
        // Loading / Ended → leave the decoder alone, it'll settle.
        _ => return,
    };
    if let Err(err) = engine.send(cmd) {
        tracing::warn!(%err, "tray play_pause: send failed");
    }
}

/// Tray "Suivant" — async because it touches the per-profile DB to
/// advance the queue cursor. Mirrors `commands::player::player_next`
/// but called outside the Tauri command pipeline so we own the
/// scheduling here.
fn spawn_next(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let engine = app.state::<Arc<AudioEngine>>();
        let pool = match state.require_profile_pool().await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(%err, "tray next: no profile pool");
                return;
            }
        };
        let profile_id = state.require_profile_id().await.ok();
        let repeat = queue::read_repeat_mode(&pool).await;
        let next = match queue::advance(&pool, Direction::Next, repeat).await {
            Ok(Some(track)) => track,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(%err, "tray next: advance failed");
                return;
            }
        };
        commands::player::emit_track_changed(&app, &state.paths, &next, profile_id);
        commands::player::emit_queue_changed(&app);
        let _ = engine.send(AudioCmd::LoadAndPlay {
            path: next.as_path(),
            start_ms: 0,
            track_id: next.id,
            duration_ms: next.duration_ms.max(0) as u64,
            source_type: "manual".into(),
            source_id: None,
        });
    });
}

/// Tray "Précédent" — same Spotify-style "seek to 0 if past 3 s, else
/// jump back" rule the in-app previous button uses.
fn spawn_previous(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let engine = app.state::<Arc<AudioEngine>>();
        if engine.shared().current_position_ms() > 3000 {
            let _ = engine.send(AudioCmd::Seek(0));
            return;
        }
        let pool = match state.require_profile_pool().await {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(%err, "tray previous: no profile pool");
                return;
            }
        };
        let profile_id = state.require_profile_id().await.ok();
        let repeat = queue::read_repeat_mode(&pool).await;
        let prev = match queue::advance(&pool, Direction::Previous, repeat).await {
            Ok(Some(track)) => track,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(%err, "tray previous: advance failed");
                return;
            }
        };
        commands::player::emit_track_changed(&app, &state.paths, &prev, profile_id);
        commands::player::emit_queue_changed(&app);
        let _ = engine.send(AudioCmd::LoadAndPlay {
            path: prev.as_path(),
            start_ms: 0,
            track_id: prev.id,
            duration_ms: prev.duration_ms.max(0) as u64,
            source_type: "manual".into(),
            source_id: None,
        });
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
