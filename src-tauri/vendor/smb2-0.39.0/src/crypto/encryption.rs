//! SMB2/3 message encryption and decryption.
//!
//! Implements AES-128-CCM, AES-128-GCM, AES-256-CCM, and AES-256-GCM
//! as specified in MS-SMB2 sections 3.1.4.3 (encrypting) and 3.1.5.1
//! (decrypting). Nonces are generated from a monotonically increasing
//! per-session counter to prevent catastrophic nonce reuse in AES-GCM.

use aes::{Aes128, Aes256};
use aes_gcm::aead::{array::Array, inout::InOutBuf, AeadInOut};
use aes_gcm::KeyInit;
use ccm::consts::{U11, U16};

use crate::msg::transform::{TransformHeader, SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED};
use crate::pack::{Pack, WriteCursor};
use crate::types::SessionId;
use crate::Error;

/// Offset in the serialized TRANSFORM_HEADER where the AAD begins.
///
/// The AAD is "the SMB2 TRANSFORM_HEADER, excluding the ProtocolId and
/// Signature fields" (MS-SMB2 section 3.1.4.3). ProtocolId is 4 bytes
/// and Signature is 16 bytes, so the AAD starts at offset 20 (the Nonce
/// field) and extends to the end of the 52-byte header.
const AAD_OFFSET: usize = 20;

/// Total size of the TRANSFORM_HEADER in bytes.
const HEADER_SIZE: usize = TransformHeader::SIZE; // 52

// ── CCM type aliases ─────────────────────────────────────────────────

/// AES-128-CCM with 16-byte tag and 11-byte nonce (SMB 3.0+).
type Aes128Ccm = ccm::Ccm<Aes128, U16, U11>;

/// AES-256-CCM with 16-byte tag and 11-byte nonce (SMB 3.1.1).
type Aes256Ccm = ccm::Ccm<Aes256, U16, U11>;

// ── Cipher enum ──────────────────────────────────────────────────────

/// Encryption cipher, determined during negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Cipher {
    /// AES-128-CCM (SMB 3.0+) -- 11-byte nonce.
    Aes128Ccm,
    /// AES-128-GCM (SMB 3.0+) -- 12-byte nonce.
    Aes128Gcm,
    /// AES-256-CCM (SMB 3.1.1) -- 11-byte nonce.
    Aes256Ccm,
    /// AES-256-GCM (SMB 3.1.1) -- 12-byte nonce.
    Aes256Gcm,
}

impl Cipher {
    /// Returns the number of nonce bytes actually used by this cipher.
    pub fn nonce_len(self) -> usize {
        match self {
            Cipher::Aes128Ccm | Cipher::Aes256Ccm => 11,
            Cipher::Aes128Gcm | Cipher::Aes256Gcm => 12,
        }
    }

    /// Returns the expected key length in bytes.
    fn key_len(self) -> usize {
        match self {
            Cipher::Aes128Ccm | Cipher::Aes128Gcm => 16,
            Cipher::Aes256Ccm | Cipher::Aes256Gcm => 32,
        }
    }
}

// ── Nonce generator ──────────────────────────────────────────────────

/// Monotonically increasing nonce generator.
///
/// Each session gets its own nonce generator. The counter MUST NOT
/// be reused -- nonce reuse breaks AES-GCM catastrophically.
pub struct NonceGenerator {
    counter: u64,
}

impl NonceGenerator {
    /// Create a new nonce generator starting at counter 0.
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    /// Generate the next nonce for the given cipher.
    ///
    /// Returns the full 16-byte nonce field for the TRANSFORM_HEADER.
    /// - CCM: 8-byte LE counter in bytes 0..8, zeros in bytes 8..16
    ///   (the cipher uses the first 11 bytes as the nonce).
    /// - GCM: 8-byte LE counter in bytes 0..8, zeros in bytes 8..16
    ///   (the cipher uses the first 12 bytes as the nonce).
    ///
    /// # Panics
    ///
    /// Panics if the counter overflows `u64::MAX`. In practice this
    /// can never happen (2^64 messages at line speed would take millennia).
    pub fn next(&mut self, _cipher: Cipher) -> [u8; 16] {
        let count = self.counter;
        self.counter = self.counter.checked_add(1).expect("nonce counter overflow");
        let mut nonce = [0u8; 16];
        nonce[..8].copy_from_slice(&count.to_le_bytes());
        nonce
    }
}

