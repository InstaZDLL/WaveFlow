//! Linux audio-device reservation over D-Bus (`org.freedesktop.ReserveDevice1`).
//!
//! ## Why
//!
//! [`super::alsa_exclusive`] opens the DAC as a raw `hw:` device, which
//! is the only way a DoP marker cadence survives to the hardware. On a
//! desktop that device is already taken: PipeWire (or PulseAudio) grabs
//! every card at login and holds it. `snd_pcm_open` then fails with
//! `EBUSY`, we logged a `warn!` nobody reads and quietly fell back to
//! DSD → PCM — so the setting the user turned on did nothing, and said
//! nothing about it.
//!
//! Waiting doesn't help: the sound server has no reason to let go. The
//! protocol every sound server on Linux implements for exactly this is
//! `org.freedesktop.ReserveDevice1`: a client that wants a card takes
//! the well-known name `org.freedesktop.ReserveDevice1.Audio<N>` on the
//! session bus, and the server — watching for `NameLost` — closes its
//! handle on that card. Taking the name is the polite, supported way to
//! ask, and it is what every other exclusive-output player does.
//!
//! ## What this does not do
//!
//! The spec's full handshake lets a would-be owner read the current
//! owner's `Priority` and call `RequestRelease` on it. We skip that and
//! request the name with `ReplaceExisting`, which is enough against the
//! sound servers in practice — they take the name with
//! `AllowReplacement` precisely so a client like this can step in. If
//! the current owner refuses, we are exactly where we were before:
//! [`Reservation::acquire`] returns `None` and the caller falls back.
//!
//! We do request `AllowReplacement` for ourselves, so anything with a
//! stronger claim can take the card back the same way. We don't export
//! the `RequestRelease` object, so that hand-back is a replacement
//! rather than a conversation — acceptable because we only hold the
//! reservation while a DSD stream is actually playing, and release it
//! on the way out.

use std::time::Duration;

use zbus::blocking::{fdo::DBusProxy, Connection};
use zbus::fdo::{ReleaseNameReply, RequestNameFlags, RequestNameReply};
use zbus::names::WellKnownName;

/// How long to keep trying the `hw:` open after the sound server has
/// been asked to let go. Releasing is asynchronous on its side: it sees
/// `NameLost`, finishes the buffer it is on and only then closes the
/// device, so the first open right after the name lands still fails.
pub const RELEASE_GRACE: Duration = Duration::from_millis(1_000);

/// One held device reservation. Dropping it hands the card back.
pub struct Reservation {
    connection: Connection,
    name: String,
}

impl Reservation {
    /// Take `org.freedesktop.ReserveDevice1.Audio<card>` on the session
    /// bus, or `None` when we can't (no session bus, name refused).
    ///
    /// `None` is never fatal — it means "carry on and try the open
    /// anyway", which is what the code did before this module existed.
    pub fn acquire(card: i32) -> Option<Self> {
        let name = format!("org.freedesktop.ReserveDevice1.Audio{card}");

        // A session bus is not guaranteed: a headless service, a login
        // without one, a sandbox that doesn't forward it. Nothing else
        // in the audio path needs D-Bus, so this stays a warning.
        let connection = match Connection::session() {
            Ok(connection) => connection,
            Err(err) => {
                tracing::warn!(%err, "device reservation: no session bus, opening the card as-is");
                return None;
            }
        };

        let proxy = match DBusProxy::new(&connection) {
            Ok(proxy) => proxy,
            Err(err) => {
                tracing::warn!(%err, "device reservation: no bus proxy, opening the card as-is");
                return None;
            }
        };

        let well_known = match WellKnownName::try_from(name.as_str()) {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(%err, %name, "device reservation: rejected bus name");
                return None;
            }
        };

        let flags = RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue
            | RequestNameFlags::AllowReplacement;

        match proxy.request_name(well_known, flags) {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
                tracing::info!(%name, "device reservation acquired");
                Some(Self { connection, name })
            }
            Ok(reply) => {
                // The current owner didn't allow replacement. Nothing
                // left to try, and the open below will say why.
                tracing::warn!(
                    %name,
                    %reply,
                    "device reservation refused; the card stays with its current owner"
                );
                None
            }
            Err(err) => {
                tracing::warn!(%err, %name, "device reservation request failed");
                None
            }
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // Dropping the connection would release the name on its own,
        // but only once the bus notices the socket closed. Asking
        // explicitly gets the card back to the sound server while the
        // user is still looking at the app.
        let Ok(name) = WellKnownName::try_from(self.name.as_str()) else {
            return;
        };
        // Two error types on the way (the bus proxy is `zbus::Error`,
        // the call is `zbus::fdo::Error`), so this is two steps rather
        // than a chain.
        let proxy = match DBusProxy::new(&self.connection) {
            Ok(proxy) => proxy,
            Err(err) => {
                tracing::warn!(%err, name = %self.name, "device reservation: no bus proxy to release with");
                return;
            }
        };
        match proxy.release_name(name) {
            Ok(ReleaseNameReply::Released) => {
                tracing::info!(name = %self.name, "device reservation released")
            }
            // We asked for `AllowReplacement`, so the sound server is
            // free to take the name back mid-stream. That costs the
            // stream nothing — the `hw:` handle is already open — but
            // the release then finds nothing of ours to give up, and
            // `Ok` covers both. Logging "released" for all three would
            // put a small lie in the only trace this path leaves.
            Ok(reply) => {
                tracing::info!(
                    name = %self.name,
                    %reply,
                    "device reservation was no longer ours to release"
                )
            }
            Err(err) => {
                tracing::warn!(%err, name = %self.name, "device reservation release failed")
            }
        }
    }
}
