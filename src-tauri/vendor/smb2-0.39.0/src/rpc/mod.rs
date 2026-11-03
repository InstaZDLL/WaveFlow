//! Named pipe RPC (MS-RPCE / NDR) for share enumeration.
//!
//! This module encodes and decodes DCE/RPC PDUs used over SMB2 named pipes.
//! The exchange for share enumeration is:
//!
//! 1. Open `\pipe\srvsvc` via CREATE
//! 2. Send RPC BIND request (type 11)
//! 3. Receive RPC BIND_ACK response (type 12)
//! 4. Send RPC REQUEST with NetShareEnumAll (type 0, opnum 15)
//! 5. Receive RPC RESPONSE with results (type 2)
//! 6. CLOSE the pipe
//!
//! Most users don't need this module directly -- use
//! [`SmbClient::list_shares`](crate::SmbClient::list_shares) instead.
//! The [`ShareInfo`](crate::ShareInfo) type is re-exported at the crate root.

pub mod srvsvc;

use crate::error::Result;
use crate::pack::guid::Guid;
use crate::pack::{Pack, ReadCursor, WriteCursor};
use crate::Error;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// RPC version 5.0 (connection-oriented).
const RPC_VERSION_MAJOR: u8 = 5;
/// RPC minor version.
const RPC_VERSION_MINOR: u8 = 0;

/// Data representation: little-endian, ASCII character set, IEEE floating point.
const DATA_REP: [u8; 4] = [0x10, 0x00, 0x00, 0x00];

/// RPC PDU type: REQUEST.
const PDU_TYPE_REQUEST: u8 = 0;
/// RPC PDU type: RESPONSE.
const PDU_TYPE_RESPONSE: u8 = 2;
/// RPC PDU type: BIND.
const PDU_TYPE_BIND: u8 = 11;
/// RPC PDU type: BIND_ACK.
const PDU_TYPE_BIND_ACK: u8 = 12;

/// Default maximum transmit fragment size.
const MAX_XMIT_FRAG: u16 = 4280;
/// Default maximum receive fragment size.
const MAX_RECV_FRAG: u16 = 4280;

/// PFC flags: first fragment.
const PFC_FIRST_FRAG: u8 = 0x01;
/// PFC flags: last fragment.
const PFC_LAST_FRAG: u8 = 0x02;

/// srvsvc abstract syntax UUID: `4B324FC8-1670-01D3-1278-5A47BF6EE188`.
const SRVSVC_UUID: Guid = Guid {
    data1: 0x4B324FC8,
    data2: 0x1670,
    data3: 0x01D3,
    data4: [0x12, 0x78, 0x5A, 0x47, 0xBF, 0x6E, 0xE1, 0x88],
};
/// srvsvc abstract syntax version.
const SRVSVC_VERSION: u32 = 3;

/// NDR transfer syntax UUID: `8A885D04-1CEB-11C9-9FE8-08002B104860`.
const NDR_UUID: Guid = Guid {
    data1: 0x8A885D04,
    data2: 0x1CEB,
    data3: 0x11C9,
    data4: [0x9F, 0xE8, 0x08, 0x00, 0x2B, 0x10, 0x48, 0x60],
};
/// NDR transfer syntax version.
const NDR_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// RPC PDU common header size
// ---------------------------------------------------------------------------

/// Size of the RPC PDU common header (16 bytes).
const RPC_HEADER_SIZE: usize = 16;

// ---------------------------------------------------------------------------
// Build functions
// ---------------------------------------------------------------------------

