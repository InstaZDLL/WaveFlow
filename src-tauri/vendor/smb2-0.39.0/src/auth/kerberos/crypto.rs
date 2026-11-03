//! Kerberos cryptographic operations.
//!
//! Supports three encryption types (etypes):
//! - **AES256-CTS-HMAC-SHA1-96** (etype 18): AES-256 with CTS mode and HMAC-SHA1 checksums.
//! - **AES128-CTS-HMAC-SHA1-96** (etype 17): AES-128 with CTS mode and HMAC-SHA1 checksums.
//! - **RC4-HMAC** (etype 23): RC4 stream cipher with HMAC-MD5 checksums.
//!
//! References:
//! - RFC 3961: Encryption and Checksum Specifications for Kerberos 5
//! - RFC 3962: AES Encryption for Kerberos 5
//! - RFC 4757: RC4-HMAC Kerberos Encryption Types
//! - MS-KILE: Kerberos Protocol Extensions

use crate::Error;
use digest::KeyInit;

// ---------------------------------------------------------------------------
// Encryption type enum
// ---------------------------------------------------------------------------

/// Kerberos encryption type identifiers.
///
/// Each variant's numeric value matches the IANA-assigned etype number
/// from RFC 3961 and RFC 4757.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionType {
    /// AES-256 with CTS mode and HMAC-SHA1-96 checksum (etype 18).
    Aes256CtsHmacSha196 = 18,
    /// AES-128 with CTS mode and HMAC-SHA1-96 checksum (etype 17).
    Aes128CtsHmacSha196 = 17,
    /// RC4 with HMAC-MD5 checksum (etype 23).
    Rc4Hmac = 23,
}

// ---------------------------------------------------------------------------
// String-to-Key: password → encryption key
// ---------------------------------------------------------------------------

/// Derive an AES encryption key from a password (RFC 3962 section 4).
///
/// Uses PBKDF2-HMAC-SHA1 with 4096 iterations, then applies the
/// DK(key, "kerberos") random-to-key folding per RFC 3961.
///
/// Salt is typically `REALM` + `username` (concatenated, case-sensitive).
/// `key_size` is 16 for AES-128 (etype 17) or 32 for AES-256 (etype 18).
pub fn string_to_key_aes(password: &str, salt: &str, key_size: usize) -> Vec<u8> {
    use sha1::Sha1;

    assert!(
        key_size == 16 || key_size == 32,
        "key_size must be 16 or 32"
    );

    // Step 1: PBKDF2-HMAC-SHA1 with 4096 iterations.
    let mut raw_key = vec![0u8; key_size];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), salt.as_bytes(), 4096, &mut raw_key);

    // Step 2: DK(raw_key, "kerberos") per RFC 3961.
    // This applies the derive-key function with the well-known constant "kerberos".
    dk_derive(&raw_key, b"kerberos")
}

/// Derive an RC4-HMAC key from a password (RFC 4757).
///
/// This is the NT hash: `MD4(UTF-16LE(password))`. Identical to the
/// NTLM NT hash computation.
pub fn string_to_key_rc4(password: &str) -> Vec<u8> {
    use digest::Digest;

    let unicode_password: Vec<u8> = password
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let mut hasher = md4::Md4::new();
    hasher.update(&unicode_password);
    hasher.finalize().to_vec()
}

// ---------------------------------------------------------------------------
// Key derivation (RFC 3961)
// ---------------------------------------------------------------------------

/// Derive a usage-specific key from a base key (RFC 3961).
///
/// Uses the `random-to-key(DR(base_key, usage))` construction.
/// The `usage` is a well-known constant (for example, `"signaturekey"`) or a
/// key usage number encoded as bytes with a type suffix:
/// - For encryption: `[usage_be32, 0xAA]`
/// - For checksum: `[usage_be32, 0x99]`
/// - For key derivation: `[usage_be32, 0x55]`
pub fn derive_key_aes(base_key: &[u8], usage: &[u8]) -> Vec<u8> {
    dk_derive(base_key, usage)
}

/// Build the 5-byte key usage constant for AES encryption keys.
///
/// Format: 4-byte big-endian usage number + `0xAA` (encryption).
pub fn usage_enc(usage: u32) -> [u8; 5] {
    let mut out = [0u8; 5];
    out[0..4].copy_from_slice(&usage.to_be_bytes());
    out[4] = 0xAA;
    out
}

/// Build the 5-byte key usage constant for AES integrity (Ki) keys.
///
/// Format: 4-byte big-endian usage number + `0x55`.
///
/// Per RFC 3961 section 3, the integrity subkey Ki is derived with
/// `0x55` and used for the HMAC inside `encrypt()`/`decrypt()`.
pub fn usage_int(usage: u32) -> [u8; 5] {
    let mut out = [0u8; 5];
    out[0..4].copy_from_slice(&usage.to_be_bytes());
    out[4] = 0x55;
    out
}

/// Build the 5-byte key usage constant for AES checksum (Kc) keys.
///
/// Format: 4-byte big-endian usage number + `0x99`.
///
/// Per RFC 3961 section 5.4, the checksum subkey Kc is derived with
/// `0x99` and used for standalone `get_mic()` / checksum operations.
pub fn usage_chk(usage: u32) -> [u8; 5] {
    let mut out = [0u8; 5];
    out[0..4].copy_from_slice(&usage.to_be_bytes());
    out[4] = 0x99;
    out
}

// ---------------------------------------------------------------------------
// AES-CTS encryption/decryption (RFC 3962 section 3)
// ---------------------------------------------------------------------------

