//! The control messages, as text frames of JSON.
//!
//! Every message is an object whose `type` names it. A peer that receives a
//! `type` it does not know answers [`HostMessage::Unsupported`] (or a guest
//! drops the connection) rather than skipping it: a newer peer must learn
//! that it was not understood. A text frame is read only up to
//! [`MAX_TEXT_FRAME`].

use serde::{Deserialize, Serialize};

use super::{Action, ParticipantId, Refusal, Secret, Sequenced, Snapshot, PROTOCOL_VERSION};

/// Largest text frame read, in bytes. A full queue's snapshot is well under
/// a quarter of it.
pub const MAX_TEXT_FRAME: usize = 1024 * 1024;

/// What a guest sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestMessage {
    /// The first message of a new participant, once the code exchange has
    /// succeeded: the name to show, and the protocol spoken.
    Join { protocol: u32, name: String },
    /// The first message of a participant coming back: who it was, the
    /// token it was given, the last op it applied.
    Resume {
        protocol: u32,
        participant: ParticipantId,
        token: Secret,
        after: u64,
    },
    /// An action, numbered by the guest so it can match the answer.
    Request { req: u32, action: Action },
    /// A clock probe: the guest's clock when it was sent.
    Ping { sent_ms: u64 },
}

/// What the host sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// Admitted. `token` is sent once, on a first admission, and never
    /// again; the host keeps only its hash.
    Welcome {
        protocol: u32,
        you: ParticipantId,
        token: Option<Secret>,
        snapshot: Snapshot,
    },
    /// Not admitted, or no longer.
    Refused { reason: Refusal },
    /// Ops, in sequence: the next ones, or those a reconnection missed.
    Ops { ops: Vec<Sequenced> },
    /// Start again from this.
    Snapshot { snapshot: Snapshot },
    /// The answer to a request: the last op it made (`seq`), or why not.
    Answer {
        req: u32,
        #[serde(flatten)]
        outcome: Outcome,
    },
    /// A clock probe's answer: the guest's time it echoes, the host's now.
    Pong { sent_ms: u64, host_ms: u64 },
    /// A message whose `type` the host does not know.
    Unsupported { kind: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    /// Done; `seq` is the last op it produced, or the current one when it
    /// changed nothing.
    Applied {
        seq: u64,
    },
    Refused {
        reason: Refusal,
    },
}

/// Why a text frame was not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("the frame is larger than {MAX_TEXT_FRAME} bytes")]
    TooLarge,
    /// A well-formed message of a type this build does not know. The type is
    /// cut to a few characters: it is the peer's text.
    #[error("unsupported message type")]
    Unsupported(String),
    #[error("malformed message")]
    Malformed,
}

/// Message types a guest may send, for telling a malformed message from an
/// unknown one.
const GUEST_TYPES: &[&str] = &["join", "resume", "request", "ping"];
const HOST_TYPES: &[&str] = &[
    "welcome",
    "refused",
    "ops",
    "snapshot",
    "answer",
    "pong",
    "unsupported",
];

/// Longest message type echoed back in [`HostMessage::Unsupported`].
const MAX_KIND_CHARS: usize = 32;

pub fn decode_guest(text: &str) -> Result<GuestMessage, WireError> {
    decode(text, GUEST_TYPES)
}

pub fn decode_host(text: &str) -> Result<HostMessage, WireError> {
    decode(text, HOST_TYPES)
}

pub fn encode<T: Serialize>(message: &T) -> String {
    // Infallible for these types: no map has a non-string key, no float is
    // serialised as anything JSON cannot hold (a NaN gain becomes null).
    serde_json::to_string(message).unwrap_or_default()
}

fn decode<T: for<'de> Deserialize<'de>>(text: &str, known: &[&str]) -> Result<T, WireError> {
    if text.len() > MAX_TEXT_FRAME {
        return Err(WireError::TooLarge);
    }
    match serde_json::from_str::<T>(text) {
        Ok(message) => Ok(message),
        Err(_) => {
            #[derive(Deserialize)]
            struct Kind {
                #[serde(rename = "type")]
                kind: String,
            }
            match serde_json::from_str::<Kind>(text) {
                Ok(Kind { kind }) if !known.contains(&kind.as_str()) => Err(
                    WireError::Unsupported(kind.chars().take(MAX_KIND_CHARS).collect()),
                ),
                _ => Err(WireError::Malformed),
            }
        }
    }
}

/// Whether a peer announcing `protocol` can take part.
pub fn check_protocol(protocol: u32) -> Result<(), Refusal> {
    if protocol == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(Refusal::ProtocolMismatch)
    }
}
