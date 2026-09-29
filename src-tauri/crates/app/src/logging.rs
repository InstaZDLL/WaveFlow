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
use std::sync::{Mutex, OnceLock};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const APP_IDENTIFIER: &str = "app.waveflow";
const LOG_FILE_PREFIX: &str = "waveflow";

/// Daily files kept on disk; older ones are deleted as a new day starts.
/// A week covers the bug reports that arrive a few days late.
const LOG_FILES_KEPT: usize = 7;

/// Most a single day's file may grow to. A day of ordinary use writes a
/// few hundred kilobytes; this is only ever reached by something logging
/// in a loop — a VM's audio device once wrote 680 MB of the same line in
/// an evening — and past it the rest of the day is dropped rather than
/// filling the disk.
const LOG_BYTES_PER_DAY: u64 = 50 * 1024 * 1024;

/// Computed at `init_tracing` and reused by Tauri commands so the
/// frontend can locate logs without re-implementing the path logic.
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The non-blocking file writer's worker guard. Dropping it flushes and
/// closes the sink, and dropping it is the *only* way to flush — the
/// type exposes no other.
///
/// It lives here rather than in a `run()` local because the startup
/// fatal paths leave through `std::process::exit`, which runs no
/// destructors: a local would strand the last error line in the
/// writer's buffer, exactly when it is the only account of what
/// happened. Those paths call [`flush`] directly. `run` still holds a
/// [`FlushOnDrop`] so the ordinary return — and an unwinding panic —
/// keep flushing on their own.
static LOG_GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);

/// Flushes the log file when it goes out of scope. Handed to `run` so
/// the normal path needs no explicit call and can't forget one.
pub struct FlushOnDrop;

impl Drop for FlushOnDrop {
    fn drop(&mut self) {
        flush();
    }
}

/// Flush and close the log file. Idempotent, and safe to call from a
/// path that is about to `std::process::exit`.
///
/// Anything logged afterwards still reaches stdout; only the file sink
/// closes. A poisoned lock is ignored rather than panicked on — this is
/// called while already handling a fatal error.
pub fn flush() {
    if let Ok(mut guard) = LOG_GUARD.lock() {
        drop(guard.take());
    }
}

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

