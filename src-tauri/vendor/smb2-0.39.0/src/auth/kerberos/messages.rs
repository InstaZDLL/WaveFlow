//! Kerberos ASN.1/DER message encoding and decoding.
//!
//! Hand-rolled ASN.1/DER for the specific Kerberos message structures needed
//! by an SMB2 client. Follows the same pattern as `spnego.rs`.
//!
//! References:
//! - RFC 4120: The Kerberos Network Authentication Service (V5)
//! - MS-KILE: Kerberos Protocol Extensions

use crate::auth::der::{der_tlv, parse_der_tlv};
use crate::auth::kerberos::crypto::EncryptionType;
use crate::Error;

// ---------------------------------------------------------------------------
// ASN.1 tag constants
// ---------------------------------------------------------------------------

const TAG_INTEGER: u8 = 0x02;
const TAG_BIT_STRING: u8 = 0x03;
const TAG_OCTET_STRING: u8 = 0x04;
const TAG_GENERAL_STRING: u8 = 0x1b;
const TAG_GENERALIZED_TIME: u8 = 0x18;
const TAG_SEQUENCE: u8 = 0x30;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Kerberos principal name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalName {
    /// Name type: KRB_NT_PRINCIPAL=1, KRB_NT_SRV_INST=2, etc.
    pub name_type: i32,
    /// Name components: for example, `["user"]` or `["cifs", "server.domain.com"]`.
    pub name_string: Vec<String>,
}

/// Kerberos ticket (opaque to the client: we don't decrypt it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    /// Ticket version number (always 5).
    pub tkt_vno: i32,
    /// Realm of the ticket.
    pub realm: String,
    /// Service principal name.
    pub sname: PrincipalName,
    /// Encrypted part (opaque).
    pub enc_part: EncryptedData,
    /// Raw DER bytes of the ticket as received from the KDC.
    /// Used to pass the ticket through to the AP-REQ verbatim,
    /// avoiding re-encoding which could corrupt the encrypted data.
    pub raw_bytes: Option<Vec<u8>>,
}

/// Generic encrypted data envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedData {
    /// Encryption type identifier.
    pub etype: i32,
    /// Key version number (optional).
    pub kvno: Option<i32>,
    /// Ciphertext bytes.
    pub cipher: Vec<u8>,
}

/// Pre-authentication data element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaData {
    /// Pre-authentication data type.
    pub padata_type: i32,
    /// Pre-authentication data value.
    pub padata_value: Vec<u8>,
}

/// Parsed KDC-REP (AS-REP or TGS-REP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdcRep {
    /// Message type: 11 = AS-REP, 13 = TGS-REP.
    pub msg_type: i32,
    /// Client realm.
    pub crealm: String,
    /// Client principal name.
    pub cname: PrincipalName,
    /// Ticket.
    pub ticket: Ticket,
    /// Encrypted part (to be decrypted by the client).
    pub enc_part: EncryptedData,
}

/// Parsed decrypted EncKDCRepPart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncKdcRepPart {
    /// Session key.
    pub key: EncryptionKey,
    /// Nonce from the request.
    pub nonce: u32,
    /// Ticket flags as a bit field.
    pub flags: u32,
    /// Authentication time.
    pub authtime: String,
    /// Ticket end time.
    pub endtime: String,
    /// Service realm.
    pub srealm: String,
    /// Service principal name.
    pub sname: PrincipalName,
}

/// Encryption key (keytype + key value).
///
/// `Debug` shows the key value as `***`.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptionKey {
    /// Key type (etype number).
    pub keytype: i32,
    /// Key value bytes.
    pub keyvalue: Vec<u8>,
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionKey")
            .field("keytype", &self.keytype)
            .field("keyvalue", &crate::crypto::Redacted)
            .finish()
    }
}

/// Parsed AP-REP (`APPLICATION [15]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApRep {
    /// Encrypted part (to be decrypted with the session key or subkey).
    pub enc_part: EncryptedData,
}

/// Parsed decrypted EncAPRepPart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncApRepPart {
    /// Optional sub-session key from the server. If present, this overrides
    /// the client's subkey as the session key for the application (SMB).
    pub subkey: Option<EncryptionKey>,
    /// Optional sequence number.
    pub seq_number: Option<u32>,
}

/// Parsed KRB-ERROR message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KrbError {
    /// Error code.
    pub error_code: i32,
    /// Client realm (optional).
    pub crealm: Option<String>,
    /// Server realm.
    pub realm: String,
    /// Server principal name.
    pub sname: PrincipalName,
    /// Error text (optional).
    pub e_text: Option<String>,
    /// Error data (optional).
    pub e_data: Option<Vec<u8>>,
}

// Core DER encoding/decoding helpers (der_length, der_tlv, parse_der_length,
// parse_der_tlv) are in `crate::auth::der`. Imported at the top.

// ---------------------------------------------------------------------------
// DER encoding helpers (Kerberos-specific)
// ---------------------------------------------------------------------------

/// Encode a context-specific constructed tag: `[tag_num]`.
fn der_context(tag_num: u8, data: &[u8]) -> Vec<u8> {
    der_tlv(0xa0 | tag_num, data)
}

/// Encode an APPLICATION constructed tag: `[APPLICATION tag_num]`.
fn der_application(tag_num: u8, data: &[u8]) -> Vec<u8> {
    der_tlv(0x60 | tag_num, data)
}

/// Encode an ASN.1 INTEGER (signed, big-endian, minimal bytes).
fn der_integer(val: i32) -> Vec<u8> {
    let bytes = val.to_be_bytes();
    // Find the first significant byte, keeping sign correct.
    let mut start = 0;
    if val >= 0 {
        // Skip leading 0x00 bytes, but keep one if the next byte has the high bit set.
        while start < 3 && bytes[start] == 0x00 && bytes[start + 1] & 0x80 == 0 {
            start += 1;
        }
    } else {
        // Skip leading 0xff bytes, but keep one if the next byte doesn't have the high bit set.
        while start < 3 && bytes[start] == 0xff && bytes[start + 1] & 0x80 != 0 {
            start += 1;
        }
    }
    der_tlv(TAG_INTEGER, &bytes[start..])
}

/// Encode an unsigned 32-bit value as ASN.1 INTEGER.
fn der_integer_u32(val: u32) -> Vec<u8> {
    // Treat as i64 to handle the full u32 range without sign issues.
    let val64 = val as i64;
    let bytes = val64.to_be_bytes();
    let mut start = 0;
    while start < 7 && bytes[start] == 0x00 && bytes[start + 1] & 0x80 == 0 {
        start += 1;
    }
    der_tlv(TAG_INTEGER, &bytes[start..])
}

/// Encode a DER OCTET STRING.
fn der_octet_string(data: &[u8]) -> Vec<u8> {
    der_tlv(TAG_OCTET_STRING, data)
}

/// Encode a DER GeneralString.
fn der_general_string(s: &str) -> Vec<u8> {
    der_tlv(TAG_GENERAL_STRING, s.as_bytes())
}

/// Encode a DER GeneralizedTime (for example, `"20260408120000Z"`).
fn der_generalized_time(time: &str) -> Vec<u8> {
    der_tlv(TAG_GENERALIZED_TIME, time.as_bytes())
}

/// Encode a DER BIT STRING. `bits` is the raw bytes; `unused` is the number
/// of unused bits in the last byte (usually 0 for 32-bit flags).
fn der_bit_string(bits: &[u8], unused: u8) -> Vec<u8> {
    let mut data = vec![unused];
    data.extend_from_slice(bits);
    der_tlv(TAG_BIT_STRING, &data)
}