/// Build an RPC BIND request for the srvsvc interface.
///
/// The BIND PDU negotiates the presentation context, binding the srvsvc
/// abstract syntax with the NDR transfer syntax.
pub fn build_srvsvc_bind(call_id: u32) -> Vec<u8> {
    let mut w = WriteCursor::with_capacity(72);

    // Common header (16 bytes) -- FragLength will be backpatched
    w.write_u8(RPC_VERSION_MAJOR);
    w.write_u8(RPC_VERSION_MINOR);
    w.write_u8(PDU_TYPE_BIND);
    w.write_u8(PFC_FIRST_FRAG | PFC_LAST_FRAG);
    w.write_bytes(&DATA_REP);
    let frag_len_pos = w.position();
    w.write_u16_le(0); // FragLength placeholder
    w.write_u16_le(0); // AuthLength
    w.write_u32_le(call_id);

    // BIND-specific fields
    w.write_u16_le(MAX_XMIT_FRAG);
    w.write_u16_le(MAX_RECV_FRAG);
    w.write_u32_le(0); // AssocGroup

    // Presentation context list
    w.write_u8(1); // NumCtxItems
    w.write_bytes(&[0, 0, 0]); // Reserved

    // Context item 0
    w.write_u16_le(0); // ContextId
    w.write_u8(1); // NumTransferSyntaxes
    w.write_u8(0); // Reserved

    // Abstract syntax: srvsvc
    SRVSVC_UUID.pack(&mut w);
    w.write_u32_le(SRVSVC_VERSION);

    // Transfer syntax: NDR
    NDR_UUID.pack(&mut w);
    w.write_u32_le(NDR_VERSION);

    // Backpatch FragLength
    let total_len = w.position();
    w.set_u16_le_at(frag_len_pos, total_len as u16);

    w.into_inner()
}

/// Parse an RPC BIND_ACK response.
///
/// Verifies that the server accepted the presentation context (result == 0).
/// Returns `Ok(())` on success, or an error if the bind was rejected or
/// the response is malformed.
pub fn parse_bind_ack(data: &[u8]) -> Result<()> {
    let mut r = ReadCursor::new(data);

    // Common header
    let version = r.read_u8()?;
    let version_minor = r.read_u8()?;
    if version != RPC_VERSION_MAJOR || version_minor != RPC_VERSION_MINOR {
        return Err(Error::invalid_data(format!(
            "unexpected RPC version {version}.{version_minor}, expected 5.0"
        )));
    }

    let ptype = r.read_u8()?;
    if ptype != PDU_TYPE_BIND_ACK {
        return Err(Error::invalid_data(format!(
            "expected BIND_ACK (type 12), got type {ptype}"
        )));
    }

    let _flags = r.read_u8()?;
    let _data_rep = r.read_bytes(4)?;
    let _frag_length = r.read_u16_le()?;
    let _auth_length = r.read_u16_le()?;
    let _call_id = r.read_u32_le()?;

    // BIND_ACK specific fields
    let _max_xmit_frag = r.read_u16_le()?;
    let _max_recv_frag = r.read_u16_le()?;
    let _assoc_group = r.read_u32_le()?;

    // Secondary address (variable length, padded to 4 bytes)
    let sec_addr_len = r.read_u16_le()?;
    r.skip(sec_addr_len as usize)?;
    // Align to 4 bytes after secondary address (the 2-byte length + string)
    let consumed = 2 + sec_addr_len as usize;
    let padding = (4 - (consumed % 4)) % 4;
    r.skip(padding)?;

    // Result list
    let num_results = r.read_u8()?;
    r.skip(3)?; // Reserved

    if num_results == 0 {
        return Err(Error::invalid_data("BIND_ACK has no context results"));
    }

    // Check first result
    let result = r.read_u16_le()?;
    if result != 0 {
        let reason = r.read_u16_le()?;
        return Err(Error::invalid_data(format!(
            "BIND rejected: result={result}, reason={reason}"
        )));
    }

    Ok(())
}

/// Build an RPC REQUEST PDU wrapping the given stub data.
///
/// The caller provides the NDR-encoded stub (the operation payload) and the
/// operation number.
pub fn build_request(call_id: u32, opnum: u16, stub_data: &[u8]) -> Vec<u8> {
    let mut w = WriteCursor::with_capacity(RPC_HEADER_SIZE + 8 + stub_data.len());

    // Common header
    w.write_u8(RPC_VERSION_MAJOR);
    w.write_u8(RPC_VERSION_MINOR);
    w.write_u8(PDU_TYPE_REQUEST);
    w.write_u8(PFC_FIRST_FRAG | PFC_LAST_FRAG);
    w.write_bytes(&DATA_REP);
    let frag_len_pos = w.position();
    w.write_u16_le(0); // FragLength placeholder
    w.write_u16_le(0); // AuthLength
    w.write_u32_le(call_id);

    // REQUEST specific fields
    w.write_u32_le(stub_data.len() as u32); // AllocHint
    w.write_u16_le(0); // ContextId
    w.write_u16_le(opnum);

    // Stub data
    w.write_bytes(stub_data);

    // Backpatch FragLength
    let total_len = w.position();
    w.set_u16_le_at(frag_len_pos, total_len as u16);

    w.into_inner()
}

