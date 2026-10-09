//! A way back to Studio when a skin makes the webview unusable.
//!
//! `--safe-mode` works on every platform. On Linux, a small marker is armed
//! while a non-Studio skin is in use. A normal exit disarms it; an abrupt
//! exit (including an OOM kill) leaves it behind, so the next launch starts
//! in Studio before the first webview is created. This is deliberately
//! separate from the renderer fallback: the UI can paint successfully and
//! only exhaust memory later.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "skin-recovery.json";
const STUDIO: &str = "studio";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryReason {
    None,
    Manual,
    PreviousExit,
    Pending,
}

impl RecoveryReason {
    pub fn is_active(self) -> bool {
        self != Self::None
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SkinState {
    skin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    armed_by: Option<u32>,
    #[serde(default)]
    recovery_pending: bool,
}

impl Default for SkinState {
    fn default() -> Self {
        Self {
            skin: STUDIO.into(),
            armed_by: None,
            recovery_pending: false,
        }
    }
}

static STATE_PATH: OnceLock<PathBuf> = OnceLock::new();
static STATE_LOCK: Mutex<()> = Mutex::new(());
#[cfg(target_os = "linux")]
static ACTIVE_SKIN: OnceLock<Mutex<String>> = OnceLock::new();
#[cfg(target_os = "linux")]
static RECOVERING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn locked<T>(f: impl FnOnce() -> T) -> T {
    let _guard = STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

fn read(path: &Path) -> SkinState {
    match std::fs::read_to_string(path) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(state) => state,
            Err(err) => {
                eprintln!("waveflow: invalid skin recovery marker: {err}");
                SkinState::default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => SkinState::default(),
        Err(err) => {
            eprintln!("waveflow: cannot read skin recovery marker: {err}");
            SkinState::default()
        }
    }
}

fn write(path: &Path, state: &SkinState) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let raw = serde_json::to_vec(state)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = parent.join(format!(
        ".skin-recovery.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = std::fs::File::create_new(&temp)?;
        file.write_all(&raw)?;
        // Unlike the renderer's marker, this one must also survive a system
        // restart after WebKit has exhausted RAM, not just process death.
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        #[cfg(target_os = "linux")]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn same_app_is_running(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        let theirs = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
        let ours = std::env::current_exe().ok();
        matches!((theirs, ours), (Some(a), Some(b)) if same_executable_name(&a, &b))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        false
    }
}

#[cfg(target_os = "linux")]
fn same_executable_name(a: &Path, b: &Path) -> bool {
    let name = |path: &Path| {
        path.file_name().map(|value| {
            value
                .to_string_lossy()
                .trim_end_matches(" (deleted)")
                .to_lowercase()
        })
    };
    matches!((name(a), name(b)), (Some(x), Some(y)) if x == y)
}

fn startup_state(
    mut state: SkinState,
    manual: bool,
    linux: bool,
    pid: u32,
    is_running: impl Fn(u32) -> bool,
) -> (SkinState, RecoveryReason) {
    if manual || state.recovery_pending {
        state.recovery_pending = true;
        state.armed_by = None;
        return (
            state,
            if manual {
                RecoveryReason::Manual
            } else {
                RecoveryReason::Pending
            },
        );
    }
    if !linux || state.skin == STUDIO {
        return (state, RecoveryReason::None);
    }
    if let Some(previous) = state.armed_by {
        if previous != pid && is_running(previous) {
            // A second launch must not diagnose a still-running first one.
            return (state, RecoveryReason::None);
        }
        state.recovery_pending = true;
        state.armed_by = None;
        return (state, RecoveryReason::PreviousExit);
    }
    state.armed_by = Some(pid);
    (state, RecoveryReason::None)
}

/// Called before Tauri creates the main webview. The caller puts the result
/// in its URL, so Studio wins even over the first-paint localStorage cache.
pub fn preflight(root: PathBuf, manual: bool) -> RecoveryReason {
    let path = root.join(FILE_NAME);
    let _ = STATE_PATH.set(path.clone());
    if !cfg!(target_os = "linux") {
        return if manual {
            RecoveryReason::Manual
        } else {
            RecoveryReason::None
        };
    }
    locked(|| {
        let previous = read(&path);
        let (next, reason) = startup_state(
            previous,
            manual,
            cfg!(target_os = "linux"),
            std::process::id(),
            same_app_is_running,
        );
        #[cfg(target_os = "linux")]
        {
            let active_skin = if reason.is_active() {
                STUDIO.to_owned()
            } else {
                next.skin.clone()
            };
            let _ = ACTIVE_SKIN.set(Mutex::new(active_skin));
            RECOVERING.store(reason.is_active(), std::sync::atomic::Ordering::Release);
        }
        if let Err(err) = write(&path, &next) {
            eprintln!("waveflow: cannot arm skin recovery: {err}");
        }
        reason
    })
}

fn path() -> Result<&'static Path, String> {
    STATE_PATH
        .get()
        .map(PathBuf::as_path)
        .ok_or_else(|| "skin recovery was not initialized".into())
}

/// Persist before the frontend applies a newly selected skin. Also called
/// after the active profile's saved skin has loaded, so profile switches and
/// launches from older versions become guarded too.
#[tauri::command]
pub fn record_skin(skin_id: String) -> Result<(), String> {
    if !["studio", "editorial", "lounge", "pulse", "liquid"].contains(&skin_id.as_str()) {
        return Err("unknown skin".into());
    }
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    locked(|| {
        let path = path()?;
        let mut state = read(path);
        if state.recovery_pending {
            return if skin_id == STUDIO {
                #[cfg(target_os = "linux")]
                if let Some(active) = ACTIVE_SKIN.get() {
                    *active.lock().unwrap_or_else(|e| e.into_inner()) = STUDIO.to_owned();
                }
                Ok(())
            } else {
                Err("finish Studio recovery before choosing another skin".into())
            };
        }
        state.skin = skin_id;
        state.armed_by = (state.skin != STUDIO).then_some(std::process::id());
        write(path, &state).map_err(|err| err.to_string())?;
        #[cfg(target_os = "linux")]
        if let Some(active) = ACTIVE_SKIN.get() {
            *active.lock().unwrap_or_else(|e| e.into_inner()) = state.skin;
        }
        Ok(())
    })
}

/// Only called after Studio has been committed to the active profile and
/// localStorage. Until then the pending marker keeps rescuing later launches.
#[tauri::command]
pub fn complete_recovery() -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    locked(|| {
        let path = path()?;
        let mut state = read(path);
        state.skin = STUDIO.into();
        state.armed_by = None;
        state.recovery_pending = false;
        write(path, &state).map_err(|err| err.to_string())?;
        #[cfg(target_os = "linux")]
        {
            if let Some(active) = ACTIVE_SKIN.get() {
                *active.lock().unwrap_or_else(|e| e.into_inner()) = STUDIO.to_owned();
            }
            RECOVERING.store(false, std::sync::atomic::Ordering::Release);
        }
        Ok(())
    })
}

/// A rejected second launch may have written its own `--safe-mode` decision
/// before the single-instance plugin stopped it. The instance that remains
/// restores the skin and recovery state it actually has on screen.
pub fn restore_after_duplicate_launch() {
    #[cfg(target_os = "linux")]
    locked(|| {
        let (Ok(path), Some(active)) = (path(), ACTIVE_SKIN.get()) else {
            return;
        };
        let state = restored_after_duplicate(
            read(path),
            &active.lock().unwrap_or_else(|e| e.into_inner()),
            RECOVERING.load(std::sync::atomic::Ordering::Acquire),
            std::process::id(),
        );
        if let Err(err) = write(path, &state) {
            tracing::warn!(%err, "could not restore skin marker after duplicate launch");
        }
    });
}

#[cfg(any(target_os = "linux", test))]
fn restored_after_duplicate(
    mut state: SkinState,
    active_skin: &str,
    recovering: bool,
    pid: u32,
) -> SkinState {
    state.skin = active_skin.to_owned();
    state.recovery_pending = recovering;
    state.armed_by = (!recovering && active_skin != STUDIO).then_some(pid);
    state
}

/// A normal quit accepts the skin. A kill, crash or logout that prevents
/// `RunEvent::Exit` leaves the marker armed for the next startup.
pub fn graceful_exit() {
    if !cfg!(target_os = "linux") {
        return;
    }
    locked(|| {
        let Ok(path) = path() else { return };
        let mut state = read(path);
        if state.armed_by == Some(std::process::id()) && !state.recovery_pending {
            state.armed_by = None;
            if let Err(err) = write(path, &state) {
                tracing::warn!(%err, "could not disarm skin recovery marker");
            }
        }
    });
}

/// Setup errors are not evidence that the selected skin exhausted memory.
pub struct SetupGuard(bool);

impl SetupGuard {
    pub fn new() -> Self {
        Self(false)
    }

