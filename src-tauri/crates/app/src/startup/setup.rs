//! The `setup` hook: everything that has to exist before the first
//! command is dispatched, in the order it has to come up.

use std::sync::Arc;
use std::time::Duration;

use tauri::{Listener, Manager};

use crate::audio::{self, AudioEngine};
use crate::error::AppError;
#[cfg(feature = "sync_v2")]
use crate::remote;
use crate::state::AppState;
#[cfg(target_os = "windows")]
use crate::taskbar_buttons;
use crate::watcher::WatcherManager;
use crate::{
    backup, backup_prompt, commands, desktop_lyrics, discord_presence, dlna, global_shortcuts,
    launch_args, logging, media_controls, mpd, plugin_attention, render_mode, scrobbler,
    skin_recovery, tasks,
};

use super::resume_point::{current_resume_point, write_resume_point};
use super::window::restore_bounds_and_reveal;

/// The body of the `setup` hook [`crate::run`] registers.
pub(crate) fn setup(
    app: &mut tauri::App,
    global_shortcut_backend: global_shortcuts::Backend,
) -> Result<(), Box<dyn std::error::Error>> {
    // Everything below can fail, and a failure here stops the
    // launch before the frontend can report a paint. This takes
    // the renderer marker down on any of those paths — including
    // the ones added after this line (#595).
    let setup_guard = render_mode::SetupGuard::new();
    let skin_setup_guard = skin_recovery::SetupGuard::new();
    let init_handle = app.handle().clone();
    let engine_handle = app.handle().clone();
    // Before any plugin runs, so a credential refused on the very
    // first lookup still reaches the user.
    plugin_attention::init(app.handle().clone());

    // Block on the async init — this runs once at startup before any
    // command can be dispatched, so blocking here is acceptable.
    let state =
        match tauri::async_runtime::block_on(async move { AppState::init(&init_handle).await }) {
            Ok(state) => state,
            Err(err) => {
                // The database from a newer build is normally
                // caught by the pre-flight in `run`, which is
                // where it gets explained. Reaching it here
                // means the pre-flight couldn't read what
                // startup then could — a lock it lost, a file
                // that needed recovery — so all that's left is
                // to stop. Not a dialog: from inside the event
                // loop that is the deadlock (#529), and a
                // frozen splash is worse than a short one.
                // Everything else keeps propagating into Tauri's
                // "Failed to setup app" panic, which is the
                // right shape for a bug.
                //
                // A database with no pre-migration copy lands
                // here the same way: the pre-flight settles the
                // two it opens, and anything left needs a
                // question this hook cannot ask.
                if matches!(
                    err,
                    AppError::SchemaFromTheFuture { .. }
                        | AppError::SchemaWrittenElsewhere { .. }
                        | AppError::PreMigrationBackupDeclined { .. }
                ) {
                    tracing::error!(%err, "fatal startup error, exiting");
                    // Stopping on purpose is not a launch that
                    // failed to paint (#595).
                    render_mode::disarm_for_deliberate_exit();
                    skin_recovery::graceful_exit();
                    // `exit` runs no destructors, so the line
                    // above would die in the writer's buffer.
                    // Here it is the only account there is —
                    // this path shows the user nothing.
                    logging::flush();
                    std::process::exit(1);
                }
                // The marker comes down through `setup_guard`,
                // like every other failure in this closure.
                return Err(Box::new(err));
            }
        };

    // Hydrate the close-to-tray flag once at boot. The atomic
    // is the source of truth for `WindowEvent::CloseRequested`
    // so it has to be in place before the first user input.
    let minimize_to_tray =
        tauri::async_runtime::block_on(commands::preferences::load_minimize_to_tray(&state.app_db));
    app.manage(commands::preferences::PreferencesState::new(
        minimize_to_tray,
    ));
    // Before the first listing: the collation reads it on every
    // comparison.
    tauri::async_runtime::block_on(commands::preferences::load_sort_ignore_articles(
        &state.app_db,
    ));
    app.manage(desktop_lyrics::DesktopLyricsState::default());
    app.manage(commands::spectrogram::SpectrogramJobs::default());

    // Artwork reaches the webview through `convertFileSrc`, which
    // the asset protocol gates on the static scope in
    // `tauri.conf.json` — `$APPDATA` and `$APPLOCALDATA` only. A
    // cache root on another drive (#619) matches none of those
    // patterns, and the failure is silent: no error, no console
    // message, just covers that never appear. Widen it before
    // the frontend exists, and on every start, because the grant
    // lives in the process rather than on disk.
    commands::storage::grant_asset_scope(app.handle(), &state.paths);

    // One registry every long operation reports into (#601), so
    // the status bar can answer "why is this machine busy"
    // without each task inventing its own surface. Managed
    // before `state` so a task started during the rest of setup
    // already has somewhere to announce itself.
    app.manage(tasks::TaskRegistry::new(app.handle().clone()));

    app.manage(state);

    // From here on a database that cannot be copied before it
    // migrates is asked about through the dialog plugin. Not
    // earlier: `setup` runs on the event loop, which drains
    // nothing until it returns, so the plugin's dialog asked from
    // `AppState::init` would wait on it forever.
    backup_prompt::install_running(app.handle().clone());

    // Plugin SDK epoch ticker. Wasmtime's epoch-interruption
    // deadline only fires when something is bumping the
    // engine's epoch counter — the runtime's `set_epoch_deadline`
    // is expressed in ticks, not wall-clock, so without a
    // ticker a runaway guest call would never trap. 10 ms
    // cadence matches `RuntimeConfig::epoch_ticks_per_call`'s
    // 3 000-tick default = ~30 s wall-clock per call. Cheap
    // — one atomic increment per tick, the tokio interval
    // handle keeps no allocator pressure.
    let plugin_ticker_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let runtime = plugin_ticker_handle.state::<AppState>().plugins.clone();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            runtime.tick_epoch();
        }
    });

    // Remote source wake-up socket (RFC-005). Gated on `sync_v2`,
    // so a build can carry the remote source or not.
    #[cfg(feature = "sync_v2")]
    {
        // rustls 0.23 needs a process-wide CryptoProvider before
        // the first TLS handshake, and `connect_async` is what
        // trips it (reqwest configures its own per-connector).
        // Without this a `sync_v2` build would panic on the first
        // wss:// upgrade. A second call returns Err, which is
        // harmless.
        let _ = rustls::crypto::ring::default_provider().install_default();

        // The subscriber owns its own gates — unbound profile,
        // signed out, offline, no journal — so spawning it
        // unconditionally costs an idle loop and nothing else.
        remote::socket::spawn(app.handle().clone());
    }

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
    let (persisted_device, persisted_exclusive_output) = tauri::async_runtime::block_on(async {
        let state = app.state::<AppState>();
        let Ok(pool) = state.require_profile_pool().await else {
            return (None, false);
        };
        let device: Option<String> = sqlx::query_scalar(
            "SELECT value FROM profile_setting WHERE key = 'audio.output_device'",
        )
        .fetch_optional(&*pool)
        .await
        .ok()
        .flatten()
        .filter(|s: &String| !s.is_empty());
        // Two keys because the setting outgrew its name: it
        // was `audio.wasapi_exclusive` while exclusive output
        // existed only on Windows. Reading both carries a
        // Windows user's opt-in across the rename, and the
        // current name wins when both rows exist — the legacy
        // one is then whatever they had before the last toggle.
        let exclusive: bool = sqlx::query_scalar::<_, String>(
            "SELECT value FROM profile_setting
                          WHERE key IN ('audio.exclusive_output', 'audio.wasapi_exclusive')
                          ORDER BY key = 'audio.exclusive_output' DESC
                          LIMIT 1",
        )
        .fetch_optional(&*pool)
        .await
        .ok()
        .flatten()
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
        // Pause on device loss (#617) is restored below with
        // every other audio setting, by
        // `restore_profile_audio_settings`; only the two the
        // engine's constructor needs are read here.
        (device, exclusive)
    });
    let engine: Arc<AudioEngine> =
        AudioEngine::new_with_device(engine_handle, persisted_device, persisted_exclusive_output);
    // The profile's audio settings — volume, normalize,
    // ReplayGain, the EQ curve, speed, the crossfades, pause on
    // device loss, the visualizer. Restored here rather than
    // left to the
    // frontend's first `player_get_state`: every getter reads
    // these atomics, and a view that asked first was answered
    // with the boot default instead of the profile's own
    // choice (#698). The same call runs on a profile switch.
    tauri::async_runtime::block_on(async {
        let state = app.state::<AppState>();
        if let Ok(pool) = state.require_profile_pool().await {
            commands::player::restore_profile_audio_settings(&pool, &engine).await;
        }
    });
    app.manage(engine);

    // Follow the system's default output while nothing is pinned
    // (#627). Spawned after the engine is in state so the first
    // notification finds it; one that arrives earlier is a
    // no-op, the open that follows reads the default anyway.
    audio::default_device::spawn(app.handle().clone());

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
        .fetch_optional(&*pool)
        .await
        .ok()
        .flatten()
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
        if !enabled {
            // No scan to wait for: rows measured by an older
            // analysis version are re-measured now, or a library
            // that gains no file would never reach the current
            // one (`maybe_auto_analyze` honours the toggle).
            commands::analysis::maybe_auto_analyze(&scan_handle);
            return;
        }
        let Ok(profile_id) = state.require_profile_id().await else {
            commands::analysis::maybe_auto_analyze(&scan_handle);
            return;
        };
        let artwork_dir = state.paths.profile_artwork_dir(profile_id);
        let folder_ids: Vec<i64> =
            match sqlx::query_scalar("SELECT id FROM library_folder ORDER BY id")
                .fetch_all(&*pool)
                .await
            {
                Ok(rows) => rows,
                Err(err) => {
                    tracing::warn!(%err, "scan-on-start: list folders failed");
                    // The scan cannot run; the re-measure still can.
                    commands::analysis::maybe_auto_analyze(&scan_handle);
                    return;
                }
            };
        for folder_id in folder_ids {
            if let Err(err) = commands::scan::scan_folder_inner(
                &pool,
                &artwork_dir,
                folder_id,
                Some(&scan_handle),
                false,
            )
            .await
            {
                tracing::warn!(folder_id, %err, "scan-on-start: folder scan failed");
            }
        }
        // After the walk, not before it: a sweep started during the
        // scan would make this one a no-op (one sweep at a time), and
        // a scan that added nothing triggers none of its own.
        commands::analysis::maybe_auto_analyze(&scan_handle);
    });

    // OS media controls (SMTC / MPRIS / MediaRemote) via
    // souvlaki. Needs the main window to exist for HWND on
    // Windows; on Linux/macOS the platform integration owns
    // its own connection. Failure is non-fatal — playback
    // keeps working without the OS overlay.
    if let Some(controls) = media_controls::init(app.handle().clone()) {
        app.manage(controls);
    }

    // Previous / play-pause / next under the taskbar thumbnail.
    // Subclasses the main window, so it has to run on the thread
    // that created it — `setup` does. Non-fatal as well.
    #[cfg(target_os = "windows")]
    if let Some(buttons) = taskbar_buttons::init(app) {
        app.manage(buttons);
    }

    // System-wide shortcuts, none bound until the user sets one.
    // Registers from a task: the native backend runs each
    // registration on this thread, free once `setup` returns.
    global_shortcuts::init(app.handle(), global_shortcut_backend);

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

    // MPD protocol server auto-start (issue #471). Same shape as
    // the DLNA block above: read the persisted `mpd.enabled`
    // flag so a user who opted in doesn't have to re-toggle it
    // every launch. Off by default — that flag *is* the security
    // decision, since enabling the server binds every interface.
    tauri::async_runtime::spawn({
        let handle = app.handle().clone();
        async move {
            let state = handle.state::<AppState>();
            let cfg = match mpd::config::load(&state.app_db).await {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(?err, "MPD config load failed");
                    return;
                }
            };
            if cfg.enabled {
                state.mpd.start(cfg, handle.clone());
            }
        }
    });

    super::tray::build(app)?;

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
    // Driven by a single reveal task so there is exactly one
    // owner of the handoff. `app://ready` merely *notifies* it;
    // the task waits for that signal OR the 15 s deadline
    // (whichever comes first), then reveals the main window,
    // retrying until a reveal actually succeeds. The previous
    // shared-`AtomicBool` coordination let the fallback observe
    // the flag the event handler had already set and `return`
    // before the event's own reveal task finished — so if that
    // task then failed, nobody retried and the user was stuck on
    // the splash (issue #363). One owner + retry-until-success
    // removes both the race and the double-reveal window.
    // Second transport, kept on purpose. The primary one is the
    // `app_ready` command: `generate_handler!` registers it at build
    // time, so unlike this listener it cannot be registered *after*
    // the webview has already signalled. The rendezvous is one-shot
    // and ignores duplicates, so having both costs nothing and the
    // splash is not worth a single point of failure (#626).
    app.listen("app://ready", move |_event| {
        commands::ready::signal("event", Default::default());
    });

    // Write the resume point while playback runs, not only on the
    // way out (#624). A crash, a kill or a power loss leaves no exit
    // event to hook at all, and ten seconds is close enough that what
    // comes back is where the user was.
    let resume_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(10));
        // The first tick fires immediately and nothing has played yet.
        ticker.tick().await;
        let mut last: Option<(i64, u64)> = None;
        loop {
            ticker.tick().await;
            // Skip the write when neither the track nor the second it
            // sits on has moved: paused and idle sessions would
            // otherwise hit the single SQLite writer every ten
            // seconds for nothing.
            let Some(point) = current_resume_point(&resume_handle) else {
                continue;
            };
            let coarse = (point.0, point.1 / 1000);
            if last == Some(coarse) {
                continue;
            }
            // Only remember a point that actually landed, so a write
            // lost to a busy database or a profile switch is retried
            // on the next tick instead of being skipped as unchanged.
            if write_resume_point(&resume_handle).await {
                last = Some(coarse);
            }
        }
    });

    let reveal_handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        // `notify_one` stores a permit even when it fires before
        // we reach this await, so a signal that arrives during the
        // heavy first-launch init is never lost.
        tokio::select! {
            _ = commands::ready::wait() => {}
            _ = tokio::time::sleep(Duration::from_secs(15)) => {
                // Left exactly as it was: it is what keeps a launch
                // merely slow instead of stuck. The elapsed time is
                // logged so this line can be read against the
                // frontend's own measurement (#626).
                tracing::warn!(
                    since_launch_ms = commands::ready::since_launch_ms(),
                    "splash handoff: no ready signal in 15s, force-revealing main window; the renderer marker stays armed, so the next launch falls back to software rendering (#595)"
                );
            }
        }
        // Retry until a reveal actually succeeds. Bounded (10 ×
        // 250 ms) so a permanently missing main window logs an
        // error instead of spinning forever; each attempt re-runs
        // the full reveal (show main + close splash).
        for attempt in 0..10 {
            if restore_bounds_and_reveal(reveal_handle.clone()).await {
                return;
            }
            tracing::warn!(attempt, "splash handoff: reveal attempt failed, retrying");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        tracing::error!(
            "splash handoff: exhausted 10 reveal attempts, user is likely stuck on splash"
        );
    });

    // The files this launch was opened with, or the playback it was
    // handed, if any.
    launch_args::open_when_ready(
        app.handle(),
        std::env::args_os(),
        std::env::current_dir().ok().as_deref(),
    );

    // Setup reached its end, so the marker stays armed until
    // something actually paints — which is what it is for.
    setup_guard.succeeded();
    skin_setup_guard.succeeded();
    Ok(())
}
