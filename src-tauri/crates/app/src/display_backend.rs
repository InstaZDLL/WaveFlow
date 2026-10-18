//! Running through XWayland on a Wayland desktop, by choice (#894).
//!
//! Wayland's standard shell protocol gives a client no way to stay above
//! other windows, nor to read or set where its window is. So on a native
//! Wayland session the mini-player's pin and the desktop lyrics' "always
//! on top" are ignored, and the lyrics overlay cannot come back where it
//! was dragged: the compositor decides, and none of the mainstream ones
//! (Mutter, KWin) honour the request. Under XWayland the window is an X11
//! client again, and both work.
//!
//! The choice is the user's, because it costs something: text is less
//! crisp under fractional scaling, which XWayland renders at an integer
//! scale and stretches. So it is a setting, off by default, and only
//! offered on a Wayland session.
//!
//! **Decided before GTK exists.** GTK picks its backend from `GDK_BACKEND`
//! when it initialises, which happens while the first window is built.
//! The choice is therefore stored in a small file under the app-data root,
//! like the renderer's ([`crate::render_mode`]), read by the preflight
//! that runs before anything else, and it takes effect on the next start.
//! A `GDK_BACKEND` the user set themselves always wins.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const FILE: &str = "display-backend.json";

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    xwayland: bool,
}

/// What this launch found and did, recorded once by [`decide`].
#[derive(Debug, Clone)]
struct Launch {
    path: PathBuf,
    /// A Wayland session, read before this module touched the environment.
    wayland_session: bool,
    /// `GDK_BACKEND` was already set when the app started: the user's
    /// own choice, which the stored one never overrides.
    set_by_user: bool,
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();

/// What the Settings card is told.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DisplayBackendStatus {
    /// Linux on a Wayland session: the only place the choice means
    /// anything, and the only place the card shows.
    pub available: bool,
    /// The stored choice, which the next start applies.
    pub xwayland: bool,
    /// This launch runs through XWayland, whoever asked for it.
    pub active: bool,
    /// `GDK_BACKEND` came from the environment, so the stored choice
    /// changes nothing until it is removed.
    pub set_by_user: bool,
}

fn wayland_session() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

fn gdk_backend_is_x11() -> bool {
    std::env::var("GDK_BACKEND").is_ok_and(|b| b.starts_with("x11"))
}

fn read(path: &Path) -> Stored {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Read the choice and, on a Wayland session, ask GTK for its X11 backend.
///
/// Called by the preflight in `lib.rs`, before logging and before any
/// window. Never fails: a file that cannot be read is the default, which
/// is native Wayland, exactly as before this existed.
pub fn decide(root: &Path) {
    let path = root.join(FILE);
    let wayland_session = cfg!(target_os = "linux") && wayland_session();
    let set_by_user = std::env::var_os("GDK_BACKEND").is_some();
    let _ = LAUNCH.set(Launch {
        path: path.clone(),
        wayland_session,
        set_by_user,
    });
    if !wayland_session || set_by_user {
        return;
    }
    if read(&path).xwayland {
        // Single-threaded here: the preflight runs before the logger,
        // the runtime or any window exists.
        std::env::set_var("GDK_BACKEND", "x11");
    }
}

pub fn status() -> DisplayBackendStatus {
    let Some(launch) = LAUNCH.get() else {
        return DisplayBackendStatus {
            available: false,
            xwayland: false,
            active: false,
            set_by_user: false,
        };
    };
    DisplayBackendStatus {
        available: launch.wayland_session,
        xwayland: read(&launch.path).xwayland,
        active: launch.wayland_session && gdk_backend_is_x11(),
        set_by_user: launch.set_by_user,
    }
}

/// Store the choice for the next start. Written to a temporary file and
/// renamed, so a crash mid-write leaves the old choice rather than a
/// truncated file read as the default.
pub fn set_xwayland(enabled: bool) -> std::io::Result<()> {
    let Some(launch) = LAUNCH.get() else {
        return Err(std::io::Error::other("no app-data directory this launch"));
    };
    let tmp = launch.path.with_extension("json.tmp");
    let body = serde_json::to_vec(&Stored { xwayland: enabled }).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &launch.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_broken_file_reads_as_native_wayland() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        assert!(!read(&path).xwayland);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(!read(&path).xwayland);
        std::fs::write(&path, br#"{"xwayland":true}"#).unwrap();
        assert!(read(&path).xwayland);
    }
}
