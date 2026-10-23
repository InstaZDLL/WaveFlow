//! The question asked when a database is about to migrate and no copy
//! of it could be made ([`crate::db::pre_migration_backup`]): try again,
//! continue without a backup, or leave it as it is.
//!
//! Two dialogs, one question. At launch it is asked from the pre-flight,
//! before the event loop exists — the only place a native dialog can be
//! shown then (issue #529) — but **from a child process**: the binary
//! started again with [`ASK_FLAG`], which shows the dialog through `rfd`
//! and exits with the answer as its status. The fatal startup dialogs can
//! show theirs in-process because they exit right after; this one is
//! followed by the event loop, and a toolkit touched first does not let
//! go: `rfd`'s GTK thread keeps the default GLib main context for the
//! life of the process, and `tao` then panics failing to acquire it
//! (measured on Fedora 44). On macOS an alert shown first would likewise
//! create the `NSApplication` before `tao` registers its own. The child
//! takes all of that with it when it exits.
//!
//! Once the app is running, the question goes through the dialog plugin,
//! which dispatches to the main thread the loop is now draining; there
//! the third button cancels the open instead of quitting.
//!
//! English, like the startup error dialogs: the launch question comes
//! before any frontend, so before i18next.

use crate::db::pre_migration_backup::{Choice, SnapshotFailure};
use crate::db::schema_guard::DbScope;

const RETRY: &str = "Try again";
const CONTINUE: &str = "Continue without a backup";
const QUIT: &str = "Quit";
const CANCEL: &str = "Cancel";

/// When the question is asked, which decides what the third button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moment {
    /// From the pre-flight: the third button quits WaveFlow.
    Launch,
    /// From a running app: the third button gives up this open.
    Running,
}

impl Moment {
    fn stop_label(self) -> &'static str {
        match self {
            Moment::Launch => QUIT,
            Moment::Running => CANCEL,
        }
    }
}

fn title() -> &'static str {
    "WaveFlow could not back up your library"
}

fn body(scope: DbScope, failure: &SnapshotFailure, moment: Moment) -> String {
    let stop = match moment {
        Moment::Launch => "quit and leave it untouched",
        Moment::Running => "cancel and leave it untouched",
    };
    format!(
        "{} has to be updated before this version of WaveFlow can open it. \
         WaveFlow copies it first, so there is something to go back to if the \
         update goes wrong, and that copy could not be made:\n\n\
         {}\n\n\
         You can try again once there is more free space beside it, continue \
         without a backup, or {stop}.\n\n\
         {}",
        scope.label(),
        failure.reason,
        failure.db.display(),
    )
}

/// The button the user pressed, by its label. The third button, and
/// anything else — the window closed, a backend that answered with a
/// stock button — is the answer that changes nothing.
fn choice_for(label: Option<&str>) -> Choice {
    match label {
        Some(RETRY) => Choice::Retry,
        Some(CONTINUE) => Choice::Continue,
        _ => Choice::Quit,
    }
}

/// The argument that makes the binary ask the launch question and exit:
/// `waveflow --ask-pre-migration-backup <app|profile> <reason> <db path>`.
const ASK_FLAG: &str = "--ask-pre-migration-backup";

/// Exit statuses of the asking child. Anything else (a crash, a dialog
/// that could not be shown) reads as [`Choice::Quit`].
const EXIT_RETRY: i32 = 20;
const EXIT_CONTINUE: i32 = 21;
const EXIT_QUIT: i32 = 22;

fn exit_code(choice: Choice) -> i32 {
    match choice {
        Choice::Retry => EXIT_RETRY,
        Choice::Continue => EXIT_CONTINUE,
        Choice::Quit => EXIT_QUIT,
    }
}

fn choice_from_exit(code: Option<i32>) -> Choice {
    match code {
        Some(EXIT_RETRY) => Choice::Retry,
        Some(EXIT_CONTINUE) => Choice::Continue,
        _ => Choice::Quit,
    }
}

/// Ask from the pre-flight, before the event loop, through a child
/// process (see the module docs). Blocks until the user answers.
pub fn ask_at_launch(scope: DbScope, failure: &SnapshotFailure) -> Choice {
    // Without a display server there is nobody to ask, and `rfd`'s GTK
    // backend would park forever trying (see `show_native_error`).
    // Leaving the database as it is is the answer that loses nothing.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        tracing::warn!("pre-migration backup: no display server to ask on; not migrating");
        return Choice::Quit;
    }

    let scope_arg = match scope {
        DbScope::App => "app",
        DbScope::Profile => "profile",
    };
    // The environment goes along as it is: the renderer and display
    // backend the pre-flights above chose are the child's too.
    let status = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg(ASK_FLAG)
            .arg(scope_arg)
            .arg(&failure.reason)
            .arg(&failure.db)
            .status()
    });
    let choice = match status {
        Ok(status) => {
            if !matches!(status.code(), Some(EXIT_RETRY | EXIT_CONTINUE | EXIT_QUIT)) {
                tracing::warn!(%status, "pre-migration backup: the question could not be asked");
            }
            choice_from_exit(status.code())
        }
        Err(err) => {
            tracing::warn!(%err, "pre-migration backup: could not start the question");
            Choice::Quit
        }
    };
    tracing::info!(?choice, ?scope, "pre-migration backup: answer at launch");
    choice
}

