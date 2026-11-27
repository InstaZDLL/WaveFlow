//! Tokens: a credential handed to a peer once, kept by the host only as a
//! hash.

use std::fmt;

use sha2::{Digest, Sha256};

/// A fresh token: `prefix` and 32 random bytes in hex. 256 bits, so a fast
/// hash is enough to store it (the server's reasoning for its own tokens).
pub fn generate(prefix: &str) -> String {
    from_random(prefix, super::random())
}

/// The token `bytes` make.
pub fn from_random(prefix: &str, bytes: [u8; 32]) -> String {
    format!("{prefix}{}", hex::encode(bytes))
}

/// What the host keeps of a token: its SHA-256. `Debug` hides it anyway, so
/// that a struct holding one can be logged.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenHash([u8; 32]);

impl TokenHash {
    pub fn of(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()).into())
    }

    /// Whether `presented` is the token, compared in constant time.
    pub fn matches(&self, presented: &str) -> bool {
        let other = Self::of(presented);
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenHash(***)")
    }
}
