//! Kerberos authentication support.
//!
//! Implements the cryptographic operations needed for Kerberos authentication
//! (etypes 17, 18, 23): string-to-key, key derivation, AES-CTS encryption,
//! RC4-HMAC encryption, and checksum computation.
//!
//! The [`KerberosAuthenticator`] wires all building blocks together into
//! a full Kerberos authentication flow: AS exchange, TGS exchange, and
//! AP-REQ construction for SMB2 SESSION_SETUP.
//!
//! The [`ccache`] module supports reading MIT Kerberos credential caches,
//! enabling authentication from cached TGTs or service tickets (for example,
//! from `kinit`) without requiring a password.

pub mod ccache;
pub mod crypto;
pub mod kdc;
pub mod messages;

mod authenticator;
pub use authenticator::{KerberosAuthenticator, KerberosCredentials};
