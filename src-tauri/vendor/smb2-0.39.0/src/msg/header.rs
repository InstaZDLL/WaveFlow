//! SMB2 packet header (64 bytes) and error response.
//!
//! The SMB2 header has two variants that share the same 64-byte layout:
//! - **Sync header:** bytes 32-35 = Reserved (u32), bytes 36-39 = TreeId (u32)
//! - **Async header:** bytes 32-39 = AsyncId (u64)
//!
//! The choice is determined by the `SMB2_FLAGS_ASYNC_COMMAND` bit in the Flags field.
//!
//! Reference: MS-SMB2 sections 2.2.1, 2.2.1.1, 2.2.1.2, 2.2.2.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::flags::HeaderFlags;
use crate::types::status::NtStatus;
use crate::types::{Command, CreditCharge, MessageId, SessionId, TreeId};
use crate::Error;

/// The 4-byte protocol identifier at the start of every SMB2 message.
pub const PROTOCOL_ID: [u8; 4] = [0xFE, b'S', b'M', b'B'];

/// SMB2 packet header (64 bytes).
///
/// Contains both sync and async variants. The `flags` field determines
/// which interpretation of bytes 32-39 is correct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Number of credits charged for this request.
    pub credit_charge: CreditCharge,
    /// In responses: NtStatus. In requests before SMB 3.x: Reserved.
    /// In requests for SMB 3.x: ChannelSequence (u16) + Reserved (u16).
    pub status: NtStatus,
    /// The command code for this packet.
    pub command: Command,
    /// In requests: credits requested. In responses: credits granted.
    pub credits: u16,
    /// Flags indicating how to process the operation.
    pub flags: HeaderFlags,
    /// Offset to the next command in a compound chain (0 = last/only).
    pub next_command: u32,
    /// Unique message identifier for request/response correlation.
    pub message_id: MessageId,
    /// Sync-only: tree identifier. None if async.
    pub tree_id: Option<TreeId>,
    /// Async-only: async identifier. None if sync.
    pub async_id: Option<u64>,
    /// Session identifier.
    pub session_id: SessionId,
    /// 16-byte message signature.
    pub signature: [u8; 16],
}

impl Header {
    pub const STRUCTURE_SIZE: u16 = 64;

    /// Total header size in bytes.
    pub const SIZE: usize = 64;

    /// Create a new request header for a given command.
    pub fn new_request(command: Command) -> Self {
        Self {
            credit_charge: CreditCharge(0),
            status: NtStatus::SUCCESS,
            command,
            credits: 1,
            flags: HeaderFlags::default(),
            next_command: 0,
            message_id: MessageId::default(),
            tree_id: Some(TreeId::default()),
            async_id: None,
            session_id: SessionId::default(),
            signature: [0u8; 16],
        }
    }

    /// Is this a response (vs request)?
    pub fn is_response(&self) -> bool {
        self.flags.is_response()
    }
}

impl Pack for Header {
    fn pack(&self, cursor: &mut WriteCursor) {
        // ProtocolId (4 bytes)
        cursor.write_bytes(&PROTOCOL_ID);
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // CreditCharge (2 bytes)
        cursor.write_u16_le(self.credit_charge.0);
        // Status (4 bytes)
        cursor.write_u32_le(self.status.0);
        // Command (2 bytes)
        cursor.write_u16_le(self.command.into());
        // CreditRequest/CreditResponse (2 bytes)
        cursor.write_u16_le(self.credits);
        // Flags (4 bytes)
        cursor.write_u32_le(self.flags.bits());
        // NextCommand (4 bytes)
        cursor.write_u32_le(self.next_command);
        // MessageId (8 bytes)
        cursor.write_u64_le(self.message_id.0);

        // Bytes 32-39: async or sync variant
        if self.flags.is_async() {
            // AsyncId (8 bytes)
            cursor.write_u64_le(self.async_id.unwrap_or(0));
        } else {
            // Reserved (4 bytes)
            cursor.write_u32_le(0);
            // TreeId (4 bytes)
            cursor.write_u32_le(self.tree_id.map_or(0, |t| t.0));
        }

        // SessionId (8 bytes)
        cursor.write_u64_le(self.session_id.0);
        // Signature (16 bytes)
        cursor.write_bytes(&self.signature);
    }
}

impl Unpack for Header {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // ProtocolId (4 bytes)
        let proto = cursor.read_bytes(4)?;
        if proto != PROTOCOL_ID {
            return Err(Error::invalid_data(format!(
                "invalid SMB2 protocol ID: expected {:02X?}, got {:02X?}",
                PROTOCOL_ID, proto
            )));
        }

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Header::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid SMB2 header structure size: expected {}, got {}",
                Header::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // CreditCharge (2 bytes)
        let credit_charge = CreditCharge(cursor.read_u16_le()?);