/// Encrypt data using AES-CTS (Cipher Text Stealing) mode.
///
/// AES-CTS is AES-CBC with the last two ciphertext blocks swapped
/// and the final block potentially truncated to the plaintext size.
/// For a single block (16 bytes or fewer), uses AES-CBC with zero-padding.
pub fn encrypt_aes_cts(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Vec<u8> {
    if plaintext.is_empty() {
        return Vec::new();
    }

    let block_size = 16;

    // For single-block or less: pad to one full block and encrypt with AES-CBC.
    // Per RFC 3962: "If the data [...] has only a single block, that block is
    // simply encrypted with AES." The ciphertext is always a full 16-byte block.
    if plaintext.len() <= block_size {
        let mut padded = [0u8; 16];
        padded[..plaintext.len()].copy_from_slice(plaintext);
        // XOR with IV, then ECB encrypt.
        for i in 0..16 {
            padded[i] ^= iv[i];
        }
        let ct = aes_ecb_encrypt(key, &padded);
        return ct.to_vec();
    }

    // Multi-block: encrypt with standard CBC, then apply CTS.
    // Pad the plaintext to a multiple of block_size.
    let n_blocks = plaintext.len().div_ceil(block_size);
    let padded_len = n_blocks * block_size;
    let mut padded = vec![0u8; padded_len];
    padded[..plaintext.len()].copy_from_slice(plaintext);

    // Encrypt with AES-CBC (no padding -- we padded ourselves).
    let cbc_out = aes_cbc_encrypt(key, iv, &padded);

    // CTS: swap the last two ciphertext blocks.
    let mut result = cbc_out;
    let second_last_start = (n_blocks - 2) * block_size;
    let last_start = (n_blocks - 1) * block_size;

    // Swap blocks.
    let mut second_last_block = [0u8; 16];
    let mut last_block = [0u8; 16];
    second_last_block.copy_from_slice(&result[second_last_start..second_last_start + block_size]);
    last_block.copy_from_slice(&result[last_start..last_start + block_size]);
    result[second_last_start..second_last_start + block_size].copy_from_slice(&last_block);
    result[last_start..last_start + block_size].copy_from_slice(&second_last_block);

    // Truncate the final block to the original plaintext length.
    result.truncate(plaintext.len());
    result
}

/// Decrypt data using AES-CTS mode.
///
/// Reverses the CTS transformation: un-swap the last two blocks,
/// then decrypt with AES-CBC.
pub fn decrypt_aes_cts(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    if ciphertext.is_empty() {
        return Ok(Vec::new());
    }

    let block_size = 16;

    // Single block (16 bytes): ECB decrypt then XOR with IV.
    // Per RFC 3962, single-block ciphertext is always exactly 16 bytes.
    if ciphertext.len() <= block_size {
        if ciphertext.len() != block_size {
            return Err(Error::invalid_data(format!(
                "AES-CTS single-block ciphertext must be exactly 16 bytes, got {}",
                ciphertext.len()
            )));
        }
        let mut pt = aes_ecb_decrypt(key, ciphertext);
        for i in 0..16 {
            pt[i] ^= iv[i];
        }
        return Ok(pt.to_vec());
    }

    // Multi-block CTS decryption.
    let orig_len = ciphertext.len();
    let n_blocks = orig_len.div_ceil(block_size);
    let padded_len = n_blocks * block_size;

    // Pad the ciphertext to a full number of blocks.
    let mut padded_ct = vec![0u8; padded_len];
    padded_ct[..orig_len].copy_from_slice(ciphertext);

    let second_last_start = (n_blocks - 2) * block_size;
    let last_start = (n_blocks - 1) * block_size;

    if !orig_len.is_multiple_of(block_size) {
        let tail_len = orig_len - (n_blocks - 1) * block_size;

        // c_{n-1} is the swapped full block (at second_last_start).
        let mut c_n_minus_1 = [0u8; 16];
        c_n_minus_1.copy_from_slice(&padded_ct[second_last_start..second_last_start + block_size]);

        // Decrypt c_{n-1} with ECB to get intermediate.
        let intermediate = aes_ecb_decrypt(key, &c_n_minus_1);

        // c_n is the partial block (tail_len bytes at last_start).
        let mut reconstructed_last = [0u8; 16];
        reconstructed_last[..tail_len]
            .copy_from_slice(&padded_ct[last_start..last_start + tail_len]);
        // Pad with tail of the intermediate.
        reconstructed_last[tail_len..].copy_from_slice(&intermediate[tail_len..]);

        // Now put them back in the right order for CBC decryption.
        padded_ct[second_last_start..second_last_start + block_size]
            .copy_from_slice(&reconstructed_last);
        padded_ct[last_start..last_start + block_size].copy_from_slice(&c_n_minus_1);
    } else {
        // Block-aligned: swap back.
        let mut second_last_block = [0u8; 16];
        let mut last_block = [0u8; 16];
        second_last_block
            .copy_from_slice(&padded_ct[second_last_start..second_last_start + block_size]);
        last_block.copy_from_slice(&padded_ct[last_start..last_start + block_size]);
        padded_ct[second_last_start..second_last_start + block_size].copy_from_slice(&last_block);
        padded_ct[last_start..last_start + block_size].copy_from_slice(&second_last_block);
    }

    // Decrypt with standard CBC.
    let plaintext = aes_cbc_decrypt(key, iv, &padded_ct);
    Ok(plaintext[..orig_len].to_vec())
}

// ---------------------------------------------------------------------------
// RC4-HMAC encryption/decryption (RFC 4757)
// ---------------------------------------------------------------------------

/// Encrypt data using RC4-HMAC (etype 23).
///
/// 1. K1 = HMAC-MD5(key, usage as little-endian i32)
/// 2. Generate random 8-byte confounder
/// 3. Compute HMAC-MD5(K1, confounder + plaintext) → checksum (16 bytes)
/// 4. K3 = HMAC-MD5(K1, checksum)
/// 5. RC4-encrypt (confounder + plaintext) using K3
/// 6. Output = checksum (16 bytes) + encrypted_data
pub fn encrypt_rc4_hmac(key: &[u8], usage: u32, plaintext: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacMd5 = Hmac<md5::Md5>;

    // K1 = HMAC-MD5(key, usage_le)
    // Note: RFC 4757 uses the usage as a signed 32-bit little-endian value.
    let usage_bytes = (usage as i32).to_le_bytes();
    let mut mac = HmacMd5::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&usage_bytes);
    let k1 = mac.finalize().into_bytes();

    // Generate random 8-byte confounder.
    let mut confounder = [0u8; 8];
    getrandom::fill(&mut confounder).expect("CSPRNG failed");

    // Build confounder + plaintext.
    let mut payload = Vec::with_capacity(8 + plaintext.len());
    payload.extend_from_slice(&confounder);
    payload.extend_from_slice(plaintext);

    // Checksum = HMAC-MD5(K1, confounder + plaintext)
    let mut mac = HmacMd5::new_from_slice(&k1).expect("HMAC accepts any key length");
    mac.update(&payload);
    let checksum = mac.finalize().into_bytes();

    // K3 = HMAC-MD5(K1, checksum)
    let mut mac = HmacMd5::new_from_slice(&k1).expect("HMAC accepts any key length");
    mac.update(&checksum);
    let k3 = mac.finalize().into_bytes();

    // Encrypt payload with RC4 using K3.
    let encrypted = rc4_transform(&k3, &payload);

    // Output = checksum (16 bytes) + encrypted_data
    let mut output = Vec::with_capacity(16 + encrypted.len());
    output.extend_from_slice(&checksum);
    output.extend_from_slice(&encrypted);
    output
}

