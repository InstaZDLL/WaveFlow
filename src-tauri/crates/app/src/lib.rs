//! WaveFlow Tauri backend entry point.
//!
//! Sets up tracing, resolves filesystem paths, opens the global `app.db`
//! (running any pending migrations) and exposes the initial set of Tauri
//! commands to the frontend.

mod adwaita_frame;
mod analysis;
mod audio;
mod autoplay;
mod backup;
mod backup_prompt;
mod commands;
mod db;
mod desktop_lyrics;
mod discord_presence;
mod display_backend;
mod dlna;
mod error;
mod external_open;
mod global_shortcuts;
mod logging;
mod media_controls;
#[cfg(target_os = "linux")]
mod media_loopback;
mod metadata_artwork;
mod mpd;
mod notifications;
mod offline;
mod paths;
mod player_actions;
mod plugin_attention;
mod power;
mod queue;
mod render_mode;
// Remote play queue (RFC-005). Plain in-memory data — compiled
// unconditionally so the always-present player control seams can probe
// and clear it; the `sync_v2`-gated orchestration lives in
// `remote::playback`.
mod drop_import;
mod external_playback;
mod launch_args;
mod remote_playback;
// Remote music source + user-data sync v2 (RFC-005). `mod sync` is now
// permanently the no-op stub — the retired v1 protocol was removed in
// the RFC-005 cutover — and v2 does not synchronize local-entity CRUD at
// all, so the ~70 emit call sites keep resolving to the stub and this
// tree never touches them.
#[cfg(feature = "sync_v2")]
mod remote;
mod scrobbler;
mod skin_recovery;
mod smart_playlists;
mod startup;
mod state;
#[cfg(target_os = "windows")]
mod taskbar_buttons;
mod tasks;
// Multi-device sync v1 (Phase 1.f, RFC-003) was retired in the RFC-005
// cutover — the server no longer speaks its protocol. `mod sync` is now
// permanently `sync_stub.rs`, a no-op surface matching the old public
// API so the ~70 emit call sites in
// `commands/{library,playlist,track,scan,duplicates}` compile unchanged.
// See `sync_stub.rs` for the short-circuit semantics.
#[path = "sync_stub.rs"]
mod sync;
mod thumbnails;
mod trash;
mod watcher;
mod webview_memory;

use std::sync::atomic::AtomicBool;

use tauri::WebviewUrl;

use state::AppState;

#[cfg(target_os = "linux")]
use startup::{preflight_appimage_gstreamer, preflight_file_dialogs};
use startup::{preflight_render_mode, preflight_schema_guard};

