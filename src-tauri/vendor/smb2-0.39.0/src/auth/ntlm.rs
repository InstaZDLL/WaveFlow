//! NTLM authentication (MS-NLMP).
//!
//! Implements the 3-message NTLM exchange:
//! 1. Client sends NEGOTIATE_MESSAGE (Type 1)
//! 2. Server sends CHALLENGE_MESSAGE (Type 2)
//! 3. Client sends AUTHENTICATE_MESSAGE (Type 3)
//!
//! Only NTLMv2 is supported. NTLMv1 is insecure and not implemented.

use log::trace;

use crate::Error;
use digest::{Digest, KeyInit};
use hmac::{Hmac, Mac};

type HmacMd5 = Hmac<md5::Md5>;

// ---------------------------------------------------------------------------
// NTLM signature and message types
// ---------------------------------------------------------------------------

/// The 8-byte NTLM signature: `"NTLMSSP\0"`.
const NTLM_SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

/// NEGOTIATE_MESSAGE type.
const MSG_TYPE_NEGOTIATE: u32 = 0x0000_0001;
/// CHALLENGE_MESSAGE type.
const MSG_TYPE_CHALLENGE: u32 = 0x0000_0002;
/// AUTHENTICATE_MESSAGE type.
const MSG_TYPE_AUTHENTICATE: u32 = 0x0000_0003;

// ---------------------------------------------------------------------------
// Negotiate flags (section 2.2.2.5)
// ---------------------------------------------------------------------------

const NTLMSSP_NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const NTLMSSP_REQUEST_TARGET: u32 = 0x0000_0004;
const NTLMSSP_NEGOTIATE_SIGN: u32 = 0x0000_0010;
const NTLMSSP_NEGOTIATE_SEAL: u32 = 0x0000_0020;
const NTLMSSP_NEGOTIATE_NTLM: u32 = 0x0000_0200;
const NTLMSSP_NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
const NTLMSSP_NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const NTLMSSP_NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
const NTLMSSP_NEGOTIATE_128: u32 = 0x2000_0000;
const NTLMSSP_NEGOTIATE_KEY_EXCH: u32 = 0x4000_0000;
const NTLMSSP_NEGOTIATE_56: u32 = 0x8000_0000;

/// Default flags the client sends in the NEGOTIATE_MESSAGE.
const DEFAULT_NEGOTIATE_FLAGS: u32 = NTLMSSP_NEGOTIATE_UNICODE
    | NTLMSSP_REQUEST_TARGET
    | NTLMSSP_NEGOTIATE_NTLM
    | NTLMSSP_NEGOTIATE_ALWAYS_SIGN
    | NTLMSSP_NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NTLMSSP_NEGOTIATE_TARGET_INFO
    | NTLMSSP_NEGOTIATE_128
    | NTLMSSP_NEGOTIATE_KEY_EXCH
    | NTLMSSP_NEGOTIATE_56
    | NTLMSSP_NEGOTIATE_SIGN
    | NTLMSSP_NEGOTIATE_SEAL;

// ---------------------------------------------------------------------------
// AV_PAIR types (section 2.2.2.1)
// ---------------------------------------------------------------------------

/// End of AV_PAIR list.
const MSV_AV_EOL: u16 = 0x0000;
/// NetBIOS computer name.
#[cfg(test)]
const MSV_AV_NB_COMPUTER_NAME: u16 = 0x0001;
/// NetBIOS domain name.
#[cfg(test)]
const MSV_AV_NB_DOMAIN_NAME: u16 = 0x0002;
/// DNS computer name.
#[allow(dead_code)]
const MSV_AV_DNS_COMPUTER_NAME: u16 = 0x0003;
/// DNS domain name.
#[allow(dead_code)]
const MSV_AV_DNS_DOMAIN_NAME: u16 = 0x0004;
/// Flags.
const MSV_AV_FLAGS: u16 = 0x0006;
/// Timestamp (FILETIME).
const MSV_AV_TIMESTAMP: u16 = 0x0007;
/// Target name (SPN).
#[allow(dead_code)]
const MSV_AV_TARGET_NAME: u16 = 0x0009;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Credentials for NTLM authentication.
pub struct NtlmCredentials {
    /// The username.
    pub username: String,
    /// The password.
    pub password: String,
    /// The domain (can be empty for local accounts).
    pub domain: String,
}

/// Stateful NTLM authenticator that manages the 3-message exchange.
///
/// Usage:
/// 1. Call [`negotiate()`](Self::negotiate) to get the NEGOTIATE_MESSAGE bytes.
/// 2. Send those bytes in SESSION_SETUP, receive the server's CHALLENGE_MESSAGE.
/// 3. Call [`authenticate()`](Self::authenticate) with the challenge bytes to
///    get the AUTHENTICATE_MESSAGE bytes.
/// 4. After authenticate succeeds, [`session_key()`](Self::session_key) returns
///    the exported session key for signing/encryption.
pub struct NtlmAuthenticator {
    credentials: NtlmCredentials,
    /// Retained for MIC computation.
    negotiate_bytes: Option<Vec<u8>>,
    /// Retained for MIC computation.
    challenge_bytes: Option<Vec<u8>>,
    /// The exported session key, available after authenticate().
    session_key: Option<Vec<u8>>,
    /// Override for the client challenge (for testing with known values).
    #[cfg(test)]
    test_client_challenge: Option<[u8; 8]>,
    /// Override for the random session key (for testing with known values).
    #[cfg(test)]
    test_random_session_key: Option<[u8; 16]>,
    /// Override for the timestamp (for testing with known values).
    #[cfg(test)]
    test_timestamp: Option<u64>,
}

impl NtlmAuthenticator {
    /// Create a new authenticator with the given credentials.
    pub fn new(credentials: NtlmCredentials) -> Self {
        Self {
            credentials,
            negotiate_bytes: None,
            challenge_bytes: None,
            session_key: None,
            #[cfg(test)]
            test_client_challenge: None,
            #[cfg(test)]
            test_random_session_key: None,
            #[cfg(test)]
            test_timestamp: None,
        }
    }

    /// Build the NEGOTIATE_MESSAGE (Type 1).
    ///
    /// Returns the raw bytes to embed in SESSION_SETUP's security buffer.
    pub fn negotiate(&mut self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32);