/// Encode a DER SEQUENCE from pre-encoded items.
fn der_sequence(items: &[&[u8]]) -> Vec<u8> {
    let mut contents = Vec::new();
    for item in items {
        contents.extend_from_slice(item);
    }
    der_tlv(TAG_SEQUENCE, &contents)
}

// ---------------------------------------------------------------------------
// DER parsing helpers (Kerberos-specific)
// ---------------------------------------------------------------------------

/// Parse all TLV elements in a SEQUENCE body, returning `(tag, value)` pairs.
fn parse_sequence_fields(data: &[u8]) -> Result<Vec<(u8, Vec<u8>)>, Error> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (tag, value, consumed) = parse_der_tlv(&data[pos..])?;
        fields.push((tag, value.to_vec()));
        pos += consumed;
    }
    Ok(fields)
}

/// Parse a DER INTEGER value (already unwrapped from TLV), returning i32.
fn parse_integer_value(data: &[u8]) -> Result<i32, Error> {
    if data.is_empty() {
        return Err(Error::invalid_data("Kerberos: empty INTEGER"));
    }
    // Sign-extend from arbitrary-length big-endian.
    let negative = data[0] & 0x80 != 0;
    let mut val: i64 = if negative { -1 } else { 0 };
    for &b in data {
        val = (val << 8) | (b as i64);
    }
    Ok(val as i32)
}

/// Parse a DER INTEGER TLV, returning i32.
fn parse_der_integer(data: &[u8]) -> Result<i32, Error> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != TAG_INTEGER {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected INTEGER (0x02), got 0x{tag:02x}"
        )));
    }
    parse_integer_value(value)
}

/// Parse a DER INTEGER TLV, returning u32.
fn parse_der_integer_u32(data: &[u8]) -> Result<u32, Error> {
    let val = parse_der_integer(data)?;
    Ok(val as u32)
}

/// Parse a DER OCTET STRING TLV, returning the raw bytes.
fn parse_der_octet_string(data: &[u8]) -> Result<Vec<u8>, Error> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != TAG_OCTET_STRING {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected OCTET STRING (0x04), got 0x{tag:02x}"
        )));
    }
    Ok(value.to_vec())
}

/// Parse a DER GeneralString TLV, returning the string.
fn parse_der_general_string(data: &[u8]) -> Result<String, Error> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != TAG_GENERAL_STRING {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected GeneralString (0x1b), got 0x{tag:02x}"
        )));
    }
    String::from_utf8(value.to_vec())
        .map_err(|_| Error::invalid_data("Kerberos: invalid UTF-8 in GeneralString"))
}

/// Parse a DER GeneralizedTime TLV, returning the time string.
fn parse_der_generalized_time(data: &[u8]) -> Result<String, Error> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != TAG_GENERALIZED_TIME {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected GeneralizedTime (0x18), got 0x{tag:02x}"
        )));
    }
    String::from_utf8(value.to_vec())
        .map_err(|_| Error::invalid_data("Kerberos: invalid UTF-8 in GeneralizedTime"))
}

/// Parse a DER BIT STRING TLV, returning the raw bit bytes (without the
/// unused-bits prefix byte) and the number of unused bits.
fn parse_der_bit_string(data: &[u8]) -> Result<(Vec<u8>, u8), Error> {
    let (tag, value, _) = parse_der_tlv(data)?;
    if tag != TAG_BIT_STRING {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected BIT STRING (0x03), got 0x{tag:02x}"
        )));
    }
    if value.is_empty() {
        return Err(Error::invalid_data("Kerberos: empty BIT STRING"));
    }
    let unused = value[0];
    Ok((value[1..].to_vec(), unused))
}

// ---------------------------------------------------------------------------
// Encoding compound types
// ---------------------------------------------------------------------------

/// Encode a PrincipalName as DER.
fn encode_principal_name(name: &PrincipalName) -> Vec<u8> {
    // PrincipalName ::= SEQUENCE {
    //   name-type   [0] Int32,
    //   name-string [1] SEQUENCE OF KerberosString (GeneralString)
    // }
    let name_type = der_context(0, &der_integer(name.name_type));
    let name_strings: Vec<Vec<u8>> = name
        .name_string
        .iter()
        .map(|s| der_general_string(s))
        .collect();
    let name_refs: Vec<&[u8]> = name_strings.iter().map(|v| v.as_slice()).collect();
    let name_seq = der_sequence(&name_refs);
    let name_string = der_context(1, &name_seq);
    der_sequence(&[&name_type, &name_string])
}

/// Encode an EncryptedData as DER.
fn encode_encrypted_data(ed: &EncryptedData) -> Vec<u8> {
    // EncryptedData ::= SEQUENCE {
    //   etype  [0] Int32,
    //   kvno   [1] UInt32 OPTIONAL,
    //   cipher [2] OCTET STRING
    // }
    let etype = der_context(0, &der_integer(ed.etype));
    let cipher = der_context(2, &der_octet_string(&ed.cipher));
    if let Some(kvno) = ed.kvno {
        let kvno_enc = der_context(1, &der_integer(kvno));
        der_sequence(&[&etype, &kvno_enc, &cipher])
    } else {
        der_sequence(&[&etype, &cipher])
    }
}

/// Encode a Ticket as DER (`APPLICATION [1]`).
fn encode_ticket(ticket: &Ticket) -> Vec<u8> {
    // Ticket ::= [APPLICATION 1] SEQUENCE {
    //   tkt-vno  [0] INTEGER (5),
    //   realm    [1] Realm (GeneralString),
    //   sname    [2] PrincipalName,
    //   enc-part [3] EncryptedData
    // }
    let tkt_vno = der_context(0, &der_integer(ticket.tkt_vno));
    let realm = der_context(1, &der_general_string(&ticket.realm));
    let sname = der_context(2, &encode_principal_name(&ticket.sname));
    let enc_part = der_context(3, &encode_encrypted_data(&ticket.enc_part));
    let seq = der_sequence(&[&tkt_vno, &realm, &sname, &enc_part]);
    der_application(1, &seq)
}

/// Encode a PaData as DER.
fn encode_pa_data(pa: &PaData) -> Vec<u8> {
    // PA-DATA ::= SEQUENCE {
    //   padata-type  [1] Int32,
    //   padata-value [2] OCTET STRING
    // }
    let padata_type = der_context(1, &der_integer(pa.padata_type));
    let padata_value = der_context(2, &der_octet_string(&pa.padata_value));
    der_sequence(&[&padata_type, &padata_value])
}

// ---------------------------------------------------------------------------
// Parsing compound types
// ---------------------------------------------------------------------------

