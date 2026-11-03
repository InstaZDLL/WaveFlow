//! SPNEGO (Simple and Protected GSS-API Negotiation Mechanism) token wrapping.
//!
//! Implements the thin ASN.1/DER wrapper that SMB2 requires around authentication
//! tokens (NTLM, Kerberos). The client sends a NegTokenInit with supported
//! mechanism OIDs and the first mechanism's token, the server responds with
//! NegTokenResp indicating the selected mechanism and its response token, and
//! subsequent client messages use NegTokenResp as well.
//!
//! References:
//! - RFC 4178 (SPNEGO)
//! - MS-SPNG (Microsoft SPNEGO Extension)

use super::der::{der_tlv, parse_der_tlv};
use crate::Error;

// ---------------------------------------------------------------------------
// OID constants (DER-encoded, including tag and length bytes)
// ---------------------------------------------------------------------------

/// SPNEGO OID: 1.3.6.1.5.5.2
pub const OID_SPNEGO: &[u8] = &[0x06, 0x06, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];

/// NTLM (NTLMSSP) OID: 1.3.6.1.4.1.311.2.2.10
pub const OID_NTLMSSP: &[u8] = &[
    0x06, 0x0a, 0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a,
];

/// Kerberos OID: 1.2.840.113554.1.2.2 (standard, RFC 4121)
pub const OID_KERBEROS: &[u8] = &[
    0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x12, 0x01, 0x02, 0x02,
];

/// Microsoft Kerberos OID: 1.2.840.48018.1.2.2 (MS-KILE, used by Windows SPNEGO)
///
/// Windows expects this OID as the primary mechanism in SPNEGO NegTokenInit.
/// Using the standard Kerberos OID causes Windows to reject the AP-REQ.
pub const OID_MS_KERBEROS: &[u8] = &[
    0x06, 0x09, 0x2a, 0x86, 0x48, 0x82, 0xf7, 0x12, 0x01, 0x02, 0x02,
];

// ---------------------------------------------------------------------------
// ASN.1 DER tag constants
// ---------------------------------------------------------------------------

/// SEQUENCE tag (constructed).
const TAG_SEQUENCE: u8 = 0x30;
/// OCTET STRING tag.
const TAG_OCTET_STRING: u8 = 0x04;
/// ENUMERATED tag.
const TAG_ENUMERATED: u8 = 0x0a;
/// APPLICATION [0] (constructed) -- wraps the initial NegotiationToken.
const TAG_APPLICATION_0: u8 = 0x60;
/// Context-specific [0] (constructed).
const TAG_CONTEXT_0: u8 = 0xa0;
/// Context-specific [1] (constructed).
const TAG_CONTEXT_1: u8 = 0xa1;
/// Context-specific [2] (constructed).
const TAG_CONTEXT_2: u8 = 0xa2;
/// Context-specific [3] (constructed).
const TAG_CONTEXT_3: u8 = 0xa3;

// ---------------------------------------------------------------------------
// NegState enum
// ---------------------------------------------------------------------------

/// SPNEGO negotiation state from NegTokenResp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegState {
    /// Authentication completed successfully.
    AcceptCompleted,
    /// Authentication is in progress (more tokens needed).
    AcceptIncomplete,
    /// Authentication was rejected.
    Reject,
}

