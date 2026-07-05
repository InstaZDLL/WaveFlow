//! Per-plugin option values — the on-disk backing for
//! `waveflow:host/config.get-option`.
//!
//! Options are DECLARED in a plugin's `manifest.toml` (`[[options]]`) and SET
//! by the user in the app's per-plugin settings. The chosen values live in a
//! single JSON file inside the plugin's state directory; the runtime reads it
//! at instantiate time into [`HostCtx::config`](crate::plugin::runtime), and
//! the app (which owns the DB-free settings UI) writes it. This file is the
//! single source of truth — no duplicate `app_setting` row.
//!
//! It lives in the state dir but is NOT part of the plugin's scratch store:
//! the guest can't address it (scratch writes are hash-named, not arbitrary
//! filenames) and [`StateStore`](crate::plugin::host_impl) excludes it from
//! the quota tally, so a plugin can observe its config but never mutate it.

use std::collections::HashMap;
use std::path::Path;

/// Filename under the plugin's state dir. The dot prefix keeps it
/// inconspicuous; kept in sync with the exclusion in `host_impl`'s
/// `StateStore`.
pub const CONFIG_FILE: &str = ".plugin-config.json";

/// Read the resolved option map for a plugin from `<state_dir>/CONFIG_FILE`.
/// Missing file, unreadable, or malformed JSON all degrade to an empty map —
/// a plugin then sees `none` for every key and falls back to its own defaults,
/// so config never hard-fails a lookup.
pub fn read(state_dir: &Path) -> HashMap<String, String> {
    let path = state_dir.join(CONFIG_FILE);
    let Ok(bytes) = std::fs::read(&path) else {
        return HashMap::new();
    };
    serde_json::from_slice::<HashMap<String, String>>(&bytes).unwrap_or_default()
}

/// Write the resolved option map, replacing the file atomically (temp +
/// rename) so a crash mid-write never leaves a truncated JSON the runtime
/// would then parse as empty. Creates the state dir if absent.
pub fn write(state_dir: &Path, values: &HashMap<String, String>) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let json = serde_json::to_vec_pretty(values)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = state_dir.join(format!("{CONFIG_FILE}.tmp"));
    std::fs::write(&tmp, &json)?;
    let dest = state_dir.join(CONFIG_FILE);
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_empty());
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut values = HashMap::new();
        values.insert("quality".to_string(), "high".to_string());
        values.insert("enabled".to_string(), "true".to_string());
        write(dir.path(), &values).unwrap();
        assert_eq!(read(dir.path()), values);
        // No temp file left behind after the atomic publish.
        assert!(!dir.path().join(format!("{CONFIG_FILE}.tmp")).exists());
    }

    #[test]
    fn malformed_json_reads_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), b"{ not json").unwrap();
        assert!(read(dir.path()).is_empty());
    }
}