        // Signature (8 bytes)
        buf.extend_from_slice(NTLM_SIGNATURE);
        // MessageType (4 bytes)
        buf.extend_from_slice(&MSG_TYPE_NEGOTIATE.to_le_bytes());
        // NegotiateFlags (4 bytes)
        buf.extend_from_slice(&DEFAULT_NEGOTIATE_FLAGS.to_le_bytes());
        // DomainNameFields: Len(2) + MaxLen(2) + Offset(4) = all zeros (no domain supplied in negotiate)
        buf.extend_from_slice(&[0u8; 8]);
        // WorkstationFields: Len(2) + MaxLen(2) + Offset(4) = all zeros
        buf.extend_from_slice(&[0u8; 8]);

        trace!("ntlm: negotiate message built, len={}", buf.len());
        self.negotiate_bytes = Some(buf.clone());
        buf
    }

    /// Process the CHALLENGE_MESSAGE (Type 2) from the server and build the
    /// AUTHENTICATE_MESSAGE (Type 3).
    ///
    /// Returns the raw bytes for the next SESSION_SETUP.
    pub fn authenticate(&mut self, challenge_bytes: &[u8]) -> Result<Vec<u8>, Error> {
        trace!("ntlm: processing challenge, len={}", challenge_bytes.len());
        self.challenge_bytes = Some(challenge_bytes.to_vec());

        // Parse the CHALLENGE_MESSAGE
        let challenge = parse_challenge_message(challenge_bytes)?;
        trace!(
            "ntlm: challenge flags=0x{:08x}, target_info_len={}",
            challenge.negotiate_flags,
            challenge.target_info.len()
        );

        // Compute NTLMv2 response
        let nt_hash = compute_nt_hash(&self.credentials.password);
        let ntlmv2_hash = compute_ntlmv2_hash(
            &nt_hash,
            &self.credentials.username,
            &self.credentials.domain,
        );

        // Get timestamp from challenge TargetInfo, or use current time
        let timestamp = self.get_timestamp(&challenge);

        // Get client challenge
        let client_challenge = self.get_client_challenge();

        // Check if MsvAvTimestamp is present (determines if MIC is required)
        let has_timestamp = find_av_pair(&challenge.target_info, MSV_AV_TIMESTAMP).is_some();

        // Build the modified target info for the authenticate message
        let auth_target_info = build_auth_target_info(&challenge.target_info, has_timestamp);

        // Build temp blob (section 3.3.2)
        let temp = build_temp(timestamp, &client_challenge, &auth_target_info);

        // NTProofStr = HMAC_MD5(NTLMv2_Hash, server_challenge + temp)
        let nt_proof_str = {
            let mut mac =
                HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
            mac.update(&challenge.server_challenge);
            mac.update(&temp);
            mac.finalize().into_bytes().to_vec()
        };

        // NtChallengeResponse = NTProofStr + temp
        let mut nt_challenge_response = nt_proof_str.clone();
        nt_challenge_response.extend_from_slice(&temp);

        // SessionBaseKey = HMAC_MD5(NTLMv2_Hash, NTProofStr)
        let session_base_key = {
            let mut mac =
                HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
            mac.update(&nt_proof_str);
            mac.finalize().into_bytes().to_vec()
        };

        // Key exchange: if KEY_EXCH is negotiated, generate random session key
        let negotiate_flags = challenge.negotiate_flags;
        let key_exch = (negotiate_flags & NTLMSSP_NEGOTIATE_KEY_EXCH) != 0
            && ((negotiate_flags & NTLMSSP_NEGOTIATE_SIGN) != 0
                || (negotiate_flags & NTLMSSP_NEGOTIATE_SEAL) != 0);

        let (exported_session_key, encrypted_random_session_key) = if key_exch {
            let random_key = self.get_random_session_key();
            let encrypted = rc4_encrypt(&session_base_key, &random_key);
            (random_key.to_vec(), encrypted)
        } else {
            (session_base_key.clone(), Vec::new())
        };

        // LmChallengeResponse: if timestamp present, send Z(24); otherwise compute LMv2
        let lm_challenge_response = if has_timestamp {
            vec![0u8; 24]
        } else {
            // LMv2: HMAC_MD5(ntlmv2_hash, server_challenge + client_challenge) + client_challenge
            let mut mac =
                HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
            mac.update(&challenge.server_challenge);
            mac.update(&client_challenge);
            let proof = mac.finalize().into_bytes();
            let mut resp = proof.to_vec();
            resp.extend_from_slice(&client_challenge);
            resp
        };

        // Build the AUTHENTICATE_MESSAGE
        let auth_msg = build_authenticate_message(
            negotiate_flags,
            &self.credentials.domain,
            &self.credentials.username,
            &lm_challenge_response,
            &nt_challenge_response,
            &encrypted_random_session_key,
            has_timestamp,
        );

        // If MIC is required, compute it and patch it in
        let final_msg = if has_timestamp {
            let negotiate_bytes = self.negotiate_bytes.as_ref().ok_or_else(|| {
                Error::invalid_data("negotiate() must be called before authenticate()")
            })?;

            let mic = compute_mic(
                &exported_session_key,
                negotiate_bytes,
                challenge_bytes,
                &auth_msg,
            );

            let mut patched = auth_msg;
            // MIC is at offset 72 (after signature(8) + type(4) + 6 fields * 8 + flags(4) + version(8))
            // = 8 + 4 + 48 + 4 + 8 = 72
            patched[72..88].copy_from_slice(&mic);
            patched
        } else {
            auth_msg
        };

        self.session_key = Some(exported_session_key);
        trace!(
            "ntlm: authenticate message built, len={}, mic={}",
            final_msg.len(),
            has_timestamp
        );
        Ok(final_msg)
    }

    /// Get the session key (available after authenticate()).
    pub fn session_key(&self) -> Option<&[u8]> {
        self.session_key.as_deref()
    }

    /// Get the timestamp to use. If the challenge contains MsvAvTimestamp, use it.
    /// Otherwise use current time (or test override).
    fn get_timestamp(&self, challenge: &ChallengeMessage) -> u64 {
        #[cfg(test)]
        if let Some(ts) = self.test_timestamp {
            return ts;
        }

        if let Some(ts_bytes) = find_av_pair(&challenge.target_info, MSV_AV_TIMESTAMP) {
            if ts_bytes.len() == 8 {
                return u64::from_le_bytes(ts_bytes.try_into().unwrap());
            }
        }

        // Current time as Windows FILETIME (100-ns intervals since 1601-01-01)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        // UNIX epoch is 11644473600 seconds after FILETIME epoch
        (now.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(now.subsec_nanos()) / 100
    }

    /// Get the client challenge (random 8 bytes, or test override).
    fn get_client_challenge(&self) -> [u8; 8] {
        #[cfg(test)]
        if let Some(cc) = self.test_client_challenge {
            return cc;
        }

        let mut challenge = [0u8; 8];
        getrandom::fill(&mut challenge).expect("system RNG failed");
        challenge
    }

    /// Get the random session key (random 16 bytes, or test override).
    ///
    /// This MUST be cryptographically secure -- the ExportedSessionKey
    /// is used for all subsequent signing and encryption. A predictable
    /// key would let an attacker forge messages and decrypt traffic.
    fn get_random_session_key(&self) -> [u8; 16] {
        #[cfg(test)]
        if let Some(rsk) = self.test_random_session_key {
            return rsk;
        }

        let mut key = [0u8; 16];
        getrandom::fill(&mut key).expect("system RNG failed");
        key
    }
}

