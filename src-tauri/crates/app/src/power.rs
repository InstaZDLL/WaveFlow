//! Keep the computer awake while music plays.
//!
//! Without it, a desktop that sees no input for its idle delay suspends in
//! the middle of an album — reported on GNOME, where the idle suspend runs
//! whatever is playing. A sleep inhibitor is held while the player is
//! `Playing` and the setting is on (`profile_setting['playback.keep_awake']`,
//! default on), and released on pause, stop, idle or when the setting is
//! turned off. It never blocks a suspend the user asks for: closing the lid
//! or picking Suspend still suspends.
//!
//! - **Linux**: the XDG desktop portal's `Inhibit` (suspend + idle), which
//!   GNOME and KDE route into their own power management and which works
//!   inside the Flatpak sandbox without extra permissions. A logind `idle`
//!   lock would not reach GNOME's auto-suspend, and a logind `sleep` lock in
//!   `block` mode would refuse even a requested suspend.
//! - **Windows**: `SetThreadExecutionState(ES_SYSTEM_REQUIRED)`. That state
//!   belongs to the thread that set it, which is why one worker thread owns
//!   the inhibitor for the life of the process.
//! - **macOS**: nothing to do. `coreaudiod` holds its own assertion while
//!   audio is being played.
//!
//! The theater view asks for more: the **display** kept on while it is open
//! and the music plays (`set_display_held`), whatever the setting — a
//! screen showing the album from across the room must not go dark between
//! two songs. Windows adds `ES_DISPLAY_REQUIRED`; Linux's `idle` flag above
//! already keeps the screen on; macOS runs `caffeinate -d` for as long as
//! the hold lasts, tied to this process so it cannot outlive it.
//!
//! Process-wide, like the offline mode: the decoder's state transitions and
//! the profile settings loader both reach it without an `AppHandle`.

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

enum Msg {
    Playing(bool),
    Enabled(bool),
    Display(bool),
}

/// What the worker should hold, from the three inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// The machine stays awake; the screen may still turn off.
    System,
    /// The machine and the screen both stay on.
    Display,
}

/// Nothing while paused; the display while the theater view is open;
/// otherwise the system, if the setting is on. The display hold ignores
/// the setting, which is about a suspend in the middle of an album.
fn wanted(playing: bool, enabled: bool, display: bool) -> Option<Hold> {
    if !playing {
        None
    } else if display {
        Some(Hold::Display)
    } else if enabled {
        Some(Hold::System)
    } else {
        None
    }
}

static WORKER: OnceLock<Sender<Msg>> = OnceLock::new();

fn send(msg: Msg) {
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        if let Err(err) = std::thread::Builder::new()
            .name("waveflow-power".into())
            .spawn(move || run(rx))
        {
            // The receiver is gone with the closure: every later send
            // fails quietly, and playback carries on without the lock.
            tracing::warn!(%err, "power: worker thread failed to start");
        }
        tx
    });
    let _ = tx.send(msg);
}

/// The player entered or left `Playing`. Cheap, non-blocking: called from
/// the decoder thread on every state transition.
pub fn set_playing(playing: bool) {
    send(Msg::Playing(playing));
}

/// The setting, read on every profile load and written by its command.
pub fn set_enabled(enabled: bool) {
    send(Msg::Enabled(enabled));
}

/// The theater view opened (`true`) or closed: keep the display on while
/// the music plays.
pub fn set_display_held(held: bool) {
    send(Msg::Display(held));
}