/// When this process was started by [`ask_at_launch`], ask the question
/// and exit with the answer. Called first thing in `run`, before anything
/// writes a file or starts a thread; any other start returns at once.
pub fn answer_if_asked() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(ASK_FLAG)) {
        return;
    }
    let scope = match args.next().as_deref().and_then(|s| s.to_str()) {
        Some("app") => DbScope::App,
        _ => DbScope::Profile,
    };
    let failure = SnapshotFailure {
        reason: args
            .next()
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default(),
        db: args.next().map(Into::into).unwrap_or_default(),
    };

    let moment = Moment::Launch;
    let dialog = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title())
        .set_description(body(scope, &failure, moment))
        .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
            RETRY.into(),
            CONTINUE.into(),
            moment.stop_label().into(),
        ));

    // AppKit wants the main thread, which this is; GTK and
    // `TaskDialogIndirect` are shown from a thread of their own and
    // joined, the shape `show_native_error` measured working.
    #[cfg(target_os = "macos")]
    let answer = Some(dialog.show());
    #[cfg(not(target_os = "macos"))]
    let answer = std::thread::spawn(move || dialog.show()).join().ok();

    let choice = match answer {
        Some(rfd::MessageDialogResult::Custom(label)) => choice_for(Some(&label)),
        _ => choice_for(None),
    };
    std::process::exit(exit_code(choice));
}

/// Ask through the dialog plugin from now on: every database opened
/// after `setup` (a profile first opened since the upgrade, an imported
/// archive) goes through it.
pub fn install_running(app: tauri::AppHandle) {
    crate::db::pre_migration_backup::install_prompt(Box::new(move |scope, failure| {
        use tauri_plugin_dialog::{
            DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
        };
        let moment = Moment::Running;
        let show = || {
            app.dialog()
                .message(body(scope, failure, moment))
                .title(title())
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::YesNoCancelCustom(
                    RETRY.into(),
                    CONTINUE.into(),
                    moment.stop_label().into(),
                ))
                .blocking_show_with_result()
        };
        // The opens run on the async runtime's workers. Handing the
        // worker back for as long as the user takes keeps the rest of
        // the runtime moving.
        let answer = match tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()) {
            Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(show),
            _ => show(),
        };
        let choice = match answer {
            MessageDialogResult::Custom(label) => choice_for(Some(&label)),
            _ => choice_for(None),
        };
        tracing::info!(?choice, ?scope, "pre-migration backup: answer");
        choice
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn each_button_is_its_choice() {
        for moment in [Moment::Launch, Moment::Running] {
            assert_eq!(choice_for(Some(RETRY)), Choice::Retry);
            assert_eq!(choice_for(Some(CONTINUE)), Choice::Continue);
            assert_eq!(choice_for(Some(moment.stop_label())), Choice::Quit);
        }
    }

    #[test]
    fn the_child_s_answer_survives_its_exit_status() {
        for choice in [Choice::Retry, Choice::Continue, Choice::Quit] {
            assert_eq!(choice_from_exit(Some(exit_code(choice))), choice);
        }
        // A child that crashed, or was killed by a signal.
        assert_eq!(choice_from_exit(Some(101)), Choice::Quit);
        assert_eq!(choice_from_exit(None), Choice::Quit);
    }

    #[test]
    fn a_closed_window_changes_nothing() {
        assert_eq!(choice_for(None), Choice::Quit);
        assert_eq!(choice_for(Some("Yes")), Choice::Quit);
    }

    #[test]
    fn the_text_names_the_database_and_the_reason() {
        let failure = SnapshotFailure {
            db: PathBuf::from("/data/profiles/3/data.db"),
            reason: "the copy could not be written: disk full".into(),
        };
        let text = body(DbScope::Profile, &failure, Moment::Launch);
        assert!(text.starts_with("This profile's library has to be updated"));
        assert!(text.contains("disk full"));
        assert!(text.contains("/data/profiles/3/data.db"));
        assert!(text.contains("quit and leave it untouched"));
        assert!(body(DbScope::App, &failure, Moment::Running).contains("cancel and leave"));
    }
}
