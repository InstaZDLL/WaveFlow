//! The LAN listener: one machine accepts, others join, on the local network
//! or a private VPN, and nobody else (RFC-010 lot 0, the foundation RFC-008's
//! phone link is built on too).
//!
//! What it does, in the order a connection meets it:
//!
//! 1. **The peer's address** ([`address::PeerFilter`]). A private, link-local,
//!    loopback or unique-local address, or a private VPN's when the user
//!    allowed it; anything else is closed before a byte of TLS.
//! 2. **TLS 1.3** with a certificate the machine made for itself
//!    ([`cert::Identity`]). No certificate authority: the guest pins the
//!    fingerprint when it knows it (an invitation link, a reconnection), and
//!    otherwise relies on step 4.
//! 3. **A WebSocket** on top, whose text frames carry JSON and binary frames
//!    the ranged reads of WaveSync.
//! 4. **The code exchange** ([`exchange`]). The code the host shows is never
//!    sent: both sides run SPAKE2 with it and prove they hold the same key,
//!    bound to this TLS session by its exporter, the host first. An impostor
//!    without the code, or a relay in the middle, fails here; a wrong code
//!    makes its sender wait before the next attempt ([`backoff`]).
//!
//! A connection that opens with anything other than the exchange — a guest
//! coming back with its token — is handed over as it is, for the feature to
//! authenticate ([`listener::Admission::Unverified`]).
//!
//! What the module does **not** do is decide whether the network may be
//! used: offline mode is the app's (`offline::is_offline()`), and so is the
//! host's approval of whoever got in.
//!
//! [`mdns`] advertises a listener and finds the others.

pub mod address;
pub mod backoff;
pub mod cert;
pub mod client;
pub mod code;
pub mod exchange;
pub mod handshake;
pub mod listener;
pub mod mdns;
pub mod token;

use std::time::Duration;

/// The handshake this build speaks. Raised when a change would make an older
/// peer misread the exchange; a peer on another version is refused.
pub const HANDSHAKE_VERSION: u32 = 1;

/// The TLS exporter label the exchange's confirmations are bound with
/// (RFC 5705): both ends of one TLS session derive the same bytes from it,
/// and two different sessions never do.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-waveflow-lan-v1";

/// What a peer may take to finish TLS, the WebSocket and the exchange.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Largest WebSocket message either side reads: a text frame of WaveSync
/// ([`crate::wavesync::wire::MAX_TEXT_FRAME`]) or a data frame
/// ([`crate::wavesync::frame::MAX_READ_LEN`] and its header), with room.
pub const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

/// `N` bytes from the system's CSPRNG, for codes, tokens and instance names.
///
/// # Panics
///
/// When the system has no randomness to give, which `ring` reports only on a
/// platform it does not support at all: nothing here could run safely then.
pub fn random<const N: usize>() -> [u8; N] {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system CSPRNG is unavailable");
    bytes
}

/// The WebSocket limits both sides use.
fn websocket_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES))
}

/// The rustls provider every config here is built on, whatever the process
/// installed as its default.
fn provider() -> std::sync::Arc<tokio_rustls::rustls::crypto::CryptoProvider> {
    std::sync::Arc::new(tokio_rustls::rustls::crypto::ring::default_provider())
}

#[cfg(test)]
mod tests;
