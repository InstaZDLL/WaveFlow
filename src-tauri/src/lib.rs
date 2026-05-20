//! WaveFlow Tauri backend entry point.
//!
//! Sets up tracing, resolves filesystem paths, opens the global `app.db`
//! (running any pending migrations) and exposes the initial set of Tauri
//! commands to the frontend.

mod analysis;
mod audio;
mod backup;
mod commands;
mod db;
mod deezer;
mod discord_presence;
mod dlna;
mod error;
mod lastfm;
mod logging;
mod lrclib;
mod media_controls;
mod metadata_artwork;
mod offline;
mod paths;
mod queue;
mod scrobbler;
mod smart_playlists;
mod spotify;
mod state;
mod thumbnails;
mod watcher;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Listener, Manager, WindowEvent,
};

use audio::{AudioCmd, AudioEngine};
use queue::Direction;
use state::AppState;
use watcher::WatcherManager;

/// Set to `true` by the tray "Quit" menu before calling `app.exit()`.
/// `WindowEvent::CloseRequested` checks the flag: if armed, the close
/// proceeds to actual shutdown; otherwise the close is intercepted and
/// the window is hidden instead (close-to-tray default).
struct QuitGate(AtomicBool);

const TRAY_ID: &str = "waveflow";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Initialize structured logging. `RUST_LOG` overrides the default
    // filter. The returned guard owns the non-blocking file writer's
    // background thread and must be held until the process exits — a
    // dropped guard flushes and closes the file early, losing the tail
    // of the log right when a crash report is most useful.
    let _log_guard = logging::init_tracing();

    // `mut` is only consumed when the updater plugin is wired in (release
    // builds); the lint would fire in debug otherwise.
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        // Single-instance MUST be the first plugin so a second launch
        // exits cleanly before any heavy init (pool open, audio engine,
        // tray, watchers) runs in the duplicate process.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // Autostart wiring. Pass `--minimized` so the OS-launched
        // instance shows up in the tray without grabbing focus — the
        // user expressed intent to "start with the system", not "open
        // a window every boot". The frontend reads `?autostart=1` from
        // `argv` if it ever wants to surface a "launched on boot"
        // banner; not used today.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());

    // Auto-updater is wired in release builds only. In dev (`tauri dev`)
    // the binary points at the local source tree, the version is the
    // working copy, and there's no signed manifest to fetch — the
    // plugin would just spam errors. Ship-only by design.
    #[cfg(not(debug_assertions))]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder
        .manage(QuitGate(AtomicBool::new(false)))
        .setup(|app| {
            let init_handle = app.handle().clone();
            let engine_handle = app.handle().clone();

            // Block on the async init — this runs once at startup before any
            // command can be dispatched, so blocking here is acceptable.
            let state =
                tauri::async_runtime::block_on(async move { AppState::init(&init_handle).await })?;

            // Hydrate the close-to-tray flag once at boot. The atomic
            // is the source of truth for `WindowEvent::CloseRequested`
            // so it has to be in place before the first user input.
            let minimize_to_tray = tauri::async_runtime::block_on(
                commands::preferences::load_minimize_to_tray(&state.app_db),
            );
            app.manage(commands::preferences::PreferencesState::new(
                minimize_to_tray,
            ));

            app.manage(state);

            // Audio engine lives alongside AppState. `new` spawns the cpal
            // output thread (silence callback) and the decoder thread, both
            // receiving a clone of the AppHandle so they can emit Tauri
            // events (player:position, player:state, player:track-ended,
            // player:error) directly.
            //
            // Pull the persisted output-device name (if any) from the
            // active profile so the user lands on whatever output they
            // last picked instead of the OS default. Empty string in
            // the row means "follow the OS default" — see
            // `player_set_output_device`.
            let (persisted_device, persisted_wasapi_exclusive) =
                tauri::async_runtime::block_on(async {
                    let state = app.state::<AppState>();
                    let Ok(pool) = state.require_profile_pool().await else {
                        return (None, false);
                    };
                    let device: Option<String> = sqlx::query_scalar(
                        "SELECT value FROM profile_setting WHERE key = 'audio.output_device'",
                    )
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten()
                    .filter(|s: &String| !s.is_empty());
                    let exclusive: bool = sqlx::query_scalar::<_, String>(
                        "SELECT value FROM profile_setting WHERE key = 'audio.wasapi_exclusive'",
                    )
                    .fetch_optional(&pool)
                    .await
                    .ok()
                    .flatten()
                    .map(|s| s == "1" || s == "true")
                    .unwrap_or(false);
                    (device, exclusive)
                });
            let engine: Arc<AudioEngine> = AudioEngine::new_with_device(
                engine_handle,
                persisted_device,
                persisted_wasapi_exclusive,
            );
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
                    if let Err(err) = watcher_for_init.restore_from_db(&pool).await {
                        tracing::warn!(%err, "watcher boot restore failed");
                    }
                }
            });
            app.manage(watcher);

            // Scan-on-start. Honours `profile_setting['library.scan_on_start']`
            // (default OFF — opt in so power users with terabyte libraries
            // don't pay the I/O at every launch). The rescan walks every
            // `library_folder` row of the active profile and runs the
            // same `scan_folder_inner` path the manual "Rescan" button
            // uses, so the `scan:progress` toast surfaces automatically.
            // Fire-and-forget — failures log but never block startup.
            let scan_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = scan_handle.state::<AppState>();
                let Ok(pool) = state.require_profile_pool().await else {
                    return;
                };
                let enabled: bool = sqlx::query_scalar::<_, String>(
                    "SELECT value FROM profile_setting WHERE key = 'library.scan_on_start'",
                )
                .fetch_optional(&pool)
                .await
                .ok()
                .flatten()
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
                if !enabled {
                    return;
                }
                let Ok(profile_id) = state.require_profile_id().await else {
                    return;
                };
                let artwork_dir = state.paths.profile_artwork_dir(profile_id);
                let folder_ids: Vec<i64> =
                    match sqlx::query_scalar("SELECT id FROM library_folder ORDER BY id")
                        .fetch_all(&pool)
                        .await
                    {
                        Ok(rows) => rows,
                        Err(err) => {
                            tracing::warn!(%err, "scan-on-start: list folders failed");
                            return;
                        }
                    };
                for folder_id in folder_ids {
                    if let Err(err) = commands::scan::scan_folder_inner(
                        &pool,
                        &artwork_dir,
                        folder_id,
                        Some(&scan_handle),
                    )
                    .await
                    {
                        tracing::warn!(folder_id, %err, "scan-on-start: folder scan failed");
                    }
                }
            });

            // OS media controls (SMTC / MPRIS / MediaRemote) via
            // souvlaki. Needs the main window to exist for HWND on
            // Windows; on Linux/macOS the platform integration owns
            // its own connection. Failure is non-fatal — playback
            // keeps working without the OS overlay.
            if let Some(controls) = media_controls::init(app.handle().clone()) {
                app.manage(controls);
            }

            // Discord Rich Presence. Always spawn the worker thread —
            // it stays idle until the user flips the opt-in toggle.
            // Reading the persisted flag here means the activity is
            // restored on the very next track-changed event without
            // waiting for the Settings view to mount.
            let initial_rpc_enabled = tauri::async_runtime::block_on(async {
                let state = app.state::<AppState>();
                discord_presence::read_enabled(&state.app_db).await
            });
            if let Some(presence) = discord_presence::init(initial_rpc_enabled) {
                app.manage(presence);
            }

            // Last.fm scrobble worker. Polls scrobble_queue every 30 s
            // and posts eligible items to track.scrobble; survives
            // profile switches because it always reads the active
            // profile's pool fresh per tick.
            scrobbler::spawn(app.handle().clone());

            // Auto-backup loop. Idle while `backup.enabled` is off
            // (parks on a Notify channel), wakes immediately when the
            // user toggles via Settings. See [`backup`] module docs.
            let backup_handle = backup::BackupHandle::new();
            app.manage(backup_handle.clone());
            backup::spawn_backup_loop(app.handle().clone(), backup_handle);

            // DLNA / UPnP MediaServer auto-start. Reads the persisted
            // `dlna.enabled` flag at boot and forwards the config to
            // the worker so users don't have to flip the toggle every
            // launch. Failure is non-fatal — the Settings page will
            // surface `last_error` next time the user opens it.
            tauri::async_runtime::spawn({
                let handle = app.handle().clone();
                async move {
                    let state = handle.state::<AppState>();
                    let cfg = match dlna::config::load(&state.app_db).await {
                        Ok(c) => c,
                        Err(err) => {
                            tracing::warn!(?err, "DLNA config load failed");
                            return;
                        }
                    };
                    if cfg.enabled {
                        match commands::dlna::build_resources(&state).await {
                            Ok(resources) => state.dlna.start(cfg, resources),
                            Err(err) => tracing::warn!(?err, "DLNA boot resources unavailable"),
                        }
                    }
                }
            });

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
            let play_pause_item =
                MenuItem::with_id(app, "play_pause", "Play / Pause", true, None::<&str>)?;
            let previous_item = MenuItem::with_id(app, "previous", "Previous", true, None::<&str>)?;
            let next_item = MenuItem::with_id(app, "next", "Next", true, None::<&str>)?;
            let show_item = MenuItem::with_id(app, "show", "Open WaveFlow", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

            let menu = Menu::with_items(
                app,
                &[
                    &play_pause_item,
                    &previous_item,
                    &next_item,
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

            // Splash → main handoff.
            //
            // The frontend emits `app://ready` once the React root has
            // committed its first useful paint (see src/main.tsx). When
            // we get that event we close the splash and reveal the
            // main window from native code — more reliable than the
            // previous IPC dance, which on Linux WebKitGTK 2.52 raced
            // against the heavy first-launch init (migrations + DB
            // pool + library scan) and left the splash hanging
            // forever (issue #42).
            //
            // A 15 s safety-net timer force-reveals the main window if
            // `app://ready` never fires — guards against a frontend
            // crash leaving the user stuck on an eternal splash.
            let handoff_done = Arc::new(AtomicBool::new(false));
            let handoff_handle = app.handle().clone();
            let handoff_done_for_event = handoff_done.clone();
            app.listen("app://ready", move |_event| {
                if handoff_done_for_event.swap(true, Ordering::SeqCst) {
                    return;
                }
                // Reset the flag if the reveal fails so the fallback
                // timer (or a subsequent `app://ready` re-emission) can
                // retry — otherwise a transient main.show() / splash.close()
                // failure would leave the user stuck on an eternal splash
                // with no recovery path.
                if !reveal_main_close_splash(&handoff_handle) {
                    handoff_done_for_event.store(false, Ordering::SeqCst);
                }
            });

            let fallback_handle = app.handle().clone();
            let fallback_done = handoff_done.clone();
            tauri::async_runtime::spawn(async move {
                // First attempt after 15 s; subsequent retries every
                // 250 ms up to 10 total attempts. Bounded so a
                // permanently missing main window doesn't spin forever
                // (the warn! log surfaces it instead). The retry exists
                // because `ReadySignal` only emits `app://ready` once
                // at mount — if we lost the race with that single
                // event AND the first reveal failed, without a retry
                // the user would be stuck on the splash.
                tokio::time::sleep(Duration::from_secs(15)).await;
                for attempt in 0..10 {
                    if fallback_done.swap(true, Ordering::SeqCst) {
                        return;
                    }
                    tracing::warn!(
                        attempt,
                        "splash handoff fallback: `app://ready` never fired in time, force-revealing main window"
                    );
                    if reveal_main_close_splash(&fallback_handle) {
                        return;
                    }
                    fallback_done.store(false, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                tracing::error!(
                    "splash handoff fallback: exhausted 10 reveal attempts, user is likely stuck on splash"
                );
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info::get_app_info,
            commands::app_info::open_data_folder,
            commands::changelog::get_changelog,
            commands::diagnostics::get_log_dir,
            commands::diagnostics::open_log_folder,
            commands::diagnostics::read_recent_logs,
            commands::profile::list_profiles,
            commands::profile::get_active_profile,
            commands::profile::create_profile,
            commands::profile::switch_profile,
            commands::profile::deactivate_profile,
            commands::profile::get_profile_setting,
            commands::profile::set_profile_setting,
            commands::profile_io::export_profile,
            commands::profile_io::import_profile,
            commands::library::list_libraries,
            commands::library::create_library,
            commands::library::update_library,
            commands::library::delete_library,
            commands::library::rescan_library,
            commands::library::add_folder_to_library,
            commands::library::list_library_folders,
            commands::library::set_folder_watched,
            commands::library::remove_folder_from_library,
            commands::library::import_paths,
            commands::duplicates::find_duplicates,
            commands::duplicates::delete_tracks,
            commands::analysis::analyze_track,
            commands::analysis::analyze_library,
            commands::analysis::get_track_analysis,
            commands::analysis::get_auto_analyze,
            commands::analysis::set_auto_analyze,
            commands::playlist::list_playlists,
            commands::playlist::get_playlist,
            commands::playlist::create_playlist,
            commands::playlist::update_playlist,
            commands::playlist::delete_playlist,
            commands::playlist::list_playlist_tracks,
            commands::playlist::list_playlists_containing_track,
            commands::playlist::add_track_to_playlist,
            commands::playlist::add_tracks_to_playlist,
            commands::playlist::remove_track_from_playlist,
            commands::playlist::reorder_playlist_track,
            commands::playlist::add_source_to_playlist,
            commands::playlist::export_playlist_m3u,
            commands::playlist::import_playlist_m3u,
            commands::playlist_cover::set_playlist_cover_from_file,
            commands::playlist_cover::regenerate_playlist_auto_cover,
            commands::playlist_cover::clear_playlist_cover,
            commands::smart_playlists::regenerate_daily_mixes,
            commands::smart_playlists::create_custom_smart_playlist,
            commands::smart_playlists::update_custom_smart_playlist,
            commands::smart_playlists::regenerate_custom_smart_playlist,
            commands::smart_playlists::get_custom_smart_playlist_rules,
            commands::smart_playlists::preview_custom_smart_playlist,
            commands::scan::scan_folder,
            commands::scan::rescan_local_artist_images,
            commands::deezer::search_artists_deezer,
            commands::deezer::set_artist_artwork_from_deezer,
            commands::deezer::set_artist_artwork_from_file,
            commands::deezer::clear_artist_artwork,
            commands::track::list_tracks,
            commands::track::get_track,
            commands::track::search_tracks,
            commands::track::search_tracks_advanced,
            commands::edit::update_track_tags,
            commands::edit::update_tracks_batch,
            commands::edit::update_track_cover,
            commands::track::toggle_like_track,
            commands::track::list_liked_track_ids,
            commands::track::list_liked_tracks,
            commands::track::set_track_rating,
            commands::browse::list_albums,
            commands::browse::list_artists,
            commands::browse::list_genres,
            commands::browse::list_folders,
            commands::browse::list_recent_plays,
            commands::browse::list_play_history,
            commands::browse::play_history_months,
            commands::browse::get_profile_stats,
            commands::browse::get_album_detail,
            commands::browse::get_artist_detail,
            commands::browse::get_genre_detail,
            commands::deezer::enrich_album_deezer,
            commands::deezer::enrich_artist_deezer,
            commands::deezer::search_albums_deezer,
            commands::deezer::set_album_artwork_from_deezer,
            commands::deezer::set_album_artwork_from_file,
            commands::deezer::batch_fetch_missing_album_covers,
            commands::deezer::batch_fetch_missing_artist_pictures,
            commands::similar::get_similar_artists,
            commands::radio::start_radio,
            commands::mood_radio::start_mood_radio,
            commands::mood_radio::mood_radio_counts,
            commands::dlna::dlna_get_config,
            commands::dlna::dlna_set_config,
            commands::dlna::dlna_get_status,
            commands::integration::get_lastfm_api_key,
            commands::integration::set_lastfm_api_key,
            commands::integration::get_lastfm_api_secret,
            commands::integration::set_lastfm_api_secret,
            commands::integration::lastfm_get_status,
            commands::integration::lastfm_login,
            commands::integration::lastfm_logout,
            commands::integration::get_discord_rpc_enabled,
            commands::integration::set_discord_rpc_enabled,
            commands::spotify::get_spotify_client_id,
            commands::spotify::set_spotify_client_id,
            commands::spotify::spotify_get_status,
            commands::spotify::spotify_login,
            commands::spotify::spotify_logout,
            commands::spotify::spotify_get_access_token,
            commands::spotify::spotify_list_playlists,
            commands::spotify::spotify_get_playlist_tracks,
            commands::spotify::spotify_get_queue,
            commands::spotify::spotify_search,
            commands::spotify::spotify_pause_local,
            commands::offline::get_offline_mode,
            commands::offline::set_offline_mode,
            commands::preferences::get_minimize_to_tray,
            commands::preferences::set_minimize_to_tray,
            commands::preferences::get_auto_start,
            commands::preferences::set_auto_start,
            commands::preferences::get_ui_zoom,
            commands::preferences::set_ui_zoom,
            commands::tray::set_tray_labels,
            commands::lyrics::get_lyrics,
            commands::lyrics::fetch_lyrics,
            commands::lyrics::import_lrc_file,
            commands::lyrics::save_lyrics,
            commands::lyrics::clear_lyrics,
            commands::lyrics::prefetch_library_lyrics,
            commands::lyrics::cancel_lyrics_prefetch,
            commands::player::player_get_state,
            commands::player::player_pause,
            commands::player::player_set_pause_after_track,
            commands::player::player_set_ab_loop,
            commands::player::player_clear_ab_loop,
            commands::player::player_get_ab_loop,
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
            commands::player::player_set_gapless,
            commands::player::player_set_speed,
            commands::player::player_get_speed,
            commands::player::player_set_visualizer,
            commands::player::player_get_visualizer,
            commands::player::player_set_smart_crossfade,
            commands::player::player_get_smart_crossfade,
            commands::player::player_set_dynamic_crossfade,
            commands::player::player_get_dynamic_crossfade,
            commands::player::player_set_replaygain,
            commands::player::player_get_eq,
            commands::player::player_set_eq_enabled,
            commands::player::player_set_eq_band,
            commands::player::player_set_eq_preset,
            commands::player::player_get_audio_settings,
            commands::player::player_list_output_devices,
            commands::player::player_set_output_device,
            commands::player::player_set_wasapi_exclusive,
            commands::player::player_get_wasapi_exclusive,
            commands::stats::stats_overview,
            commands::stats::stats_top_tracks,
            commands::stats::stats_top_artists,
            commands::stats::stats_top_albums,
            commands::stats::stats_listening_by_day,
            commands::stats::stats_listening_by_hour,
            commands::maintenance::regenerate_thumbnails,
            commands::backup::get_backup_config,
            commands::backup::set_backup_config,
            commands::backup::run_backup_now,
            commands::stats::export_stats_json,
            commands::wrapped::get_wrapped,
            commands::wrapped::available_wrapped_years,
            commands::wrapped::wrapped_current_year,
            commands::share::save_share_image,
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
                if quitting {
                    return;
                }
                // The mini-player window is its own dispensable surface
                // — closing it should just close it, never tear down
                // the whole app. Only the main window participates in
                // the close-to-tray decision.
                if window.label() != "main" {
                    return;
                }
                let minimize_to_tray = app
                    .state::<commands::preferences::PreferencesState>()
                    .minimize_to_tray
                    .load(Ordering::Acquire);
                if minimize_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // Arm the quit gate so the impending Destroyed
                    // event runs the normal shutdown path (persist
                    // resume point, shut the audio engine down) and
                    // doesn't bounce back into close-to-tray on any
                    // subsequent CloseRequested fired during teardown.
                    app.state::<QuitGate>().0.store(true, Ordering::Release);
                }
            }
            // Real shutdown path: fired only after the QuitGate has
            // been armed, so we can safely persist the resume point and
            // shut the audio engine down.
            WindowEvent::Destroyed => {
                if window.label() != "main" {
                    return;
                }
                let app = window.app_handle().clone();
                let quitting = app.state::<QuitGate>().0.load(Ordering::Acquire);
                if !quitting {
                    return;
                }
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
                    engine.shared().paused_output.store(true, Ordering::Release);

                    let track_id = engine.shared().current_track_id.load(Ordering::Acquire);
                    let position_ms = engine.shared().current_position_ms();
                    if track_id > 0 {
                        if let Ok(pool) = state.require_profile_pool().await {
                            let _ = queue::persist_resume_point(&pool, track_id, position_ms).await;
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
    if let Some(splash) = app.get_webview_window("splashscreen") {
        if let Err(err) = splash.close() {
            tracing::warn!(?err, "splash handoff: splash.close failed");
            return false;
        }
    }
    true
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
        let replay_gain_db = commands::player::fetch_replay_gain_db(&pool, next.id).await;
        let _ = engine.send(AudioCmd::LoadAndPlay {
            path: next.as_path(),
            start_ms: 0,
            track_id: next.id,
            duration_ms: next.duration_ms.max(0) as u64,
            source_type: "manual".into(),
            source_id: None,
            replay_gain_db,
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
        let replay_gain_db = commands::player::fetch_replay_gain_db(&pool, prev.id).await;
        let _ = engine.send(AudioCmd::LoadAndPlay {
            path: prev.as_path(),
            start_ms: 0,
            track_id: prev.id,
            duration_ms: prev.duration_ms.max(0) as u64,
            source_type: "manual".into(),
            source_id: None,
            replay_gain_db,
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