/// Parse a PrincipalName from DER bytes.
fn parse_principal_name(data: &[u8]) -> Result<PrincipalName, Error> {
    let (tag, seq_data, _) = parse_der_tlv(data)?;
    if tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE for PrincipalName, got 0x{tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;
    let mut name_type = None;
    let mut name_string = Vec::new();
    for (ftag, fvalue) in &fields {
        match ftag {
            0xa0 => name_type = Some(parse_der_integer(fvalue)?),
            0xa1 => {
                // SEQUENCE OF GeneralString
                let (stag, sdata, _) = parse_der_tlv(fvalue)?;
                if stag != TAG_SEQUENCE {
                    return Err(Error::invalid_data(
                        "Kerberos: expected SEQUENCE for name-string",
                    ));
                }
                let mut pos = 0;
                while pos < sdata.len() {
                    let (_, sv, consumed) = parse_der_tlv(&sdata[pos..])?;
                    name_string.push(String::from_utf8(sv.to_vec()).map_err(|_| {
                        Error::invalid_data("Kerberos: invalid UTF-8 in name-string")
                    })?);
                    pos += consumed;
                }
            }
            _ => {} // ignore unknown fields
        }
    }
    Ok(PrincipalName {
        name_type: name_type
            .ok_or_else(|| Error::invalid_data("Kerberos: missing name-type in PrincipalName"))?,
        name_string,
    })
}

/// Parse an EncryptedData from DER bytes.
fn parse_encrypted_data(data: &[u8]) -> Result<EncryptedData, Error> {
    let (tag, seq_data, _) = parse_der_tlv(data)?;
    if tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE for EncryptedData, got 0x{tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;
    let mut etype = None;
    let mut kvno = None;
    let mut cipher = None;
    for (ftag, fvalue) in &fields {
        match ftag {
            0xa0 => etype = Some(parse_der_integer(fvalue)?),
            0xa1 => kvno = Some(parse_der_integer(fvalue)?),
            0xa2 => cipher = Some(parse_der_octet_string(fvalue)?),
            _ => {}
        }
    }
    Ok(EncryptedData {
        etype: etype
            .ok_or_else(|| Error::invalid_data("Kerberos: missing etype in EncryptedData"))?,
        kvno,
        cipher: cipher
            .ok_or_else(|| Error::invalid_data("Kerberos: missing cipher in EncryptedData"))?,
    })
}

/// Parse a Ticket from DER bytes (expects `APPLICATION [1]` wrapper).
///
/// Stores the raw DER bytes so the ticket can be passed through to the
/// AP-REQ verbatim. Re-encoding the ticket from parsed fields can produce
/// different DER bytes (e.g., different length encoding, field order), which
/// corrupts the encrypted data and causes the server to fail decryption.
pub fn parse_ticket(data: &[u8]) -> Result<Ticket, Error> {
    let (tag, inner, total_consumed) = parse_der_tlv(data)?;
    // APPLICATION [1] = 0x61
    if tag != 0x61 {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected APPLICATION [1] (0x61) for Ticket, got 0x{tag:02x}"
        )));
    }

    // Store raw bytes for verbatim pass-through.
    let raw_bytes = data[..total_consumed].to_vec();

    let (seq_tag, seq_data, _) = parse_der_tlv(inner)?;
    if seq_tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE in Ticket, got 0x{seq_tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;
    let mut tkt_vno = None;
    let mut realm = None;
    let mut sname = None;
    let mut enc_part = None;
    for (ftag, fvalue) in &fields {
        match ftag {
            0xa0 => tkt_vno = Some(parse_der_integer(fvalue)?),
            0xa1 => realm = Some(parse_der_general_string(fvalue)?),
            0xa2 => sname = Some(parse_principal_name(fvalue)?),
            0xa3 => enc_part = Some(parse_encrypted_data(fvalue)?),
            _ => {}
        }
    }
    Ok(Ticket {
        tkt_vno: tkt_vno
            .ok_or_else(|| Error::invalid_data("Kerberos: missing tkt-vno in Ticket"))?,
        realm: realm.ok_or_else(|| Error::invalid_data("Kerberos: missing realm in Ticket"))?,
        sname: sname.ok_or_else(|| Error::invalid_data("Kerberos: missing sname in Ticket"))?,
        enc_part: enc_part
            .ok_or_else(|| Error::invalid_data("Kerberos: missing enc-part in Ticket"))?,
        raw_bytes: Some(raw_bytes),
    })
}

