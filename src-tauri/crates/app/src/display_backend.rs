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
    /// An X server to fall back to: XWayland exports `DISPLAY`. Without
    /// one, asking GTK for X11 would leave it no display to open, and the
    /// app would not start — with the switch that caused it out of reach.
    xwayland_present: bool,
    /// `GDK_BACKEND` was already set when the app started: the user's
    /// own choice, which the stored one never overrides.
    set_by_user: bool,
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();

/// What the Settings card is told.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DisplayBackendStatus {
    /// Linux on a Wayland session that runs XWayland: the only place the
    /// choice means anything, and the only place the card shows.
    pub available: bool,
    /// The stored choice, which the next start applies.
    pub xwayland: bool,
    /// This launch runs through XWayland, whoever asked for it.
    pub active: bool,
    /// `GDK_BACKEND` came from the environment, so the stored choice
    /// changes nothing until it is removed.
    pub set_by_user: bool,
}

/// A Wayland desktop session, whatever backend GTK opened the window on:
/// under XWayland the session is still Wayland (the global shortcuts
/// depend on that, not on the window).
pub(crate) fn wayland_session() -> bool {
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
/// Called by the preflight in `startup/preflight.rs`, before logging and before any
/// window. Never fails: a file that cannot be read is the default, which
/// is native Wayland, exactly as before this existed.
pub fn decide(root: &Path) {
    let path = root.join(FILE);
    let wayland_session = cfg!(target_os = "linux") && wayland_session();
    let set_by_user = std::env::var_os("GDK_BACKEND").is_some();
    let xwayland_present = std::env::var_os("DISPLAY").is_some();
    let _ = LAUNCH.set(Launch {
        path: path.clone(),
        wayland_session,
        xwayland_present,
        set_by_user,
    });
    // Checked on every start, not only when the choice was stored: a
    // session that ran XWayland yesterday may not today.
    if !wayland_session || !xwayland_present || set_by_user {
        return;
    }
    if read(&path).xwayland {
        // Single-threaded here: the preflight runs before the logger,
        // the runtime or any window exists. `x11,wayland` rather than
        // `x11`: should X11 fail to open after all, GTK falls back to
        // Wayland instead of showing nothing.
        std::env::set_var("GDK_BACKEND", "x11,wayland");
        // Ours, not the user's: a browser started for a link must not
        // inherit it and open under XWayland too.
        #[cfg(target_os = "linux")]
        crate::external_open::note_own_variable("GDK_BACKEND");
    }
}

/// `opened_x11`: whether GTK really opened the main window on X11, read
/// from its handle by the caller. `GDK_BACKEND=x11,wayland` falls back to
/// Wayland when X11 fails to open, so the variable alone can claim
/// XWayland for a launch that is not using it; the variable is only the
/// answer when no window exists to ask.
pub fn status(opened_x11: Option<bool>) -> DisplayBackendStatus {
    let Some(launch) = LAUNCH.get() else {
        return DisplayBackendStatus {
            available: false,
            xwayland: false,
            active: false,
            set_by_user: false,
        };
    };
    DisplayBackendStatus {
        available: launch.wayland_session && launch.xwayland_present,
        xwayland: read(&launch.path).xwayland,
        active: launch.wayland_session && opened_x11.unwrap_or_else(gdk_backend_is_x11),
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
mod tests;
