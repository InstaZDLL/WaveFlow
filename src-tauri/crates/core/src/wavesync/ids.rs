//! The identifiers a session hands out.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An entry of the queue, as every participant names it.
///
/// Minted by the owner when the track is added, from 128 random bits the
/// caller draws ([`EntryId::from_bytes`]); this module draws none itself, so
/// that it stays deterministic under test. The owner keeps the mapping from
/// an entry to its track in memory: nothing in the id says which track, which
/// folder or which library it comes from, and an id is never reused once the
/// host has seen it ([`super::Host`] refuses a second entry under it).
///
/// Written `wse_` and 32 lowercase hex digits; nothing else parses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId([u8; 16]);

impl EntryId {
    /// What every written id starts with.
    pub const PREFIX: &'static str = "wse_";

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, hex::encode(self.0))
    }
}

impl fmt::Debug for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Why a written entry id was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a WaveSync entry id")]
pub struct BadEntryId;

impl FromStr for EntryId {
    type Err = BadEntryId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digits = s.strip_prefix(Self::PREFIX).ok_or(BadEntryId)?;
        // Lowercase only, so that one entry has one spelling: an id is
        // compared as a string by whoever logs or displays it.
        if digits.len() != 32
            || !digits
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(BadEntryId);
        }
        let mut bytes = [0u8; 16];
        hex::decode_to_slice(digits, &mut bytes).map_err(|_| BadEntryId)?;
        Ok(Self(bytes))
    }
}

impl Serialize for EntryId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EntryId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A participant, numbered by the host in the order they were admitted. The
/// host is the first. A number is never given twice in one session, so an
/// entry's owner can never come to mean somebody else.
///
/// Not a secret and not stable across sessions: it is what the log names a
/// participant by, instead of the name they typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParticipantId(pub u32);

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "p{}", self.0)
    }
}

/// A credential that must not be printed: the participant token, the
/// admission code. `Debug` shows a placeholder, so a message that holds one
/// can be logged whole without leaking it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value itself, for the one place that checks it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}
