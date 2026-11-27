//! The code exchange: SPAKE2 with the code, then a confirmation each way,
//! bound to the TLS session (RFC-010 Decision 5).
//!
//! ```text
//! guest                                   host
//!   start:  SPAKE2 message A  ───────────▶
//!                             ◀───────────  SPAKE2 message B, host confirmation
//!   checks the host's confirmation; stops here if it is wrong
//!   guest confirmation        ───────────▶
//!                                           checks it
//! ```
//!
//! A confirmation is HMAC-SHA256 under the key SPAKE2 agreed, over the role
//! and the TLS exporter of the connection it travels on. So:
//!
//! - a wrong code gives the two sides different keys, and the first
//!   confirmation checked fails;
//! - an impostor host without the code cannot make a confirmation the guest
//!   accepts, and the guest sends nothing that would let it test guesses
//!   offline (SPAKE2 messages reveal nothing about the code without the
//!   other side's secret);
//! - a relay in the middle runs two TLS sessions with two exporters, so a
//!   confirmation made on one fails on the other, even with the code.
//!
//! `context` names what the listener is for (`"wavesync"`, `"phone-link"`): it
//! is part of both identities, so a code shown for one cannot open the other.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use super::code::Code;

type HmacSha256 = Hmac<Sha256>;

/// Length of a confirmation.
pub const CONFIRM_LEN: usize = 32;

/// Why an exchange failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExchangeError {
    /// The other side's message is not a SPAKE2 message.
    #[error("malformed exchange message")]
    Malformed,
    /// The confirmation does not match: a wrong code, a code for another
    /// context, or somebody between the two.
    #[error("the code did not match")]
    Mismatch,
}

/// The guest's half, between its first message and the host's answer.
pub struct GuestExchange {
    spake: Spake2<Ed25519Group>,
}

/// The host's half, between its answer and the guest's confirmation.
pub struct HostExchange {
    key: Vec<u8>,
    exporter: Vec<u8>,
}

fn identities(context: &str) -> (Identity, Identity) {
    (
        Identity::new(format!("waveflow-lan guest {context}").as_bytes()),
        Identity::new(format!("waveflow-lan host {context}").as_bytes()),
    )
}

fn confirmation(key: &[u8], role: &str, exporter: &[u8]) -> HmacSha256 {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(b"waveflow-lan confirm ");
    mac.update(role.as_bytes());
    mac.update(&[0]);
    mac.update(exporter);
    mac
}

impl GuestExchange {
    /// Begin with `code`: the state to keep, and the message to send.
    pub fn start(code: &Code, context: &str) -> (Self, Vec<u8>) {
        let (guest, host) = identities(context);
        let (spake, message) = Spake2::<Ed25519Group>::start_a(
            &Password::new(code.as_str().as_bytes()),
            &guest,
            &host,
        );
        (Self { spake }, message)
    }

    /// Check the host's answer on the TLS session whose exporter is
    /// `exporter`; on success, the confirmation to send back.
    pub fn finish(
        self,
        host_message: &[u8],
        host_confirm: &[u8],
        exporter: &[u8],
    ) -> Result<[u8; CONFIRM_LEN], ExchangeError> {
        let key = self
            .spake
            .finish(host_message)
            .map_err(|_| ExchangeError::Malformed)?;
        confirmation(&key, "host", exporter)
            .verify_slice(host_confirm)
            .map_err(|_| ExchangeError::Mismatch)?;
        Ok(confirmation(&key, "guest", exporter)
            .finalize()
            .into_bytes()
            .into())
    }
}

impl HostExchange {
    /// Answer a guest's first message with `code`, on the TLS session whose
    /// exporter is `exporter`: the state to keep, the SPAKE2 message and the
    /// host's confirmation to send.
    pub fn respond(
        code: &Code,
        context: &str,
        guest_message: &[u8],
        exporter: &[u8],
    ) -> Result<(Self, Vec<u8>, [u8; CONFIRM_LEN]), ExchangeError> {
        let (guest, host) = identities(context);
        let (spake, message) = Spake2::<Ed25519Group>::start_b(
            &Password::new(code.as_str().as_bytes()),
            &guest,
            &host,
        );
        let key = spake
            .finish(guest_message)
            .map_err(|_| ExchangeError::Malformed)?;
        let confirm = confirmation(&key, "host", exporter)
            .finalize()
            .into_bytes()
            .into();
        Ok((
            Self {
                key,
                exporter: exporter.to_vec(),
            },
            message,
            confirm,
        ))
    }

    /// Check the guest's confirmation.
    pub fn verify(self, guest_confirm: &[u8]) -> Result<(), ExchangeError> {
        confirmation(&self.key, "guest", &self.exporter)
            .verify_slice(guest_confirm)
            .map_err(|_| ExchangeError::Mismatch)
    }
}