/// Parse a single RPC RESPONSE PDU, returning its stub data and whether it is
/// the final fragment (`PFC_LAST_FRAG` set).
///
/// DCE/RPC servers may split a large response across several fragment PDUs,
/// clearing `PFC_LAST_FRAG` on every fragment but the last (MS-RPCE 2.2.2.6).
/// Callers reassemble by concatenating each fragment's stub until `is_last` is
/// `true`. See `client::shares` for the read-and-reassemble loop.
pub fn parse_response_fragment(data: &[u8]) -> Result<(&[u8], bool)> {
    let mut r = ReadCursor::new(data);

    // Common header
    let version = r.read_u8()?;
    let version_minor = r.read_u8()?;
    if version != RPC_VERSION_MAJOR || version_minor != RPC_VERSION_MINOR {
        return Err(Error::invalid_data(format!(
            "unexpected RPC version {version}.{version_minor}, expected 5.0"
        )));
    }

    let ptype = r.read_u8()?;
    if ptype != PDU_TYPE_RESPONSE {
        return Err(Error::invalid_data(format!(
            "expected RESPONSE (type 2), got type {ptype}"
        )));
    }

    let flags = r.read_u8()?;
    let _data_rep = r.read_bytes(4)?;
    let frag_length = r.read_u16_le()? as usize;
    let _auth_length = r.read_u16_le()?;
    let _call_id = r.read_u32_le()?;

    // RESPONSE specific fields
    let _alloc_hint = r.read_u32_le()?;
    let _context_id = r.read_u16_le()?;
    let _cancel_count = r.read_u8()?;
    let _reserved = r.read_u8()?;

    // Stub data is the rest (up to frag_length).
    let header_consumed = r.position();
    if frag_length < header_consumed {
        return Err(Error::invalid_data(format!(
            "RPC frag_length {frag_length} shorter than header {header_consumed}"
        )));
    }
    let stub_data = r.read_bytes(frag_length - header_consumed)?;

    let is_last = flags & PFC_LAST_FRAG != 0;
    Ok((stub_data, is_last))
}