/// Parse an EncryptionKey from DER bytes.
fn parse_encryption_key(data: &[u8]) -> Result<EncryptionKey, Error> {
    let (tag, seq_data, _) = parse_der_tlv(data)?;
    if tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE for EncryptionKey, got 0x{tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;
    let mut keytype = None;
    let mut keyvalue = None;
    for (ftag, fvalue) in &fields {
        match ftag {
            0xa0 => keytype = Some(parse_der_integer(fvalue)?),
            0xa1 => keyvalue = Some(parse_der_octet_string(fvalue)?),
            _ => {}
        }
    }
    Ok(EncryptionKey {
        keytype: keytype
            .ok_or_else(|| Error::invalid_data("Kerberos: missing keytype in EncryptionKey"))?,
        keyvalue: keyvalue
            .ok_or_else(|| Error::invalid_data("Kerberos: missing keyvalue in EncryptionKey"))?,
    })
}

// ---------------------------------------------------------------------------
// Public API: encoding
// ---------------------------------------------------------------------------

/// Encode a KRB_AS_REQ message (`APPLICATION [10]`).
pub fn encode_as_req(
    cname: &PrincipalName,
    realm: &str,
    sname: &PrincipalName,
    nonce: u32,
    etypes: &[EncryptionType],
    padata: &[PaData],
) -> Vec<u8> {
    encode_kdc_req(10, Some(cname), realm, sname, nonce, etypes, padata)
}

/// Encode the KDC-REQ-BODY for a TGS-REQ.
///
/// Returns the DER-encoded body, which is needed for computing the
/// checksum in the Authenticator (per RFC 4120 section 7.2.2).
pub fn encode_tgs_req_body(
    realm: &str,
    sname: &PrincipalName,
    nonce: u32,
    etypes: &[EncryptionType],
) -> Vec<u8> {
    encode_kdc_req_body(None, realm, sname, nonce, etypes)
}

/// Encode a KRB_TGS_REQ message (`APPLICATION [12]`).
///
/// The `tgt_ap_req` is an AP-REQ wrapping the TGT, placed in PA-TGS-REQ (padata type 1).
/// The `req_body` must be the same bytes returned by `encode_tgs_req_body` (used for
/// the Authenticator checksum).
pub fn encode_tgs_req(
    realm: &str,
    sname: &PrincipalName,
    nonce: u32,
    etypes: &[EncryptionType],
    tgt_ap_req: &[u8],
) -> Vec<u8> {
    let padata = [PaData {
        padata_type: 1, // PA-TGS-REQ
        padata_value: tgt_ap_req.to_vec(),
    }];
    encode_kdc_req(12, None, realm, sname, nonce, etypes, &padata)
}

/// Encode a KRB_AP_REQ message (`APPLICATION [14]`).
///
/// When `mutual_required` is true, sets the mutual-required bit (bit 2) in
/// AP-OPTIONS, requesting the server to prove its identity via an AP-REP.
pub fn encode_ap_req(
    ticket: &Ticket,
    encrypted_authenticator: &EncryptedData,
    mutual_required: bool,
) -> Vec<u8> {
    // AP-REQ ::= [APPLICATION 14] SEQUENCE {
    //   pvno       [0] INTEGER (5),
    //   msg-type   [1] INTEGER (14),
    //   ap-options [2] APOptions (BIT STRING, 32 bits),
    //   ticket     [3] Ticket,
    //   authenticator [4] EncryptedData
    // }
    let pvno = der_context(0, &der_integer(5));
    let msg_type = der_context(1, &der_integer(14));
    // AP-OPTIONS: bit 2 = mutual-required (0x20 in the first byte).
    let opts_byte0 = if mutual_required { 0x20 } else { 0x00 };
    let ap_options = der_context(2, &der_bit_string(&[opts_byte0, 0x00, 0x00, 0x00], 0));
    // Use raw ticket bytes if available (preserves exact DER encoding from KDC).
    // Re-encoding can produce different bytes and corrupt the encrypted ticket.
    let ticket_raw = ticket
        .raw_bytes
        .as_ref()
        .map(|b| der_context(3, b))
        .unwrap_or_else(|| der_context(3, &encode_ticket(ticket)));
    let authenticator = der_context(4, &encode_encrypted_data(encrypted_authenticator));
    let seq = der_sequence(&[&pvno, &msg_type, &ap_options, &ticket_raw, &authenticator]);
    der_application(14, &seq)
}

/// Encode an Authenticator (`APPLICATION [2]`), to be encrypted before embedding in AP-REQ.
///
/// The optional `cksum` parameter adds a checksum field `[3]`, used in TGS-REQ
/// to authenticate the KDC-REQ-BODY (RFC 4120 section 7.2.2).
pub fn encode_authenticator(
    crealm: &str,
    cname: &PrincipalName,
    ctime: &str,
    cusec: u32,
    subkey: Option<(&[u8], i32)>,
    seq_number: Option<u32>,
    cksum: Option<(&[u8], i32)>,
) -> Vec<u8> {
    // Authenticator ::= [APPLICATION 2] SEQUENCE {
    //   authenticator-vno [0] INTEGER (5),
    //   crealm           [1] Realm (GeneralString),
    //   cname            [2] PrincipalName,
    //   cksum            [3] Checksum OPTIONAL,
    //   cusec            [4] Microseconds (INTEGER),
    //   ctime            [5] KerberosTime (GeneralizedTime),
    //   subkey           [6] EncryptionKey OPTIONAL,
    //   seq-number       [7] UInt32 OPTIONAL,
    // }
    let auth_vno = der_context(0, &der_integer(5));
    let crealm_enc = der_context(1, &der_general_string(crealm));
    let cname_enc = der_context(2, &encode_principal_name(cname));

    let mut items: Vec<Vec<u8>> = vec![auth_vno, crealm_enc, cname_enc];

    if let Some((checksum_data, checksum_type)) = cksum {
        // Checksum ::= SEQUENCE { cksumtype [0] Int32, checksum [1] OCTET STRING }
        let cktype = der_context(0, &der_integer(checksum_type));
        let ckval = der_context(1, &der_octet_string(checksum_data));
        let ck = der_sequence(&[&cktype, &ckval]);
        items.push(der_context(3, &ck));
    }

    let cusec_enc = der_context(4, &der_integer_u32(cusec));
    let ctime_enc = der_context(5, &der_generalized_time(ctime));
    items.push(cusec_enc);
    items.push(ctime_enc);

    if let Some((key_value, key_type)) = subkey {
        // EncryptionKey ::= SEQUENCE { keytype [0], keyvalue [1] }
        let kt = der_context(0, &der_integer(key_type));
        let kv = der_context(1, &der_octet_string(key_value));
        let ek = der_sequence(&[&kt, &kv]);
        items.push(der_context(6, &ek));
    }

    if let Some(seq) = seq_number {
        items.push(der_context(7, &der_integer_u32(seq)));
    }

    let item_refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
    let seq = der_sequence(&item_refs);
    der_application(2, &seq)
}

/// Encode a PA-ENC-TIMESTAMP pre-authentication data (the plaintext to be encrypted).
///
/// Returns the DER encoding of `PA-ENC-TS-ENC ::= SEQUENCE { patimestamp [0] GeneralizedTime, pausec [1] Microseconds }`.
pub fn encode_pa_enc_timestamp(ctime: &str, cusec: u32) -> Vec<u8> {
    let patimestamp = der_context(0, &der_generalized_time(ctime));
    let pausec = der_context(1, &der_integer_u32(cusec));
    der_sequence(&[&patimestamp, &pausec])
}

// ---------------------------------------------------------------------------
// Public API: parsing
// ---------------------------------------------------------------------------

/// Parse a KRB_AS_REP (`APPLICATION [11]`) or KRB_TGS_REP (`APPLICATION [13]`) message.
pub fn parse_kdc_rep(data: &[u8]) -> Result<KdcRep, Error> {
    let (tag, inner, _) = parse_der_tlv(data)?;
    // APPLICATION [11] = 0x6b, APPLICATION [13] = 0x6d
    let expected_msg_type = match tag {
        0x6b => 11,
        0x6d => 13,
        _ => {
            return Err(Error::invalid_data(format!(
                "Kerberos: expected APPLICATION [11] or [13] for KDC-REP, got 0x{tag:02x}"
            )));
        }
    };

    let (seq_tag, seq_data, _) = parse_der_tlv(inner)?;
    if seq_tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE in KDC-REP, got 0x{seq_tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;
    let mut msg_type = None;
    let mut crealm = None;
    let mut cname = None;
    let mut ticket = None;
    let mut enc_part = None;

    for (ftag, fvalue) in &fields {
        match ftag {
            // RFC 4120 section 5.4.2: KDC-REP fields
            0xa0 => {
                // pvno [0] — skip validation
            }
            0xa1 => msg_type = Some(parse_der_integer(fvalue)?),
            // [2] padata — skip
            0xa3 => crealm = Some(parse_der_general_string(fvalue)?),
            0xa4 => cname = Some(parse_principal_name(fvalue)?),
            0xa5 => ticket = Some(parse_ticket(fvalue)?),
            0xa6 => enc_part = Some(parse_encrypted_data(fvalue)?),
            _ => {}
        }
    }

    let msg_type =
        msg_type.ok_or_else(|| Error::invalid_data("Kerberos: missing msg-type in KDC-REP"))?;
    if msg_type != expected_msg_type {
        return Err(Error::invalid_data(format!(
            "Kerberos: KDC-REP msg-type mismatch: tag says {expected_msg_type}, field says {msg_type}"
        )));
    }

    Ok(KdcRep {
        msg_type,
        crealm: crealm.ok_or_else(|| Error::invalid_data("Kerberos: missing crealm in KDC-REP"))?,
        cname: cname.ok_or_else(|| Error::invalid_data("Kerberos: missing cname in KDC-REP"))?,
        ticket: ticket.ok_or_else(|| Error::invalid_data("Kerberos: missing ticket in KDC-REP"))?,
        enc_part: enc_part
            .ok_or_else(|| Error::invalid_data("Kerberos: missing enc-part in KDC-REP"))?,
    })
}

/// Parse the decrypted EncKDCRepPart.
///
/// This can be wrapped in `APPLICATION [25]` (EncASRepPart) or `APPLICATION [26]` (EncTGSRepPart),
/// or may appear as a bare SEQUENCE (some implementations).
pub fn parse_enc_kdc_rep_part(data: &[u8]) -> Result<EncKdcRepPart, Error> {
    let (tag, inner, _) = parse_der_tlv(data)?;

    // APPLICATION [25] = 0x79, APPLICATION [26] = 0x7a, or bare SEQUENCE
    let seq_data = if tag == 0x79 || tag == 0x7a {
        let (seq_tag, sd, _) = parse_der_tlv(inner)?;
        if seq_tag != TAG_SEQUENCE {
            return Err(Error::invalid_data(format!(
                "Kerberos: expected SEQUENCE in EncKDCRepPart, got 0x{seq_tag:02x}"
            )));
        }
        sd
    } else if tag == TAG_SEQUENCE {
        inner
    } else {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected APPLICATION [25/26] or SEQUENCE for EncKDCRepPart, got 0x{tag:02x}"
        )));
    };

    let fields = parse_sequence_fields(seq_data)?;
    let mut key = None;
    let mut nonce = None;
    let mut flags = None;
    let mut authtime = None;
    let mut endtime = None;
    let mut srealm = None;
    let mut sname = None;

    for (ftag, fvalue) in &fields {
        match ftag {
            0xa0 => key = Some(parse_encryption_key(fvalue)?),
            // [1] last-req — skip
            0xa2 => nonce = Some(parse_der_integer_u32(fvalue)?),
            // [3] key-expiration — skip
            0xa4 => {
                let (bits, _unused) = parse_der_bit_string(fvalue)?;
                if bits.len() >= 4 {
                    flags = Some(u32::from_be_bytes([bits[0], bits[1], bits[2], bits[3]]));
                }
            }
            0xa5 => authtime = Some(parse_der_generalized_time(fvalue)?),
            // [6] starttime — skip
            0xa7 => endtime = Some(parse_der_generalized_time(fvalue)?),
            // [8] renew-till — skip
            0xa9 => srealm = Some(parse_der_general_string(fvalue)?),
            0xaa => sname = Some(parse_principal_name(fvalue)?),
            _ => {}
        }
    }

    Ok(EncKdcRepPart {
        key: key.ok_or_else(|| Error::invalid_data("Kerberos: missing key in EncKDCRepPart"))?,
        nonce: nonce
            .ok_or_else(|| Error::invalid_data("Kerberos: missing nonce in EncKDCRepPart"))?,
        flags: flags.unwrap_or(0),
        authtime: authtime
            .ok_or_else(|| Error::invalid_data("Kerberos: missing authtime in EncKDCRepPart"))?,
        endtime: endtime
            .ok_or_else(|| Error::invalid_data("Kerberos: missing endtime in EncKDCRepPart"))?,
        srealm: srealm
            .ok_or_else(|| Error::invalid_data("Kerberos: missing srealm in EncKDCRepPart"))?,
        sname: sname
            .ok_or_else(|| Error::invalid_data("Kerberos: missing sname in EncKDCRepPart"))?,
    })
}

