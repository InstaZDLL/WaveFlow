//! Persistent log infrastructure.
//!
//! `tracing` events are forked to two sinks:
//!
//! - stdout (kept verbatim for `bun run tauri dev` and terminal launches)
//! - a daily-rotated file under the user's data directory
//!
//! When users run a packaged build (AppImage, MSI, .app, …) there is no
//! terminal attached, so the file sink is the only place a maintainer can
//! recover diagnostics from when a bug report comes in. The `get_log_dir`
//! and `read_recent_logs` Tauri commands let the in-app UI surface those
//! files to the user without forcing them to dig through `~/.local/share`.
//!
//! The directory layout matches Tauri's PathResolver convention:
//!
//! - Linux:   `~/.local/share/app.waveflow/logs/`
//! - macOS:   `~/Library/Logs/app.waveflow/`
//! - Windows: `%LOCALAPPDATA%\app.waveflow\logs\`
//!
//! We compute the directory directly via `dirs` rather than asking
//! Tauri's PathResolver because the subscriber must be installed before
//! `tauri::Builder` is built.
use std::path::PathBuf;
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const APP_IDENTIFIER: &str = "app.waveflow";
const LOG_FILE_PREFIX: &str = "waveflow";

/// Computed at `init_tracing` and reused by Tauri commands so the
/// frontend can locate logs without re-implementing the path logic.
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Compute the log directory path for the current OS without creating
/// it. Returns `None` only on truly exotic platforms where `dirs`
/// cannot resolve a base directory.
fn resolve_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = dirs::home_dir().map(|home| home.join("Library").join("Logs"));
    #[cfg(not(target_os = "macos"))]
    let base = dirs::data_local_dir();

    base.map(|dir| {
        let app_dir = dir.join(APP_IDENTIFIER);
        if cfg!(target_os = "macos") {
            app_dir
        } else {
            app_dir.join("logs")
        }
    })
}

/// Install the global tracing subscriber. Returns a `WorkerGuard` that
/// must outlive the program — dropping it flushes the non-blocking file
/// writer, so callers must hold it for the entire app lifetime.
pub fn init_tracing() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,lofty=error"));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_target(true);

    let log_dir = resolve_log_dir();
    let (file_layer, guard) = match log_dir.as_ref() {
        Some(dir) => match std::fs::create_dir_all(dir) {
            Ok(()) => {
                let file_appender = tracing_appender::rolling::daily(dir, LOG_FILE_PREFIX);
                let (writer, guard) = tracing_appender::non_blocking(file_appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false)
                    .with_target(true);
                (Some(layer), Some(guard))
            }
            Err(err) => {
                eprintln!(
                    "[logging] failed to create log dir {}: {err} — file logs disabled",
                    dir.display()
                );
                (None, None)
            }
        },
        None => {
            eprintln!("[logging] could not resolve a log directory — file logs disabled");
            (None, None)
        }
    };

    if let Some(dir) = log_dir {
        let _ = LOG_DIR.set(dir);
    }

    tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();

    guard
}

/// Path of the directory that holds rolling log files.
pub fn log_dir() -> Option<&'static PathBuf> {
    LOG_DIR.get()
}
