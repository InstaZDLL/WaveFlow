//! WaveSync, the app's half (RFC-010): hosting or joining a session over
//! the LAN listener of [`waveflow_core::lan`], and keeping the interface told.
//!
//! The protocol is the core's ([`waveflow_core::wavesync`]): the host's
//! [`Host`](waveflow_core::wavesync::Host) is the only writer, every guest
//! holds a [`Replica`](waveflow_core::wavesync::Replica). This module owns
//! the rest: the connections, the admission (the code, then the host's
//! approval), the participants' tokens, the tracks of this library put in the
//! queue, and the [`View`] the interface draws from, emitted as
//! [`CHANGED_EVENT`] after every change.
//!
//! ## Lot 2: no audio crosses yet
//!
//! A participant plays only the entries it owns, as ordinary library tracks
//! (so they count in its history, decision 9); the others' are listed, not
//! playable. The ranged reads and the following of the host come in lot 3.
//!
//! ## Locks
//!
//! One `std::sync::Mutex` over the whole state, never held across an await:
//! a connection's messages go out through an unbounded channel its writer
//! task drains, so sending is a call, not a wait. Every task names the
//! session it belongs to ([`Inner::generation`]); a task of a session that
//! has since ended changes nothing.
//!
//! ## What never reaches the log
//!
//! The code, the tokens, an invitation, a participant's name, a track's
//! title: a line names a participant by its number and an entry by its id
//! ([invariant](../../../../docs/architecture/invariants.md#no-secret-reaches-the-log)).

mod entries;
mod guest;
mod host;
mod view;

use std::sync::{Mutex, MutexGuard, PoisonError};

use futures::{SinkExt, StreamExt};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use waveflow_core::lan::handshake::HandshakeRefusal;
use waveflow_core::wavesync::Refusal;

use crate::error::{AppError, AppResult, ShownKind};

pub use entries::AddOutcome;
pub use guest::Discovered;
pub use view::View;

/// What the listener is for: a code shown for WaveSync opens nothing else.
pub const CONTEXT: &str = "wavesync";

/// The mDNS service a hosted session is advertised under.
pub const SERVICE: &str = "_wavesync._tcp.local.";

/// Sent with the [`View`] after every change.
pub const CHANGED_EVENT: &str = "wavesync:changed";

/// How long a guest waits for the host to answer a request.
const ANSWER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why the last session ended, for the interface to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EndReason {
    /// This machine ended the session it hosted.
    Ended,
    /// This machine left the session it had joined.
    Left,
    /// The host ended the session.
    HostEnded,
    /// The connection to the host was lost.
    HostLeft,
    /// The host removed this participant.
    Removed,
    /// The host did not let this participant in.
    Refused,
    /// The host runs another version of WaveSync.
    Incompatible,
    /// The session was full.
    Full,
    /// Offline mode came on.
    Offline,
    /// Another profile became active.
    ProfileChanged,
    /// The host's state and this replica disagreed.
    Diverged,
}

/// The process-wide WaveSync state, managed by Tauri.
#[derive(Default)]
pub struct WaveSync {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    session: Option<Session>,
    /// Names the session now running: bumped at every start, join and end,
    /// so a task of an older one can tell.
    generation: u64,
    last_end: Option<EndReason>,
}

enum Session {
    Host(Box<host::HostSide>),
    Guest(Box<guest::GuestSide>),
}

/// The sending half of a connection: what goes out is queued here and
/// written by the connection's own task.
type Tx = mpsc::UnboundedSender<Message>;

impl WaveSync {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What the interface draws.
    pub fn view(&self) -> View {
        view::build(&self.lock())
    }

    fn emit(&self, app: &AppHandle) {
        let _ = app.emit(CHANGED_EVENT, self.view());
    }

    /// End whatever runs, for `reason`: offline mode, a profile switch, the
    /// user. Nothing happens when nothing runs.
    pub fn end(&self, app: &AppHandle, reason: EndReason) {
        let ended = {
            let mut inner = self.lock();
            let had = match inner.session.take() {
                Some(Session::Host(mut side)) => {
                    side.end();
                    true
                }
                Some(Session::Guest(side)) => {
                    side.leave();
                    true
                }
                None => false,
            };
            if had {
                inner.generation += 1;
                inner.last_end = Some(reason);
            }
            had
        };
        if ended {
            tracing::info!(?reason, "wavesync: session ended");
            self.emit(app);
        }
    }

    /// Forget why the last session ended, once the interface has said it.
    pub fn dismiss(&self, app: &AppHandle) {
        self.lock().last_end = None;
        self.emit(app);
    }
}

/// End the session if offline mode just came on: a guest's connection is
/// outbound, and a session is one thing (RFC-010 decision 8).
pub fn offline_changed(app: &AppHandle) {
    if crate::offline::is_offline() {
        if let Some(ws) = app.try_state::<WaveSync>() {
            ws.end(app, EndReason::Offline);
        }
    }
}