impl NegState {
    /// Parse from the DER enumerated value.
    fn from_value(v: u8) -> Option<NegState> {
        match v {
            0 => Some(NegState::AcceptCompleted),
            1 => Some(NegState::AcceptIncomplete),
            2 => Some(NegState::Reject),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// NegTokenResp struct
// ---------------------------------------------------------------------------

/// Parsed SPNEGO NegTokenResp from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegTokenResp {
    /// The negotiation state.
    pub neg_state: Option<NegState>,
    /// The selected mechanism OID (raw DER-encoded OID TLV).
    pub supported_mech: Option<Vec<u8>>,
    /// The mechanism-specific response token.
    pub response_token: Option<Vec<u8>>,
    /// The mechanism list MIC.
    pub mech_list_mic: Option<Vec<u8>>,
}

// DER encoding/decoding helpers are in `super::der`. Imported at the top.

// ---------------------------------------------------------------------------
// Public API: wrapping
// ---------------------------------------------------------------------------

/// Wrap a mechanism token in a SPNEGO NegTokenInit.
///
/// The initial token sent by the client. Wraps the raw NTLM or Kerberos
/// token with mechanism OID negotiation.
///
/// Structure (RFC 4178 section 4.2):
/// ```text
/// APPLICATION [0] {
///   OID_SPNEGO,
///   [0] {  -- NegTokenInit choice tag
///     SEQUENCE {
///       [0] { SEQUENCE { mechOID1, mechOID2, ... } },  -- mechTypes
///       [2] { OCTET STRING { mechToken } }             -- mechToken
///     }
///   }
/// }
/// ```
pub fn wrap_neg_token_init(mech_oids: &[&[u8]], mech_token: &[u8]) -> Vec<u8> {
    // Build mechTypes: SEQUENCE OF OID
    let mut mech_list_contents = Vec::new();
    for oid in mech_oids {
        mech_list_contents.extend_from_slice(oid);
    }
    let mech_list_seq = der_tlv(TAG_SEQUENCE, &mech_list_contents);
    let mech_types = der_tlv(TAG_CONTEXT_0, &mech_list_seq);

    // Build mechToken: [2] OCTET STRING
    let mech_token_octet = der_tlv(TAG_OCTET_STRING, mech_token);
    let mech_token_ctx = der_tlv(TAG_CONTEXT_2, &mech_token_octet);

    // NegTokenInit SEQUENCE
    let mut init_contents = Vec::new();
    init_contents.extend_from_slice(&mech_types);
    init_contents.extend_from_slice(&mech_token_ctx);
    let init_seq = der_tlv(TAG_SEQUENCE, &init_contents);

    // Wrap in context [0] (NegotiationToken CHOICE for negTokenInit)
    let choice = der_tlv(TAG_CONTEXT_0, &init_seq);

    // Wrap in APPLICATION [0] with SPNEGO OID
    let mut app_contents = Vec::new();
    app_contents.extend_from_slice(OID_SPNEGO);
    app_contents.extend_from_slice(&choice);
    der_tlv(TAG_APPLICATION_0, &app_contents)
}

/// Wrap a mechanism token in a SPNEGO NegTokenResp.
///
/// Used by the client in the second round-trip (for example, the NTLM
/// AUTHENTICATE_MESSAGE). Only the responseToken field is set.
///
/// Structure:
/// ```text
/// [1] {  -- NegotiationToken CHOICE for negTokenResp
///   SEQUENCE {
///     [2] { OCTET STRING { mechToken } }  -- responseToken
///   }
/// }
/// ```
pub fn wrap_neg_token_resp(mech_token: &[u8]) -> Vec<u8> {
    // Build responseToken: [2] OCTET STRING
    let mech_token_octet = der_tlv(TAG_OCTET_STRING, mech_token);
    let response_token_ctx = der_tlv(TAG_CONTEXT_2, &mech_token_octet);

    // NegTokenResp SEQUENCE
    let resp_seq = der_tlv(TAG_SEQUENCE, &response_token_ctx);

    // Wrap in context [1] (NegotiationToken CHOICE for negTokenResp)
    der_tlv(TAG_CONTEXT_1, &resp_seq)
}

// ---------------------------------------------------------------------------
// Public API: parsing
// ---------------------------------------------------------------------------

/// Parse a SPNEGO NegTokenResp from the server.
///
/// The input can be either:
/// - A bare `[1] { SEQUENCE { ... } }` NegTokenResp
/// - An `APPLICATION [0] { OID, [0] { ... } }` wrapping a NegTokenInit2
///   (server-initiated SPNEGO, which we parse the inner token from)
///
/// Extracts the negotiation state, selected mechanism, and response token.
pub fn parse_neg_token_resp(data: &[u8]) -> Result<NegTokenResp, Error> {
    if data.is_empty() {
        return Err(Error::invalid_data("SPNEGO: empty token"));
    }

    // Check if this is an APPLICATION [0] wrapper (server-initiated NegTokenInit2)
    // or a NegTokenResp [1] wrapper.
    let (tag, value, _) = parse_der_tlv(data)?;

    match tag {
        TAG_CONTEXT_1 => {
            // Standard NegTokenResp: [1] { SEQUENCE { ... } }
            parse_neg_token_resp_inner(value)
        }
        TAG_APPLICATION_0 => {
            // APPLICATION [0] { OID_SPNEGO, [0] { NegTokenInit2 } }
            // or could contain a [1] { NegTokenResp }
            // Skip the SPNEGO OID
            let (oid_tag, _, oid_total) = parse_der_tlv(value)?;
            if oid_tag != 0x06 {
                return Err(Error::invalid_data(format!(
                    "SPNEGO: expected OID in APPLICATION [0], got tag 0x{oid_tag:02x}"
                )));
            }
            let remaining = &value[oid_total..];
            let (inner_tag, inner_value, _) = parse_der_tlv(remaining)?;
            match inner_tag {
                TAG_CONTEXT_0 => {
                    // NegTokenInit2 wrapped in [0]: parse as NegTokenInit2
                    // to extract mechTypes (as supportedMech) and mechToken
                    parse_neg_token_init2_as_resp(inner_value)
                }
                TAG_CONTEXT_1 => {
                    // NegTokenResp wrapped inside APPLICATION [0]
                    parse_neg_token_resp_inner(inner_value)
                }
                _ => Err(Error::invalid_data(format!(
                    "SPNEGO: unexpected tag 0x{inner_tag:02x} inside APPLICATION [0]"
                ))),
            }
        }
        _ => Err(Error::invalid_data(format!(
            "SPNEGO: expected NegTokenResp [1] or APPLICATION [0], got tag 0x{tag:02x}"
        ))),
    }
}

/// Parse the inner SEQUENCE of a NegTokenResp.
fn parse_neg_token_resp_inner(data: &[u8]) -> Result<NegTokenResp, Error> {
    // Expect SEQUENCE
    let (tag, seq_data, _) = parse_der_tlv(data)?;
    if tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "SPNEGO: expected SEQUENCE in NegTokenResp, got tag 0x{tag:02x}"
        )));
    }

    let mut neg_state = None;
    let mut supported_mech = None;
    let mut response_token = None;
    let mut mech_list_mic = None;

    let mut pos = 0;
    while pos < seq_data.len() {
        let (ctx_tag, ctx_value, ctx_total) = parse_der_tlv(&seq_data[pos..])?;
        match ctx_tag {
            TAG_CONTEXT_0 => {
                // negState: ENUMERATED
                let (enum_tag, enum_value, _) = parse_der_tlv(ctx_value)?;
                if enum_tag != TAG_ENUMERATED {
                    return Err(Error::invalid_data(format!(
                        "SPNEGO: expected ENUMERATED for negState, got tag 0x{enum_tag:02x}"
                    )));
                }
                if enum_value.is_empty() {
                    return Err(Error::invalid_data("SPNEGO: empty ENUMERATED for negState"));
                }
                neg_state = NegState::from_value(enum_value[0]);
                if neg_state.is_none() {
                    return Err(Error::invalid_data(format!(
                        "SPNEGO: unknown negState value: {}",
                        enum_value[0]
                    )));
                }
            }
            TAG_CONTEXT_1 => {
                // supportedMech: OID (the full TLV)
                supported_mech = Some(ctx_value.to_vec());
            }
            TAG_CONTEXT_2 => {
                // responseToken: OCTET STRING
                let (oct_tag, oct_value, _) = parse_der_tlv(ctx_value)?;
                if oct_tag != TAG_OCTET_STRING {
                    return Err(Error::invalid_data(format!(
                        "SPNEGO: expected OCTET STRING for responseToken, got tag 0x{oct_tag:02x}"
                    )));
                }
                response_token = Some(oct_value.to_vec());
            }
            TAG_CONTEXT_3 => {
                // mechListMIC: OCTET STRING
                let (oct_tag, oct_value, _) = parse_der_tlv(ctx_value)?;
                if oct_tag != TAG_OCTET_STRING {
                    return Err(Error::invalid_data(format!(
                        "SPNEGO: expected OCTET STRING for mechListMIC, got tag 0x{oct_tag:02x}"
                    )));
                }
                mech_list_mic = Some(oct_value.to_vec());
            }
            _ => {
                // Unknown context tag, skip it (forward compatibility).
            }
        }
        pos += ctx_total;
    }

    Ok(NegTokenResp {
        neg_state,
        supported_mech,
        response_token,
        mech_list_mic,
    })
}