fn run(rx: mpsc::Receiver<Msg>) {
    let mut playing = false;
    let mut enabled = true;
    let mut display = false;
    let mut held: Option<(Hold, sys::Inhibitor)> = None;
    // Without a portal every play would fail the same way: say it once.
    let mut warned = false;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Playing(value) => playing = value,
            Msg::Enabled(value) => enabled = value,
            Msg::Display(value) => display = value,
        }
        let want = wanted(playing, enabled, display);
        if held.as_ref().map(|(hold, _)| *hold) == want {
            continue;
        }
        // Released before the next one is taken: on Windows both are the
        // same thread state, and the old one's drop would clear the new.
        if held.take().is_some() {
            tracing::debug!("power: inhibitor released");
        }
        let Some(hold) = want else { continue };
        match sys::Inhibitor::acquire(hold == Hold::Display) {
            Ok(inhibitor) => {
                tracing::debug!(?hold, "power: sleep inhibited while playing");
                held = Some((hold, inhibitor));
            }
            // No portal (a bare window manager), or the call failed:
            // the machine may sleep, nothing else is affected.
            Err(err) if !warned => {
                warned = true;
                tracing::warn!(%err, "power: could not inhibit sleep");
            }
            Err(err) => tracing::debug!(%err, "power: could not inhibit sleep"),
        }
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::collections::HashMap;

    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedObjectPath, Value};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    /// `Inhibit` flags: 4 = suspend, 8 = idle.
    const SUSPEND_AND_IDLE: u32 = 4 | 8;

    /// A portal inhibition, held until `Request.Close` on its handle.
    pub struct Inhibitor {
        conn: Connection,
        handle: OwnedObjectPath,
    }

    impl Inhibitor {
        /// `display` changes nothing here: the `idle` flag already keeps
        /// the screen from blanking.
        pub fn acquire(_display: bool) -> Result<Self, String> {
            let conn = Connection::session().map_err(|e| e.to_string())?;
            let mut options: HashMap<&str, Value<'_>> = HashMap::new();
            options.insert("reason", Value::from("Playing music"));
            let reply = conn
                .call_method(
                    Some(PORTAL),
                    "/org/freedesktop/portal/desktop",
                    Some("org.freedesktop.portal.Inhibit"),
                    "Inhibit",
                    &("", SUSPEND_AND_IDLE, options),
                )
                .map_err(|e| e.to_string())?;
            let handle: OwnedObjectPath = reply.body().deserialize().map_err(|e| e.to_string())?;
            Ok(Self { conn, handle })
        }
    }

    impl Drop for Inhibitor {
        fn drop(&mut self) {
            // Closing the connection would end it too; closing the request
            // says so explicitly and leaves nothing to the bus.
            if let Err(err) = self.conn.call_method(
                Some(PORTAL),
                &self.handle,
                Some("org.freedesktop.portal.Request"),
                "Close",
                &(),
            ) {
                tracing::debug!(%err, "power: closing the inhibit request failed");
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod sys {
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    /// `ES_SYSTEM_REQUIRED` (plus `ES_DISPLAY_REQUIRED` for the display)
    /// for the calling thread, cleared on drop. Both happen on the power
    /// worker, the one thread that ever touches it.
    pub struct Inhibitor;

    impl Inhibitor {
        pub fn acquire(display: bool) -> Result<Self, String> {
            let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
            if display {
                flags |= ES_DISPLAY_REQUIRED;
            }
            // SAFETY: plain Win32 call with documented flag values.
            let previous = unsafe { SetThreadExecutionState(flags) };
            if previous.0 == 0 {
                return Err("SetThreadExecutionState failed".into());
            }
            Ok(Self)
        }
    }

    impl Drop for Inhibitor {
        fn drop(&mut self) {
            // SAFETY: as above; ES_CONTINUOUS alone clears the request.
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::process::{Child, Command};

    /// `coreaudiod` already keeps the machine awake while audio plays; the
    /// display needs `caffeinate -d`, which `-w` ties to this process so a
    /// crash cannot leave the screen held.
    pub struct Inhibitor(Option<Child>);

    impl Inhibitor {
        pub fn acquire(display: bool) -> Result<Self, String> {
            if !display {
                return Ok(Self(None));
            }
            Command::new("/usr/bin/caffeinate")
                .args(["-d", "-w", &std::process::id().to_string()])
                .spawn()
                .map(|child| Self(Some(child)))
                .map_err(|e| e.to_string())
        }
    }

    impl Drop for Inhibitor {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod sys {
    pub struct Inhibitor;

    impl Inhibitor {
        pub fn acquire(_display: bool) -> Result<Self, String> {
            Ok(Self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{wanted, Hold};

    #[test]
    fn nothing_is_held_while_paused() {
        for enabled in [false, true] {
            for display in [false, true] {
                assert_eq!(wanted(false, enabled, display), None);
            }
        }
    }

    #[test]
    fn the_setting_keeps_the_system_awake_while_playing() {
        assert_eq!(wanted(true, true, false), Some(Hold::System));
        assert_eq!(wanted(true, false, false), None);
    }

    #[test]
    fn the_theater_view_holds_the_display_whatever_the_setting() {
        assert_eq!(wanted(true, true, true), Some(Hold::Display));
        assert_eq!(wanted(true, false, true), Some(Hold::Display));
    }
}
