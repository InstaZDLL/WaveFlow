//! Frontend hooks for the bug-reporting workflow: locate and read the
//! rolling log files written by `logging::init_tracing`.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};

use crate::error::{AppError, AppResult};
use crate::logging;

/// Absolute path of the directory holding `waveflow.YYYY-MM-DD` logs.
/// The frontend may surface this for users to attach manually to a bug
/// report; for "Reveal in Files" use [`open_log_folder`] instead.
#[tauri::command]
pub fn get_log_dir() -> AppResult<Option<String>> {
    Ok(logging::log_dir().map(|p| p.display().to_string()))
}

/// Open the rolling-log directory in the user's system file manager.
#[tauri::command]
pub fn open_log_folder() -> AppResult<()> {
    let dir = logging::log_dir()
        .ok_or_else(|| AppError::Other("log directory not initialised".into()))?;
    tauri_plugin_opener::open_path(dir, None::<&str>)
        .map_err(|err| AppError::Other(format!("open_path: {err}")))
}

/// Concatenate the most recent rolling log files and return their tail
/// of `max_lines` rows. Used by the "Copy logs" button in Settings:
/// the user pastes the result into a GitHub issue.
#[tauri::command]
pub fn read_recent_logs(max_lines: Option<usize>) -> AppResult<String> {
    let limit = max_lines.unwrap_or(2000).max(1);

    let dir = logging::log_dir()
        .ok_or_else(|| AppError::Other("log directory not initialised".into()))?;

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|err| AppError::Other(format!("read_dir({}): {err}", dir.display())))?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().map(|ft| ft.is_file()).unwrap_or(false)
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("waveflow"))
        })
        .collect();

    // Lexicographic sort works because files are
    // `waveflow.YYYY-MM-DD` — older sorts first.
    entries.sort_by_key(|e| e.file_name());

    // Walk newest-to-oldest, gathering the last `limit` lines into a
    // ring buffer so we don't load arbitrarily large logs into memory.
    let mut buf: VecDeque<String> = VecDeque::with_capacity(limit);
    for entry in entries.into_iter().rev() {
        let file = match std::fs::File::open(entry.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(file);
        // Collect this file's lines in order, then prepend in reverse
        // so the final buffer reads chronologically.
        let mut lines: Vec<String> = Vec::new();
        for line in reader.lines() {
            match line {
                Ok(s) => lines.push(s),
                Err(_) => break,
            }
        }
        for line in lines.into_iter().rev() {
            if buf.len() >= limit {
                return Ok(into_chronological_string(buf));
            }
            buf.push_front(line);
        }
    }

    Ok(into_chronological_string(buf))
}

fn into_chronological_string(buf: VecDeque<String>) -> String {
    let mut out = String::with_capacity(buf.iter().map(String::len).sum::<usize>() + buf.len());
    for line in buf {
        out.push_str(&line);
        out.push('\n');
    }
    out
}
