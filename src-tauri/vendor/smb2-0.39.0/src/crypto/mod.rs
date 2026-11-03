//! Cryptographic operations for SMB2/3: signing, encryption, key derivation, and compression.
//!
//! Most users don't need this module directly -- [`SmbClient`](crate::SmbClient)
//! handles signing and encryption automatically.

pub mod compression;
pub mod encryption;
pub mod kdf;
pub mod signing;

/// Stands in for a password or key in `Debug` output.
///
/// Anything holding one implements `Debug` by hand and prints this in its
/// place, so a consumer logging a config or a session with `{:?}` can't leak
/// it. ❌ Don't `derive(Debug)` on a struct that holds a secret.
pub(crate) struct Redacted;

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}