/// End the session before another profile becomes active: the entries are
/// the old profile's tracks.
pub fn profile_changing(app: &AppHandle) {
    if let Some(ws) = app.try_state::<WaveSync>() {
        ws.end(app, EndReason::ProfileChanged);
    }
}

fn refuse_offline() -> AppResult<()> {
    if crate::offline::is_offline() {
        return Err(AppError::shown(
            ShownKind::WavesyncOffline,
            "Offline mode is on: WaveSync is off.",
        ));
    }
    Ok(())
}

/// The sentence for a refusal of the host's.
fn refusal_error(reason: Refusal) -> AppError {
    let (kind, message) = match reason {
        Refusal::SessionFull => (ShownKind::WavesyncSessionFull, "The session is full."),
        Refusal::QueueFull => (ShownKind::WavesyncQueueFull, "The queue is full."),
        Refusal::EntryGone => (
            ShownKind::WavesyncEntryGone,
            "That track is no longer in the queue.",
        ),
        Refusal::NotShareable | Refusal::TooLarge | Refusal::InvalidEntry => (
            ShownKind::WavesyncNotShareable,
            "That track cannot be shared.",
        ),
        Refusal::ProtocolMismatch => (
            ShownKind::WavesyncIncompatible,
            "The host runs another version of WaveSync.",
        ),
        Refusal::SessionEnded => (ShownKind::WavesyncNoSession, "The session has ended."),
        Refusal::InvalidName => (ShownKind::WavesyncNameRequired, "A name is needed."),
        Refusal::NotAllowed
        | Refusal::UnknownParticipant
        | Refusal::DuplicateEntry
        | Refusal::NothingToPlay => (
            ShownKind::WavesyncNotAllowed,
            "The session does not allow that.",
        ),
    };
    AppError::shown(kind, message)
}

/// The sentence for a failed join.
fn join_error(error: waveflow_core::lan::client::JoinError) -> AppError {
    use waveflow_core::lan::client::JoinError;
    match error {
        JoinError::NotPrivate => AppError::shown(
            ShownKind::WavesyncNotPrivate,
            "That address is not on the local network.",
        ),
        JoinError::Unreachable | JoinError::TimedOut | JoinError::Tls | JoinError::Closed => {
            AppError::shown(ShownKind::WavesyncUnreachable, "The host does not answer.")
        }
        JoinError::Protocol => AppError::shown(
            ShownKind::WavesyncIncompatible,
            "The host runs another version of WaveSync.",
        ),
        JoinError::FingerprintMismatch => AppError::shown(
            ShownKind::WavesyncFingerprint,
            "The host is not the one the invitation names.",
        ),
        JoinError::CodeRejected => {
            AppError::shown(ShownKind::WavesyncCodeRejected, "The code did not match.")
        }
        JoinError::Refused(reason, wait) => match reason {
            HandshakeRefusal::WrongCode | HandshakeRefusal::Malformed => {
                AppError::shown(ShownKind::WavesyncCodeRejected, "The code did not match.")
            }
            HandshakeRefusal::RetryLater => {
                let seconds = wait.unwrap_or(1_000).div_ceil(1_000).max(1);
                AppError::shown(
                    ShownKind::WavesyncRetryLater,
                    format!("Too many wrong codes: wait {seconds} s."),
                )
                .with_param("seconds", seconds.to_string())
            }
            HandshakeRefusal::Closed => AppError::shown(
                ShownKind::WavesyncClosed,
                "The host is not letting anyone in.",
            ),
            HandshakeRefusal::Expired => AppError::shown(
                ShownKind::WavesyncCodeExpired,
                "The host drew a new code: ask for it.",
            ),
            HandshakeRefusal::Context | HandshakeRefusal::Version => AppError::shown(
                ShownKind::WavesyncIncompatible,
                "The host runs another version of WaveSync.",
            ),
        },
    }
}

/// Write `rx` to `sink` until every sender is gone, then close it.
async fn write_loop<S>(mut sink: S, mut rx: mpsc::UnboundedReceiver<Message>)
where
    S: futures::Sink<Message> + Unpin,
{
    while let Some(message) = rx.recv().await {
        if sink.send(message).await.is_err() {
            return;
        }
    }
    let _ = sink.close().await;
}

/// A JSON message as a text frame.
fn text<T: Serialize>(message: &T) -> Message {
    Message::text(waveflow_core::wavesync::wire::encode(message))
}

/// The next text frame of `stream`, skipping control and binary frames;
/// `None` once it closes.
async fn next_text<S>(stream: &mut S) -> Option<String>
where
    S: futures::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await? {
            Ok(Message::Text(text)) => return Some(text.as_str().to_owned()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

/// The name a participant shows, or the error for an empty one.
fn display_name(name: &str) -> AppResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::shown(
            ShownKind::WavesyncNameRequired,
            "A name is needed.",
        ));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests;