    pub fn succeeded(mut self) {
        self.0 = true;
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        if !self.0 {
            graceful_exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_non_studio_launch_recovers() {
        let state = SkinState {
            skin: "lounge".into(),
            armed_by: Some(10),
            recovery_pending: false,
        };
        let (next, reason) = startup_state(state, false, true, 20, |_| false);
        assert_eq!(reason, RecoveryReason::PreviousExit);
        assert!(next.recovery_pending);
        assert_eq!(next.armed_by, None);
    }

    #[test]
    fn second_launch_does_not_steal_live_marker() {
        let state = SkinState {
            skin: "liquid".into(),
            armed_by: Some(10),
            recovery_pending: false,
        };
        let (next, reason) = startup_state(state, false, true, 20, |_| true);
        assert_eq!(reason, RecoveryReason::None);
        assert_eq!(next.armed_by, Some(10));
    }

    #[test]
    fn normal_launch_arms_non_studio_skin() {
        let state = SkinState {
            skin: "editorial".into(),
            armed_by: None,
            recovery_pending: false,
        };
        let (next, reason) = startup_state(state, false, true, 20, |_| false);
        assert_eq!(reason, RecoveryReason::None);
        assert_eq!(next.armed_by, Some(20));
    }

    #[test]
    fn manual_recovery_also_works_outside_linux() {
        let (next, reason) = startup_state(SkinState::default(), true, false, 20, |_| false);
        assert_eq!(reason, RecoveryReason::Manual);
        assert!(next.recovery_pending);
    }

    #[test]
    fn duplicate_manual_launch_does_not_reset_active_skin() {
        let overwritten = SkinState {
            skin: "lounge".into(),
            armed_by: None,
            recovery_pending: true,
        };
        let restored = restored_after_duplicate(overwritten, "lounge", false, 42);
        assert_eq!(restored.skin, "lounge");
        assert_eq!(restored.armed_by, Some(42));
        assert!(!restored.recovery_pending);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn updated_binary_still_counts_as_running() {
        assert!(same_executable_name(
            Path::new("/tmp/app/waveflow (deleted)"),
            Path::new("/usr/bin/waveflow")
        ));
    }
}
