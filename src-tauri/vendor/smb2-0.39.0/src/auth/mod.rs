//! Authentication mechanisms for SMB2.
//!
//! Supports NTLM authentication (MS-NLMP) and Kerberos authentication
//! (RFC 4120, MS-KILE).
//!
//! Most users don't need this module directly -- [`SmbClient`](crate::SmbClient)
//! handles authentication during [`connect`](crate::connect).

pub(crate) mod der;
pub mod kerberos;
pub mod ntlm;
pub mod spnego;

pub use kerberos::{KerberosAuthenticator, KerberosCredentials};
pub use ntlm::{NtlmAuthenticator, NtlmCredentials};
