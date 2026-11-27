//! The exchange's messages, as text frames of JSON. Binary values are hex.

use serde::{Deserialize, Serialize};

/// What a guest sends during the exchange.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GuestHandshake {
    /// The first frame of a guest with a code.
    ExchangeStart {
        version: u32,
        context: String,
        guest: String,
    },
    ExchangeConfirm {
        confirm: String,
    },
}

/// What the host answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostHandshake {
    ExchangeReply {
        host: String,
        confirm: String,
    },
    /// The guest is in. What follows is the feature's.
    ExchangeDone,
    ExchangeRefused {
        reason: HandshakeRefusal,
        /// With [`HandshakeRefusal::RetryLater`]: how long to wait.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeRefusal {
    #[error("the code did not match")]
    WrongCode,
    #[error("too many wrong codes from this address; wait")]
    RetryLater,
    #[error("the host is not admitting anyone")]
    Closed,
    #[error("the host is listening for something else")]
    Context,
    #[error("the host speaks another handshake version")]
    Version,
    #[error("the exchange message was not understood")]
    Malformed,
    /// The host drew a new code, or closed admission, while the guest was
    /// answering: ask for the code again.
    #[error("the code changed while joining")]
    Expired,
}

/// The `type` of a first frame, without reading the rest: what tells the
/// listener whether a connection starts the exchange or brings something of
/// its own (a token).
pub fn message_type(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Kind {
        #[serde(rename = "type")]
        kind: String,
    }
    serde_json::from_str::<Kind>(text).ok().map(|k| k.kind)
}

/// The first frame's type that starts the exchange.
pub const EXCHANGE_START: &str = "exchange_start";