        // Status (4 bytes)
        let status = NtStatus(cursor.read_u32_le()?);

        // Command (2 bytes)
        let command_raw = cursor.read_u16_le()?;
        let command = Command::try_from(command_raw).map_err(|_| {
            Error::invalid_data(format!("invalid SMB2 command code: 0x{:04X}", command_raw))
        })?;

        // CreditRequest/CreditResponse (2 bytes)
        let credits = cursor.read_u16_le()?;

        // Flags (4 bytes)
        let flags = HeaderFlags::new(cursor.read_u32_le()?);

        // NextCommand (4 bytes)
        let next_command = cursor.read_u32_le()?;

        // MessageId (8 bytes)
        let message_id = MessageId(cursor.read_u64_le()?);

        // Bytes 32-39: async or sync variant
        let (tree_id, async_id) = if flags.is_async() {
            let async_id = cursor.read_u64_le()?;
            (None, Some(async_id))
        } else {
            let _reserved = cursor.read_u32_le()?;
            let tree_id = TreeId(cursor.read_u32_le()?);
            (Some(tree_id), None)
        };

        // SessionId (8 bytes)
        let session_id = SessionId(cursor.read_u64_le()?);

        // Signature (16 bytes)
        let sig_bytes = cursor.read_bytes(16)?;
        let mut signature = [0u8; 16];
        signature.copy_from_slice(sig_bytes);

        Ok(Header {
            credit_charge,
            status,
            command,
            credits,
            flags,
            next_command,
            message_id,
            tree_id,
            async_id,
            session_id,
            signature,
        })
    }
}

/// SMB2 ERROR Response body (spec section 2.2.2).
///
/// Sent by the server when a request fails. The structure is:
/// - StructureSize (2 bytes, must be 9)
/// - ErrorContextCount (1 byte)
/// - Reserved (1 byte)
/// - ByteCount (4 bytes)
/// - ErrorData (variable, ByteCount bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorResponse {
    /// Number of error contexts (SMB 3.1.1 only, otherwise 0).
    pub error_context_count: u8,
    /// Variable-length error data.
    pub error_data: Vec<u8>,
}

impl ErrorResponse {
    pub const STRUCTURE_SIZE: u16 = 9;

    /// `SMB2_ERROR_ID_SHARE_REDIRECT` (MS-SMB2 § 2.2.2.1).
    const ERROR_ID_SHARE_REDIRECT: u32 = 0x7264_5253;

    /// Whether any error context says this is a scale-out cluster redirect
    /// rather than whatever the status code usually means.
    ///
    /// `STATUS_BAD_NETWORK_NAME` carries an `SMB2_ERROR_ID_SHARE_REDIRECT`
    /// context when a server moves a share to another cluster node
    /// (§ 2.2.2.2.2), so the same status stands for two unrelated things and
    /// only the context tells them apart. A DFS namespace root refuses
    /// `TREE_CONNECT` with that same status, which is why this check exists.
    ///
    /// Contexts are `ErrorDataLength(4)` + `ErrorId(4)` + data, each starting
    /// on an 8-byte boundary relative to the start of the ERROR response — and
    /// the fixed part is exactly 8 bytes, so the same alignment holds inside
    /// `error_data`. A context that runs off the end ends the walk; a
    /// malformed tail cannot make this say "yes".
    pub fn is_share_redirect(&self) -> bool {
        if self.error_context_count == 0 {
            return false;
        }
        let mut pos = 0usize;
        for _ in 0..self.error_context_count {
            if pos + 8 > self.error_data.len() {
                return false;
            }
            let data_len = u32::from_le_bytes(
                self.error_data[pos..pos + 4]
                    .try_into()
                    .expect("4 bytes, just bounds-checked"),
            ) as usize;
            let error_id = u32::from_le_bytes(
                self.error_data[pos + 4..pos + 8]
                    .try_into()
                    .expect("4 bytes, just bounds-checked"),
            );
            if error_id == Self::ERROR_ID_SHARE_REDIRECT {
                return true;
            }
            // Next context, 8-byte aligned.
            let advance = 8usize
                .saturating_add(data_len)
                .checked_next_multiple_of(8)
                .unwrap_or(usize::MAX);
            match pos.checked_add(advance) {
                Some(next) if next > pos => pos = next,
                _ => return false,
            }
        }
        false
    }
}