/// Parse a KRB-ERROR message (`APPLICATION [30]`).
pub fn parse_krb_error(data: &[u8]) -> Result<KrbError, Error> {
    let (tag, inner, _) = parse_der_tlv(data)?;
    // APPLICATION [30] = 0x7e
    if tag != 0x7e {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected APPLICATION [30] (0x7e) for KRB-ERROR, got 0x{tag:02x}"
        )));
    }
    let (seq_tag, seq_data, _) = parse_der_tlv(inner)?;
    if seq_tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE in KRB-ERROR, got 0x{seq_tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;

    let mut error_code = None;
    let mut crealm = None;
    let mut realm = None;
    let mut sname = None;
    let mut e_text = None;
    let mut e_data = None;

    for (ftag, fvalue) in &fields {
        match ftag {
            // [0] pvno — skip
            // [1] msg-type — skip
            // [2] ctime — skip
            // [3] cusec — skip
            // [4] stime — skip
            // [5] susec — skip
            0xa6 => error_code = Some(parse_der_integer(fvalue)?),
            0xa7 => crealm = Some(parse_der_general_string(fvalue)?),
            0xa8 => {
                // cname — skip (we don't need it in the error struct, but parse to validate)
            }
            0xa9 => realm = Some(parse_der_general_string(fvalue)?),
            0xaa => sname = Some(parse_principal_name(fvalue)?),
            0xab => e_text = Some(parse_der_general_string(fvalue)?),
            0xac => e_data = Some(parse_der_octet_string(fvalue)?),
            _ => {}
        }
    }

    Ok(KrbError {
        error_code: error_code
            .ok_or_else(|| Error::invalid_data("Kerberos: missing error-code in KRB-ERROR"))?,
        crealm,
        realm: realm.ok_or_else(|| Error::invalid_data("Kerberos: missing realm in KRB-ERROR"))?,
        sname: sname.ok_or_else(|| Error::invalid_data("Kerberos: missing sname in KRB-ERROR"))?,
        e_text,
        e_data,
    })
}

/// Unwrap a GSS-API token: `APPLICATION [0] { OID, inner-data }`.
///
/// Returns the inner data after the OID as a `Vec<u8>`.
pub fn parse_gss_api_wrapper(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>, usize), Error> {
    let (tag, inner, total) = parse_der_tlv(data)?;
    if tag != 0x60 {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected GSS-API wrapper (0x60), got 0x{tag:02x}"
        )));
    }
    // Skip the OID TLV.
    let (_oid_tag, oid_data, oid_consumed) = parse_der_tlv(inner)?;
    let oid = oid_data.to_vec();
    let rest = inner[oid_consumed..].to_vec();
    Ok((oid, rest, total))
}

/// Parse a KRB_AP_REP message (`APPLICATION [15]`).
///
/// Handles both bare AP-REP and GSS-API wrapped tokens (`APPLICATION [0]`
/// containing an OID followed by the AP-REP).
pub fn parse_ap_rep(data: &[u8]) -> Result<ApRep, Error> {
    let (tag, inner, _) = parse_der_tlv(data)?;

    // If wrapped in GSS-API APPLICATION [0], unwrap first.
    let inner = if tag == 0x60 {
        // APPLICATION [0] { OID, AP-REP }
        // Skip the OID TLV to get to the AP-REP.
        let (_oid_tag, _oid_data, oid_consumed) = parse_der_tlv(inner)?;
        let ap_rep_data = &inner[oid_consumed..];
        let (ap_tag, ap_inner, _) = parse_der_tlv(ap_rep_data)?;
        if ap_tag != 0x6f {
            return Err(Error::invalid_data(format!(
                "Kerberos: expected AP-REP (0x6f) inside GSS wrapper, got 0x{ap_tag:02x}"
            )));
        }
        ap_inner
    } else if tag == 0x6f {
        inner
    } else {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected APPLICATION [15] (0x6f) or GSS wrapper (0x60) for AP-REP, got 0x{tag:02x}"
        )));
    };
    let (seq_tag, seq_data, _) = parse_der_tlv(inner)?;
    if seq_tag != TAG_SEQUENCE {
        return Err(Error::invalid_data(format!(
            "Kerberos: expected SEQUENCE in AP-REP, got 0x{seq_tag:02x}"
        )));
    }
    let fields = parse_sequence_fields(seq_data)?;

    let mut enc_part = None;
    for (ftag, fvalue) in &fields {
        // [0] pvno — skip, [1] msg-type — skip
        if ftag == &0xa2 {
            enc_part = Some(parse_encrypted_data(fvalue)?);
        }
    }

    Ok(ApRep {
        enc_part: enc_part
            .ok_or_else(|| Error::invalid_data("Kerberos: missing enc-part in AP-REP"))?,
    })
}

/// Parse the decrypted EncAPRepPart (`APPLICATION [27]`).
pub fn parse_enc_ap_rep_part(data: &[u8]) -> Result<EncApRepPart, Error> {
    let (tag, inner, _) = parse_der_tlv(data)?;
    // APPLICATION [27] = 0x7b, or bare SEQUENCE
    let seq_data = match tag {
        0x7b => {
            let (seq_tag, seq_data, _) = parse_der_tlv(inner)?;
            if seq_tag != TAG_SEQUENCE {
                return Err(Error::invalid_data(format!(
                    "Kerberos: expected SEQUENCE in EncAPRepPart, got 0x{seq_tag:02x}"
                )));
            }
            seq_data
        }
        TAG_SEQUENCE => inner,
        _ => {
            return Err(Error::invalid_data(format!(
                "Kerberos: expected APPLICATION [27] or SEQUENCE for EncAPRepPart, got 0x{tag:02x}"
            )));
        }
    };

    let fields = parse_sequence_fields(seq_data)?;

    let mut subkey = None;
    let mut seq_number = None;
    for (ftag, fvalue) in &fields {
        match ftag {
            // [0] ctime — skip
            // [1] cusec — skip
            0xa2 => subkey = Some(parse_encryption_key(fvalue)?),
            0xa3 => seq_number = Some(parse_der_integer_u32(fvalue)?),
            _ => {}
        }
    }

    Ok(EncApRepPart { subkey, seq_number })
}