/// Parse an RPC RESPONSE PDU, returning the stub data.
///
/// Validates the PDU header and extracts the embedded stub data for
/// further NDR decoding. Assumes a single, complete fragment; for fragmented
/// responses use [`parse_response_fragment`] and reassemble.
pub fn parse_response(data: &[u8]) -> Result<&[u8]> {
    parse_response_fragment(data).map(|(stub, _is_last)| stub)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Unpack;

    #[test]
    fn bind_request_has_correct_header() {
        let pdu = build_srvsvc_bind(1);

        assert_eq!(pdu[0], RPC_VERSION_MAJOR, "version major");
        assert_eq!(pdu[1], RPC_VERSION_MINOR, "version minor");
        assert_eq!(pdu[2], PDU_TYPE_BIND, "packet type");
        assert_eq!(pdu[3], PFC_FIRST_FRAG | PFC_LAST_FRAG, "flags");

        // Data representation
        assert_eq!(&pdu[4..8], &DATA_REP);

        // FragLength should match actual PDU length
        let frag_len = u16::from_le_bytes([pdu[8], pdu[9]]);
        assert_eq!(frag_len as usize, pdu.len());

        // AuthLength = 0
        let auth_len = u16::from_le_bytes([pdu[10], pdu[11]]);
        assert_eq!(auth_len, 0);

        // CallId = 1
        let call_id = u32::from_le_bytes([pdu[12], pdu[13], pdu[14], pdu[15]]);
        assert_eq!(call_id, 1);
    }

    #[test]
    fn bind_request_contains_srvsvc_uuid() {
        let pdu = build_srvsvc_bind(1);

        // After common header (16) + MaxXmitFrag(2) + MaxRecvFrag(2) + AssocGroup(4) +
        // NumCtxItems(1) + Reserved(3) + ContextId(2) + NumTransferSyntaxes(1) + Reserved(1) = 32
        let uuid_offset = 32;

        // Extract the abstract syntax UUID bytes
        let mut cursor = ReadCursor::new(&pdu[uuid_offset..]);
        let guid = Guid::unpack(&mut cursor).unwrap();
        assert_eq!(guid, SRVSVC_UUID);

        let version = cursor.read_u32_le().unwrap();
        assert_eq!(version, SRVSVC_VERSION);
    }

    #[test]
    fn bind_request_contains_ndr_transfer_syntax() {
        let pdu = build_srvsvc_bind(1);

        // Transfer syntax starts after abstract syntax (UUID=16 + version=4 = 20 bytes after uuid_offset)
        let transfer_offset = 32 + 20;

        let mut cursor = ReadCursor::new(&pdu[transfer_offset..]);
        let guid = Guid::unpack(&mut cursor).unwrap();
        assert_eq!(guid, NDR_UUID);

        let version = cursor.read_u32_le().unwrap();
        assert_eq!(version, NDR_VERSION);
    }

    #[test]
    fn bind_request_total_length() {
        let pdu = build_srvsvc_bind(1);
        // 16 (header) + 4 (max frags) + 4 (assoc) + 4 (ctx list header) +
        // 4 (ctx item header) + 20 (abstract) + 20 (transfer) = 72
        assert_eq!(pdu.len(), 72);
    }

    #[test]
    fn parse_valid_bind_ack() {
        let ack = build_test_bind_ack(0); // result = 0 = accepted
        assert!(parse_bind_ack(&ack).is_ok());
    }

    #[test]
    fn parse_rejected_bind_ack() {
        let ack = build_test_bind_ack(2); // result = 2 = provider_rejection
        let err = parse_bind_ack(&ack).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("rejected"),
            "error should mention rejection: {msg}"
        );
    }

    #[test]
    fn parse_bind_ack_wrong_version() {
        let mut ack = build_test_bind_ack(0);
        ack[0] = 4; // wrong version
        assert!(parse_bind_ack(&ack).is_err());
    }

    #[test]
    fn parse_bind_ack_wrong_type() {
        let mut ack = build_test_bind_ack(0);
        ack[2] = PDU_TYPE_BIND; // wrong type
        assert!(parse_bind_ack(&ack).is_err());
    }

    #[test]
    fn request_pdu_has_correct_opnum() {
        let stub = vec![0xAA, 0xBB, 0xCC];
        let pdu = build_request(1, 15, &stub);

        // OpNum is at offset 22 (header=16 + AllocHint=4 + ContextId=2)
        let opnum = u16::from_le_bytes([pdu[22], pdu[23]]);
        assert_eq!(opnum, 15);
    }

    #[test]
    fn request_pdu_has_correct_alloc_hint() {
        let stub = vec![0xAA, 0xBB, 0xCC];
        let pdu = build_request(1, 15, &stub);

        let alloc_hint = u32::from_le_bytes([pdu[16], pdu[17], pdu[18], pdu[19]]);
        assert_eq!(alloc_hint, 3);
    }

    #[test]
    fn request_pdu_contains_stub_data() {
        let stub = vec![0xAA, 0xBB, 0xCC];
        let pdu = build_request(1, 15, &stub);

        // Stub starts at offset 24 (header=16 + request fields=8)
        assert_eq!(&pdu[24..], &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn request_pdu_frag_length_matches() {
        let stub = vec![0xAA, 0xBB, 0xCC];
        let pdu = build_request(1, 15, &stub);

        let frag_len = u16::from_le_bytes([pdu[8], pdu[9]]);
        assert_eq!(frag_len as usize, pdu.len());
    }

    #[test]
    fn parse_response_extracts_stub() {
        let stub = b"hello stub data";
        let response_pdu = build_test_response(1, stub);

        let extracted = parse_response(&response_pdu).unwrap();
        assert_eq!(extracted, stub);
    }

    #[test]
    fn parse_response_wrong_version() {
        let mut pdu = build_test_response(1, b"data");
        pdu[0] = 4; // wrong version
        assert!(parse_response(&pdu).is_err());
    }

    #[test]
    fn parse_response_fragment_reports_last_flag() {
        // build_test_response sets PFC_FIRST_FRAG | PFC_LAST_FRAG.
        let pdu = build_test_response(1, b"stub");
        let (stub, is_last) = parse_response_fragment(&pdu).unwrap();
        assert_eq!(stub, b"stub");
        assert!(is_last, "FIRST|LAST PDU should be the last fragment");

        // Clear PFC_LAST_FRAG in the flags byte: now it's a non-final fragment.
        let mut frag = pdu.clone();
        frag[3] &= !PFC_LAST_FRAG;
        let (stub, is_last) = parse_response_fragment(&frag).unwrap();
        assert_eq!(stub, b"stub");
        assert!(!is_last, "FIRST-only PDU should not be the last fragment");
    }

    #[test]
    fn parse_response_rejects_frag_length_shorter_than_header() {
        let mut pdu = build_test_response(1, b"data");
        // FragLength lives at offset 8 (u16 LE); set it below the 24-byte header.
        pdu[8] = 4;
        pdu[9] = 0;
        assert!(parse_response(&pdu).is_err());
    }

    #[test]
    fn parse_response_wrong_type() {
        let mut pdu = build_test_response(1, b"data");
        pdu[2] = PDU_TYPE_REQUEST; // wrong type
        assert!(parse_response(&pdu).is_err());
    }

    // -- Test helpers --

    /// Build a minimal BIND_ACK for testing.
    fn build_test_bind_ack(result: u16) -> Vec<u8> {
        let mut w = WriteCursor::with_capacity(64);

        // Common header
        w.write_u8(RPC_VERSION_MAJOR);
        w.write_u8(RPC_VERSION_MINOR);
        w.write_u8(PDU_TYPE_BIND_ACK);
        w.write_u8(PFC_FIRST_FRAG | PFC_LAST_FRAG);
        w.write_bytes(&DATA_REP);
        let frag_len_pos = w.position();
        w.write_u16_le(0); // FragLength placeholder
        w.write_u16_le(0); // AuthLength
        w.write_u32_le(1); // CallId

        // BIND_ACK specific
        w.write_u16_le(MAX_XMIT_FRAG);
        w.write_u16_le(MAX_RECV_FRAG);
        w.write_u32_le(0x12345); // AssocGroup

        // Secondary address: "\pipe\srvsvc\0" (empty for simplicity -- use length 0)
        w.write_u16_le(0); // SecAddrLen = 0
        w.write_bytes(&[0, 0]); // Padding to 4-byte alignment

        // Result list
        w.write_u8(1); // NumResults
        w.write_bytes(&[0, 0, 0]); // Reserved

        // Result entry
        w.write_u16_le(result); // Result
        w.write_u16_le(0); // Reason
                           // Transfer syntax (16 bytes UUID + 4 bytes version)
        NDR_UUID.pack(&mut w);
        w.write_u32_le(NDR_VERSION);

        let total_len = w.position();
        w.set_u16_le_at(frag_len_pos, total_len as u16);

        w.into_inner()
    }

    /// Build a minimal RPC RESPONSE PDU wrapping the given stub data.
    fn build_test_response(call_id: u32, stub: &[u8]) -> Vec<u8> {
        let mut w = WriteCursor::with_capacity(RPC_HEADER_SIZE + 8 + stub.len());

        w.write_u8(RPC_VERSION_MAJOR);
        w.write_u8(RPC_VERSION_MINOR);
        w.write_u8(PDU_TYPE_RESPONSE);
        w.write_u8(PFC_FIRST_FRAG | PFC_LAST_FRAG);
        w.write_bytes(&DATA_REP);
        let frag_len_pos = w.position();
        w.write_u16_le(0); // FragLength placeholder
        w.write_u16_le(0); // AuthLength
        w.write_u32_le(call_id);

        // RESPONSE specific
        w.write_u32_le(stub.len() as u32); // AllocHint
        w.write_u16_le(0); // ContextId
        w.write_u8(0); // CancelCount
        w.write_u8(0); // Reserved

        w.write_bytes(stub);

        let total_len = w.position();
        w.set_u16_le_at(frag_len_pos, total_len as u16);

        w.into_inner()
    }
}
