//! The handlers `run` registers on the builder: a second launch, the
//! window events, and the run events.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager, WindowEvent};

use crate::audio::{AudioCmd, AudioEngine};
use crate::QuitGate;
use crate::{
    commands, desktop_lyrics, error, launch_args, render_mode, skin_recovery, webview_memory,
};

use super::resume_point::write_resume_point;
#[cfg(target_os = "macos")]
use super::window::show_main_window;

/// A launch turned away by the single-instance plugin, seen from the
/// instance that is already running.
pub(crate) fn on_second_instance(app: &AppHandle, argv: Vec<String>, cwd: String) {
    // The launch that was just turned away armed the renderer
    // marker before this plugin could stop it — it runs from
    // inside `Builder`, and the marker has to be on disk before
    // any window can fail to paint. Left there, it would send
    // the *next* launch into software rendering because someone
    // opened the app twice (#595). This instance is the one
    // that knows the truth, so it writes it back.
    render_mode::restore_after_duplicate_launch();
    skin_recovery::restore_after_duplicate_launch();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        webview_memory::sync(&window);
    }
    // A file the turned-away launch was opened with plays here, and
    // a handoff it carried is taken over here.
    launch_args::open_when_ready(app, argv, Some(std::path::Path::new(&cwd)));
}

pub(crate) fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    match event {
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
            if window.label() == desktop_lyrics::LABEL {
                desktop_lyrics::mark_closing(window.app_handle());
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
                if let Some(main) = app.get_webview_window("main") {
                    webview_memory::sync(&main);
                }
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
            if window.label() == desktop_lyrics::LABEL {
                desktop_lyrics::on_destroyed(window.app_handle());
                return;
            }
            if window.label() != "main" {
                return;
            }
            let app = window.app_handle().clone();
            let quitting = app.state::<QuitGate>().0.load(Ordering::Acquire);
            if !quitting {
                return;
            }
            let _ = tauri::async_runtime::block_on(async move {
                let engine = app.state::<Arc<AudioEngine>>();

                // Silence the cpal output IMMEDIATELY. The rtrb
                // ring still holds a few hundred ms of decoded
                // samples from before the user paused; without
                // this flag, those samples flush to the device
                // while we persist resume state and the stream
                // tears down, producing a jarring ~2 s of audio
                // at shutdown.
                engine.shared().paused_output.store(true, Ordering::Release);

                // The resume point is written from `RunEvent::Exit`
                // below, which this path reaches too — and which the
                // tray's Quit reaches *only* through there (#624).
                // Tell the decoder thread to stop and drop the cpal
                // stream cleanly.
                let _ = engine.send(AudioCmd::Shutdown);
                Ok::<_, error::AppError>(())
            });
        }
        // Minimize / restore fire `Resized`, and a hide or show moves
        // focus: re-read the window's state and let an unseen WebView2
        // give memory back. See `webview_memory`.
        WindowEvent::Resized(_) | WindowEvent::Focused(_)
            if webview_memory::LABELS.contains(&window.label()) =>
        {
            if let Some(webview) = window.app_handle().get_webview_window(window.label()) {
                webview_memory::sync(&webview);
            }
        }
        _ => {}
    }
}

pub(crate) fn on_run_event(app: &AppHandle, event: tauri::RunEvent) {
    // macOS opens files by sending them to the running app rather
    // than on its command line, at launch as well as later.
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Opened { urls } = &event {
        let paths = launch_args::paths_from_urls(urls);
        if !paths.is_empty() {
            // Brought forward, as the single-instance callback does
            // on the other systems: a window hidden to the tray
            // would play the files out of sight. Not while the
            // splash still stands, at a launch: the handoff shows
            // the window once it has painted, and showing it before
            // would put an unfinished interface on screen.
            if app.get_webview_window("splashscreen").is_none() {
                show_main_window(app);
            }
            launch_args::play_when_ready(app, paths);
        }
    }
    // Every way out lands here: the tray's "Quit" (which calls
    // `AppHandle::exit`), a window close that isn't close-to-tray, a
    // session logout. `WindowEvent::Destroyed` does not — `exit`
    // triggers `RunEvent::ExitRequested` / `Exit` instead, which is
    // why the resume point was never written (#624).
    if matches!(event, tauri::RunEvent::Exit) {
        skin_recovery::graceful_exit();
        tauri::async_runtime::block_on(async {
            let engine = app.try_state::<Arc<AudioEngine>>();
            // Silent first, as the close path makes it: the ring still
            // holds audio that would play through both writes below.
            if let Some(engine) = &engine {
                engine.shared().paused_output.store(true, Ordering::Release);
            }
            write_resume_point(app).await;
            // After the resume point, which reads the position the
            // shutdown is about to leave behind.
            if let Some(engine) = engine {
                engine.shut_down_and_flush(Duration::from_secs(2)).await;
            }
        });
    }
}