// ---------------------------------------------------------------------------
// Internal: KDC-REQ encoding (shared by AS-REQ and TGS-REQ)
// ---------------------------------------------------------------------------

/// Encode just the KDC-REQ-BODY portion of a KDC-REQ.
fn encode_kdc_req_body(
    cname: Option<&PrincipalName>,
    realm: &str,
    sname: &PrincipalName,
    nonce: u32,
    etypes: &[EncryptionType],
) -> Vec<u8> {
    let kdc_options = der_context(0, &der_bit_string(&[0x40, 0x81, 0x00, 0x10], 0));
    let mut body_items: Vec<Vec<u8>> = vec![kdc_options];

    if let Some(cn) = cname {
        body_items.push(der_context(1, &encode_principal_name(cn)));
    }
    body_items.push(der_context(2, &der_general_string(realm)));
    body_items.push(der_context(3, &encode_principal_name(sname)));
    // till: set far in the future
    body_items.push(der_context(5, &der_generalized_time("20370913024805Z")));
    body_items.push(der_context(7, &der_integer_u32(nonce)));

    // etype: SEQUENCE OF INTEGER
    let etype_ints: Vec<Vec<u8>> = etypes.iter().map(|e| der_integer(*e as i32)).collect();
    let etype_refs: Vec<&[u8]> = etype_ints.iter().map(|v| v.as_slice()).collect();
    let etype_seq = der_sequence(&etype_refs);
    body_items.push(der_context(8, &etype_seq));

    let body_refs: Vec<&[u8]> = body_items.iter().map(|v| v.as_slice()).collect();
    der_sequence(&body_refs)
}

