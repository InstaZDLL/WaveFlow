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
//! Process-wide, like the offline mode: the decoder's state transitions and
//! the profile settings loader both reach it without an `AppHandle`.

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

enum Msg {
    Playing(bool),
    Enabled(bool),
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

fn run(rx: mpsc::Receiver<Msg>) {
    let mut playing = false;
    let mut enabled = true;
    let mut held: Option<sys::Inhibitor> = None;
    // Without a portal every play would fail the same way: say it once.
    let mut warned = false;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Playing(value) => playing = value,
            Msg::Enabled(value) => enabled = value,
        }
        let wanted = playing && enabled;
        if wanted && held.is_none() {
            match sys::Inhibitor::acquire() {
                Ok(inhibitor) => {
                    tracing::debug!("power: sleep inhibited while playing");
                    held = Some(inhibitor);
                }
                // No portal (a bare window manager), or the call failed:
                // the machine may sleep, nothing else is affected.
                Err(err) if !warned => {
                    warned = true;
                    tracing::warn!(%err, "power: could not inhibit sleep");
                }
                Err(err) => tracing::debug!(%err, "power: could not inhibit sleep"),
            }
        } else if !wanted && held.is_some() {
            // Dropping releases it.
            held = None;
            tracing::debug!("power: sleep inhibitor released");
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
        pub fn acquire() -> Result<Self, String> {
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
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    /// `ES_SYSTEM_REQUIRED` for the calling thread, cleared on drop. Both
    /// happen on the power worker, the one thread that ever touches it.
    pub struct Inhibitor;

    impl Inhibitor {
        pub fn acquire() -> Result<Self, String> {
            // SAFETY: plain Win32 call with documented flag values.
            let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED) };
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

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod sys {
    /// macOS: `coreaudiod` already asserts while audio plays.
    pub struct Inhibitor;

    impl Inhibitor {
        pub fn acquire() -> Result<Self, String> {
            Ok(Self)
        }
    }
}