/// Decrypt data using RC4-HMAC (etype 23).
///
/// Reverses the `encrypt_rc4_hmac` process and verifies the checksum.
pub fn decrypt_rc4_hmac(key: &[u8], usage: u32, ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    use hmac::{Hmac, Mac};
    type HmacMd5 = Hmac<md5::Md5>;

    if ciphertext.len() < 24 {
        return Err(Error::invalid_data(
            "RC4-HMAC ciphertext too short (need at least 16-byte checksum + 8-byte confounder)",
        ));
    }

    let checksum = &ciphertext[..16];
    let encrypted_data = &ciphertext[16..];

    // K1 = HMAC-MD5(key, usage_le)
    let usage_bytes = (usage as i32).to_le_bytes();
    let mut mac = HmacMd5::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(&usage_bytes);
    let k1 = mac.finalize().into_bytes();

    // K3 = HMAC-MD5(K1, checksum)
    let mut mac = HmacMd5::new_from_slice(&k1).expect("HMAC accepts any key length");
    mac.update(checksum);
    let k3 = mac.finalize().into_bytes();

    // Decrypt payload with RC4 using K3.
    let payload = rc4_transform(&k3, encrypted_data);

    // Verify: HMAC-MD5(K1, decrypted_payload) must equal the checksum.
    let mut mac = HmacMd5::new_from_slice(&k1).expect("HMAC accepts any key length");
    mac.update(&payload);
    let computed_checksum = mac.finalize().into_bytes();

    if computed_checksum.as_slice() != checksum {
        return Err(Error::invalid_data("RC4-HMAC checksum verification failed"));
    }

    // Strip the 8-byte confounder.
    if payload.len() < 8 {
        return Err(Error::invalid_data("RC4-HMAC decrypted payload too short"));
    }
    Ok(payload[8..].to_vec())
}

// ---------------------------------------------------------------------------
// Checksum computation
// ---------------------------------------------------------------------------

