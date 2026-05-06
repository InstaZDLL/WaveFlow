import { invoke } from "@tauri-apps/api/core";

/**
 * Absolute path of the rolling-log directory the Rust backend writes to,
 * or `null` if the backend couldn't resolve a writable data directory at
 * startup (very unusual).
 */
export function getLogDir(): Promise<string | null> {
  return invoke<string | null>("get_log_dir");
}

/** Open the log folder in the system file manager. */
export function openLogFolder(): Promise<void> {
  return invoke<void>("open_log_folder");
}

/**
 * Tail of the most recent log files concatenated into one string,
 * chronologically. Defaults to ~2000 lines, which is plenty to grab a
 * full app session while staying small enough for clipboard / GitHub
 * issue paste.
 */
export function readRecentLogs(maxLines?: number): Promise<string> {
  return invoke<string>("read_recent_logs", { maxLines });
}