impl Default for NonceGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Encrypt ──────────────────────────────────────────────────────────

/// Encrypt an SMB2 message.
///
/// Returns `(transform_header_bytes, encrypted_message)`. The 52-byte
/// transform header includes the protocol ID, auth tag (in the Signature
/// field), nonce, original message size, flags, and session ID. The
/// encrypted message replaces the plaintext.
pub fn encrypt_message(
    plaintext: &[u8],
    key: &[u8],
    cipher: Cipher,
    nonce: &[u8; 16],
    session_id: u64,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    if key.len() != cipher.key_len() {
        return Err(Error::invalid_data(format!(
            "encryption key length mismatch: expected {}, got {}",
            cipher.key_len(),
            key.len()
        )));
    }

    // Build the TRANSFORM_HEADER with a zeroed signature (will be filled
    // with the auth tag after encryption).
    let header = TransformHeader {
        signature: [0u8; 16],
        nonce: *nonce,
        original_message_size: plaintext.len() as u32,
        flags: SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED,
        session_id: SessionId(session_id),
    };

    let mut header_bytes = {
        let mut w = WriteCursor::new();
        header.pack(&mut w);
        w.into_inner()
    };

    // AAD = header bytes 20..52 (Nonce + OriginalMessageSize + Reserved + Flags + SessionId)
    let aad = &header_bytes[AAD_OFFSET..HEADER_SIZE];

    // Encrypt and get the auth tag.
    let mut buffer = plaintext.to_vec();
    let nonce_slice = &nonce[..cipher.nonce_len()];

    let tag = encrypt_raw(cipher, key, nonce_slice, aad, &mut buffer)?;

    // Write the 16-byte auth tag into the Signature field (bytes 4..20).
    header_bytes[4..20].copy_from_slice(&tag);

    Ok((header_bytes, buffer))
}

// ── Decrypt ──────────────────────────────────────────────────────────

/// Decrypt an SMB2 message.
///
/// `transform_header` is the 52-byte TRANSFORM_HEADER (as received on
/// the wire). `ciphertext` is the encrypted message data that follows
/// the header. Returns the decrypted plaintext.
pub fn decrypt_message(
    transform_header: &[u8],
    ciphertext: &[u8],
    key: &[u8],
    cipher: Cipher,
) -> Result<Vec<u8>, Error> {
    if transform_header.len() != HEADER_SIZE {
        return Err(Error::invalid_data(format!(
            "transform header must be {} bytes, got {}",
            HEADER_SIZE,
            transform_header.len()
        )));
    }
    if key.len() != cipher.key_len() {
        return Err(Error::invalid_data(format!(
            "decryption key length mismatch: expected {}, got {}",
            cipher.key_len(),
            key.len()
        )));
    }

    // Extract auth tag (Signature) from bytes 4..20.
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&transform_header[4..20]);

    // Extract nonce from bytes 20..36.
    let nonce = &transform_header[20..20 + cipher.nonce_len()];

    // AAD = header bytes 20..52.
    let aad = &transform_header[AAD_OFFSET..HEADER_SIZE];

    let mut buffer = ciphertext.to_vec();
    decrypt_raw(cipher, key, nonce, aad, &tag, &mut buffer)?;

    Ok(buffer)
}

// ── Raw encrypt/decrypt helpers ──────────────────────────────────────

/// Copy an auth tag array into a fixed-size `[u8; 16]` array.
fn tag_to_array<N: aes_gcm::aead::array::ArraySize>(tag: Array<u8, N>) -> [u8; 16] {
    let mut arr = [0u8; 16];
    arr.copy_from_slice(tag.as_slice());
    arr
}