// ---------------------------------------------------------------------------
// Parsed CHALLENGE_MESSAGE
// ---------------------------------------------------------------------------

/// Parsed fields from a CHALLENGE_MESSAGE (Type 2).
struct ChallengeMessage {
    /// The server's negotiate flags.
    negotiate_flags: u32,
    /// The 8-byte server challenge.
    server_challenge: [u8; 8],
    /// Raw TargetInfo bytes (sequence of AV_PAIRs).
    target_info: Vec<u8>,
}

/// Parse a CHALLENGE_MESSAGE from raw bytes.
fn parse_challenge_message(data: &[u8]) -> Result<ChallengeMessage, Error> {
    if data.len() < 32 {
        return Err(Error::invalid_data("CHALLENGE_MESSAGE too short"));
    }

    // Verify signature
    if &data[0..8] != NTLM_SIGNATURE {
        return Err(Error::invalid_data(
            "invalid NTLM signature in CHALLENGE_MESSAGE",
        ));
    }

    // Verify message type
    let msg_type = u32::from_le_bytes(data[8..12].try_into().unwrap());
    if msg_type != MSG_TYPE_CHALLENGE {
        return Err(Error::invalid_data(format!(
            "expected CHALLENGE_MESSAGE type 2, got {}",
            msg_type
        )));
    }

    // TargetNameFields at offset 12: Len(2) + MaxLen(2) + Offset(4)
    // We don't need the target name for authentication, but we parse past it.

    // NegotiateFlags at offset 20
    let negotiate_flags = u32::from_le_bytes(data[20..24].try_into().unwrap());

    // ServerChallenge at offset 24 (8 bytes)
    let mut server_challenge = [0u8; 8];
    server_challenge.copy_from_slice(&data[24..32]);

    // Reserved at offset 32 (8 bytes) - skip

    // TargetInfoFields at offset 40: Len(2) + MaxLen(2) + Offset(4)
    let target_info = if data.len() >= 48 {
        let ti_len = u16::from_le_bytes(data[40..42].try_into().unwrap()) as usize;
        let ti_offset = u32::from_le_bytes(data[44..48].try_into().unwrap()) as usize;
        if ti_len > 0 && ti_offset + ti_len <= data.len() {
            data[ti_offset..ti_offset + ti_len].to_vec()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    Ok(ChallengeMessage {
        negotiate_flags,
        server_challenge,
        target_info,
    })
}

// ---------------------------------------------------------------------------
// AV_PAIR parsing and building
// ---------------------------------------------------------------------------

/// Find an AV_PAIR with the given AvId in a TargetInfo byte sequence.
/// Returns the value bytes if found, or None.
fn find_av_pair(target_info: &[u8], av_id: u16) -> Option<Vec<u8>> {
    let mut offset = 0;
    while offset + 4 <= target_info.len() {
        let id = u16::from_le_bytes(target_info[offset..offset + 2].try_into().unwrap());
        let len =
            u16::from_le_bytes(target_info[offset + 2..offset + 4].try_into().unwrap()) as usize;

        if id == av_id {
            if offset + 4 + len <= target_info.len() {
                return Some(target_info[offset + 4..offset + 4 + len].to_vec());
            }
            return None;
        }

        if id == MSV_AV_EOL {
            break;
        }

        offset += 4 + len;
    }
    None
}

/// Parse all AV_PAIRs from a TargetInfo byte sequence.
/// Returns a list of (AvId, Value) pairs.
fn parse_av_pairs(target_info: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut pairs = Vec::new();
    let mut offset = 0;
    while offset + 4 <= target_info.len() {
        let id = u16::from_le_bytes(target_info[offset..offset + 2].try_into().unwrap());
        let len =
            u16::from_le_bytes(target_info[offset + 2..offset + 4].try_into().unwrap()) as usize;

        if id == MSV_AV_EOL {
            pairs.push((id, Vec::new()));
            break;
        }

        if offset + 4 + len > target_info.len() {
            break;
        }

        pairs.push((id, target_info[offset + 4..offset + 4 + len].to_vec()));
        offset += 4 + len;
    }
    pairs
}

/// Build the TargetInfo for the AUTHENTICATE_MESSAGE.
///
/// If `has_timestamp` is true, adds MsvAvFlags with bit 0x2 set (MIC present).
/// Removes the trailing MsvAvEOL, adds new pairs, then re-adds MsvAvEOL.
fn build_auth_target_info(challenge_target_info: &[u8], has_timestamp: bool) -> Vec<u8> {
    let pairs = parse_av_pairs(challenge_target_info);
    let mut result = Vec::new();

    // Copy all existing pairs except MsvAvEOL and MsvAvFlags (we'll re-add flags if needed)
    for (id, value) in &pairs {
        if *id == MSV_AV_EOL {
            continue;
        }
        if *id == MSV_AV_FLAGS && has_timestamp {
            // We'll add our own flags entry
            continue;
        }
        result.extend_from_slice(&id.to_le_bytes());
        result.extend_from_slice(&(value.len() as u16).to_le_bytes());
        result.extend_from_slice(value);
    }

    // If MIC is required, add MsvAvFlags with bit 0x2
    if has_timestamp {
        // Check if there was an existing flags value to preserve other bits
        let existing_flags = pairs
            .iter()
            .find(|(id, _)| *id == MSV_AV_FLAGS)
            .map(|(_, v)| {
                if v.len() >= 4 {
                    u32::from_le_bytes(v[..4].try_into().unwrap())
                } else {
                    0
                }
            })
            .unwrap_or(0);
        let flags = existing_flags | 0x0000_0002; // MIC present
        result.extend_from_slice(&MSV_AV_FLAGS.to_le_bytes());
        result.extend_from_slice(&4u16.to_le_bytes());
        result.extend_from_slice(&flags.to_le_bytes());
    }

    // Terminate with MsvAvEOL
    result.extend_from_slice(&MSV_AV_EOL.to_le_bytes());
    result.extend_from_slice(&0u16.to_le_bytes());

    result
}

// ---------------------------------------------------------------------------
// Crypto helpers
// ---------------------------------------------------------------------------

/// Compute the NT hash: MD4(UTF-16LE(password)).
fn compute_nt_hash(password: &str) -> Vec<u8> {
    let unicode_password: Vec<u8> = password
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let mut hasher = md4::Md4::new();
    hasher.update(&unicode_password);
    hasher.finalize().to_vec()
}

/// Compute the NTLMv2 hash: HMAC_MD5(NT_Hash, uppercase(UTF-16LE(username)) + UTF-16LE(domain)).
fn compute_ntlmv2_hash(nt_hash: &[u8], username: &str, domain: &str) -> Vec<u8> {
    let user_upper: Vec<u8> = username
        .to_uppercase()
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    let domain_unicode: Vec<u8> = domain
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();

    let mut mac = HmacMd5::new_from_slice(nt_hash).expect("HMAC accepts any key length");
    mac.update(&user_upper);
    mac.update(&domain_unicode);
    mac.finalize().into_bytes().to_vec()
}

/// Build the temp blob for NTLMv2 (section 3.3.2).
///
/// ```text
/// temp = 0x01 0x01 + Z(6) + Time(8) + ClientChallenge(8) + Z(4) + ServerName + Z(4)
/// ```
///
/// Here `ServerName` is the AV_PAIR sequence (target_info for the authenticate message).
fn build_temp(timestamp: u64, client_challenge: &[u8; 8], target_info: &[u8]) -> Vec<u8> {
    let mut temp = Vec::new();
    temp.push(0x01); // Responserversion
    temp.push(0x01); // HiResponserversion
    temp.extend_from_slice(&[0u8; 6]); // Z(6)
    temp.extend_from_slice(&timestamp.to_le_bytes()); // Time (8 bytes)
    temp.extend_from_slice(client_challenge); // ClientChallenge (8 bytes)
    temp.extend_from_slice(&[0u8; 4]); // Z(4)
    temp.extend_from_slice(target_info); // ServerName (AV_PAIRs)
    temp.extend_from_slice(&[0u8; 4]); // Z(4) - trailing padding
    temp
}

/// RC4 encryption (symmetric -- encrypt and decrypt are the same operation).
fn rc4_encrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
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

/// Recover the exported session key from an AUTHENTICATE_MESSAGE the way a
/// SERVER does, so a test can sign a response exactly as a real one would.
///
/// Test-only: nothing in this crate ever plays the server. It exists because
/// the client refuses a final SESSION_SETUP response it cannot verify, and the
/// key that verifies it comes out of the client's own random choices (the
/// client challenge, the key-exchange key), which only this derivation sees
/// through.
#[cfg(test)]
pub(crate) fn exported_session_key_as_server(
    authenticate: &[u8],
    username: &str,
    password: &str,
    domain: &str,
) -> Vec<u8> {
    let field = |at: usize| -> &[u8] {
        let len = u16::from_le_bytes([authenticate[at], authenticate[at + 1]]) as usize;
        let offset = u32::from_le_bytes(authenticate[at + 4..at + 8].try_into().unwrap()) as usize;
        &authenticate[offset..offset + len]
    };
    // NtChallengeResponseFields at 20, EncryptedRandomSessionKeyFields at 52
    // (MS-NLMP § 2.2.1.3).
    let nt_proof_str = &field(20)[..16];
    let encrypted_random_session_key = field(52);

    let ntlmv2_hash = compute_ntlmv2_hash(&compute_nt_hash(password), username, domain);
    let mut mac = HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
    mac.update(nt_proof_str);
    let session_base_key = mac.finalize().into_bytes().to_vec();

    if encrypted_random_session_key.is_empty() {
        session_base_key
    } else {
        rc4_encrypt(&session_base_key, encrypted_random_session_key)
    }
}

/// Compute the MIC: HMAC_MD5(ExportedSessionKey, negotiate || challenge || authenticate).
fn compute_mic(
    exported_session_key: &[u8],
    negotiate_bytes: &[u8],
    challenge_bytes: &[u8],
    authenticate_bytes: &[u8],
) -> Vec<u8> {
    let mut mac =
        HmacMd5::new_from_slice(exported_session_key).expect("HMAC accepts any key length");
    mac.update(negotiate_bytes);
    mac.update(challenge_bytes);
    mac.update(authenticate_bytes);
    mac.finalize().into_bytes().to_vec()
}

/// Encode a string as UTF-16LE bytes.
fn encode_utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

// ---------------------------------------------------------------------------
// AUTHENTICATE_MESSAGE construction
// ---------------------------------------------------------------------------

/// Build the AUTHENTICATE_MESSAGE (Type 3).
///
/// The MIC field (16 bytes at offset 72) is initially zeroed.
/// The caller must patch it in if MIC is required.
fn build_authenticate_message(
    negotiate_flags: u32,
    domain: &str,
    username: &str,
    lm_challenge_response: &[u8],
    nt_challenge_response: &[u8],
    encrypted_random_session_key: &[u8],
    include_mic: bool,
) -> Vec<u8> {
    let domain_bytes = encode_utf16le(domain);
    let user_bytes = encode_utf16le(username);
    let workstation_bytes: Vec<u8> = Vec::new(); // Empty workstation

    // Fixed header size:
    // Signature(8) + MessageType(4) + 6 * Fields(8 each) + NegotiateFlags(4) + Version(8)
    // = 8 + 4 + 48 + 4 + 8 = 72
    // + MIC(16) if included = 88
    let header_size = if include_mic { 88 } else { 72 };

    // Payload offsets (payload starts after the fixed header)
    let domain_offset = header_size;
    let user_offset = domain_offset + domain_bytes.len();
    let workstation_offset = user_offset + user_bytes.len();
    let lm_offset = workstation_offset + workstation_bytes.len();
    let nt_offset = lm_offset + lm_challenge_response.len();
    let session_key_offset = nt_offset + nt_challenge_response.len();

    let mut buf = Vec::with_capacity(session_key_offset + encrypted_random_session_key.len());

    // Signature (8 bytes)
    buf.extend_from_slice(NTLM_SIGNATURE);
    // MessageType (4 bytes)
    buf.extend_from_slice(&MSG_TYPE_AUTHENTICATE.to_le_bytes());

    // LmChallengeResponseFields (8 bytes)
    buf.extend_from_slice(&(lm_challenge_response.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(lm_challenge_response.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(lm_offset as u32).to_le_bytes());

    // NtChallengeResponseFields (8 bytes)
    buf.extend_from_slice(&(nt_challenge_response.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(nt_challenge_response.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(nt_offset as u32).to_le_bytes());

    // DomainNameFields (8 bytes)
    buf.extend_from_slice(&(domain_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(domain_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(domain_offset as u32).to_le_bytes());

    // UserNameFields (8 bytes)
    buf.extend_from_slice(&(user_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(user_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(user_offset as u32).to_le_bytes());

    // WorkstationFields (8 bytes)
    buf.extend_from_slice(&(workstation_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(workstation_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(workstation_offset as u32).to_le_bytes());

    // EncryptedRandomSessionKeyFields (8 bytes)
    buf.extend_from_slice(&(encrypted_random_session_key.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(encrypted_random_session_key.len() as u16).to_le_bytes());
    buf.extend_from_slice(&(session_key_offset as u32).to_le_bytes());

    // NegotiateFlags (4 bytes)
    buf.extend_from_slice(&negotiate_flags.to_le_bytes());

    // Version (8 bytes) - zeros (no NTLMSSP_NEGOTIATE_VERSION flag set)
    buf.extend_from_slice(&[0u8; 8]);

    // MIC (16 bytes) - zeroed, caller patches if needed
    if include_mic {
        buf.extend_from_slice(&[0u8; 16]);
    }

    // Payload
    buf.extend_from_slice(&domain_bytes);
    buf.extend_from_slice(&user_bytes);
    buf.extend_from_slice(&workstation_bytes);
    buf.extend_from_slice(lm_challenge_response);
    buf.extend_from_slice(nt_challenge_response);
    buf.extend_from_slice(encrypted_random_session_key);

    buf
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // =======================================================================
    // Test vectors from MS-NLMP section 4.2.1 (Common Values)
    // =======================================================================

    const TEST_USER: &str = "User";
    const TEST_PASSWORD: &str = "Password";
    const TEST_DOMAIN: &str = "Domain";
    const TEST_SERVER_CHALLENGE: [u8; 8] = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
    const TEST_CLIENT_CHALLENGE: [u8; 8] = [0xaa; 8];
    const TEST_RANDOM_SESSION_KEY: [u8; 16] = [0x55; 16];
    const TEST_TIME: u64 = 0; // All zeros in the test vectors

    // =======================================================================
    // NT hash tests
    // =======================================================================

    #[test]
    fn nt_hash_of_password() {
        // From section 4.2.2.1.2: NTOWFv1("Password", ...) =
        // a4 f4 9c 40 65 10 bd ca b6 82 4e e7 c3 0f d8 52
        let expected = [
            0xa4, 0xf4, 0x9c, 0x40, 0x65, 0x10, 0xbd, 0xca, 0xb6, 0x82, 0x4e, 0xe7, 0xc3, 0x0f,
            0xd8, 0x52,
        ];
        let hash = compute_nt_hash(TEST_PASSWORD);
        assert_eq!(hash, expected);
    }

    // =======================================================================
    // NTLMv2 hash tests
    // =======================================================================

    #[test]
    fn ntlmv2_hash_computation() {
        // From section 4.2.4.1.1: NTOWFv2("Password", "User", "Domain") =
        // 0c 86 8a 40 3b fd 7a 93 a3 00 1e f2 2e f0 2e 3f
        let expected = [
            0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0,
            0x2e, 0x3f,
        ];
        let nt_hash = compute_nt_hash(TEST_PASSWORD);
        let ntlmv2_hash = compute_ntlmv2_hash(&nt_hash, TEST_USER, TEST_DOMAIN);
        assert_eq!(ntlmv2_hash, expected);
    }

    // =======================================================================
    // NTProofStr and SessionBaseKey tests (section 4.2.4)
    // =======================================================================

    #[test]
    fn nt_proof_str_computation() {
        // From section 4.2.4.2.2: NTLMv2 Response starts with NTProofStr =
        // 68 cd 0a b8 51 e5 1c 96 aa bc 92 7b eb ef 6a 1c
        let expected_nt_proof_str = [
            0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef,
            0x6a, 0x1c,
        ];

        let nt_hash = compute_nt_hash(TEST_PASSWORD);
        let ntlmv2_hash = compute_ntlmv2_hash(&nt_hash, TEST_USER, TEST_DOMAIN);

        // Build the target info that matches the test vectors:
        // AV_PAIR: MsvAvNbDomainName(2) = "Domain"
        // AV_PAIR: MsvAvNbComputerName(1) = "Server"
        // AV_PAIR: MsvAvEOL(0)
        let target_info = build_test_target_info();
        let temp = build_temp(TEST_TIME, &TEST_CLIENT_CHALLENGE, &target_info);

        let mut mac = HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
        mac.update(&TEST_SERVER_CHALLENGE);
        mac.update(&temp);
        let nt_proof_str = mac.finalize().into_bytes().to_vec();

        assert_eq!(nt_proof_str, expected_nt_proof_str);
    }

    #[test]
    fn session_base_key_computation() {
        // From section 4.2.4.1.2: SessionBaseKey =
        // 8d e4 0c ca db c1 4a 82 f1 5c b0 ad 0d e9 5c a3
        let expected = [
            0x8d, 0xe4, 0x0c, 0xca, 0xdb, 0xc1, 0x4a, 0x82, 0xf1, 0x5c, 0xb0, 0xad, 0x0d, 0xe9,
            0x5c, 0xa3,
        ];

        let nt_hash = compute_nt_hash(TEST_PASSWORD);
        let ntlmv2_hash = compute_ntlmv2_hash(&nt_hash, TEST_USER, TEST_DOMAIN);

        let target_info = build_test_target_info();
        let temp = build_temp(TEST_TIME, &TEST_CLIENT_CHALLENGE, &target_info);

        // NTProofStr
        let mut mac = HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
        mac.update(&TEST_SERVER_CHALLENGE);
        mac.update(&temp);
        let nt_proof_str = mac.finalize().into_bytes().to_vec();

        // SessionBaseKey = HMAC_MD5(ntlmv2_hash, NTProofStr)
        let mut mac = HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
        mac.update(&nt_proof_str);
        let session_base_key = mac.finalize().into_bytes().to_vec();

        assert_eq!(session_base_key, expected);
    }

    // =======================================================================
    // RC4 / Encrypted Session Key tests
    // =======================================================================

    #[test]
    fn rc4_encrypted_session_key() {
        // From section 4.2.4.2.3: RC4(SessionBaseKey, RandomSessionKey) =
        // c5 da d2 54 4f c9 79 90 94 ce 1c e9 0b c9 d0 3e
        let expected = [
            0xc5, 0xda, 0xd2, 0x54, 0x4f, 0xc9, 0x79, 0x90, 0x94, 0xce, 0x1c, 0xe9, 0x0b, 0xc9,
            0xd0, 0x3e,
        ];

        let session_base_key = [
            0x8d, 0xe4, 0x0c, 0xca, 0xdb, 0xc1, 0x4a, 0x82, 0xf1, 0x5c, 0xb0, 0xad, 0x0d, 0xe9,
            0x5c, 0xa3,
        ];

        let result = rc4_encrypt(&session_base_key, &TEST_RANDOM_SESSION_KEY);
        assert_eq!(result, expected);
    }

    #[test]
    fn rc4_roundtrip() {
        let key = b"test key";
        let data = b"hello, world!";
        let encrypted = rc4_encrypt(key, data);
        let decrypted = rc4_encrypt(key, &encrypted);
        assert_eq!(decrypted, data);
    }

    // =======================================================================
    // AV_PAIR tests
    // =======================================================================

    #[test]
    fn parse_av_pairs_from_target_info() {
        let target_info = build_test_target_info();
        let pairs = parse_av_pairs(&target_info);

        assert_eq!(pairs.len(), 3); // NbDomainName, NbComputerName, EOL

        // First pair: MsvAvNbDomainName = "Domain"
        assert_eq!(pairs[0].0, MSV_AV_NB_DOMAIN_NAME);
        let domain = String::from_utf16(
            &pairs[0]
                .1
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(domain, "Domain");

        // Second pair: MsvAvNbComputerName = "Server"
        assert_eq!(pairs[1].0, MSV_AV_NB_COMPUTER_NAME);

        // Last pair: MsvAvEOL
        assert_eq!(pairs[2].0, MSV_AV_EOL);
    }

    #[test]
    fn find_av_pair_present() {
        let target_info = build_test_target_info();
        let domain = find_av_pair(&target_info, MSV_AV_NB_DOMAIN_NAME);
        assert!(domain.is_some());
    }

    #[test]
    fn find_av_pair_absent() {
        let target_info = build_test_target_info();
        let timestamp = find_av_pair(&target_info, MSV_AV_TIMESTAMP);
        assert!(timestamp.is_none());
    }

    #[test]
    fn detect_timestamp_in_target_info() {
        // Build a target info with MsvAvTimestamp present
        let mut target_info = Vec::new();
        // MsvAvNbDomainName = "Domain"
        let domain_bytes = encode_utf16le("Domain");
        target_info.extend_from_slice(&MSV_AV_NB_DOMAIN_NAME.to_le_bytes());
        target_info.extend_from_slice(&(domain_bytes.len() as u16).to_le_bytes());
        target_info.extend_from_slice(&domain_bytes);
        // MsvAvTimestamp
        target_info.extend_from_slice(&MSV_AV_TIMESTAMP.to_le_bytes());
        target_info.extend_from_slice(&8u16.to_le_bytes());
        target_info.extend_from_slice(&0u64.to_le_bytes());
        // MsvAvEOL
        target_info.extend_from_slice(&MSV_AV_EOL.to_le_bytes());
        target_info.extend_from_slice(&0u16.to_le_bytes());

        assert!(find_av_pair(&target_info, MSV_AV_TIMESTAMP).is_some());
    }

    // =======================================================================
    // NEGOTIATE_MESSAGE tests
    // =======================================================================

    #[test]
    fn negotiate_message_has_correct_signature() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        let msg = auth.negotiate();

        // Signature
        assert_eq!(&msg[0..8], NTLM_SIGNATURE);
    }

    #[test]
    fn negotiate_message_has_correct_type() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        let msg = auth.negotiate();

        let msg_type = u32::from_le_bytes(msg[8..12].try_into().unwrap());
        assert_eq!(msg_type, MSG_TYPE_NEGOTIATE);
    }

    #[test]
    fn negotiate_message_has_expected_flags() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        let msg = auth.negotiate();

        let flags = u32::from_le_bytes(msg[12..16].try_into().unwrap());
        // Check that key flags are set
        assert_ne!(flags & NTLMSSP_NEGOTIATE_UNICODE, 0);
        assert_ne!(flags & NTLMSSP_NEGOTIATE_NTLM, 0);
        assert_ne!(flags & NTLMSSP_NEGOTIATE_KEY_EXCH, 0);
        assert_ne!(flags & NTLMSSP_NEGOTIATE_128, 0);
    }

    #[test]
    fn negotiate_message_minimum_size() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: String::new(),
            password: String::new(),
            domain: String::new(),
        });
        let msg = auth.negotiate();

        // Minimum: signature(8) + type(4) + flags(4) + domain fields(8) + workstation fields(8)
        assert_eq!(msg.len(), 32);
    }

    // =======================================================================
    // CHALLENGE_MESSAGE parsing tests
    // =======================================================================

    #[test]
    fn parse_challenge_message_from_spec() {
        // Challenge message from section 4.2.4.3
        let challenge_bytes = build_test_challenge_message();
        let challenge = parse_challenge_message(&challenge_bytes).unwrap();

        assert_eq!(challenge.server_challenge, TEST_SERVER_CHALLENGE);
        assert!(!challenge.target_info.is_empty());
    }

    #[test]
    fn parse_challenge_message_rejects_wrong_signature() {
        let mut bad = build_test_challenge_message();
        bad[0] = 0x00; // Corrupt signature
        assert!(parse_challenge_message(&bad).is_err());
    }

    #[test]
    fn parse_challenge_message_rejects_wrong_type() {
        let mut bad = build_test_challenge_message();
        // Change message type from 2 to 1
        bad[8] = 0x01;
        assert!(parse_challenge_message(&bad).is_err());
    }

    #[test]
    fn parse_challenge_message_rejects_too_short() {
        assert!(parse_challenge_message(&[0u8; 16]).is_err());
    }

    // =======================================================================
    // Full flow tests
    // =======================================================================

    #[test]
    fn full_negotiate_authenticate_flow_no_timestamp() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        // Step 1: Negotiate
        let _negotiate = auth.negotiate();

        // Step 2: Build a challenge message (no timestamp = no MIC)
        let challenge_bytes = build_test_challenge_message();

        // Step 3: Authenticate
        let authenticate = auth.authenticate(&challenge_bytes).unwrap();

        // Verify the authenticate message
        assert_eq!(&authenticate[0..8], NTLM_SIGNATURE);
        let msg_type = u32::from_le_bytes(authenticate[8..12].try_into().unwrap());
        assert_eq!(msg_type, MSG_TYPE_AUTHENTICATE);

        // Session key should be available
        assert!(auth.session_key().is_some());
        assert_eq!(auth.session_key().unwrap().len(), 16);
    }

    #[test]
    fn full_flow_with_timestamp_includes_mic() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        let _negotiate = auth.negotiate();

        // Challenge with MsvAvTimestamp
        let challenge_bytes = build_test_challenge_message_with_timestamp();

        let authenticate = auth.authenticate(&challenge_bytes).unwrap();

        // Verify signature and type
        assert_eq!(&authenticate[0..8], NTLM_SIGNATURE);
        let msg_type = u32::from_le_bytes(authenticate[8..12].try_into().unwrap());
        assert_eq!(msg_type, MSG_TYPE_AUTHENTICATE);

        // MIC field at offset 72 should NOT be all zeros (it was patched)
        let mic = &authenticate[72..88];
        assert_ne!(
            mic, &[0u8; 16],
            "MIC should be non-zero when timestamp is present"
        );

        // Session key should be available
        assert!(auth.session_key().is_some());
    }

    #[test]
    fn session_key_not_available_before_authenticate() {
        let auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        assert!(auth.session_key().is_none());
    }

    #[test]
    fn authenticate_without_negotiate_and_timestamp_still_works() {
        // If there's no timestamp, MIC isn't required, so negotiate_bytes not needed
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        // Skip negotiate, go straight to authenticate with no-timestamp challenge
        let challenge_bytes = build_test_challenge_message();
        let result = auth.authenticate(&challenge_bytes);
        assert!(result.is_ok());
    }

    #[test]
    fn authenticate_with_timestamp_requires_negotiate() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        // Skip negotiate, try authenticate with timestamp challenge
        let challenge_bytes = build_test_challenge_message_with_timestamp();
        let result = auth.authenticate(&challenge_bytes);
        // Should fail because negotiate_bytes is needed for MIC
        assert!(result.is_err());
    }

    // =======================================================================
    // Edge case tests
    // =======================================================================

    #[test]
    fn empty_domain() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: String::new(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        let _negotiate = auth.negotiate();
        let challenge_bytes = build_test_challenge_message();
        let result = auth.authenticate(&challenge_bytes);
        assert!(result.is_ok());
    }

    #[test]
    fn unicode_username_with_special_characters() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: "Us\u{00e9}r".to_string(), // "User" with e-acute
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        let _negotiate = auth.negotiate();
        let challenge_bytes = build_test_challenge_message();
        let result = auth.authenticate(&challenge_bytes);
        assert!(result.is_ok());
    }

    #[test]
    fn build_auth_target_info_adds_flags_when_timestamp_present() {
        // Target info with timestamp
        let mut target_info = Vec::new();
        let domain_bytes = encode_utf16le("Domain");
        target_info.extend_from_slice(&MSV_AV_NB_DOMAIN_NAME.to_le_bytes());
        target_info.extend_from_slice(&(domain_bytes.len() as u16).to_le_bytes());
        target_info.extend_from_slice(&domain_bytes);
        target_info.extend_from_slice(&MSV_AV_TIMESTAMP.to_le_bytes());
        target_info.extend_from_slice(&8u16.to_le_bytes());
        target_info.extend_from_slice(&0u64.to_le_bytes());
        target_info.extend_from_slice(&MSV_AV_EOL.to_le_bytes());
        target_info.extend_from_slice(&0u16.to_le_bytes());

        let auth_info = build_auth_target_info(&target_info, true);
        let pairs = parse_av_pairs(&auth_info);

        // Should contain MsvAvFlags with bit 0x2 set
        let flags_pair = pairs.iter().find(|(id, _)| *id == MSV_AV_FLAGS);
        assert!(flags_pair.is_some(), "MsvAvFlags should be present");
        let flags_value = u32::from_le_bytes(flags_pair.unwrap().1[..4].try_into().unwrap());
        assert_ne!(flags_value & 0x2, 0, "MIC bit should be set in MsvAvFlags");
    }

    #[test]
    fn build_auth_target_info_no_flags_when_no_timestamp() {
        let target_info = build_test_target_info();
        let auth_info = build_auth_target_info(&target_info, false);
        let pairs = parse_av_pairs(&auth_info);

        // Should NOT contain MsvAvFlags
        let flags_pair = pairs.iter().find(|(id, _)| *id == MSV_AV_FLAGS);
        assert!(flags_pair.is_none());
    }

    #[test]
    fn lm_challenge_response_is_zeroed_when_timestamp_present() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        let _negotiate = auth.negotiate();
        let challenge_bytes = build_test_challenge_message_with_timestamp();
        let authenticate = auth.authenticate(&challenge_bytes).unwrap();

        // Parse LmChallengeResponseFields from the authenticate message
        let lm_len = u16::from_le_bytes(authenticate[12..14].try_into().unwrap()) as usize;
        let lm_offset = u32::from_le_bytes(authenticate[16..20].try_into().unwrap()) as usize;

        // LM response should be 24 bytes of zeros
        assert_eq!(lm_len, 24);
        let lm_data = &authenticate[lm_offset..lm_offset + lm_len];
        assert_eq!(lm_data, &[0u8; 24]);
    }

    #[test]
    fn authenticate_message_contains_correct_domain_and_user() {
        let mut auth = NtlmAuthenticator::new(NtlmCredentials {
            username: TEST_USER.to_string(),
            password: TEST_PASSWORD.to_string(),
            domain: TEST_DOMAIN.to_string(),
        });
        auth.test_client_challenge = Some(TEST_CLIENT_CHALLENGE);
        auth.test_random_session_key = Some(TEST_RANDOM_SESSION_KEY);
        auth.test_timestamp = Some(TEST_TIME);

        let _negotiate = auth.negotiate();
        let challenge_bytes = build_test_challenge_message();
        let authenticate = auth.authenticate(&challenge_bytes).unwrap();

        // DomainNameFields at offset 28
        let domain_len = u16::from_le_bytes(authenticate[28..30].try_into().unwrap()) as usize;
        let domain_offset = u32::from_le_bytes(authenticate[32..36].try_into().unwrap()) as usize;
        let domain_bytes = &authenticate[domain_offset..domain_offset + domain_len];
        let domain = String::from_utf16(
            &domain_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(domain, TEST_DOMAIN);

        // UserNameFields at offset 36
        let user_len = u16::from_le_bytes(authenticate[36..38].try_into().unwrap()) as usize;
        let user_offset = u32::from_le_bytes(authenticate[40..44].try_into().unwrap()) as usize;
        let user_bytes = &authenticate[user_offset..user_offset + user_len];
        let user = String::from_utf16(
            &user_bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_le_bytes(pair))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(user, TEST_USER);
    }

    // =======================================================================
    // Known-answer test: full NTLMv2 from spec section 4.2.4
    // =======================================================================

    #[test]
    fn ntlmv2_full_known_answer_lm_response() {
        // From section 4.2.4.2.1: LMv2 Response
        let expected = [
            0x86, 0xc3, 0x50, 0x97, 0xac, 0x9c, 0xec, 0x10, 0x25, 0x54, 0x76, 0x4a, 0x57, 0xcc,
            0xcc, 0x19, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa, 0xaa,
        ];

        let nt_hash = compute_nt_hash(TEST_PASSWORD);
        let ntlmv2_hash = compute_ntlmv2_hash(&nt_hash, TEST_USER, TEST_DOMAIN);

        // LMv2: HMAC_MD5(ntlmv2_hash, server_challenge + client_challenge) + client_challenge
        let mut mac = HmacMd5::new_from_slice(&ntlmv2_hash).expect("HMAC accepts any key length");
        mac.update(&TEST_SERVER_CHALLENGE);
        mac.update(&TEST_CLIENT_CHALLENGE);
        let proof = mac.finalize().into_bytes();
        let mut resp = proof.to_vec();
        resp.extend_from_slice(&TEST_CLIENT_CHALLENGE);

        assert_eq!(resp, expected);
    }

    // =======================================================================
    // Test helpers
    // =======================================================================

    /// Build a test target info matching the NTLMv2 test vectors from section 4.2.4.
    /// Contains: MsvAvNbDomainName("Domain"), MsvAvNbComputerName("Server"), MsvAvEOL.
    fn build_test_target_info() -> Vec<u8> {
        let mut info = Vec::new();

        // MsvAvNbDomainName = "Domain"
        let domain_bytes = encode_utf16le("Domain");
        info.extend_from_slice(&MSV_AV_NB_DOMAIN_NAME.to_le_bytes());
        info.extend_from_slice(&(domain_bytes.len() as u16).to_le_bytes());
        info.extend_from_slice(&domain_bytes);

        // MsvAvNbComputerName = "Server"
        let server_bytes = encode_utf16le("Server");
        info.extend_from_slice(&MSV_AV_NB_COMPUTER_NAME.to_le_bytes());
        info.extend_from_slice(&(server_bytes.len() as u16).to_le_bytes());
        info.extend_from_slice(&server_bytes);

        // MsvAvEOL
        info.extend_from_slice(&MSV_AV_EOL.to_le_bytes());
        info.extend_from_slice(&0u16.to_le_bytes());

        info
    }

    /// Build a CHALLENGE_MESSAGE matching the NTLMv2 test vectors (no MsvAvTimestamp).
    fn build_test_challenge_message() -> Vec<u8> {
        let target_info = build_test_target_info();
        let target_name = encode_utf16le("Server");

        // NTLMv2 challenge flags from section 4.2.4
        let flags: u32 = 0xe28a8233;

        build_challenge_message_bytes(flags, &target_name, &target_info)
    }

    /// Build a CHALLENGE_MESSAGE with MsvAvTimestamp present (triggers MIC).
    fn build_test_challenge_message_with_timestamp() -> Vec<u8> {
        let mut target_info = Vec::new();

        // MsvAvNbDomainName = "Domain"
        let domain_bytes = encode_utf16le("Domain");
        target_info.extend_from_slice(&MSV_AV_NB_DOMAIN_NAME.to_le_bytes());
        target_info.extend_from_slice(&(domain_bytes.len() as u16).to_le_bytes());
        target_info.extend_from_slice(&domain_bytes);

        // MsvAvNbComputerName = "Server"
        let server_bytes = encode_utf16le("Server");
        target_info.extend_from_slice(&MSV_AV_NB_COMPUTER_NAME.to_le_bytes());
        target_info.extend_from_slice(&(server_bytes.len() as u16).to_le_bytes());
        target_info.extend_from_slice(&server_bytes);

        // MsvAvTimestamp
        target_info.extend_from_slice(&MSV_AV_TIMESTAMP.to_le_bytes());
        target_info.extend_from_slice(&8u16.to_le_bytes());
        target_info.extend_from_slice(&0u64.to_le_bytes()); // timestamp = 0

        // MsvAvEOL
        target_info.extend_from_slice(&MSV_AV_EOL.to_le_bytes());
        target_info.extend_from_slice(&0u16.to_le_bytes());

        let target_name = encode_utf16le("Server");
        let flags: u32 = 0xe28a8233;

        build_challenge_message_bytes(flags, &target_name, &target_info)
    }

    /// Helper to construct a raw CHALLENGE_MESSAGE.
    fn build_challenge_message_bytes(
        flags: u32,
        target_name: &[u8],
        target_info: &[u8],
    ) -> Vec<u8> {
        // Fixed header: 56 bytes (up to and including version)
        // Payload starts at offset 56 (no VERSION in our simplified messages)
        // Actually, the challenge message layout:
        // Signature(8) + Type(4) + TargetNameFields(8) + Flags(4) + ServerChallenge(8)
        // + Reserved(8) + TargetInfoFields(8) + Version(8)
        // = 56 bytes header
        let header_size = 56;
        let target_name_offset = header_size;
        let target_info_offset = target_name_offset + target_name.len();

        let mut buf = Vec::with_capacity(target_info_offset + target_info.len());

        // Signature
        buf.extend_from_slice(NTLM_SIGNATURE);
        // MessageType
        buf.extend_from_slice(&MSG_TYPE_CHALLENGE.to_le_bytes());
        // TargetNameFields
        buf.extend_from_slice(&(target_name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_name_offset as u32).to_le_bytes());
        // NegotiateFlags
        buf.extend_from_slice(&flags.to_le_bytes());
        // ServerChallenge
        buf.extend_from_slice(&TEST_SERVER_CHALLENGE);
        // Reserved
        buf.extend_from_slice(&[0u8; 8]);
        // TargetInfoFields
        buf.extend_from_slice(&(target_info.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_info.len() as u16).to_le_bytes());
        buf.extend_from_slice(&(target_info_offset as u32).to_le_bytes());
        // Version (8 bytes)
        buf.extend_from_slice(&[0x06, 0x00, 0x70, 0x17, 0x00, 0x00, 0x00, 0x0f]);

        // Payload
        buf.extend_from_slice(target_name);
        buf.extend_from_slice(target_info);

        buf
    }
}