/// Compute a standalone Kerberos checksum (MIC) for the given data.
///
/// Uses the checksum subkey Kc (derived with `0x99`) per RFC 3961 section 5.4.
/// This is for standalone checksum operations (for example, the body checksum
/// in the TGS-REQ Authenticator), NOT for the HMAC inside encrypt/decrypt
/// (which uses Ki derived with `0x55`).
///
/// - For AES (etypes 17, 18): HMAC-SHA1 truncated to 12 bytes (96 bits).
/// - For RC4 (etype 23): HMAC-MD5, producing 16 bytes.
pub fn compute_checksum(key: &[u8], usage: u32, data: &[u8], etype: EncryptionType) -> Vec<u8> {
    match etype {
        EncryptionType::Aes128CtsHmacSha196 | EncryptionType::Aes256CtsHmacSha196 => {
            // Derive the checksum key Kc for this usage.
            let kc = derive_key_aes(key, &usage_chk(usage));
            hmac_sha1_96(&kc, data)
        }
        EncryptionType::Rc4Hmac => {
            use hmac::{Hmac, Mac};
            type HmacMd5 = Hmac<md5::Md5>;

            // K1 = HMAC-MD5(key, usage_le)
            let usage_bytes = (usage as i32).to_le_bytes();
            let mut mac = HmacMd5::new_from_slice(key).expect("HMAC accepts any key length");
            mac.update(&usage_bytes);
            let k1 = mac.finalize().into_bytes();

            // Checksum = HMAC-MD5(K1, data)
            let mut mac = HmacMd5::new_from_slice(&k1).expect("HMAC accepts any key length");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// HMAC-SHA1 truncated to 12 bytes (96 bits), as used by AES Kerberos checksums.
fn hmac_sha1_96(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    type HmacSha1 = Hmac<Sha1>;

    let mut mac = HmacSha1::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let result = mac.finalize().into_bytes();
    result[..12].to_vec()
}

/// DK(base_key, constant) per RFC 3961 section 5.1.
///
/// DK = random-to-key(DR(base_key, constant))
/// DR = k-truncate(E(base_key, n-fold(constant, block_size)))
///
/// For AES, random-to-key is the identity function, so DK = DR.
fn dk_derive(base_key: &[u8], constant: &[u8]) -> Vec<u8> {
    let block_size = 16; // AES block size is always 16.
    let key_size = base_key.len();

    // n-fold the constant to the cipher's block size.
    let folded = nfold(constant, block_size);

    // DR: repeatedly encrypt to produce enough key material.
    let mut result = Vec::with_capacity(key_size);
    let mut input = [0u8; 16];
    input.copy_from_slice(&folded);

    while result.len() < key_size {
        // Encrypt the input block with AES-ECB (single block, no IV needed).
        let encrypted = aes_ecb_encrypt(base_key, &input);
        result.extend_from_slice(&encrypted);
        input = encrypted;
    }

    result.truncate(key_size);
    result
}

/// N-fold operation per RFC 3961 section 5.1.
///
/// Takes an input byte string and produces an output of `output_len` bytes.
/// The algorithm rotates the input by 13 bits for each successive copy and
/// sums them with one's-complement-like carry propagation.
fn nfold(input: &[u8], output_len: usize) -> Vec<u8> {
    let in_len = input.len();

    // Helper: get a single byte from `input` RIGHT-rotated by `rot` bits.
    // Right rotation by `rot`: bit `j` of the result comes from
    // bit `(j - rot) mod in_bits` of the original. Equivalently,
    // bit `(j + in_bits - rot) mod in_bits`.
    let rotated_byte = |rot: usize, byte_idx: usize| -> u8 {
        let in_bits = in_len * 8;
        let rot_mod = rot % in_bits;
        let bit = (byte_idx * 8 + in_bits - rot_mod) % in_bits;
        let b = bit / 8;
        let s = bit % 8;
        if s == 0 {
            input[b]
        } else {
            (((input[b] as u16) << s) | ((input[(b + 1) % in_len] as u16) >> (8 - s))) as u8
        }
    };

    let in_bits = in_len * 8;
    let out_bits = output_len * 8;
    let lcm_bits = lcm(in_bits, out_bits);

    // Total bytes to iterate over all copies laid end-to-end.
    let lcm_bytes = lcm_bits / 8;

    // Accumulator (u32 to handle carries).
    let mut result = vec![0u32; output_len];

    // Walk through lcm_bytes bytes, each one coming from a specific
    // rotated copy. The output byte it maps to wraps modulo output_len.
    for i in 0..lcm_bytes {
        // Which copy is this byte from?
        let copy = i / in_len;
        // Which byte within that copy?
        let byte_in_copy = i % in_len;
        // Each copy is rotated 13 bits further than the previous.
        let rotation = copy * 13;
        let val = rotated_byte(rotation, byte_in_copy);
        // Map to output position, wrapping.
        let out_idx = i % output_len;
        result[out_idx] += val as u32;
    }

    // Propagate carries from right to left (big-endian addition).
    // The carry wraps around from the most-significant byte to the
    // least-significant, like one's-complement addition.
    loop {
        let mut carry = 0u32;
        for i in (0..output_len).rev() {
            result[i] += carry;
            carry = result[i] >> 8;
            result[i] &= 0xFF;
        }
        if carry == 0 {
            break;
        }
        // Wrap carry to LSB.
        result[output_len - 1] += carry;
    }

    result.iter().map(|&v| v as u8).collect()
}

/// Least common multiple.
fn lcm(a: usize, b: usize) -> usize {
    a / gcd(a, b) * b
}

/// Greatest common divisor (Euclidean algorithm).
fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// AES-ECB encrypt a single 16-byte block.
fn aes_ecb_encrypt(key: &[u8], block: &[u8]) -> [u8; 16] {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};

    let mut output = [0u8; 16];
    output.copy_from_slice(block);

    match key.len() {
        16 => {
            let cipher = aes::Aes128::new_from_slice(key).expect("valid key");
            cipher.encrypt_block((&mut output).into());
        }
        32 => {
            let cipher = aes::Aes256::new_from_slice(key).expect("valid key");
            cipher.encrypt_block((&mut output).into());
        }
        _ => panic!("AES key must be 16 or 32 bytes, got {}", key.len()),
    }
    output
}

/// AES-ECB decrypt a single 16-byte block.
fn aes_ecb_decrypt(key: &[u8], block: &[u8]) -> [u8; 16] {
    use aes::cipher::{BlockCipherDecrypt, KeyInit};

    let mut output = [0u8; 16];
    output.copy_from_slice(block);

    match key.len() {
        16 => {
            let cipher = aes::Aes128::new_from_slice(key).expect("valid key");
            cipher.decrypt_block((&mut output).into());
        }
        32 => {
            let cipher = aes::Aes256::new_from_slice(key).expect("valid key");
            cipher.decrypt_block((&mut output).into());
        }
        _ => panic!("AES key must be 16 or 32 bytes, got {}", key.len()),
    }
    output
}

/// AES-CBC encrypt (no padding -- input must be a multiple of 16 bytes).
/// Implemented manually using AES-ECB to avoid cbc crate API complexity.
fn aes_cbc_encrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    assert!(
        data.len().is_multiple_of(16),
        "AES-CBC input must be a multiple of 16 bytes"
    );

    let n_blocks = data.len() / 16;
    let mut output = vec![0u8; data.len()];
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);

    for i in 0..n_blocks {
        let start = i * 16;
        let mut block = [0u8; 16];
        block.copy_from_slice(&data[start..start + 16]);
        // XOR with previous ciphertext block (or IV for first block).
        for j in 0..16 {
            block[j] ^= prev[j];
        }
        let encrypted = aes_ecb_encrypt(key, &block);
        output[start..start + 16].copy_from_slice(&encrypted);
        prev = encrypted;
    }
    output
}

/// AES-CBC decrypt (no padding -- input must be a multiple of 16 bytes).
/// Implemented manually using AES-ECB to avoid cbc crate API complexity.
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Vec<u8> {
    assert!(
        data.len().is_multiple_of(16),
        "AES-CBC input must be a multiple of 16 bytes"
    );

    let n_blocks = data.len() / 16;
    let mut output = vec![0u8; data.len()];
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);

    for i in 0..n_blocks {
        let start = i * 16;
        let mut ct_block = [0u8; 16];
        ct_block.copy_from_slice(&data[start..start + 16]);
        let mut decrypted = aes_ecb_decrypt(key, &ct_block);
        // XOR with previous ciphertext block (or IV for first block).
        for j in 0..16 {
            decrypted[j] ^= prev[j];
        }
        output[start..start + 16].copy_from_slice(&decrypted);
        prev = ct_block;
    }
    output
}