/// Encrypt `buffer` in place and return the 16-byte auth tag.
fn encrypt_raw(
    cipher: Cipher,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    buffer: &mut [u8],
) -> Result<[u8; 16], Error> {
    let map_err = |_| Error::invalid_data("encryption failed");
    let buf = InOutBuf::from(buffer);

    let tag = match cipher {
        Cipher::Aes128Ccm => {
            let c = Aes128Ccm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.encrypt_inout_detached(n, aad, buf)
                .map(tag_to_array)
                .map_err(map_err)?
        }
        Cipher::Aes128Gcm => {
            let c = aes_gcm::Aes128Gcm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.encrypt_inout_detached(n, aad, buf)
                .map(tag_to_array)
                .map_err(map_err)?
        }
        Cipher::Aes256Ccm => {
            let c = Aes256Ccm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.encrypt_inout_detached(n, aad, buf)
                .map(tag_to_array)
                .map_err(map_err)?
        }
        Cipher::Aes256Gcm => {
            let c = aes_gcm::Aes256Gcm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.encrypt_inout_detached(n, aad, buf)
                .map(tag_to_array)
                .map_err(map_err)?
        }
    };

    Ok(tag)
}

/// Decrypt `buffer` in place, verifying the 16-byte auth tag.
fn decrypt_raw(
    cipher: Cipher,
    key: &[u8],
    nonce: &[u8],
    aad: &[u8],
    tag: &[u8; 16],
    buffer: &mut [u8],
) -> Result<(), Error> {
    let map_err = |_| Error::invalid_data("decryption failed: authentication tag mismatch");
    let buf = InOutBuf::from(buffer);
    let t: &Array<u8, _> = tag.into();

    match cipher {
        Cipher::Aes128Ccm => {
            let c = Aes128Ccm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.decrypt_inout_detached(n, aad, buf, t).map_err(map_err)
        }
        Cipher::Aes128Gcm => {
            let c = aes_gcm::Aes128Gcm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.decrypt_inout_detached(n, aad, buf, t).map_err(map_err)
        }
        Cipher::Aes256Ccm => {
            let c = Aes256Ccm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.decrypt_inout_detached(n, aad, buf, t).map_err(map_err)
        }
        Cipher::Aes256Gcm => {
            let c = aes_gcm::Aes256Gcm::new(key.try_into().expect("key length validated"));
            let n = nonce.try_into().expect("nonce length validated");
            c.decrypt_inout_detached(n, aad, buf, t).map_err(map_err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::transform::TRANSFORM_PROTOCOL_ID;

    // ── Helper ────────────────────────────────────────────────────────

    fn test_key(cipher: Cipher) -> Vec<u8> {
        vec![0x42; cipher.key_len()]
    }

    // ── Encrypt-then-decrypt roundtrip (one per cipher) ──────────────

    #[test]
    fn roundtrip_aes128_ccm() {
        roundtrip_cipher(Cipher::Aes128Ccm);
    }

    #[test]
    fn roundtrip_aes128_gcm() {
        roundtrip_cipher(Cipher::Aes128Gcm);
    }

    #[test]
    fn roundtrip_aes256_ccm() {
        roundtrip_cipher(Cipher::Aes256Ccm);
    }

    #[test]
    fn roundtrip_aes256_gcm() {
        roundtrip_cipher(Cipher::Aes256Gcm);
    }

    fn roundtrip_cipher(cipher: Cipher) {
        let key = test_key(cipher);
        let plaintext = b"Hello, SMB2 encryption roundtrip!";
        let session_id = 0xDEAD_BEEF_CAFE_FACE;

        let mut nonce_gen = NonceGenerator::new();
        let nonce = nonce_gen.next(cipher);

        let (header, ciphertext) =
            encrypt_message(plaintext, &key, cipher, &nonce, session_id).unwrap();

        // Ciphertext must differ from plaintext.
        assert_ne!(&ciphertext[..], &plaintext[..]);

        let decrypted = decrypt_message(&header, &ciphertext, &key, cipher).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    // ── Nonce generator monotonically increases ──────────────────────

    #[test]
    fn nonce_generator_monotonic() {
        let mut gen = NonceGenerator::new();
        let mut prev = [0u8; 16]; // counter 0 hasn't been generated yet

        for i in 0u64..100 {
            let nonce = gen.next(Cipher::Aes128Gcm);
            // Extract the 8-byte LE counter from the nonce.
            let counter = u64::from_le_bytes(nonce[..8].try_into().unwrap());
            assert_eq!(counter, i, "counter should equal {i}");

            if i > 0 {
                assert_ne!(nonce, prev, "each nonce must be unique");
            }
            prev = nonce;
        }
    }

    // ── Nonce format for GCM ─────────────────────────────────────────

    #[test]
    fn nonce_format_gcm() {
        let mut gen = NonceGenerator::new();
        // Advance to counter = 7 to have a non-trivial value.
        for _ in 0..7 {
            gen.next(Cipher::Aes128Gcm);
        }
        let nonce = gen.next(Cipher::Aes128Gcm); // counter = 7

        // First 8 bytes: LE counter (7).
        assert_eq!(
            u64::from_le_bytes(nonce[..8].try_into().unwrap()),
            7,
            "counter value"
        );
        // Bytes 8..12: zeros (padding to 12-byte GCM nonce).
        assert_eq!(nonce[8..12], [0, 0, 0, 0], "GCM nonce padding (8..12)");
        // Bytes 12..16: zeros (unused portion of the 16-byte field).
        assert_eq!(nonce[12..16], [0, 0, 0, 0], "unused nonce bytes (12..16)");
    }

    // ── Nonce format for CCM ─────────────────────────────────────────

    #[test]
    fn nonce_format_ccm() {
        let mut gen = NonceGenerator::new();
        // Advance to counter = 5.
        for _ in 0..5 {
            gen.next(Cipher::Aes128Ccm);
        }
        let nonce = gen.next(Cipher::Aes128Ccm); // counter = 5

        // First 8 bytes: LE counter (5).
        assert_eq!(
            u64::from_le_bytes(nonce[..8].try_into().unwrap()),
            5,
            "counter value"
        );
        // Bytes 8..11: zeros (padding to 11-byte CCM nonce).
        assert_eq!(nonce[8..11], [0, 0, 0], "CCM nonce padding (8..11)");
        // Bytes 11..16: zeros (unused portion of the 16-byte field).
        assert_eq!(
            nonce[11..16],
            [0, 0, 0, 0, 0],
            "unused nonce bytes (11..16)"
        );
    }

    // ── Tampered ciphertext fails decryption ─────────────────────────

    #[test]
    fn tampered_ciphertext_fails() {
        let cipher = Cipher::Aes128Gcm;
        let key = test_key(cipher);
        let plaintext = b"Do not tamper with me!";
        let session_id = 42;

        let mut gen = NonceGenerator::new();
        let nonce = gen.next(cipher);

        let (header, mut ciphertext) =
            encrypt_message(plaintext, &key, cipher, &nonce, session_id).unwrap();

        // Flip a byte in the ciphertext.
        ciphertext[0] ^= 0xFF;

        let result = decrypt_message(&header, &ciphertext, &key, cipher);
        assert!(result.is_err(), "tampered ciphertext must fail decryption");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("tag mismatch") || err.contains("decryption failed"),
            "error was: {err}"
        );
    }

    // ── Wrong key fails decryption ───────────────────────────────────

    #[test]
    fn wrong_key_fails() {
        let cipher = Cipher::Aes256Gcm;
        let key = test_key(cipher);
        let wrong_key = vec![0x99; cipher.key_len()];
        let plaintext = b"Secret message";
        let session_id = 100;

        let mut gen = NonceGenerator::new();
        let nonce = gen.next(cipher);

        let (header, ciphertext) =
            encrypt_message(plaintext, &key, cipher, &nonce, session_id).unwrap();

        let result = decrypt_message(&header, &ciphertext, &wrong_key, cipher);
        assert!(result.is_err(), "wrong key must fail decryption");
    }

    // ── AAD includes correct TRANSFORM_HEADER bytes (offset 20-51) ──

    #[test]
    fn aad_is_correct_header_region() {
        // Verify the AAD constants match the spec.
        assert_eq!(AAD_OFFSET, 20, "AAD starts at byte 20");
        assert_eq!(
            HEADER_SIZE - AAD_OFFSET,
            32,
            "AAD is 32 bytes (Nonce + OrigMsgSize + Reserved + Flags + SessionId)"
        );
        assert_eq!(HEADER_SIZE, 52, "TRANSFORM_HEADER is 52 bytes");

        // Build a header and verify the AAD region contains the expected fields.
        let mut nonce = [0u8; 16];
        nonce[0] = 0xAA;
        nonce[7] = 0xBB;

        let header = TransformHeader {
            signature: [0xFF; 16],
            nonce,
            original_message_size: 1024,
            flags: SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED,
            session_id: SessionId(0x0123_4567_89AB_CDEF),
        };

        let mut w = WriteCursor::new();
        header.pack(&mut w);
        let bytes = w.into_inner();

        let aad = &bytes[AAD_OFFSET..HEADER_SIZE];
        assert_eq!(aad.len(), 32);

        // First 16 bytes of AAD should be the nonce.
        assert_eq!(aad[0], 0xAA, "nonce byte 0");
        assert_eq!(aad[7], 0xBB, "nonce byte 7");

        // Bytes 16..20 of AAD should be OriginalMessageSize (1024 LE).
        assert_eq!(
            u32::from_le_bytes(aad[16..20].try_into().unwrap()),
            1024,
            "OriginalMessageSize"
        );

        // Bytes 20..22 of AAD should be Reserved (0).
        assert_eq!(aad[20..22], [0, 0], "Reserved");

        // Bytes 22..24 of AAD should be Flags (0x0001).
        assert_eq!(
            u16::from_le_bytes(aad[22..24].try_into().unwrap()),
            SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED,
            "Flags"
        );

        // Bytes 24..32 of AAD should be SessionId.
        assert_eq!(
            u64::from_le_bytes(aad[24..32].try_into().unwrap()),
            0x0123_4567_89AB_CDEF,
            "SessionId"
        );
    }

    // ── Transform header has correct protocol ID ─────────────────────

    #[test]
    fn transform_header_protocol_id() {
        let cipher = Cipher::Aes128Gcm;
        let key = test_key(cipher);
        let plaintext = b"test";
        let session_id = 1;

        let mut gen = NonceGenerator::new();
        let nonce = gen.next(cipher);

        let (header, _) = encrypt_message(plaintext, &key, cipher, &nonce, session_id).unwrap();

        // First 4 bytes must be 0xFD 'S' 'M' 'B'.
        assert_eq!(&header[..4], &TRANSFORM_PROTOCOL_ID);
        assert_eq!(header[0], 0xFD, "protocol ID first byte must be 0xFD");
        assert_eq!(header[1], b'S');
        assert_eq!(header[2], b'M');
        assert_eq!(header[3], b'B');
    }

    // ── Auth tag (signature) is at bytes 4..20 ──────────────────────

    #[test]
    fn signature_position_in_header() {
        let cipher = Cipher::Aes256Ccm;
        let key = test_key(cipher);
        let plaintext = b"Check signature position";
        let session_id = 99;

        let mut gen = NonceGenerator::new();
        let nonce = gen.next(cipher);

        let (header, _) = encrypt_message(plaintext, &key, cipher, &nonce, session_id).unwrap();

        // The signature (auth tag) lives at bytes 4..20.
        let signature = &header[4..20];

        // It should NOT be all zeros (that would mean we forgot to write it).
        assert_ne!(
            signature, &[0u8; 16],
            "signature must not be all zeros after encryption"
        );

        // Verify that using this tag allows successful decryption
        // (already covered by roundtrip tests, but this confirms the
        // position explicitly).
        let decrypted = decrypt_message(&header, &header[..0], &key, cipher);
        // This will fail because we passed empty ciphertext, but that's
        // not the point -- the roundtrip tests cover correctness.
        // Instead, let's verify the tag by a proper roundtrip.
        drop(decrypted);

        let (header2, ct2) = encrypt_message(plaintext, &key, cipher, &nonce, session_id).unwrap();
        let result = decrypt_message(&header2, &ct2, &key, cipher).unwrap();
        assert_eq!(result, plaintext);
    }
}