/// Set to `true` by the tray "Quit" menu before calling `app.exit()`.
/// `WindowEvent::CloseRequested` checks the flag: if armed, the close
/// proceeds to actual shutdown; otherwise the close is intercepted and
/// the window is hidden instead (close-to-tray default).
struct QuitGate(AtomicBool);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A copy of this binary started only to ask whether to migrate
    // without a backup shows that dialog and exits here, before it can
    // touch a file or a toolkit (see `backup_prompt`).
    backup_prompt::answer_if_asked();

    // Initialize structured logging. `RUST_LOG` overrides the default
    // filter. The returned value must be held until the process exits —
    // dropping it flushes and closes the file early, losing the tail of
    // the log right when a crash report is most useful. It covers the
    // ordinary return and an unwinding panic; the paths that leave
    // through `std::process::exit` call `logging::flush` themselves,
    // because `exit` runs no destructors at all.
    // Resolved up front because the pre-flights below need the bundle
    // identifier to find the app-data root — `tauri.conf.json` stays
    // its single source of truth — and because everything this call
    // does is compile-time work anyway.
    let mut context = tauri::generate_context!();

    // Before logging, and that ordering is the point: choosing a
    // renderer sets process-wide environment variables (#595), and
    // `init_tracing` starts `tracing_appender`'s worker thread. Mutating
    // the environment while another thread may be reading it is a race
    // this can simply avoid by going first. Nothing here logs; what it
    // decided is reported a few lines down.
    preflight_render_mode(&context.config().identifier);
    #[cfg(target_os = "linux")]
    preflight_appimage_gstreamer(&context.config().identifier);
    #[cfg(target_os = "linux")]
    preflight_file_dialogs();

    let _log_guard = logging::init_tracing();
    render_mode::log_decision();

    // Start the splash-handoff clock before anything else, so the timings
    // logged when the frontend reports ready are measured from the process
    // start rather than from whichever half touched the gate first (#626).
    commands::ready::mark_launch();

    // Refuse a database written by a newer build BEFORE the event loop
    // exists. This is the only startup failure with a remedy the user
    // can act on, and the only place it can be *said* on all three
    // platforms — see `report_fatal_and_exit` for why "inside `setup`"
    // is not that place (issues #526, #529). The same pass takes the
    // pre-migration copies, for the same reason: when one fails, the
    // user is asked whether to go on without it.
    preflight_schema_guard(&context.config().identifier);

    let safe_mode = std::env::args_os().any(|arg| arg == "--safe-mode");
    let skin_recovery = match paths::AppPaths::root_for_identifier(&context.config().identifier) {
        Ok(root) => skin_recovery::preflight(root, safe_mode),
        Err(err) => {
            tracing::warn!(%err, "skin recovery unavailable");
            if safe_mode {
                skin_recovery::RecoveryReason::Manual
            } else {
                skin_recovery::RecoveryReason::None
            }
        }
    };
    // What the first paint has to know before any script can ask: the
    // skin recovery, and whether this launch draws without the GPU, which
    // the interface's automatic effects level reads to draw its surfaces
    // flat (`src/lib/effects.ts`).
    let software =
        render_mode::current().is_some_and(|d| d.mode == render_mode::RenderMode::Software);
    let mut query = Vec::new();
    if skin_recovery.is_active() {
        query.push("safe-mode=1");
        tracing::warn!(reason = ?skin_recovery, "Studio skin recovery selected");
    }
    if software {
        query.push("renderer=software");
    }
    if !query.is_empty() {
        if let Some(main) = context
            .config_mut()
            .app
            .windows
            .iter_mut()
            .find(|w| w.label == "main")
        {
            main.url = WebviewUrl::App(format!("index.html?{}", query.join("&")).into());
        }
    }

    // `mut` is only consumed when the updater plugin is wired in (release
    // builds); the lint would fire in debug otherwise.
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        // Single-instance MUST be the first plugin so a second launch
        // exits cleanly before any heavy init (pool open, audio engine,
        // tray, watchers) runs in the duplicate process.
        .plugin(tauri_plugin_single_instance::init(startup::on_second_instance))
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init());

    // System-wide shortcuts. The plugin is left out where it cannot work —
    // a Wayland session, which goes through the portal instead — because
    // there it would accept every combination and deliver none.
    let global_shortcut_backend = global_shortcuts::initial_backend();
    if global_shortcut_backend == global_shortcuts::Backend::Native {
        builder = builder.plugin(tauri_plugin_global_shortcut::Builder::new().build());
    }

    // Auto-updater is wired in release builds only. In dev (`tauri dev`)
    // the binary points at the local source tree, the version is the
    // working copy, and there's no signed manifest to fetch — the
    // plugin would just spam errors. Ship-only by design.
    //
    // Also gated on the `updater` Cargo feature (default-on) so the
    // Flatpak / app-store builds can compile it out — those channels
    // manage updates themselves and the in-app updater would clash
    // with their read-only install root.
    #[cfg(all(not(debug_assertions), feature = "updater"))]
    {
        builder = builder
            .plugin(tauri_plugin_updater::Builder::new().build())
            // Holds the verified Update between `check_for_update` and
            // `install_update` (see commands/updater.rs). Gated on the
            // same build conditions as the plugin itself.
            .manage(commands::updater::PendingUpdate::default());
    }

    builder
        .manage(QuitGate(AtomicBool::new(false)))
        .setup(move |app| startup::setup(app, global_shortcut_backend))
        .invoke_handler(tauri::generate_handler![
            commands::ready::app_ready,
            skin_recovery::record_skin,
            skin_recovery::complete_recovery,
            commands::renderer::renderer_status,
            commands::display_backend::display_backend_status,
            commands::display_backend::display_backend_set_xwayland,
            commands::renderer::renderer_retry_gpu,
            commands::app_info::get_app_info,
            commands::app_info::open_data_folder,
            commands::app_info::open_external_url,
            commands::changelog::get_changelog,
            commands::diagnostics::get_log_dir,
            commands::diagnostics::open_log_folder,
            commands::diagnostics::read_recent_logs,
            commands::profile::list_profiles,
            commands::profile::get_active_profile,
            commands::profile::create_profile,
            commands::profile::rename_profile,
            commands::profile::switch_profile,
            commands::profile::deactivate_profile,
            commands::profile::delete_profile,
            commands::profile::get_profile_setting,
            commands::profile::set_profile_setting,
            commands::profile_io::export_profile,
            commands::profile_io::import_profile,
            commands::profile_io::preview_profile_archive,
            commands::library::list_libraries,
            commands::library::create_library,
            commands::library::update_library,
            commands::library::delete_library,
            commands::library::rescan_library,
            commands::library::add_folder_to_library,
            commands::library::list_library_folders,
            commands::library::set_folder_watched,
            commands::library::remove_folder_from_library,
            commands::library::count_folder_play_events,
            commands::library::import_dropped,
            commands::duplicates::find_duplicates,
            commands::duplicates::move_tracks_to_trash,
            commands::replace::check_track_replacement,
            commands::replace::replace_track_file,
            commands::analysis::analyze_track,
            commands::analysis::analyze_library,
            commands::analysis::cancel_library_analysis,
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
            commands::playlist_import::import_playlist_file,
            commands::playlist_import::import_playlist_deezer,
            commands::playlist_import::list_playlist_placeholders,
            commands::playlist_import::remove_playlist_placeholder,
            commands::playlist_cover::set_playlist_cover_from_file,
            commands::playlist_cover::preview_playlist_cover_from_file,
            commands::playlist_cover::regenerate_playlist_auto_cover,
            commands::playlist_cover::clear_playlist_cover,
            commands::smart_playlists::regenerate_daily_mixes,
            commands::smart_playlists::regenerate_on_repeat,
            commands::smart_playlists::regenerate_all_smart_playlists,
            commands::smart_playlists::create_custom_smart_playlist,
            commands::smart_playlists::update_custom_smart_playlist,
            commands::smart_playlists::regenerate_custom_smart_playlist,
            commands::smart_playlists::get_custom_smart_playlist_rules,
            commands::smart_playlists::count_custom_smart_playlist,
            commands::release_radar::release_radar_scan,
            commands::release_radar::release_radar_list,
            commands::release_radar::release_radar_refresh_playlist,
            commands::scan::scan_folder,
            commands::scan::rescan_local_artist_images,
            commands::deezer::search_artists_deezer,
            commands::deezer::set_artist_artwork_from_deezer,
            commands::deezer::set_artist_artwork_from_file,
            commands::deezer::clear_artist_artwork,
            commands::deezer::get_artist_backdrop_candidates,
            commands::deezer::set_artist_background_from_url,
            commands::deezer::set_artist_background_from_file,
            commands::deezer::clear_artist_background,
            commands::track::list_tracks,
            commands::track::get_track,
            commands::track::get_track_genres,
            commands::track::get_track_artists,
            commands::track::get_track_credits,
            commands::track::search_tracks,
            commands::track::search_tracks_advanced,
            commands::edit::update_track_tags,
            commands::edit::update_tracks_batch,
            commands::edit::get_track_file_stamp,
            commands::edit::reread_track_folder,
            commands::edit::update_track_cover,
            commands::track::toggle_like_track,
            commands::track::list_liked_track_ids,
            commands::track::list_liked_tracks,
            commands::track::set_track_rating,
            commands::browse::list_albums,
            commands::browse::list_library_albums,
            commands::browse::list_library_artists,
            commands::browse::list_library_tracks,
            commands::browse::list_artists,
            commands::browse::search_albums,
            commands::browse::search_artists,
            commands::browse::list_genres,
            commands::browse::list_folders,
            commands::browse::browse_folders,
            commands::browse::list_folder_tracks,
            commands::browse::folder_track_ids,
            commands::browse::list_recent_plays,
            commands::browse::list_play_history,
            commands::browse::play_history_months,
            commands::browse::get_profile_stats,
            commands::browse::get_album_detail,
            commands::browse::get_artist_detail,
            commands::browse::get_genre_detail,
            commands::years::list_years,
            commands::years::get_year_detail,
            commands::browse::set_genre_artwork_from_file,
            commands::browse::clear_genre_artwork,
            commands::deezer::enrich_album_deezer,
            commands::deezer::enrich_artist_deezer,
            commands::deezer::enrich_artist_by_name,
            commands::artist_overrides::get_artist_overrides,
            commands::artist_overrides::set_artist_metadata_overrides,
            commands::artist_split::split_artist,
            commands::deezer::search_albums_deezer,
            commands::deezer::set_album_artwork_from_deezer,
            commands::deezer::set_album_artwork_from_file,
            commands::deezer::batch_fetch_missing_album_covers,
            commands::deezer::batch_fetch_missing_artist_pictures,
            commands::deezer::fetch_radio_artwork,
            commands::similar::get_similar_artists,
            #[cfg(feature = "sync_v2")]
            commands::similar::get_similar_artists_by_name,
            commands::radio::start_radio,
            commands::home_suggestion::get_home_suggestion,
            commands::web_radio_catalogue::radio_catalogue_status,
            commands::web_radio_catalogue::set_radio_catalogue_local_first,
            commands::web_radio_catalogue::clear_radio_catalogue,
            commands::web_radio_catalogue::download_radio_catalogue,
            commands::web_radio_catalogue::resolve_radio_catalogue,
            commands::web_radio_catalogue::get_radio_preferred_country,
            commands::web_radio_catalogue::set_radio_preferred_country,
            commands::web_radio_custom::web_radio_probe_url,
            commands::web_radio_custom::web_radio_read_playlist_file,
            commands::mood_radio::start_mood_radio,
            commands::mood_radio::mood_radio_counts,
            commands::mood_radio::track_mood_overrides,
            commands::mood_radio::set_mood_override,
            commands::mood_radio::list_mood_overrides,
            commands::global_shortcuts::global_shortcuts_status,
            commands::global_shortcuts::global_shortcuts_set,
            commands::global_shortcuts::global_shortcuts_suspend,
            commands::global_shortcuts::global_shortcuts_clear,
            commands::spectrogram::track_spectrogram,
            commands::spectrogram::cancel_track_spectrogram,
            commands::disk_usage::disk_usage,
            commands::mpd::mpd_get_config,
            commands::mpd::mpd_set_config,
            commands::mpd::mpd_get_status,
            commands::dlna::dlna_get_config,
            commands::dlna::dlna_set_config,
            commands::dlna::dlna_get_status,
            commands::dlna::dlna_set_labels,
            commands::plugins::list_installed_plugins,
            commands::plugins::get_plugin_info,
            commands::plugins::set_plugin_enabled,
            commands::plugins::uninstall_plugin,
            commands::plugins::plugin_list_entries,
            commands::plugins::plugin_resolve,
            commands::plugins::plugin_stream_url,
            commands::plugins::get_plugin_favorites,
            commands::plugins::set_plugin_favorites,
            commands::plugins::get_plugin_options,
            commands::plugins::set_plugin_option,
            commands::plugins::list_ui_plugins,
            commands::plugins::plugin_ui_render,
            commands::plugins::plugin_ui_event,
            commands::plugins::open_plugins_folder,
            commands::plugin_store::list_plugin_marketplace,
            commands::plugin_store::install_plugin_from_registry,
            commands::motion_artwork::fetch_album_motion_artwork,
            commands::motion_artwork::get_motion_cache_info,
            commands::motion_artwork::set_motion_cache_enabled,
            commands::motion_artwork::clear_motion_cache,
            commands::motion_artwork::set_album_motion_artwork_from_file,
            commands::motion_artwork::clear_album_motion_artwork,
            commands::motion_artwork::has_manual_album_motion_artwork,
            commands::canvas::get_track_canvas,
            commands::canvas::set_track_canvas_from_file,
            commands::canvas::clear_track_canvas,
            commands::canvas::fetch_track_canvas,
            commands::canvas::get_canvas_cache_info,
            commands::canvas::set_canvas_cache_enabled,
            commands::canvas::clear_canvas_cache,
            commands::integration::get_lastfm_api_key,
            commands::integration::set_lastfm_api_key,
            commands::integration::get_bio_source,
            commands::integration::set_bio_source,
            commands::integration::get_bio_language,
            commands::integration::set_bio_language,
            commands::integration::get_lastfm_api_secret,
            commands::integration::set_lastfm_api_secret,
            commands::integration::lastfm_get_status,
            commands::integration::lastfm_login,
            commands::integration::lastfm_logout,
            commands::integration::listenbrainz_get_status,
            commands::integration::listenbrainz_connect,
            commands::integration::listenbrainz_disconnect,
            commands::integration::get_discord_rpc_enabled,
            commands::integration::set_discord_rpc_enabled,
            commands::integration::get_notifications_track_change,
            commands::integration::set_notifications_track_change,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_get_status,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_detect_server,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_begin_login,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_sign_out,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_forget_server,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_sync_now,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_get_overview,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_stream_url,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_transcode_status,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_download_track,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_upload_libraries,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_upload_survey,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_upload_tracks,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_cancel_upload,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_import_folders,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_import_tracks,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_list_downloads,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_downloads_info,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_remove_download,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_clear_downloads,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_stream_cache_info,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_clear_stream_cache,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_artwork,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_play_playlist,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_get_play_queue,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_queue_jump,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_list_playlists,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_list_playlist_tracks,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_list_queue,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_set_favorite,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_set_rating,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_track_canvas,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_get_track_tags,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_update_track_tags,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_discard_failed_changes,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_create_playlist,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_update_playlist,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_delete_playlist,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_remove_playlist_track,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_reorder_playlist_track,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_search_catalogue,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_get_album,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_get_artist,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_play_tracks,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_add_playlist_tracks,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_scrobble,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_save_queue,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_create_share,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_update_share,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_delete_share,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_artwork_cache_info,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_clear_artwork_cache,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_mirror_catalogue,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_cancel_catalogue_mirror,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_catalogue_stats,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_clear_catalogue,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_reconcile_scan,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_cancel_reconcile_scan,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_list_reconciliation_links,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_confirm_reconciliation,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_reject_reconciliation,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_set_reconciliation_preference,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_remove_reconciliation_link,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_copy_reconciliation_favorite,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_copy_reconciliation_rating,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_preview_playlist_conversion,
            #[cfg(feature = "sync_v2")]
            commands::remote_auth::remote_convert_playlist,
            #[cfg(feature = "sync_v2")]
            commands::lyrics::fetch_remote_lyrics,
            commands::offline::get_offline_mode,
            commands::offline::set_offline_mode,
            commands::preferences::get_minimize_to_tray,
            commands::preferences::set_minimize_to_tray,
            commands::preferences::get_sort_ignore_articles,
            commands::preferences::set_sort_ignore_articles,
            commands::preferences::get_auto_start,
            commands::preferences::set_auto_start,
            commands::preferences::get_ui_zoom,
            commands::preferences::set_ui_zoom,
            commands::preferences::get_window_chrome,
            commands::preferences::set_window_chrome,
            commands::preferences::set_window_frame_colors,
            commands::library_media::get_clips_in_library,
            commands::library_media::set_clips_in_library,
            commands::library_media::get_local_video_base_url,
            commands::preferences::get_mini_player_bounds,
            commands::preferences::set_mini_player_bounds,
            commands::preferences::clear_mini_player_bounds,
            commands::preferences::sync_webview_memory,
            commands::preferences::get_main_window_bounds,
            commands::preferences::set_main_window_bounds,
            commands::preferences::clear_main_window_bounds,
            commands::updater::get_update_channel,
            commands::updater::set_update_channel,
            commands::updater::check_for_update,
            commands::updater::install_update,
            commands::tray::set_tray_labels,
            commands::desktop_lyrics::desktop_lyrics_status,
            commands::desktop_lyrics::open_desktop_lyrics,
            commands::desktop_lyrics::close_desktop_lyrics,
            commands::desktop_lyrics::set_desktop_lyrics_locked,
            commands::preferences::set_desktop_lyrics_bounds,
            commands::lyrics::get_lyrics,
            commands::lyrics::fetch_lyrics,
            commands::lyrics::lyrics_excluded_genre,
            commands::lyrics::fetch_radio_lyrics,
            commands::lyrics::refetch_lyrics,
            commands::lyrics::import_lrc_file,
            commands::lyrics::save_lyrics,
            commands::lyrics::export_lyrics_to_path,
            commands::lyrics::clear_lyrics,
            commands::lyrics::get_lyrics_translation_lang,
            commands::lyrics::set_lyrics_translation_lang,
            commands::lyrics::get_prefer_lrclib,
            commands::lyrics::set_prefer_lrclib,
            commands::lyrics::get_lyrics_default_destination,
            commands::lyrics::set_lyrics_default_destination,
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
            commands::player::player_play_test_tone,
            commands::player::player_seek,
            commands::player::player_set_volume,
            commands::player::player_play_tracks,
            commands::player::player_play_url,
            commands::player::player_play_paths,
            commands::player::player_get_external_queue,
            commands::player::player_external_jump,
            commands::player::get_current_radio_metadata,
            commands::player::player_add_to_queue,
            commands::player::player_play_next,
            commands::player::player_reorder_queue,
            commands::player::player_next,
            commands::player::player_previous,
            commands::player::player_toggle_shuffle,
            commands::player::player_set_shuffle_mode,
            commands::player::player_cycle_repeat,
            commands::player::player_resume_last,
            commands::player::player_get_queue,
            commands::player::player_jump_to_index,
            commands::player::player_set_normalize,
            commands::player::player_set_mono,
            commands::player::player_set_balance,
            commands::player::player_set_stereo_width,
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
            commands::player::player_set_automix,
            commands::player::player_get_automix,
            commands::player::player_set_replaygain,
            commands::player::player_set_replaygain_options,
            commands::player::player_set_replaygain_mode,
            commands::player::player_set_dsd_precision,
            commands::player::player_set_dsd_dop,
            commands::player::player_get_eq,
            commands::player::player_set_eq_enabled,
            commands::player::player_set_eq_band,
            commands::player::player_set_eq_preset,
            commands::player::player_get_audio_settings,
            commands::player::player_list_output_devices,
            commands::player::player_set_output_device,
            commands::player::player_reopen_output_device,
            commands::player::player_set_exclusive_output,
            commands::player::player_get_exclusive_output,
            commands::player::player_get_exclusive_output_state,
            commands::player::player_set_pause_on_device_loss,
            commands::player::player_set_keep_awake,
            commands::player::player_hold_display,
            commands::player::player_probe_output_device,
            commands::player::player_set_match_source_rate,
            commands::inventory::inventory_summary,
            commands::inventory::inventory_tracks,
            plugin_attention::plugin_attention_history,
            commands::inventory::inventory_phantom_artists,
            commands::tag_fetch::search_album_tag_sources,
            commands::enrichment::enrichment_status,
            commands::enrichment::enrichment_run,
            commands::enrichment::enrichment_cancel,
            commands::enrichment::enrichment_album_decisions,
            commands::enrichment::enrichment_accept_album,
            commands::enrichment::enrichment_reject_album,
            commands::enrichment::enrichment_undo_album,
            commands::enrichment::enrichment_forget_album,
            commands::enrichment::enrichment_artist_decisions,
            commands::enrichment::enrichment_accept_artists,
            commands::enrichment::enrichment_reject_artist,
            commands::enrichment::enrichment_forget_artist,
            commands::enrichment::get_artist_musicbrainz,
            commands::enrichment::get_track_musicbrainz_credits,
            commands::tag_workshop::tag_workshop_open,
            commands::tag_workshop::tag_workshop_preview,
            commands::tag_workshop::tag_workshop_apply,
            commands::tag_workshop::tag_workshop_batches,
            commands::tag_workshop::tag_workshop_batch_files,
            commands::tag_workshop::tag_workshop_undo,
            commands::tag_workshop::tag_workshop_match,
            commands::tag_fetch::fetch_album_tag_proposals,
            commands::track_tags::list_track_tag_keys,
            commands::track_tags::list_track_tag_values,
            commands::tasks::list_tasks,
            commands::tasks::cancel_task,
            commands::storage::get_cache_location,
            commands::storage::set_cache_location,
            commands::storage::restart_for_cache_move,
            commands::stats::stats_overview,
            commands::stats::stats_top_tracks,
            commands::stats::stats_top_artists,
            commands::stats::stats_top_albums,
            commands::stats::stats_top_genres,
            commands::stats::stats_listening_by_day,
            commands::stats::stats_listening_by_hour,
            commands::maintenance::regenerate_thumbnails,
            commands::maintenance::prune_cached_album_covers,
            commands::maintenance::reset_app,
            commands::backup::get_backup_config,
            commands::backup::set_backup_config,
            commands::backup::run_backup_now,
            commands::stats::export_stats_json,
            commands::wrapped::get_wrapped,
            commands::wrapped::available_wrapped_years,
            commands::wrapped::wrapped_current_year,
            commands::share_image::save_share_image,
            commands::sidebar_pins::list_sidebar_pins,
            commands::sidebar_pins::set_sidebar_pin,
        ])
        .on_window_event(startup::on_window_event)
        .build(context)
        .expect("error while building tauri application")
        .run(startup::on_run_event);
}