/// RC4 stream cipher (symmetric -- encrypt and decrypt are the same operation).
fn rc4_transform(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = (0..=255).collect();
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut i: u8 = 0;
    j = 0;
    data.iter()
        .map(|&byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            byte ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Kerberos encrypt/decrypt (RFC 3961 section 5.3)
// ---------------------------------------------------------------------------
//
// For AES (etypes 17, 18):
//   1. Derive encryption key: Ke = DK(base_key, usage || 0xAA)
//   2. Derive integrity key: Ki = DK(base_key, usage || 0x55)
//   3. Generate random 16-byte confounder
//   4. Plaintext' = confounder || plaintext
//   5. Ciphertext = AES-CTS(Ke, iv=0, plaintext')
//   6. HMAC = HMAC-SHA1-96(Ki, plaintext')
//   7. Output = ciphertext || HMAC (12 bytes)
//
// For RC4-HMAC (etype 23):
//   Uses the encrypt_rc4_hmac function directly (it handles confounder
//   and checksum internally).

/// Encrypt data using the Kerberos profile for the given etype and key usage.
pub(crate) fn kerberos_encrypt(
    base_key: &[u8],
    usage: u32,
    plaintext: &[u8],
    etype: EncryptionType,
) -> Vec<u8> {
    match etype {
        EncryptionType::Aes128CtsHmacSha196 | EncryptionType::Aes256CtsHmacSha196 => {
            // Derive Ke (encryption key) and Ki (integrity key).
            let ke = derive_key_aes(base_key, &usage_enc(usage));
            let ki = derive_key_aes(base_key, &usage_int(usage));

            // Generate 16-byte random confounder.
            let mut confounder = [0u8; 16];
            getrandom::fill(&mut confounder).expect("CSPRNG failed");

            // Build plaintext' = confounder || plaintext.
            let mut full_plain = Vec::with_capacity(16 + plaintext.len());
            full_plain.extend_from_slice(&confounder);
            full_plain.extend_from_slice(plaintext);

            // Compute HMAC-SHA1-96 over plaintext' using Ki.
            let hmac = hmac_sha1_96(&ki, &full_plain);

            // Encrypt plaintext' with AES-CTS using Ke and IV=0.
            let iv = [0u8; 16];
            let ciphertext = encrypt_aes_cts(&ke, &iv, &full_plain);

            // Output = ciphertext || HMAC (12 bytes).
            let mut output = ciphertext;
            output.extend_from_slice(&hmac);
            output
        }
        EncryptionType::Rc4Hmac => encrypt_rc4_hmac(base_key, usage, plaintext),
    }
}

/// Decrypt data using the Kerberos profile for the given etype and key usage.
pub(crate) fn kerberos_decrypt(
    base_key: &[u8],
    usage: u32,
    ciphertext: &[u8],
    etype: EncryptionType,
) -> Result<Vec<u8>, Error> {
    match etype {
        EncryptionType::Aes128CtsHmacSha196 | EncryptionType::Aes256CtsHmacSha196 => {
            // HMAC-SHA1-96 is 12 bytes, appended to the ciphertext.
            if ciphertext.len() < 12 + 16 {
                return Err(Error::invalid_data(
                    "Kerberos AES ciphertext too short (need at least confounder + HMAC)",
                ));
            }

            let hmac_offset = ciphertext.len() - 12;
            let enc_data = &ciphertext[..hmac_offset];
            let expected_hmac = &ciphertext[hmac_offset..];

            // Derive Ke (encryption key) and Ki (integrity key).
            let ke = derive_key_aes(base_key, &usage_enc(usage));
            let ki = derive_key_aes(base_key, &usage_int(usage));

            // Decrypt with AES-CTS using Ke and IV=0.
            let iv = [0u8; 16];
            let full_plain = decrypt_aes_cts(&ke, &iv, enc_data)?;

            // Verify HMAC-SHA1-96 using Ki.
            let computed_hmac = hmac_sha1_96(&ki, &full_plain);
            if computed_hmac != expected_hmac {
                return Err(Error::Auth {
                    message: "Kerberos AES HMAC verification failed".to_string(),
                });
            }

            // Strip the 16-byte confounder.
            if full_plain.len() < 16 {
                return Err(Error::invalid_data(
                    "Kerberos AES decrypted data too short for confounder",
                ));
            }
            Ok(full_plain[16..].to_vec())
        }
        EncryptionType::Rc4Hmac => decrypt_rc4_hmac(base_key, usage, ciphertext),
    }
}

// ---------------------------------------------------------------------------
// Etype conversion
// ---------------------------------------------------------------------------

/// Convert an etype integer value to our enum.
pub(crate) fn etype_from_i32(val: i32) -> Result<EncryptionType, Error> {
    match val {
        18 => Ok(EncryptionType::Aes256CtsHmacSha196),
        17 => Ok(EncryptionType::Aes128CtsHmacSha196),
        23 => Ok(EncryptionType::Rc4Hmac),
        _ => Err(Error::Auth {
            message: format!("unsupported Kerberos encryption type: {val}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Random key generation (test support)
// ---------------------------------------------------------------------------

/// Generate a random key of the appropriate size for the given etype.
#[cfg(test)]
pub(crate) fn generate_random_key(etype: EncryptionType) -> Vec<u8> {
    let key_size = match etype {
        EncryptionType::Aes256CtsHmacSha196 => 32,
        EncryptionType::Aes128CtsHmacSha196 => 16,
        EncryptionType::Rc4Hmac => 16,
    };
    let mut key = vec![0u8; key_size];
    getrandom::fill(&mut key).expect("CSPRNG failed");
    key
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── EncryptionType ────────────────────────────────────────────────

    #[test]
    fn encryption_type_values() {
        assert_eq!(EncryptionType::Aes256CtsHmacSha196 as u32, 18);
        assert_eq!(EncryptionType::Aes128CtsHmacSha196 as u32, 17);
        assert_eq!(EncryptionType::Rc4Hmac as u32, 23);
    }

    // ── n-fold ────────────────────────────────────────────────────────

    #[test]
    fn nfold_rfc3961_test_vectors() {
        // RFC 3961 section 5.1 test vectors.
        // 64-fold("012345") = 0xBE072631276B1955
        let result = nfold(b"012345", 8);
        assert_eq!(result, hex("be072631276b1955"));

        // 56-fold("password") = 0x78A07B6CAF85FA
        let result = nfold(b"password", 7);
        assert_eq!(result, hex("78a07b6caf85fa"));

        // 64-fold("Rough Consensus, and Running Code")
        let result = nfold(b"Rough Consensus, and Running Code", 8);
        assert_eq!(result, hex("bb6ed30870b7f0e0"));

        // 168-fold("password")
        let result = nfold(b"password", 21);
        assert_eq!(result, hex("59e4a8ca7c0385c3c37b3f6d2000247cb6e6bd5b3e"));

        // 128-fold("kerberos")
        let result = nfold(b"kerberos", 16);
        assert_eq!(result, hex("6b65726265726f737b9b5b2b93132b93"));

        // 168-fold("kerberos")
        let result = nfold(b"kerberos", 21);
        assert_eq!(result, hex("8372c236344e5f1550cd0747e15d62ca7a5a3bcea4"));

        // 256-fold("kerberos")
        let result = nfold(b"kerberos", 32);
        assert_eq!(
            result,
            hex("6b65726265726f737b9b5b2b93132b935c9bdcdad95c9899c4cae4dee6d6cae4")
        );
    }

    // ── String-to-Key (RC4) ───────────────────────────────────────────

    #[test]
    fn string_to_key_rc4_produces_nt_hash() {
        // MS-NLMP test vector: password "Password"
        // NT hash = MD4(UTF-16LE("Password"))
        // = a4f49c406510bdcab6824ee7c30fd852
        let key = string_to_key_rc4("Password");
        assert_eq!(key, hex("a4f49c406510bdcab6824ee7c30fd852"));
    }

    #[test]
    fn string_to_key_rc4_empty_password() {
        // Empty password still produces a valid 16-byte hash.
        let key = string_to_key_rc4("");
        assert_eq!(key.len(), 16);
        // MD4 of empty UTF-16LE is: 31d6cfe0d16ae931b73c59d7e0c089c0
        assert_eq!(key, hex("31d6cfe0d16ae931b73c59d7e0c089c0"));
    }

    // ── String-to-Key (AES) ──────────────────────────────────────────

    #[test]
    fn string_to_key_aes256_rfc3962_test_vector() {
        // RFC 3962 Appendix B, Test Vector 4 (iterations = 4096):
        // password = "password", salt = "ATHENA.MIT.EDUraeburn"
        // Verified with Python hashlib.pbkdf2_hmac + AES-ECB DK derivation.
        let key = string_to_key_aes("password", "ATHENA.MIT.EDUraeburn", 32);
        assert_eq!(
            key,
            hex("01b897121d933ab44b47eb5494db15e50eb74530dbdae9b634d65020ff5d88c1")
        );
    }

    #[test]
    fn string_to_key_aes128_rfc3962_test_vector() {
        // RFC 3962 Appendix B, Test Vector 4 (iterations = 4096):
        // password = "password", salt = "ATHENA.MIT.EDUraeburn"
        // Verified with Python hashlib.pbkdf2_hmac + AES-ECB DK derivation.
        let key = string_to_key_aes("password", "ATHENA.MIT.EDUraeburn", 16);
        assert_eq!(key, hex("fca822951813fb252154c883f5ee1cf4"));
    }

    #[test]
    fn string_to_key_aes256_produces_32_bytes() {
        let key = string_to_key_aes("test", "EXAMPLE.COMtest", 32);
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn string_to_key_aes128_produces_16_bytes() {
        let key = string_to_key_aes("test", "EXAMPLE.COMtest", 16);
        assert_eq!(key.len(), 16);
    }

    // ── Key Derivation (AES) ─────────────────────────────────────────

    #[test]
    fn derive_key_aes_deterministic() {
        let base_key = [0xAA; 16];
        let usage = usage_enc(7);
        let k1 = derive_key_aes(&base_key, &usage);
        let k2 = derive_key_aes(&base_key, &usage);
        assert_eq!(k1, k2, "same inputs must produce same output");
    }

    #[test]
    fn derive_key_aes_different_usages_produce_different_keys() {
        let base_key = [0xBB; 16];
        let k_enc = derive_key_aes(&base_key, &usage_enc(7));
        let k_int = derive_key_aes(&base_key, &usage_int(7));
        assert_ne!(
            k_enc, k_int,
            "different usage types must produce different keys"
        );
    }

    #[test]
    fn derive_key_aes_different_usage_numbers_produce_different_keys() {
        let base_key = [0xCC; 32];
        let k1 = derive_key_aes(&base_key, &usage_enc(1));
        let k7 = derive_key_aes(&base_key, &usage_enc(7));
        assert_ne!(
            k1, k7,
            "different usage numbers must produce different keys"
        );
    }

    #[test]
    fn derive_key_aes128_preserves_key_length() {
        let base_key = [0xDD; 16];
        let derived = derive_key_aes(&base_key, &usage_enc(1));
        assert_eq!(derived.len(), 16);
    }

    #[test]
    fn derive_key_aes256_preserves_key_length() {
        let base_key = [0xEE; 32];
        let derived = derive_key_aes(&base_key, &usage_enc(1));
        assert_eq!(derived.len(), 32);
    }

    // ── AES-CTS encryption/decryption ────────────────────────────────

    #[test]
    fn aes_cts_empty_input() {
        let key = [0x11; 16];
        let iv = [0u8; 16];
        let ct = encrypt_aes_cts(&key, &iv, &[]);
        assert!(ct.is_empty());
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert!(pt.is_empty());
    }

    #[test]
    fn aes_cts_single_block_roundtrip() {
        let key = [0x22; 16];
        let iv = [0u8; 16];
        let plaintext = b"sixteen bytes!!!";
        assert_eq!(plaintext.len(), 16);

        let ct = encrypt_aes_cts(&key, &iv, plaintext);
        assert_eq!(ct.len(), 16);
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_cts_two_blocks_roundtrip() {
        let key = [0x33; 16];
        let iv = [0u8; 16];
        let plaintext = [0x42u8; 32]; // Exactly 2 blocks.

        let ct = encrypt_aes_cts(&key, &iv, &plaintext);
        assert_eq!(ct.len(), 32);
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_cts_non_block_aligned_roundtrip() {
        let key = [0x44; 16];
        let iv = [0u8; 16];
        let plaintext = [0x55u8; 30]; // Not a multiple of 16.

        let ct = encrypt_aes_cts(&key, &iv, &plaintext);
        assert_eq!(
            ct.len(),
            30,
            "CTS ciphertext length equals plaintext length"
        );
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_cts_three_blocks_roundtrip() {
        let key = [0x55; 32]; // AES-256
        let iv = [0u8; 16];
        let plaintext = [0x66u8; 48]; // Exactly 3 blocks.

        let ct = encrypt_aes_cts(&key, &iv, &plaintext);
        assert_eq!(ct.len(), 48);
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_cts_non_aligned_aes256_roundtrip() {
        let key = [0x77; 32]; // AES-256
        let iv = [0u8; 16];
        let plaintext: Vec<u8> = (0..50).collect(); // 50 bytes, not block-aligned.

        let ct = encrypt_aes_cts(&key, &iv, &plaintext);
        assert_eq!(ct.len(), 50);
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_cts_sub_block_pads_to_full_block() {
        // Per RFC 3962, a single block (even if plaintext < 16 bytes) produces
        // a full 16-byte ciphertext. The plaintext is zero-padded to 16 bytes
        // before encryption.
        let key = [0x88; 16];
        let iv = [0u8; 16];
        let plaintext = b"short"; // Less than one block.

        let ct = encrypt_aes_cts(&key, &iv, plaintext);
        assert_eq!(ct.len(), 16, "single-block ciphertext is always 16 bytes");

        // Decrypting gives back the zero-padded 16-byte block.
        let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
        assert_eq!(pt.len(), 16);
        assert_eq!(&pt[..5], plaintext.as_slice());
        assert_eq!(&pt[5..], &[0u8; 11]); // Zero padding.
    }

    #[test]
    fn aes_cts_ciphertext_differs_from_plaintext() {
        let key = [0x99; 16];
        let iv = [0u8; 16];
        let plaintext = [0xAA; 32];

        let ct = encrypt_aes_cts(&key, &iv, &plaintext);
        assert_ne!(ct, plaintext, "ciphertext must differ from plaintext");
    }

    // ── RC4-HMAC encryption/decryption ───────────────────────────────

    #[test]
    fn rc4_hmac_roundtrip() {
        let key = hex("a4f49c406510bdcab6824ee7c30fd852");
        let plaintext = b"Hello, Kerberos!";
        let usage = 7u32;

        let ct = encrypt_rc4_hmac(&key, usage, plaintext);
        // Ciphertext should be 16-byte checksum + 8-byte confounder + plaintext.
        assert_eq!(ct.len(), 16 + 8 + plaintext.len());

        let pt = decrypt_rc4_hmac(&key, usage, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn rc4_hmac_empty_plaintext_roundtrip() {
        let key = [0xBB; 16];
        let ct = encrypt_rc4_hmac(&key, 1, &[]);
        // 16-byte checksum + 8-byte confounder + 0-byte plaintext.
        assert_eq!(ct.len(), 24);
        let pt = decrypt_rc4_hmac(&key, 1, &ct).unwrap();
        assert!(pt.is_empty());
    }

    #[test]
    fn rc4_hmac_wrong_key_fails() {
        let key = [0xCC; 16];
        let ct = encrypt_rc4_hmac(&key, 1, b"secret data");

        let wrong_key = [0xDD; 16];
        let result = decrypt_rc4_hmac(&wrong_key, 1, &ct);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum verification failed"));
    }

    #[test]
    fn rc4_hmac_wrong_usage_fails() {
        let key = [0xEE; 16];
        let ct = encrypt_rc4_hmac(&key, 1, b"usage test");

        let result = decrypt_rc4_hmac(&key, 2, &ct);
        assert!(result.is_err());
    }

    #[test]
    fn rc4_hmac_ciphertext_too_short() {
        let key = [0xFF; 16];
        let result = decrypt_rc4_hmac(&key, 1, &[0u8; 23]); // Need at least 24.
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too short"));
    }

    #[test]
    fn rc4_hmac_tampered_ciphertext_fails() {
        let key = [0x11; 16];
        let mut ct = encrypt_rc4_hmac(&key, 1, b"tamper test");

        // Flip a byte in the encrypted data (after the 16-byte checksum).
        let last = ct.len() - 1;
        ct[last] ^= 0xFF;

        let result = decrypt_rc4_hmac(&key, 1, &ct);
        assert!(result.is_err());
    }

    // ── Checksum ─────────────────────────────────────────────────────

    #[test]
    fn checksum_aes_produces_12_bytes() {
        let key = [0x11; 16];
        let data = b"checksum test data";
        let checksum = compute_checksum(&key, 7, data, EncryptionType::Aes128CtsHmacSha196);
        assert_eq!(checksum.len(), 12, "HMAC-SHA1-96 produces 12 bytes");
    }

    #[test]
    fn checksum_aes256_produces_12_bytes() {
        let key = [0x22; 32];
        let data = b"checksum test data";
        let checksum = compute_checksum(&key, 7, data, EncryptionType::Aes256CtsHmacSha196);
        assert_eq!(checksum.len(), 12);
    }

    #[test]
    fn checksum_rc4_produces_16_bytes() {
        let key = [0x33; 16];
        let data = b"checksum test data";
        let checksum = compute_checksum(&key, 7, data, EncryptionType::Rc4Hmac);
        assert_eq!(checksum.len(), 16, "HMAC-MD5 produces 16 bytes");
    }

    #[test]
    fn checksum_aes_deterministic() {
        let key = [0x44; 16];
        let data = b"determinism test";
        let c1 = compute_checksum(&key, 7, data, EncryptionType::Aes128CtsHmacSha196);
        let c2 = compute_checksum(&key, 7, data, EncryptionType::Aes128CtsHmacSha196);
        assert_eq!(c1, c2);
    }

    #[test]
    fn checksum_different_usage_produces_different_result() {
        let key = [0x55; 16];
        let data = b"usage test";
        let c1 = compute_checksum(&key, 1, data, EncryptionType::Aes128CtsHmacSha196);
        let c2 = compute_checksum(&key, 2, data, EncryptionType::Aes128CtsHmacSha196);
        assert_ne!(c1, c2);
    }

    #[test]
    fn checksum_rc4_deterministic() {
        let key = [0x66; 16];
        let data = b"rc4 checksum test";
        let c1 = compute_checksum(&key, 7, data, EncryptionType::Rc4Hmac);
        let c2 = compute_checksum(&key, 7, data, EncryptionType::Rc4Hmac);
        assert_eq!(c1, c2);
    }

    // ── Usage constant helpers ───────────────────────────────────────

    #[test]
    fn usage_enc_format() {
        let u = usage_enc(7);
        assert_eq!(u, [0, 0, 0, 7, 0xAA]);
    }

    #[test]
    fn usage_int_format() {
        let u = usage_int(7);
        assert_eq!(u, [0, 0, 0, 7, 0x55]);
    }

    // ── Helper ───────────────────────────────────────────────────────

    /// Parse a hex string into bytes (ignores spaces).
    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn string_to_key_aes256_matches_mit_kdc_keytab() {
        // Key from MIT KDC keytab for testuser@TEST.LOCAL with password "testpass"
        // Salt = "TEST.LOCALtestuser"
        let key = string_to_key_aes("testpass", "TEST.LOCALtestuser", 32);
        let expected = hex("7964c7e6f475912def26f886f2683da03f58257a987bca47e461daddb18cb336");
        assert_eq!(key, expected, "key must match MIT KDC keytab");
    }

    #[test]
    fn aes_cts_known_vectors() {
        // AES-CTS test vectors. Key: "chicken teriyaki", IV: all zeros.
        // Plaintext: "I would like the General Gau's Chicken, please, and wonton soup."
        let key = hex("636869636b656e207465726979616b69");
        let iv = [0u8; 16];
        let full_plain = b"I would like the General Gau's Chicken, please, and wonton soup.";

        // 17 bytes: verified against minikerberos (Python Kerberos reference).
        let ct_17 = encrypt_aes_cts(&key, &iv, &full_plain[..17]);
        assert_eq!(
            ct_17,
            hex("c6353568f2bf8cb4d8a580362da7ff7f97"),
            "17-byte CTS failed"
        );

        // All CTS vectors must roundtrip correctly.
        for len in [17, 31, 32, 47, 48, 64] {
            let ct = encrypt_aes_cts(&key, &iv, &full_plain[..len]);
            assert_eq!(ct.len(), len, "CTS ciphertext length for {len} bytes");
            let pt = decrypt_aes_cts(&key, &iv, &ct).unwrap();
            assert_eq!(&pt[..], &full_plain[..len], "CTS roundtrip for {len} bytes");
        }
    }

    // ── Kerberos encrypt/decrypt roundtrip ───────────────────────────

    #[test]
    fn kerberos_encrypt_decrypt_aes256() {
        let key = string_to_key_aes("password", "EXAMPLE.COMuser", 32);
        let plaintext = b"Hello, Kerberos!";

        let ciphertext = kerberos_encrypt(&key, 7, plaintext, EncryptionType::Aes256CtsHmacSha196);
        let decrypted =
            kerberos_decrypt(&key, 7, &ciphertext, EncryptionType::Aes256CtsHmacSha196).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn kerberos_encrypt_decrypt_aes128() {
        let key = string_to_key_aes("password", "EXAMPLE.COMuser", 16);
        let plaintext = b"Hello, Kerberos AES-128!";

        let ciphertext = kerberos_encrypt(&key, 3, plaintext, EncryptionType::Aes128CtsHmacSha196);
        let decrypted =
            kerberos_decrypt(&key, 3, &ciphertext, EncryptionType::Aes128CtsHmacSha196).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn kerberos_encrypt_decrypt_rc4() {
        let key = string_to_key_rc4("password");
        let plaintext = b"Hello, RC4!";

        let ciphertext = kerberos_encrypt(&key, 7, plaintext, EncryptionType::Rc4Hmac);
        let decrypted = kerberos_decrypt(&key, 7, &ciphertext, EncryptionType::Rc4Hmac).unwrap();

        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn kerberos_decrypt_wrong_key_fails() {
        let key = string_to_key_aes("password", "EXAMPLE.COMuser", 32);
        let wrong_key = string_to_key_aes("wrong", "EXAMPLE.COMuser", 32);
        let plaintext = b"secret data";

        let ciphertext = kerberos_encrypt(&key, 1, plaintext, EncryptionType::Aes256CtsHmacSha196);
        let result = kerberos_decrypt(
            &wrong_key,
            1,
            &ciphertext,
            EncryptionType::Aes256CtsHmacSha196,
        );

        assert!(result.is_err(), "decryption with wrong key should fail");
    }

    #[test]
    fn kerberos_decrypt_wrong_usage_fails() {
        let key = string_to_key_aes("password", "EXAMPLE.COMuser", 32);
        let plaintext = b"secret data";

        let ciphertext = kerberos_encrypt(&key, 1, plaintext, EncryptionType::Aes256CtsHmacSha196);
        let result = kerberos_decrypt(&key, 7, &ciphertext, EncryptionType::Aes256CtsHmacSha196);

        assert!(result.is_err(), "decryption with wrong usage should fail");
    }

    // ── Etype conversion ─────────────────────────────────────────────

    #[test]
    fn etype_from_i32_valid() {
        assert_eq!(
            etype_from_i32(18).unwrap(),
            EncryptionType::Aes256CtsHmacSha196
        );
        assert_eq!(
            etype_from_i32(17).unwrap(),
            EncryptionType::Aes128CtsHmacSha196
        );
        assert_eq!(etype_from_i32(23).unwrap(), EncryptionType::Rc4Hmac);
    }

    #[test]
    fn etype_from_i32_unsupported() {
        assert!(etype_from_i32(99).is_err());
        assert!(etype_from_i32(0).is_err());
    }

    // ── Random key generation ────────────────────────────────────────

    #[test]
    fn generate_random_key_sizes() {
        assert_eq!(
            generate_random_key(EncryptionType::Aes256CtsHmacSha196).len(),
            32
        );
        assert_eq!(
            generate_random_key(EncryptionType::Aes128CtsHmacSha196).len(),
            16
        );
        assert_eq!(generate_random_key(EncryptionType::Rc4Hmac).len(), 16);
    }
}