/// Parse a NegTokenInit2 (server-initiated) and return it as a NegTokenResp.
///
/// NegTokenInit2 has mechTypes at [0] and mechToken at [2]. We map the
/// first mechType to supportedMech and mechToken to responseToken.
fn parse_neg_token_init2_as_resp(data: &[u8]) -> Result<NegTokenResp, Error> {
    let (tag, seq_data, _) = parse_der_tlv(data)?;
    if tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "SPNEGO: expected SEQUENCE in NegTokenInit2, got tag 0x{tag:02x}"
        )));
    }

    let mut supported_mech = None;
    let mut response_token = None;

    let mut pos = 0;
    while pos < seq_data.len() {
        let (ctx_tag, ctx_value, ctx_total) = parse_der_tlv(&seq_data[pos..])?;
        match ctx_tag {
            TAG_CONTEXT_0 => {
                // mechTypes: SEQUENCE OF OID -- take the first one
                let (seq_tag, mech_list_data, _) = parse_der_tlv(ctx_value)?;
                if seq_tag != TAG_SEQUENCE {
                    return Err(Error::invalid_data(
                        "SPNEGO: expected SEQUENCE for mechTypes",
                    ));
                }
                if !mech_list_data.is_empty() {
                    // Take the first OID TLV as the supported mech
                    let (oid_tag, _, oid_total) = parse_der_tlv(mech_list_data)?;
                    if oid_tag == 0x06 {
                        supported_mech = Some(mech_list_data[..oid_total].to_vec());
                    }
                }
            }
            TAG_CONTEXT_2 => {
                // mechToken: OCTET STRING
                let (oct_tag, oct_value, _) = parse_der_tlv(ctx_value)?;
                if oct_tag == TAG_OCTET_STRING {
                    response_token = Some(oct_value.to_vec());
                }
            }
            _ => {
                // Skip reqFlags [1], negHints [3], mechListMIC [4]
            }
        }
        pos += ctx_total;
    }

    Ok(NegTokenResp {
        neg_state: None,
        supported_mech,
        response_token,
        mech_list_mic: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // DER primitive tests (der_length, der_tlv, parse_der_length, parse_der_tlv)
    // live in `auth::der::tests`.

    // =======================================================================
    // NegTokenInit wrapping tests
    // =======================================================================

    #[test]
    fn neg_token_init_starts_with_application_tag() {
        let token = wrap_neg_token_init(&[OID_NTLMSSP], b"NTLMSSP\0test");
        assert_eq!(
            token[0], TAG_APPLICATION_0,
            "must start with APPLICATION [0]"
        );
    }

    #[test]
    fn neg_token_init_contains_spnego_oid() {
        let token = wrap_neg_token_init(&[OID_NTLMSSP], b"NTLMSSP\0test");
        // The SPNEGO OID value bytes (without the 0x06 tag and 0x06 length)
        let oid_value = &OID_SPNEGO[2..]; // skip tag+length
        assert!(
            token.windows(oid_value.len()).any(|w| w == oid_value),
            "token must contain SPNEGO OID"
        );
    }

    #[test]
    fn neg_token_init_contains_mech_oid() {
        let token = wrap_neg_token_init(&[OID_NTLMSSP], b"test");
        // The NTLMSSP OID value bytes (without the 0x06 tag)
        let oid_value = &OID_NTLMSSP[2..]; // skip tag+length
        assert!(
            token.windows(oid_value.len()).any(|w| w == oid_value),
            "token must contain NTLMSSP OID"
        );
    }

    #[test]
    fn neg_token_init_contains_mech_token() {
        let mech_token = b"NTLMSSP\0negotiate_payload_here";
        let token = wrap_neg_token_init(&[OID_NTLMSSP], mech_token);
        assert!(
            token.windows(mech_token.len()).any(|w| w == mech_token),
            "token must contain the raw mech token"
        );
    }

    #[test]
    fn neg_token_init_multiple_mechs() {
        let token = wrap_neg_token_init(&[OID_NTLMSSP, OID_KERBEROS], b"tok");
        // Both OIDs should be present
        let ntlm_oid_value = &OID_NTLMSSP[2..];
        let kerb_oid_value = &OID_KERBEROS[2..];
        assert!(
            token
                .windows(ntlm_oid_value.len())
                .any(|w| w == ntlm_oid_value),
            "must contain NTLMSSP OID"
        );
        assert!(
            token
                .windows(kerb_oid_value.len())
                .any(|w| w == kerb_oid_value),
            "must contain Kerberos OID"
        );
    }

    #[test]
    fn neg_token_init_structure_is_valid_der() {
        let token = wrap_neg_token_init(&[OID_NTLMSSP], b"test_token");
        // Parse the outer APPLICATION [0]
        let (tag, value, total) = parse_der_tlv(&token).unwrap();
        assert_eq!(tag, TAG_APPLICATION_0);
        assert_eq!(total, token.len(), "entire token should be consumed");

        // Inside: OID_SPNEGO followed by [0] { SEQUENCE { ... } }
        let (oid_tag, _, oid_total) = parse_der_tlv(value).unwrap();
        assert_eq!(oid_tag, 0x06, "first element should be OID");

        let (choice_tag, _, _) = parse_der_tlv(&value[oid_total..]).unwrap();
        assert_eq!(choice_tag, TAG_CONTEXT_0, "second element should be [0]");
    }

    #[test]
    fn neg_token_init_parseable_structure() {
        // Wrap a token and verify we can walk the entire structure
        let mech_token = b"the_raw_ntlm_token";
        let token = wrap_neg_token_init(&[OID_NTLMSSP], mech_token);

        // APPLICATION [0]
        let (_, app_value, _) = parse_der_tlv(&token).unwrap();
        // Skip SPNEGO OID
        let (_, _, oid_total) = parse_der_tlv(app_value).unwrap();
        // [0] CHOICE
        let (_, choice_value, _) = parse_der_tlv(&app_value[oid_total..]).unwrap();
        // SEQUENCE
        let (_, seq_value, _) = parse_der_tlv(choice_value).unwrap();
        // [0] mechTypes
        let (tag0, ctx0_value, ctx0_total) = parse_der_tlv(seq_value).unwrap();
        assert_eq!(tag0, TAG_CONTEXT_0);
        // SEQUENCE OF OID inside mechTypes
        let (_, mech_list, _) = parse_der_tlv(ctx0_value).unwrap();
        // First OID should be NTLMSSP
        assert_eq!(&mech_list[..OID_NTLMSSP.len()], OID_NTLMSSP);

        // [2] mechToken
        let (tag2, ctx2_value, _) = parse_der_tlv(&seq_value[ctx0_total..]).unwrap();
        assert_eq!(tag2, TAG_CONTEXT_2);
        // OCTET STRING
        let (_, oct_value, _) = parse_der_tlv(ctx2_value).unwrap();
        assert_eq!(oct_value, mech_token);
    }

    // =======================================================================
    // NegTokenResp wrapping tests
    // =======================================================================

    #[test]
    fn neg_token_resp_wrap_starts_with_context_1() {
        let token = wrap_neg_token_resp(b"auth_token");
        assert_eq!(token[0], TAG_CONTEXT_1, "must start with [1]");
    }

    #[test]
    fn neg_token_resp_wrap_contains_mech_token() {
        let mech_token = b"NTLMSSP\0authenticate_payload";
        let token = wrap_neg_token_resp(mech_token);
        assert!(
            token.windows(mech_token.len()).any(|w| w == mech_token),
            "wrapped token must contain the raw mech token"
        );
    }

    #[test]
    fn neg_token_resp_wrap_valid_structure() {
        let mech_token = b"authenticate_me";
        let token = wrap_neg_token_resp(mech_token);

        // [1]
        let (tag, ctx1_value, _) = parse_der_tlv(&token).unwrap();
        assert_eq!(tag, TAG_CONTEXT_1);
        // SEQUENCE
        let (tag, seq_value, _) = parse_der_tlv(ctx1_value).unwrap();
        assert_eq!(tag, TAG_SEQUENCE);
        // [2] responseToken
        let (tag, ctx2_value, _) = parse_der_tlv(seq_value).unwrap();
        assert_eq!(tag, TAG_CONTEXT_2);
        // OCTET STRING
        let (tag, oct_value, _) = parse_der_tlv(ctx2_value).unwrap();
        assert_eq!(tag, TAG_OCTET_STRING);
        assert_eq!(oct_value, mech_token);
    }

    // =======================================================================
    // NegTokenResp parsing tests
    // =======================================================================

    /// Build a NegTokenResp with known fields for testing.
    fn build_test_neg_token_resp(
        neg_state: Option<u8>,
        supported_mech: Option<&[u8]>,
        response_token: Option<&[u8]>,
        mech_list_mic: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut seq_contents = Vec::new();

        if let Some(state) = neg_state {
            let enumerated = der_tlv(TAG_ENUMERATED, &[state]);
            seq_contents.extend_from_slice(&der_tlv(TAG_CONTEXT_0, &enumerated));
        }

        if let Some(oid) = supported_mech {
            seq_contents.extend_from_slice(&der_tlv(TAG_CONTEXT_1, oid));
        }

        if let Some(tok) = response_token {
            let octet = der_tlv(TAG_OCTET_STRING, tok);
            seq_contents.extend_from_slice(&der_tlv(TAG_CONTEXT_2, &octet));
        }

        if let Some(mic) = mech_list_mic {
            let octet = der_tlv(TAG_OCTET_STRING, mic);
            seq_contents.extend_from_slice(&der_tlv(TAG_CONTEXT_3, &octet));
        }

        let seq = der_tlv(TAG_SEQUENCE, &seq_contents);
        der_tlv(TAG_CONTEXT_1, &seq)
    }

    #[test]
    fn parse_neg_token_resp_accept_incomplete() {
        let token = build_test_neg_token_resp(
            Some(1), // accept-incomplete
            Some(OID_NTLMSSP),
            Some(b"challenge_token"),
            None,
        );

        let resp = parse_neg_token_resp(&token).unwrap();
        assert_eq!(resp.neg_state, Some(NegState::AcceptIncomplete));
        assert_eq!(resp.supported_mech.as_deref(), Some(OID_NTLMSSP));
        assert_eq!(
            resp.response_token.as_deref(),
            Some(&b"challenge_token"[..])
        );
        assert!(resp.mech_list_mic.is_none());
    }

    #[test]
    fn parse_neg_token_resp_accept_completed() {
        let token = build_test_neg_token_resp(Some(0), None, None, None);

        let resp = parse_neg_token_resp(&token).unwrap();
        assert_eq!(resp.neg_state, Some(NegState::AcceptCompleted));
        assert!(resp.supported_mech.is_none());
        assert!(resp.response_token.is_none());
    }

    #[test]
    fn parse_neg_token_resp_reject() {
        let token = build_test_neg_token_resp(Some(2), None, None, None);

        let resp = parse_neg_token_resp(&token).unwrap();
        assert_eq!(resp.neg_state, Some(NegState::Reject));
    }

    #[test]
    fn parse_neg_token_resp_all_fields() {
        let token = build_test_neg_token_resp(
            Some(1),
            Some(OID_NTLMSSP),
            Some(b"response_data"),
            Some(b"mic_data"),
        );

        let resp = parse_neg_token_resp(&token).unwrap();
        assert_eq!(resp.neg_state, Some(NegState::AcceptIncomplete));
        assert_eq!(resp.supported_mech.as_deref(), Some(OID_NTLMSSP));
        assert_eq!(resp.response_token.as_deref(), Some(&b"response_data"[..]));
        assert_eq!(resp.mech_list_mic.as_deref(), Some(&b"mic_data"[..]));
    }

    #[test]
    fn parse_neg_token_resp_no_fields() {
        // All fields optional
        let token = build_test_neg_token_resp(None, None, None, None);

        let resp = parse_neg_token_resp(&token).unwrap();
        assert!(resp.neg_state.is_none());
        assert!(resp.supported_mech.is_none());
        assert!(resp.response_token.is_none());
        assert!(resp.mech_list_mic.is_none());
    }

    #[test]
    fn parse_neg_token_resp_empty_data_error() {
        let result = parse_neg_token_resp(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_neg_token_resp_truncated_error() {
        // Just a tag byte, no length
        let result = parse_neg_token_resp(&[TAG_CONTEXT_1]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_neg_token_resp_wrong_tag_error() {
        // SEQUENCE tag instead of [1]
        let data = der_tlv(TAG_SEQUENCE, &[0x00]);
        let result = parse_neg_token_resp(&data);
        assert!(result.is_err());
    }

    #[test]
    fn parse_neg_token_resp_unknown_neg_state_error() {
        let token = build_test_neg_token_resp(Some(99), None, None, None);
        let result = parse_neg_token_resp(&token);
        assert!(result.is_err());
    }

    // =======================================================================
    // Cross-validation: construct a realistic server response
    // =======================================================================

    #[test]
    fn parse_realistic_server_challenge_response() {
        // Simulate a typical Samba/Windows SPNEGO response to the first
        // SESSION_SETUP: accept-incomplete with NTLMSSP OID and an NTLM
        // challenge token.
        let ntlm_challenge = b"NTLMSSP\0\x02\x00\x00\x00fake_challenge_data";

        let token = build_test_neg_token_resp(
            Some(1), // accept-incomplete
            Some(OID_NTLMSSP),
            Some(ntlm_challenge),
            None,
        );

        let resp = parse_neg_token_resp(&token).unwrap();
        assert_eq!(resp.neg_state, Some(NegState::AcceptIncomplete));
        assert_eq!(resp.response_token.as_deref(), Some(&ntlm_challenge[..]));
    }

    #[test]
    fn parse_realistic_server_accept_with_mic() {
        // Final server response: accept-completed with mechListMIC
        let mic = [0xaa; 16];
        let token = build_test_neg_token_resp(Some(0), None, None, Some(&mic));

        let resp = parse_neg_token_resp(&token).unwrap();
        assert_eq!(resp.neg_state, Some(NegState::AcceptCompleted));
        assert_eq!(resp.mech_list_mic.as_deref(), Some(&mic[..]));
    }

    // =======================================================================
    // Roundtrip: wrap and parse NegTokenResp
    // =======================================================================

    #[test]
    fn neg_token_resp_wrap_then_parse() {
        let mech_token = b"roundtrip_test_token";
        let wrapped = wrap_neg_token_resp(mech_token);
        let parsed = parse_neg_token_resp(&wrapped).unwrap();

        // Wrapped with only responseToken, so:
        assert!(parsed.neg_state.is_none());
        assert!(parsed.supported_mech.is_none());
        assert_eq!(parsed.response_token.as_deref(), Some(&mech_token[..]));
        assert!(parsed.mech_list_mic.is_none());
    }

    // =======================================================================
    // Wire capture cross-validation
    // =======================================================================

    #[test]
    fn parse_hand_constructed_wire_bytes() {
        // Hand-constructed NegTokenResp matching what a Windows/Samba server
        // sends after receiving NegTokenInit with NTLMSSP:
        //
        // a1 XX                          -- [1] NegTokenResp
        //   30 XX                        -- SEQUENCE
        //     a0 03                      -- [0] negState
        //       0a 01 01                 -- ENUMERATED accept-incomplete (1)
        //     a1 0c                      -- [1] supportedMech
        //       06 0a 2b 06 01 04 01 82 37 02 02 0a  -- NTLMSSP OID
        //     a2 XX                      -- [2] responseToken
        //       04 XX                    -- OCTET STRING
        //         <ntlm challenge bytes>
        let ntlm_challenge = b"NTLMSSP\0fake";

        // Build by hand
        let neg_state_enum = vec![0x0a, 0x01, 0x01]; // ENUMERATED 1
        let neg_state_ctx = der_tlv(TAG_CONTEXT_0, &neg_state_enum);

        let mech_ctx = der_tlv(TAG_CONTEXT_1, OID_NTLMSSP);

        let resp_octet = der_tlv(TAG_OCTET_STRING, ntlm_challenge);
        let resp_ctx = der_tlv(TAG_CONTEXT_2, &resp_octet);

        let mut seq_content = Vec::new();
        seq_content.extend_from_slice(&neg_state_ctx);
        seq_content.extend_from_slice(&mech_ctx);
        seq_content.extend_from_slice(&resp_ctx);
        let seq = der_tlv(TAG_SEQUENCE, &seq_content);
        let wire_bytes = der_tlv(TAG_CONTEXT_1, &seq);

        let parsed = parse_neg_token_resp(&wire_bytes).unwrap();
        assert_eq!(parsed.neg_state, Some(NegState::AcceptIncomplete));
        assert_eq!(parsed.supported_mech.as_deref(), Some(OID_NTLMSSP));
        assert_eq!(parsed.response_token.as_deref(), Some(&ntlm_challenge[..]));
    }

    // =======================================================================
    // OID constant verification
    // =======================================================================

    #[test]
    fn oid_constants_are_valid_der() {
        // Each OID constant should parse as a valid DER TLV with tag 0x06
        for (name, oid) in [
            ("SPNEGO", OID_SPNEGO),
            ("NTLMSSP", OID_NTLMSSP),
            ("Kerberos", OID_KERBEROS),
        ] {
            let (tag, _, total) =
                parse_der_tlv(oid).unwrap_or_else(|e| panic!("{name} OID is not valid DER: {e}"));
            assert_eq!(tag, 0x06, "{name} OID tag should be 0x06");
            assert_eq!(total, oid.len(), "{name} OID should be fully consumed");
        }
    }

    // =======================================================================
    // Large token handling
    // =======================================================================

    #[test]
    fn neg_token_init_with_large_mech_token() {
        // Kerberos tokens can be several KB
        let large_token = vec![0xab; 4096];
        let wrapped = wrap_neg_token_init(&[OID_KERBEROS], &large_token);

        // Should parse without error
        let (tag, _, total) = parse_der_tlv(&wrapped).unwrap();
        assert_eq!(tag, TAG_APPLICATION_0);
        assert_eq!(total, wrapped.len());

        // The large token should be embedded
        assert!(
            wrapped.windows(100).any(|w| w == &large_token[..100]),
            "large token content must be present"
        );
    }

    #[test]
    fn neg_token_resp_with_large_response_token() {
        let large_token = vec![0xcd; 4096];
        let built = build_test_neg_token_resp(Some(1), None, Some(&large_token), None);
        let parsed = parse_neg_token_resp(&built).unwrap();
        assert_eq!(parsed.response_token.as_deref(), Some(&large_token[..]));
    }
}