fn encode_kdc_req(
    msg_type_val: i32,
    cname: Option<&PrincipalName>,
    realm: &str,
    sname: &PrincipalName,
    nonce: u32,
    etypes: &[EncryptionType],
    padata: &[PaData],
) -> Vec<u8> {
    let req_body = encode_kdc_req_body(cname, realm, sname, nonce, etypes);

    // KDC-REQ
    let pvno = der_context(1, &der_integer(5));
    let msg_type = der_context(2, &der_integer(msg_type_val));

    let mut kdc_req_items: Vec<Vec<u8>> = vec![pvno, msg_type];

    if !padata.is_empty() {
        let pa_items: Vec<Vec<u8>> = padata.iter().map(encode_pa_data).collect();
        let pa_refs: Vec<&[u8]> = pa_items.iter().map(|v| v.as_slice()).collect();
        let pa_seq = der_sequence(&pa_refs);
        kdc_req_items.push(der_context(3, &pa_seq));
    }

    kdc_req_items.push(der_context(4, &req_body));

    let kdc_req_refs: Vec<&[u8]> = kdc_req_items.iter().map(|v| v.as_slice()).collect();
    let kdc_req_seq = der_sequence(&kdc_req_refs);

    // APPLICATION tag for the message type
    let app_tag = match msg_type_val {
        10 => 10, // AS-REQ
        12 => 12, // TGS-REQ
        _ => msg_type_val as u8,
    };
    der_application(app_tag, &kdc_req_seq)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // DER helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_der_integer_positive() {
        // 5 should encode as 02 01 05
        let encoded = der_integer(5);
        assert_eq!(encoded, vec![0x02, 0x01, 0x05]);
    }

    #[test]
    fn test_der_integer_zero() {
        // 0 should encode as 02 01 00
        let encoded = der_integer(0);
        assert_eq!(encoded, vec![0x02, 0x01, 0x00]);
    }

    #[test]
    fn test_der_integer_negative() {
        // -1 should encode as 02 01 ff
        let encoded = der_integer(-1);
        assert_eq!(encoded, vec![0x02, 0x01, 0xff]);
    }

    #[test]
    fn test_der_integer_128() {
        // 128 needs leading 0x00: 02 02 00 80
        let encoded = der_integer(128);
        assert_eq!(encoded, vec![0x02, 0x02, 0x00, 0x80]);
    }

    #[test]
    fn test_der_integer_256() {
        // 256 = 0x0100: 02 02 01 00
        let encoded = der_integer(256);
        assert_eq!(encoded, vec![0x02, 0x02, 0x01, 0x00]);
    }

    #[test]
    fn test_der_integer_large_positive() {
        // 65536 = 0x10000: 02 03 01 00 00
        let encoded = der_integer(65536);
        assert_eq!(encoded, vec![0x02, 0x03, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn test_der_integer_u32_max() {
        // u32::MAX = 0xFFFFFFFF: needs 02 05 00 ff ff ff ff
        let encoded = der_integer_u32(u32::MAX);
        assert_eq!(encoded, vec![0x02, 0x05, 0x00, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn test_der_generalized_time() {
        let encoded = der_generalized_time("20260408120000Z");
        assert_eq!(encoded[0], TAG_GENERALIZED_TIME);
        assert_eq!(encoded[1], 15); // length
        assert_eq!(&encoded[2..], b"20260408120000Z");
    }

    #[test]
    fn test_der_bit_string_32bit_flags() {
        let encoded = der_bit_string(&[0x40, 0x81, 0x00, 0x10], 0);
        assert_eq!(encoded[0], TAG_BIT_STRING);
        assert_eq!(encoded[1], 5); // 1 unused-bits byte + 4 bytes
        assert_eq!(encoded[2], 0); // 0 unused bits
        assert_eq!(&encoded[3..], &[0x40, 0x81, 0x00, 0x10]);
    }

    #[test]
    fn test_der_general_string() {
        let encoded = der_general_string("EXAMPLE.COM");
        assert_eq!(encoded[0], TAG_GENERAL_STRING);
        assert_eq!(encoded[1], 11);
        assert_eq!(&encoded[2..], b"EXAMPLE.COM");
    }

    // DER primitive tests (der_length, der_tlv, parse_der_length, parse_der_tlv)
    // live in `auth::der::tests`.

    // -----------------------------------------------------------------------
    // Parse helper tests (Kerberos-specific)
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_der_integer_roundtrip() {
        for val in [0, 1, 5, 127, 128, 255, 256, 1000, -1, -128, -129] {
            let encoded = der_integer(val);
            let parsed = parse_der_integer(&encoded).unwrap();
            assert_eq!(parsed, val, "roundtrip failed for {val}");
        }
    }

    #[test]
    fn test_parse_der_octet_string_roundtrip() {
        let data = vec![0x01, 0x02, 0x03, 0xff];
        let encoded = der_octet_string(&data);
        let parsed = parse_der_octet_string(&encoded).unwrap();
        assert_eq!(parsed, data);
    }

    #[test]
    fn test_parse_der_general_string_roundtrip() {
        let encoded = der_general_string("EXAMPLE.COM");
        let parsed = parse_der_general_string(&encoded).unwrap();
        assert_eq!(parsed, "EXAMPLE.COM");
    }

    #[test]
    fn test_parse_der_generalized_time_roundtrip() {
        let encoded = der_generalized_time("20260408120000Z");
        let parsed = parse_der_generalized_time(&encoded).unwrap();
        assert_eq!(parsed, "20260408120000Z");
    }

    #[test]
    fn test_parse_der_bit_string_roundtrip() {
        let bits = vec![0x40, 0x81, 0x00, 0x10];
        let encoded = der_bit_string(&bits, 0);
        let (parsed_bits, unused) = parse_der_bit_string(&encoded).unwrap();
        assert_eq!(parsed_bits, bits);
        assert_eq!(unused, 0);
    }

    // -----------------------------------------------------------------------
    // Encoding tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_encode_as_req_application_tag() {
        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };
        let sname = PrincipalName {
            name_type: 2,
            name_string: vec!["krbtgt".to_string(), "EXAMPLE.COM".to_string()],
        };
        let encoded = encode_as_req(
            &cname,
            "EXAMPLE.COM",
            &sname,
            12345,
            &[EncryptionType::Aes256CtsHmacSha196],
            &[],
        );
        // APPLICATION [10] = 0x6a
        assert_eq!(encoded[0], 0x6a, "AS-REQ must start with APPLICATION [10]");
    }

    #[test]
    fn test_encode_as_req_contains_pvno_and_msg_type() {
        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };
        let sname = PrincipalName {
            name_type: 2,
            name_string: vec!["krbtgt".to_string(), "EXAMPLE.COM".to_string()],
        };
        let encoded = encode_as_req(
            &cname,
            "EXAMPLE.COM",
            &sname,
            12345,
            &[EncryptionType::Aes256CtsHmacSha196],
            &[],
        );
        // Should contain pvno=5 somewhere: a1 03 02 01 05
        let pvno_pattern = [0xa1, 0x03, 0x02, 0x01, 0x05];
        assert!(
            contains_subsequence(&encoded, &pvno_pattern),
            "AS-REQ must contain pvno=5"
        );
        // Should contain msg-type=10: a2 03 02 01 0a
        let msg_type_pattern = [0xa2, 0x03, 0x02, 0x01, 0x0a];
        assert!(
            contains_subsequence(&encoded, &msg_type_pattern),
            "AS-REQ must contain msg-type=10"
        );
    }

    #[test]
    fn test_encode_tgs_req_application_tag() {
        let sname = PrincipalName {
            name_type: 2,
            name_string: vec!["cifs".to_string(), "server.example.com".to_string()],
        };
        let fake_ap_req = vec![0x6e, 0x03, 0x01, 0x02, 0x03];
        let encoded = encode_tgs_req(
            "EXAMPLE.COM",
            &sname,
            54321,
            &[EncryptionType::Aes256CtsHmacSha196],
            &fake_ap_req,
        );
        // APPLICATION [12] = 0x6c
        assert_eq!(encoded[0], 0x6c, "TGS-REQ must start with APPLICATION [12]");
    }

    #[test]
    fn test_encode_tgs_req_contains_msg_type_12() {
        let sname = PrincipalName {
            name_type: 2,
            name_string: vec!["cifs".to_string(), "server.example.com".to_string()],
        };
        let fake_ap_req = vec![0x6e, 0x03, 0x01, 0x02, 0x03];
        let encoded = encode_tgs_req(
            "EXAMPLE.COM",
            &sname,
            54321,
            &[EncryptionType::Aes256CtsHmacSha196],
            &fake_ap_req,
        );
        // msg-type=12: a2 03 02 01 0c
        let msg_type_pattern = [0xa2, 0x03, 0x02, 0x01, 0x0c];
        assert!(
            contains_subsequence(&encoded, &msg_type_pattern),
            "TGS-REQ must contain msg-type=12"
        );
    }

    #[test]
    fn test_encode_ap_req_application_tag() {
        let ticket = make_test_ticket();
        let auth = EncryptedData {
            etype: 18,
            kvno: None,
            cipher: vec![0xaa, 0xbb],
        };
        let encoded = encode_ap_req(&ticket, &auth, false);
        // APPLICATION [14] = 0x6e
        assert_eq!(encoded[0], 0x6e, "AP-REQ must start with APPLICATION [14]");
    }

    #[test]
    fn test_encode_ap_req_contains_pvno_and_msg_type() {
        let ticket = make_test_ticket();
        let auth = EncryptedData {
            etype: 18,
            kvno: None,
            cipher: vec![0xaa, 0xbb],
        };
        let encoded = encode_ap_req(&ticket, &auth, false);
        // pvno=5: a0 03 02 01 05
        let pvno_pattern = [0xa0, 0x03, 0x02, 0x01, 0x05];
        assert!(
            contains_subsequence(&encoded, &pvno_pattern),
            "AP-REQ must contain pvno=5"
        );
        // msg-type=14: a1 03 02 01 0e
        let msg_type_pattern = [0xa1, 0x03, 0x02, 0x01, 0x0e];
        assert!(
            contains_subsequence(&encoded, &msg_type_pattern),
            "AP-REQ must contain msg-type=14"
        );
    }

    #[test]
    fn test_encode_authenticator_application_tag() {
        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };
        let encoded = encode_authenticator(
            "EXAMPLE.COM",
            &cname,
            "20260408120000Z",
            123456,
            None,
            None,
            None,
        );
        // APPLICATION [2] = 0x62
        assert_eq!(
            encoded[0], 0x62,
            "Authenticator must start with APPLICATION [2]"
        );
    }

    #[test]
    fn test_encode_authenticator_with_subkey() {
        let cname = PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        };
        let subkey_value = vec![0x01; 32];
        let encoded = encode_authenticator(
            "EXAMPLE.COM",
            &cname,
            "20260408120000Z",
            0,
            Some((&subkey_value, 18)),
            Some(42),
            None,
        );
        assert_eq!(encoded[0], 0x62);
        // Should contain the subkey context tag [6] = 0xa6
        assert!(
            contains_subsequence(&encoded, &[0xa6]),
            "Authenticator with subkey must contain [6]"
        );
        // Should contain seq-number context tag [7] = 0xa7
        assert!(
            contains_subsequence(&encoded, &[0xa7]),
            "Authenticator with seq-number must contain [7]"
        );
    }

    #[test]
    fn test_encode_pa_enc_timestamp() {
        let encoded = encode_pa_enc_timestamp("20260408120000Z", 123456);
        // Should be a SEQUENCE starting with 0x30
        assert_eq!(encoded[0], TAG_SEQUENCE);
        // Should contain [0] with GeneralizedTime
        assert!(contains_subsequence(&encoded, &[0xa0]));
        // Should contain [1] with INTEGER
        assert!(contains_subsequence(&encoded, &[0xa1]));
    }

    // -----------------------------------------------------------------------
    // Parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_kdc_rep_as_rep() {
        let rep_bytes = build_test_kdc_rep(11);
        let rep = parse_kdc_rep(&rep_bytes).unwrap();
        assert_eq!(rep.msg_type, 11);
        assert_eq!(rep.crealm, "EXAMPLE.COM");
        assert_eq!(rep.cname.name_type, 1);
        assert_eq!(rep.cname.name_string, vec!["user"]);
        assert_eq!(rep.ticket.realm, "EXAMPLE.COM");
        assert_eq!(rep.enc_part.etype, 18);
    }

    #[test]
    fn test_parse_kdc_rep_tgs_rep() {
        let rep_bytes = build_test_kdc_rep(13);
        let rep = parse_kdc_rep(&rep_bytes).unwrap();
        assert_eq!(rep.msg_type, 13);
    }

    #[test]
    fn test_parse_krb_error() {
        let err_bytes = build_test_krb_error(25); // KDC_ERR_PREAUTH_REQUIRED
        let err = parse_krb_error(&err_bytes).unwrap();
        assert_eq!(err.error_code, 25);
        assert_eq!(err.realm, "EXAMPLE.COM");
        assert_eq!(err.sname.name_type, 2);
        assert_eq!(err.sname.name_string, vec!["krbtgt", "EXAMPLE.COM"]);
    }

    #[test]
    fn test_parse_ticket_roundtrip() {
        let ticket = make_test_ticket();
        let encoded = encode_ticket(&ticket);
        let parsed = parse_ticket(&encoded).unwrap();
        assert_eq!(parsed.tkt_vno, 5);
        assert_eq!(parsed.realm, "EXAMPLE.COM");
        assert_eq!(parsed.sname.name_type, 2);
        assert_eq!(parsed.sname.name_string, vec!["krbtgt", "EXAMPLE.COM"]);
        assert_eq!(parsed.enc_part.etype, 18);
        assert_eq!(parsed.enc_part.cipher, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_parse_enc_kdc_rep_part() {
        let part_bytes = build_test_enc_kdc_rep_part();
        let part = parse_enc_kdc_rep_part(&part_bytes).unwrap();
        assert_eq!(part.key.keytype, 18);
        assert_eq!(part.key.keyvalue, vec![0x01; 32]);
        assert_eq!(part.nonce, 12345);
        assert_eq!(part.authtime, "20260408120000Z");
        assert_eq!(part.endtime, "20260409120000Z");
        assert_eq!(part.srealm, "EXAMPLE.COM");
        assert_eq!(part.sname.name_type, 2);
    }

    // -----------------------------------------------------------------------
    // Roundtrip tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_principal_name_roundtrip() {
        let name = PrincipalName {
            name_type: 2,
            name_string: vec!["cifs".to_string(), "server.example.com".to_string()],
        };
        let encoded = encode_principal_name(&name);
        let parsed = parse_principal_name(&encoded).unwrap();
        assert_eq!(parsed, name);
    }

    #[test]
    fn test_encrypted_data_roundtrip() {
        let ed = EncryptedData {
            etype: 17,
            kvno: Some(3),
            cipher: vec![0x01, 0x02, 0x03, 0x04],
        };
        let encoded = encode_encrypted_data(&ed);
        let parsed = parse_encrypted_data(&encoded).unwrap();
        assert_eq!(parsed, ed);
    }

    #[test]
    fn test_encrypted_data_no_kvno_roundtrip() {
        let ed = EncryptedData {
            etype: 23,
            kvno: None,
            cipher: vec![0xff; 16],
        };
        let encoded = encode_encrypted_data(&ed);
        let parsed = parse_encrypted_data(&encoded).unwrap();
        assert_eq!(parsed, ed);
    }

    #[test]
    fn test_ticket_roundtrip() {
        let ticket = make_test_ticket();
        let encoded = encode_ticket(&ticket);
        let parsed = parse_ticket(&encoded).unwrap();
        // Compare fields (raw_bytes differs: None vs Some).
        assert_eq!(parsed.tkt_vno, ticket.tkt_vno);
        assert_eq!(parsed.realm, ticket.realm);
        assert_eq!(parsed.sname, ticket.sname);
        assert_eq!(parsed.enc_part, ticket.enc_part);
        // Parsed ticket should have raw_bytes.
        assert!(parsed.raw_bytes.is_some());
        assert_eq!(parsed.raw_bytes.as_ref().unwrap(), &encoded);
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn contains_subsequence(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn make_test_ticket() -> Ticket {
        Ticket {
            tkt_vno: 5,
            realm: "EXAMPLE.COM".to_string(),
            sname: PrincipalName {
                name_type: 2,
                name_string: vec!["krbtgt".to_string(), "EXAMPLE.COM".to_string()],
            },
            enc_part: EncryptedData {
                etype: 18,
                kvno: Some(2),
                cipher: vec![0xde, 0xad, 0xbe, 0xef],
            },
            raw_bytes: None,
        }
    }

    /// Build a test KDC-REP (AS-REP or TGS-REP) in DER.
    fn build_test_kdc_rep(msg_type_val: i32) -> Vec<u8> {
        // RFC 4120 section 5.4.2: KDC-REP fields start at [0]
        let pvno = der_context(0, &der_integer(5));
        let msg_type = der_context(1, &der_integer(msg_type_val));
        let crealm = der_context(3, &der_general_string("EXAMPLE.COM"));

        let cname_inner = encode_principal_name(&PrincipalName {
            name_type: 1,
            name_string: vec!["user".to_string()],
        });
        let cname = der_context(4, &cname_inner);

        let ticket = der_context(5, &encode_ticket(&make_test_ticket()));

        let enc_part_inner = encode_encrypted_data(&EncryptedData {
            etype: 18,
            kvno: Some(1),
            cipher: vec![0xca, 0xfe],
        });
        let enc_part = der_context(6, &enc_part_inner);

        let seq = der_sequence(&[&pvno, &msg_type, &crealm, &cname, &ticket, &enc_part]);

        let app_tag = match msg_type_val {
            11 => 11, // AS-REP
            13 => 13, // TGS-REP
            _ => panic!("unexpected msg_type_val"),
        };
        der_application(app_tag, &seq)
    }

    /// Build a test KRB-ERROR in DER.
    fn build_test_krb_error(error_code_val: i32) -> Vec<u8> {
        let pvno = der_context(0, &der_integer(5));
        let msg_type = der_context(1, &der_integer(30));
        let stime = der_context(4, &der_generalized_time("20260408120000Z"));
        let susec = der_context(5, &der_integer(0));
        let error_code = der_context(6, &der_integer(error_code_val));
        let realm = der_context(9, &der_general_string("EXAMPLE.COM"));
        let sname_inner = encode_principal_name(&PrincipalName {
            name_type: 2,
            name_string: vec!["krbtgt".to_string(), "EXAMPLE.COM".to_string()],
        });
        let sname = der_context(10, &sname_inner);

        let seq = der_sequence(&[
            &pvno,
            &msg_type,
            &stime,
            &susec,
            &error_code,
            &realm,
            &sname,
        ]);
        // APPLICATION [30] = 0x7e
        der_application(30, &seq)
    }

    /// Build a test EncKDCRepPart in DER.
    fn build_test_enc_kdc_rep_part() -> Vec<u8> {
        // key [0]: EncryptionKey { keytype=18, keyvalue=0x01*32 }
        let kt = der_context(0, &der_integer(18));
        let kv = der_context(1, &der_octet_string(&[0x01; 32]));
        let key_seq = der_sequence(&[&kt, &kv]);
        let key = der_context(0, &key_seq);

        // last-req [1]: minimal (empty sequence)
        let last_req = der_context(1, &der_sequence(&[]));

        // nonce [2]
        let nonce = der_context(2, &der_integer_u32(12345));

        // flags [4]: BIT STRING
        let flags = der_context(4, &der_bit_string(&[0x50, 0x80, 0x00, 0x00], 0));

        // authtime [5]
        let authtime = der_context(5, &der_generalized_time("20260408120000Z"));

        // endtime [7]
        let endtime = der_context(7, &der_generalized_time("20260409120000Z"));

        // srealm [9]
        let srealm = der_context(9, &der_general_string("EXAMPLE.COM"));

        // sname [10]
        let sname_inner = encode_principal_name(&PrincipalName {
            name_type: 2,
            name_string: vec!["krbtgt".to_string(), "EXAMPLE.COM".to_string()],
        });
        let sname = der_context(10, &sname_inner);

        let seq = der_sequence(&[
            &key, &last_req, &nonce, &flags, &authtime, &endtime, &srealm, &sname,
        ]);

        // Wrap in APPLICATION [25] (EncASRepPart)
        der_application(25, &seq)
    }

    /// Covers every message carrying a key (`EncKdcRepPart`, `EncApRepPart`),
    /// since they hold it as an `EncryptionKey`.
    #[test]
    fn an_encryption_keys_debug_output_hides_the_key() {
        let key = EncryptionKey {
            keytype: 18,
            keyvalue: vec![0xAB; 32],
        };
        let printed = format!("{key:?}");
        assert!(!printed.contains("171"), "{printed}");
        assert!(printed.contains("keytype: 18"), "{printed}");
    }
}
