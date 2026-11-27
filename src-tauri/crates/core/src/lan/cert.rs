//! The certificate a listener presents, made on the machine.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// The name the certificate is made for. Nobody checks it: the guest pins the
/// fingerprint, or relies on the code exchange.
pub const SERVER_NAME: &str = "waveflow.local";

/// A certificate and its private key.
///
/// WaveSync makes one per session and forgets it; the phone link keeps one
/// (its fingerprint is what a phone pins at pairing), which is what
/// [`Identity::from_der`] and [`Identity::key_der`] are for.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: Vec<u8>,
}

/// Why a certificate could not be made or read.
#[derive(Debug, thiserror::Error)]
#[error("certificate: {0}")]
pub struct CertError(String);

impl Identity {
    /// A new self-signed certificate (ECDSA P-256).
    pub fn generate() -> Result<Self, CertError> {
        let made = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()])
            .map_err(|e| CertError(e.to_string()))?;
        Ok(Self {
            cert: made.cert.der().clone(),
            key: made.signing_key.serialize_der(),
        })
    }

    /// One kept from before: the certificate's DER and the key's PKCS #8 DER.
    pub fn from_der(cert: Vec<u8>, key: Vec<u8>) -> Self {
        Self {
            cert: CertificateDer::from(cert),
            key,
        }
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert
    }

    /// The private key, PKCS #8 DER: a secret, for whoever stores it.
    pub fn key_der(&self) -> &[u8] {
        &self.key
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.cert)
    }

    pub(super) fn rustls_parts(&self) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        (
            vec![self.cert.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone())),
        )
    }
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

/// The SHA-256 of a certificate's DER.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn of(cert_der: &[u8]) -> Self {
        Self(Sha256::digest(cert_der).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// What a person compares between two screens: the first 64 bits, as
    /// four groups of four characters (`3F9A 77C2 0B1E D4A8`). Short enough
    /// to read, long enough that nobody makes a certificate to match it.
    pub fn short(&self) -> String {
        let hex = hex::encode_upper(&self.0[..8]);
        format!(
            "{} {} {} {}",
            &hex[..4],
            &hex[4..8],
            &hex[8..12],
            &hex[12..]
        )
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a certificate fingerprint")]
pub struct BadFingerprint;

impl FromStr for Fingerprint {
    type Err = BadFingerprint;

    /// 64 hex digits, either case.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes).map_err(|_| BadFingerprint)?;
        Ok(Self(bytes))
    }
}