/// Install the global tracing subscriber.
///
/// Parks the file writer's guard in [`LOG_GUARD`] and returns a
/// [`FlushOnDrop`] the caller must hold for the whole program: let it
/// fall out of scope early and the file sink closes with it.
pub fn init_tracing() -> FlushOnDrop {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,lofty=error"));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_target(true);

    let log_dir = resolve_log_dir();
    let (file_layer, guard) = match log_dir.as_ref() {
        Some(dir) => match std::fs::create_dir_all(dir)
            .map_err(|e| e.to_string())
            .and_then(|()| {
                tracing_appender::rolling::Builder::new()
                    .rotation(tracing_appender::rolling::Rotation::DAILY)
                    .filename_prefix(LOG_FILE_PREFIX)
                    .max_log_files(LOG_FILES_KEPT)
                    .build(dir)
                    .map_err(|e| e.to_string())
            }) {
            Ok(file_appender) => {
                let capped = CappedWriter::new(file_appender, dir.clone());
                let (writer, guard) = tracing_appender::non_blocking(capped);
                let layer = tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false)
                    .with_target(true);
                (Some(layer), Some(guard))
            }
            Err(err) => {
                eprintln!(
                    "[logging] could not open the log file in {}: {err} — file logs disabled",
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

    if let Ok(mut slot) = LOG_GUARD.lock() {
        *slot = guard;
    }

    FlushOnDrop
}

/// What a write does to the day's budget. See [`DailyBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admit {
    Write,
    /// The write that crosses the limit: say so in the file, once.
    Notice,
    Drop,
}

/// Bytes written to today's log file, against [`LOG_BYTES_PER_DAY`].
#[derive(Debug)]
struct DailyBudget {
    day: chrono::NaiveDate,
    written: u64,
    limit: u64,
    exhausted: bool,
}

impl DailyBudget {
    fn new(day: chrono::NaiveDate, already_written: u64, limit: u64) -> Self {
        Self {
            day,
            written: already_written,
            limit,
            exhausted: already_written >= limit,
        }
    }

    fn admit(&mut self, today: chrono::NaiveDate, len: u64) -> Admit {
        if today != self.day {
            *self = Self::new(today, 0, self.limit);
        }
        if self.exhausted {
            return Admit::Drop;
        }
        if self.written.saturating_add(len) > self.limit {
            self.exhausted = true;
            return Admit::Notice;
        }
        self.written += len;
        Admit::Write
    }

    /// Give back the bytes of a write that did not reach the file.
    fn refund(&mut self, len: u64) {
        self.written = self.written.saturating_sub(len);
    }
}

/// The daily file appender, stopped at [`LOG_BYTES_PER_DAY`] for the rest
/// of the day. Days are UTC, as the appender's own file names are, and a
/// restart picks the budget up from the size today's file already has.
struct CappedWriter<W> {
    inner: W,
    budget: DailyBudget,
}

impl<W: std::io::Write> CappedWriter<W> {
    fn new(inner: W, dir: PathBuf) -> Self {
        let today = chrono::Utc::now().date_naive();
        let already =
            std::fs::metadata(dir.join(format!("{LOG_FILE_PREFIX}.{}", today.format("%Y-%m-%d"))))
                .map(|m| m.len())
                .unwrap_or(0);
        Self {
            inner,
            budget: DailyBudget::new(today, already, LOG_BYTES_PER_DAY),
        }
    }
}

impl<W: std::io::Write> std::io::Write for CappedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let today = chrono::Utc::now().date_naive();
        match self.budget.admit(today, buf.len() as u64) {
            // Whole or not at all, so the budget counts what is really
            // in the file: a failed write gives its bytes back.
            Admit::Write => match self.inner.write_all(buf) {
                Ok(()) => Ok(buf.len()),
                Err(err) => {
                    self.budget.refund(buf.len() as u64);
                    Err(err)
                }
            },
            Admit::Notice => {
                let _ = self.inner.write_all(
                    format!(
                        "[logging] today's log reached {} MB; the rest of the day is not kept\n",
                        LOG_BYTES_PER_DAY / (1024 * 1024)
                    )
                    .as_bytes(),
                );
                Ok(buf.len())
            }
            // Reported as written: the caller has nothing to retry.
            Admit::Drop => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Path of the directory that holds rolling log files.
pub fn log_dir() -> Option<&'static PathBuf> {
    LOG_DIR.get()
}

#[cfg(test)]
mod tests {
    use super::{Admit, DailyBudget};
    use chrono::NaiveDate;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn writes_pass_until_the_limit_then_one_notice_then_nothing() {
        let mut budget = DailyBudget::new(day(29), 0, 100);
        assert_eq!(budget.admit(day(29), 60), Admit::Write);
        assert_eq!(budget.admit(day(29), 40), Admit::Write);
        assert_eq!(budget.admit(day(29), 1), Admit::Notice);
        assert_eq!(budget.admit(day(29), 1), Admit::Drop);
    }

    #[test]
    fn a_new_day_starts_a_new_budget() {
        let mut budget = DailyBudget::new(day(29), 0, 100);
        assert_eq!(budget.admit(day(29), 200), Admit::Notice);
        assert_eq!(budget.admit(day(30), 50), Admit::Write);
    }

    #[test]
    fn a_failed_write_is_given_back() {
        let mut budget = DailyBudget::new(day(29), 0, 100);
        assert_eq!(budget.admit(day(29), 100), Admit::Write);
        budget.refund(100);
        assert_eq!(budget.admit(day(29), 100), Admit::Write);
    }

    #[test]
    fn a_restart_counts_what_today_already_wrote() {
        let mut budget = DailyBudget::new(day(29), 100, 100);
        assert_eq!(budget.admit(day(29), 1), Admit::Drop);
    }
}
