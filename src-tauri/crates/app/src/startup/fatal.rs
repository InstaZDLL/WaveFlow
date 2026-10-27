//! Stopping before the event loop exists, and telling the user why.

use crate::error::AppError;
use crate::{logging, render_mode, skin_recovery};

/// Stop because the user said so, before anything was built.
pub(super) fn exit_on_request() -> ! {
    tracing::info!("quitting at the user's request, before any database was migrated");
    render_mode::disarm_for_deliberate_exit();
    skin_recovery::graceful_exit();
    logging::flush();
    std::process::exit(0);
}

/// Tell the user why WaveFlow is stopping, then stop.
///
/// One startup failure has a remedy the user can act on: the database
/// was written by a newer build. Everything else keeps propagating into
/// Tauri's `"Failed to setup app"` panic, which is the right shape for
/// a bug — but was the wrong shape for this, where the splash painted
/// and the process vanished with it, leaving nothing on screen to act
/// on (issue #526).
///
/// **This runs before the event loop, and that is load-bearing.**
/// `setup` runs from *inside* the loop, dispatched on
/// `RuntimeRunEvent::Ready`, so nothing queued from there is processed
/// until it returns — and a fatal path never returns. Presenting from
/// there cost one workaround per platform and still hung two of them
/// (issue #529):
///
/// - **Linux.** `rfd`'s GTK backend runs every dialog on a global
///   thread that iterates the *default* GLib main context, and `tao`
///   owns that context on the main thread for the life of the loop.
///   Waiting there for the dialog blocks the very context the dialog
///   needs: no window is ever created and the process hangs.
/// - **macOS**, inferred rather than observed — read out of `rfd`
///   0.16's sources, and there is no macOS job in CI to check it
///   against. `run_on_main` dispatches to the main queue when called
///   off-thread — a queue the blocked main thread never drains — and
///   panics outright when `NSApplication` isn't running yet.
/// - **Windows.** `TaskDialogIndirect` on the main thread from inside
///   the loop creates its window and never shows it (`WS_VISIBLE` stays
///   clear).
///
/// Before the loop, none of it applies: no main context is held, no
/// window exists — so there is no splash to hide either, which is the
/// other half of what made the old shape fragile (`close()` was a
/// silent no-op there, `hide()` the only thing that landed).
///
/// The message is English, like the tray menu's seed labels: there is
/// no frontend yet, so no i18next — and no readable language
/// preference, because that setting lives in the database we just
/// refused to open.
pub(super) fn report_fatal_and_exit(err: &AppError) -> ! {
    tracing::error!(%err, "fatal startup error, exiting");

    let (title, body) = match err {
        AppError::SchemaFromTheFuture {
            scope,
            version,
            installed_on,
        } => {
            let stamp = match installed_on {
                Some(when) => format!("Database schema {version}, applied on {when}."),
                None => format!("Database schema {version}."),
            };
            (
                "WaveFlow can't open this library".to_string(),
                format!(
                    "{} was written by a newer version of WaveFlow.\n\n\
                     Opening it with this older build could corrupt it, so WaveFlow \
                     stopped before touching anything.\n\n\
                     {}\n\n\
                     {stamp}",
                    scope.label(),
                    scope.remedy(),
                ),
            )
        }
        AppError::SchemaWrittenElsewhere { scope, version } => (
            "WaveFlow can't open this library".to_string(),
            format!(
                "{} was set up by a different build of WaveFlow, and one of the \
                 database updates it applied does not match the one this build \
                 ships.\n\n\
                 Running it anyway could corrupt the library, so WaveFlow stopped \
                 before touching anything.\n\n\
                 {}\n\n\
                 Database update {version}.",
                scope.label(),
                scope.remedy(),
            ),
        ),
        // Not reachable today — the caller filters — but a `match` that
        // can't fall over is one less way for a future variant to reach
        // the user as an empty dialog.
        other => ("WaveFlow can't start".to_string(), other.to_string()),
    };

    show_native_error(title, body);

    // The renderer marker comes down with it: this process is stopping
    // because it was told to, not because it could not draw (#595).
    render_mode::disarm_for_deliberate_exit();
    skin_recovery::graceful_exit();

    // Straight out rather than unwinding: there is no state to tear
    // down (nothing has been built yet, and the pools the pre-flight
    // opened are closed and WAL, which is crash-consistent by
    // construction). The log file is the one thing `exit` would
    // truncate, so flush it by hand first.
    logging::flush();
    std::process::exit(1);
}

/// Put a native error box on screen from a process with no event loop,
/// and block until it's dismissed.
///
/// Which thread is a toolkit rule, not a preference. AppKit demands the
/// main thread, and `MainThreadMarker::new()` succeeds there without a
/// running `NSApplication` — off-thread, `rfd` panics in exactly this
/// situation. GTK and `TaskDialogIndirect` accept any thread, and a
/// dedicated one is the shape actually measured working on both: on
/// Windows in issue #528, on Linux in #529's standalone control.
fn show_native_error(title: String, body: String) {
    fn dialog(title: &str, body: &str) -> rfd::MessageDialog {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(body)
            .set_buttons(rfd::MessageButtons::Ok)
    }

    // A GTK dialog needs a display server, and `rfd`'s GTK backend has
    // no way to report that it didn't get one: a failed `gtk_init_check`
    // leaves its global thread dead, and `run_blocking` then parks on a
    // condvar nobody will ever notify. Ask first — headless is not
    // where this message was going to help anyway, and the log line
    // above already carries it.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tracing::warn!("no display server; the error above is the whole story");
        return;
    }

    #[cfg(target_os = "macos")]
    {
        dialog(&title, &body).show();
    }

    // Joined, so the process outlives the dialog: `show()` blocks the
    // spawned thread until the user dismisses it.
    #[cfg(not(target_os = "macos"))]
    {
        let _ = std::thread::spawn(move || dialog(&title, &body).show()).join();
    }
}