impl Pack for ErrorResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // ErrorContextCount (1 byte)
        cursor.write_u8(self.error_context_count);
        // Reserved (1 byte)
        cursor.write_u8(0);
        // ByteCount (4 bytes)
        cursor.write_u32_le(self.error_data.len() as u32);
        // ErrorData (variable)
        cursor.write_bytes(&self.error_data);
    }
}

impl Unpack for ErrorResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid ErrorResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // ErrorContextCount (1 byte)
        let error_context_count = cursor.read_u8()?;

        // Reserved (1 byte)
        let _reserved = cursor.read_u8()?;

        // ByteCount (4 bytes)
        let byte_count = cursor.read_u32_le()? as usize;

        // ErrorData (variable)
        let error_data = if byte_count > 0 {
            cursor.read_bytes_bounded(byte_count)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(ErrorResponse {
            error_context_count,
            error_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Error context tests ─────────────────────────────────────────

    /// One SMB2 ERROR Context: length, id, then `body_len` bytes of body,
    /// padded out to the next 8-byte boundary.
    fn error_context(error_id: u32, body_len: usize) -> Vec<u8> {
        let mut ctx = Vec::new();
        ctx.extend_from_slice(&(body_len as u32).to_le_bytes());
        ctx.extend_from_slice(&error_id.to_le_bytes());
        ctx.resize(ctx.len() + body_len, 0xAB);
        while ctx.len() % 8 != 0 {
            ctx.push(0);
        }
        ctx
    }

    #[test]
    fn share_redirect_context_is_recognized() {
        let resp = ErrorResponse {
            error_context_count: 1,
            error_data: error_context(0x7264_5253, 48),
        };
        assert!(resp.is_share_redirect());
    }

    /// The one that matters for DFS: a plain `STATUS_BAD_NETWORK_NAME` carries
    /// no contexts at all, and must not read as a redirect.
    #[test]
    fn a_plain_error_response_is_not_a_share_redirect() {
        let resp = ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };
        assert!(!resp.is_share_redirect());
    }

    #[test]
    fn share_redirect_is_found_after_another_context() {
        let mut data = error_context(0x0000_0000, 12);
        data.extend_from_slice(&error_context(0x7264_5253, 48));
        let resp = ErrorResponse {
            error_context_count: 2,
            error_data: data,
        };
        assert!(resp.is_share_redirect());
    }

    /// A count or a length the buffer cannot back ends the walk. ❌ A
    /// malformed tail must never be able to answer "yes": that would turn a
    /// missing share into an unfollowable cluster redirect.
    #[test]
    fn a_malformed_context_chain_never_claims_a_redirect() {
        // Claims two contexts, holds one.
        let resp = ErrorResponse {
            error_context_count: 2,
            error_data: error_context(0x0000_0000, 8),
        };
        assert!(!resp.is_share_redirect());

        // A length that runs off the end.
        let mut data = Vec::new();
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        let resp = ErrorResponse {
            error_context_count: 2,
            error_data: data,
        };
        assert!(!resp.is_share_redirect());

        // A truncated first context.
        let resp = ErrorResponse {
            error_context_count: 1,
            error_data: vec![0x53, 0x52],
        };
        assert!(!resp.is_share_redirect());
    }

    // ── Header tests ────────────────────────────────────────────────

    #[test]
    fn pack_request_header_produces_64_bytes_with_correct_magic() {
        let header = Header::new_request(Command::Negotiate);
        let mut cursor = WriteCursor::new();
        header.pack(&mut cursor);
        let bytes = cursor.into_inner();

        assert_eq!(bytes.len(), Header::SIZE);
        assert_eq!(&bytes[0..4], &PROTOCOL_ID);
    }

    #[test]
    fn unpack_known_64_byte_buffer() {
        // Build a known buffer manually: sync Negotiate request
        let mut buf = [0u8; 64];
        // ProtocolId
        buf[0..4].copy_from_slice(&PROTOCOL_ID);
        // StructureSize = 64
        buf[4..6].copy_from_slice(&64u16.to_le_bytes());
        // CreditCharge = 1
        buf[6..8].copy_from_slice(&1u16.to_le_bytes());
        // Status = SUCCESS (0)
        buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        // Command = Negotiate (0)
        buf[12..14].copy_from_slice(&0u16.to_le_bytes());
        // Credits = 31
        buf[14..16].copy_from_slice(&31u16.to_le_bytes());
        // Flags = 0 (sync, request)
        buf[16..20].copy_from_slice(&0u32.to_le_bytes());
        // NextCommand = 0
        buf[20..24].copy_from_slice(&0u32.to_le_bytes());
        // MessageId = 42
        buf[24..32].copy_from_slice(&42u64.to_le_bytes());
        // Reserved = 0
        buf[32..36].copy_from_slice(&0u32.to_le_bytes());
        // TreeId = 7
        buf[36..40].copy_from_slice(&7u32.to_le_bytes());
        // SessionId = 0x1234
        buf[40..48].copy_from_slice(&0x1234u64.to_le_bytes());
        // Signature = all zeros
        // (already zero)

        let mut cursor = ReadCursor::new(&buf);
        let header = Header::unpack(&mut cursor).unwrap();

        assert_eq!(header.credit_charge, CreditCharge(1));
        assert_eq!(header.status, NtStatus::SUCCESS);
        assert_eq!(header.command, Command::Negotiate);
        assert_eq!(header.credits, 31);
        assert!(!header.flags.is_async());
        assert!(!header.flags.is_response());
        assert_eq!(header.next_command, 0);
        assert_eq!(header.message_id, MessageId(42));
        assert_eq!(header.tree_id, Some(TreeId(7)));
        assert_eq!(header.async_id, None);
        assert_eq!(header.session_id, SessionId(0x1234));
        assert_eq!(header.signature, [0u8; 16]);
    }

    #[test]
    fn roundtrip_sync_header() {
        let original = Header {
            credit_charge: CreditCharge(3),
            status: NtStatus::ACCESS_DENIED,
            command: Command::Read,
            credits: 10,
            flags: {
                let mut f = HeaderFlags::default();
                f.set_response();
                f
            },
            next_command: 0,
            message_id: MessageId(99),
            tree_id: Some(TreeId(42)),
            async_id: None,
            session_id: SessionId(0xDEAD_BEEF),
            signature: [
                0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
                0x0F, 0x10,
            ],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), Header::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = Header::unpack(&mut r).unwrap();

        assert_eq!(decoded.credit_charge, original.credit_charge);
        assert_eq!(decoded.status, original.status);
        assert_eq!(decoded.command, original.command);
        assert_eq!(decoded.credits, original.credits);
        assert_eq!(decoded.flags.bits(), original.flags.bits());
        assert_eq!(decoded.next_command, original.next_command);
        assert_eq!(decoded.message_id, original.message_id);
        assert_eq!(decoded.tree_id, original.tree_id);
        assert_eq!(decoded.async_id, original.async_id);
        assert_eq!(decoded.session_id, original.session_id);
        assert_eq!(decoded.signature, original.signature);
    }

    #[test]
    fn wrong_magic_bytes_returns_error() {
        let mut buf = [0u8; 64];
        // Wrong magic
        buf[0..4].copy_from_slice(&[0xFF, b'X', b'Y', b'Z']);
        buf[4..6].copy_from_slice(&64u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = Header::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("protocol ID"), "error was: {err}");
    }

    #[test]
    fn wrong_structure_size_returns_error() {
        let mut buf = [0u8; 64];
        buf[0..4].copy_from_slice(&PROTOCOL_ID);
        // Wrong structure size
        buf[4..6].copy_from_slice(&32u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = Header::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn async_header_pack_unpack() {
        let mut flags = HeaderFlags::default();
        flags.set_async();
        flags.set_response();

        let original = Header {
            credit_charge: CreditCharge(0),
            status: NtStatus::PENDING,
            command: Command::ChangeNotify,
            credits: 1,
            flags,
            next_command: 0,
            message_id: MessageId(8),
            tree_id: None,
            async_id: Some(0x0000_0000_0000_0008),
            session_id: SessionId(0x0000_0000_0853_27D7),
            signature: [0u8; 16],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), Header::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = Header::unpack(&mut r).unwrap();

        assert!(decoded.flags.is_async());
        assert_eq!(decoded.async_id, Some(8));
        assert_eq!(decoded.tree_id, None);
        assert_eq!(decoded.command, Command::ChangeNotify);
        assert_eq!(decoded.status, NtStatus::PENDING);
        assert_eq!(decoded.session_id, SessionId(0x0000_0000_0853_27D7));
    }

    #[test]
    fn sync_header_has_tree_id_and_no_async_id() {
        let header = Header::new_request(Command::Create);

        let mut w = WriteCursor::new();
        header.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = Header::unpack(&mut r).unwrap();

        assert!(!decoded.flags.is_async());
        assert!(decoded.tree_id.is_some());
        assert_eq!(decoded.async_id, None);
    }

    #[test]
    fn signature_field_preserved() {
        let sig = [
            0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
            0x99, 0x00,
        ];
        let mut header = Header::new_request(Command::Echo);
        header.signature = sig;

        let mut w = WriteCursor::new();
        header.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = Header::unpack(&mut r).unwrap();

        assert_eq!(decoded.signature, sig);
    }

    #[test]
    fn new_request_produces_correct_defaults() {
        let header = Header::new_request(Command::Write);

        assert_eq!(header.command, Command::Write);
        assert_eq!(header.credit_charge, CreditCharge(0));
        assert_eq!(header.status, NtStatus::SUCCESS);
        assert_eq!(header.credits, 1);
        assert!(!header.flags.is_response());
        assert!(!header.flags.is_async());
        assert_eq!(header.next_command, 0);
        assert_eq!(header.message_id, MessageId(0));
        assert_eq!(header.tree_id, Some(TreeId(0)));
        assert_eq!(header.async_id, None);
        assert_eq!(header.session_id, SessionId(0));
        assert_eq!(header.signature, [0u8; 16]);
        assert!(!header.is_response());
    }

    // ── ErrorResponse tests ─────────────────────────────────────────

    #[test]
    fn error_response_pack_unpack_empty() {
        let original = ErrorResponse {
            error_context_count: 0,
            error_data: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // StructureSize(2) + ErrorContextCount(1) + Reserved(1) + ByteCount(4) = 8
        assert_eq!(bytes.len(), 8);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ErrorResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.error_context_count, 0);
        assert!(decoded.error_data.is_empty());
    }

    #[test]
    fn error_response_pack_unpack_with_data() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE];
        let original = ErrorResponse {
            error_context_count: 1,
            error_data: data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // 8 bytes fixed + 6 bytes data
        assert_eq!(bytes.len(), 14);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ErrorResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.error_context_count, 1);
        assert_eq!(decoded.error_data, data);
    }

    #[test]
    fn error_response_roundtrip() {
        let original = ErrorResponse {
            error_context_count: 2,
            error_data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = ErrorResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.error_context_count, original.error_context_count);
        assert_eq!(decoded.error_data, original.error_data);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{
        arb_command, arb_credit_charge, arb_header_flags, arb_message_id, arb_nt_status,
        arb_session_id, arb_small_bytes, arb_tree_id,
    };
    use proptest::prelude::*;

    /// Generate a `Header` whose `flags.is_async()` matches which of
    /// `tree_id`/`async_id` is set. Any other combination wouldn't round-trip
    /// (pack writes one or the other based on flags, and clears the other on
    /// unpack), so we never generate it.
    fn arb_header() -> impl Strategy<Value = Header> {
        (
            arb_credit_charge(),
            arb_nt_status(),
            arb_command(),
            any::<u16>(),
            arb_header_flags(),
            any::<u32>(),
            arb_message_id(),
            any::<bool>(),
            arb_tree_id(),
            any::<u64>(),
            arb_session_id(),
            any::<[u8; 16]>(),
        )
            .prop_map(
                |(
                    credit_charge,
                    status,
                    command,
                    credits,
                    raw_flags,
                    next_command,
                    message_id,
                    make_async,
                    tree_id,
                    async_id,
                    session_id,
                    signature,
                )| {
                    // Force `flags.ASYNC_COMMAND` to match `make_async` so
                    // the pack path and the `Option<T>` fields agree.
                    let flags = if make_async {
                        let mut f = raw_flags;
                        f.set(HeaderFlags::ASYNC_COMMAND);
                        f
                    } else {
                        let mut f = raw_flags;
                        f.clear(HeaderFlags::ASYNC_COMMAND);
                        f
                    };
                    let (tree_id, async_id) = if make_async {
                        (None, Some(async_id))
                    } else {
                        (Some(tree_id), None)
                    };
                    Header {
                        credit_charge,
                        status,
                        command,
                        credits,
                        flags,
                        next_command,
                        message_id,
                        tree_id,
                        async_id,
                        session_id,
                        signature,
                    }
                },
            )
    }

    proptest! {
        #[test]
        fn header_pack_unpack(header in arb_header()) {
            let mut w = WriteCursor::new();
            header.pack(&mut w);
            let bytes = w.into_inner();
            prop_assert_eq!(bytes.len(), Header::SIZE);

            let mut r = ReadCursor::new(&bytes);
            let decoded = Header::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, header);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn error_response_pack_unpack(
            error_context_count in any::<u8>(),
            error_data in arb_small_bytes(),
        ) {
            let original = ErrorResponse {
                error_context_count,
                error_data,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = ErrorResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
