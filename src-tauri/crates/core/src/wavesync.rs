//! WaveSync: several desktops listening to one queue, without a server
//! ([RFC-010](../../../../docs/rfcs/RFC-010-wavesync.md)).
//!
//! This module is the protocol and nothing else: no socket, no file, no
//! clock, no randomness. Everything that happens to a session is a value in,
//! values out, so the host and every guest run the same code on the same
//! inputs and land in the same state, and the tests can play a whole evening
//! in a few microseconds. The listener, the TLS, the connections and the
//! audio are the app's.
//!
//! ## The host is the only writer
//!
//! One participant hosts. [`Host`] holds the session: who is in it, the
//! queue, what plays and from where. A guest changes nothing on its own; it
//! sends an [`Action`] and the host either refuses it ([`Refusal`]) or turns
//! it into [`Op`]s, numbered in one sequence ([`Sequenced`]) and broadcast.
//! Every participant, the host included, applies those ops to a
//! [`SessionState`] through the same [`SessionState::apply`], so a replica
//! ([`Replica`]) cannot drift from the host unless it misses an op, which it
//! notices by the number and repairs with a [`Snapshot`].
//!
//! There are no concurrent writers, so nothing is merged: no vector clock,
//! no last-write-wins. RFC-005 dropped logical clocks for the server's sync
//! for the same reason.
//!
//! ## What may be read
//!
//! An entry's bytes are readable while the session's cursor has not passed
//! it ([`SessionState::readable`]): the one playing and those after it.
//! Removing it, or playing past it, revokes it. The ranged reads themselves
//! are [`frame`]s.
//!
//! ## Following the host
//!
//! What plays is an [`Anchor`]: an entry, a position, the host's time at
//! that position, playing or paused. A guest estimates the host's clock from
//! round trips and decides whether to load, seek, pause or leave its player
//! alone ([`clock`]).
//!
//! The module is named `wavesync`, never `sync`: that name is the server's
//! sync v2 ([`crate::sync`] and the app's `remote`).

pub mod clock;
mod entry;
pub mod frame;
mod host;
mod ids;
mod replica;
mod state;
pub mod wire;

pub use entry::{Entry, Gain};
pub use host::{Action, CatchUp, Host};
pub use ids::{BadEntryId, EntryId, ParticipantId, Secret};
pub use replica::{Applied, Replica, ReplicaError};
pub use state::{
    Anchor, ApplyError, Op, Participant, ParticipantState, Position, Rights, Role, Sequenced,
    SessionState, Snapshot,
};

use serde::{Deserialize, Serialize};

/// The protocol this build speaks. A peer on another version is refused
/// with [`Refusal::ProtocolMismatch`] at the first message, never half
/// understood.
pub const PROTOCOL_VERSION: u32 = 1;

/// Most participants in one session, the host included (RFC-010 Decision 6).
pub const MAX_PARTICIPANTS: usize = 8;

/// Most entries still to play (the current one included).
pub const MAX_UPCOMING: usize = 500;

/// Played entries kept listed in the queue; older ones are dropped, on every
/// participant at the same op, so the state of an evening stays small.
pub const MAX_PLAYED_KEPT: usize = 100;

/// Largest file an entry may describe.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// How many ops the host keeps for a guest that reconnects; one that missed
/// more gets a [`Snapshot`].
pub const LOG_KEPT: usize = 512;

/// Longest participant or session name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

/// A name as the session keeps it: trimmed, without control characters, cut
/// to [`MAX_NAME_CHARS`]. What the host's approval prompt shows is this, so
/// the name approved is the name admitted.
pub fn clean_name(name: &str) -> String {
    entry::clean(name, MAX_NAME_CHARS)
}

/// Why the host said no. Sent to the participant whose request it was, so
/// the button they pressed can say what happened; also the codes a ranged
/// read fails with ([`frame::ReadFailure`] is the binary half).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    #[error("this participant may not do that")]
    NotAllowed,
    #[error("the entry is not in the queue")]
    EntryGone,
    #[error("the queue is full")]
    QueueFull,
    #[error("the session is full")]
    SessionFull,
    #[error("the entry is not valid")]
    InvalidEntry,
    #[error("an entry with this id was already added")]
    DuplicateEntry,
    #[error("the file is too large to share")]
    TooLarge,
    #[error("this format cannot be shared")]
    NotShareable,
    #[error("the name is empty")]
    InvalidName,
    #[error("no such participant")]
    UnknownParticipant,
    #[error("nothing in the queue can be played")]
    NothingToPlay,
    #[error("the peer speaks another protocol version")]
    ProtocolMismatch,
    #[error("the session has ended")]
    SessionEnded,
}

#[cfg(test)]
mod tests;
