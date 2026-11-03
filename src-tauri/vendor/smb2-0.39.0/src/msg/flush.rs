//! SMB2 FLUSH request and response (spec sections 2.2.17, 2.2.18).
//!
//! Flush messages request that the server flush all cached file information
//! for a specified open to persistent storage. If the open refers to a
//! named pipe, the operation completes once all written data has been
//! consumed by a reader.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

/// SMB2 FLUSH request (spec section 2.2.17).
///
/// Sent by the client to request that the server flush cached data for a file.
///
/// Wire layout (24 bytes):
/// - StructureSize (2 bytes): must be 24
/// - Reserved1 (2 bytes): must be 0
/// - Reserved2 (4 bytes): must be 0
/// - FileId (16 bytes): persistent (8 bytes) + volatile (8 bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushRequest {
    pub file_id: FileId,
}

impl FlushRequest {
    pub const STRUCTURE_SIZE: u16 = 24;
}

impl Pack for FlushRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Reserved1 (2 bytes)
        cursor.write_u16_le(0);
        // Reserved2 (4 bytes)
        cursor.write_u32_le(0);
        // FileId: Persistent (8 bytes) + Volatile (8 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
    }
}

impl Unpack for FlushRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid FlushRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // Reserved1 (2 bytes)
        let _reserved1 = cursor.read_u16_le()?;

        // Reserved2 (4 bytes)
        let _reserved2 = cursor.read_u32_le()?;

        // FileId: Persistent (8 bytes) + Volatile (8 bytes)
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;

        Ok(FlushRequest {
            file_id: FileId {
                persistent,
                volatile,
            },
        })
    }
}

super::trivial_message! {
    /// SMB2 FLUSH response (spec section 2.2.18).
    ///
    /// Sent by the server to confirm that a FLUSH request was processed.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct FlushResponse;
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── FlushRequest tests ─────────────────────────────────────────

    #[test]
    fn flush_request_pack_produces_24_bytes() {
        let req = FlushRequest {
            file_id: FileId::default(),
        };
        let mut cursor = WriteCursor::new();
        req.pack(&mut cursor);
        let bytes = cursor.into_inner();
        assert_eq!(bytes.len(), 24);
    }

    #[test]
    fn flush_request_known_bytes() {
        let req = FlushRequest {
            file_id: FileId {
                persistent: 0x0102_0304_0506_0708,
                volatile: 0x090A_0B0C_0D0E_0F10,
            },
        };
        let mut cursor = WriteCursor::new();
        req.pack(&mut cursor);
        let bytes = cursor.into_inner();

        #[rustfmt::skip]
        let expected: [u8; 24] = [
            // StructureSize = 24
            0x18, 0x00,
            // Reserved1 = 0
            0x00, 0x00,
            // Reserved2 = 0
            0x00, 0x00, 0x00, 0x00,
            // FileId.Persistent (LE)
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01,
            // FileId.Volatile (LE)
            0x10, 0x0F, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A, 0x09,
        ];
        assert_eq!(bytes, expected);
    }

    #[test]
    fn flush_request_unpack_known_bytes() {
        #[rustfmt::skip]
        let bytes: [u8; 24] = [
            // StructureSize = 24
            0x18, 0x00,
            // Reserved1 = 0
            0x00, 0x00,
            // Reserved2 = 0
            0x00, 0x00, 0x00, 0x00,
            // FileId.Persistent = 0xDEADBEEFCAFEBABE
            0xBE, 0xBA, 0xFE, 0xCA, 0xEF, 0xBE, 0xAD, 0xDE,
            // FileId.Volatile = 0x1234567890ABCDEF
            0xEF, 0xCD, 0xAB, 0x90, 0x78, 0x56, 0x34, 0x12,
        ];
        let mut cursor = ReadCursor::new(&bytes);
        let req = FlushRequest::unpack(&mut cursor).unwrap();

        assert_eq!(req.file_id.persistent, 0xDEAD_BEEF_CAFE_BABE);
        assert_eq!(req.file_id.volatile, 0x1234_5678_90AB_CDEF);
        assert!(cursor.is_empty());
    }

    #[test]
    fn flush_request_roundtrip() {
        let original = FlushRequest {
            file_id: FileId {
                persistent: 0xAAAA_BBBB_CCCC_DDDD,
                volatile: 0x1111_2222_3333_4444,
            },
        };
        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = FlushRequest::unpack(&mut r).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn flush_request_roundtrip_sentinel_file_id() {
        let original = FlushRequest {
            file_id: FileId::SENTINEL,
        };
        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = FlushRequest::unpack(&mut r).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn flush_request_wrong_structure_size() {
        let mut bytes = [0u8; 24];
        // Wrong structure size = 4 instead of 24
        bytes[0..2].copy_from_slice(&4u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&bytes);
        let result = FlushRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn flush_request_too_short() {
        let bytes = [0x18, 0x00, 0x00, 0x00];
        let mut cursor = ReadCursor::new(&bytes);
        let result = FlushRequest::unpack(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn flush_request_ignores_reserved_values() {
        #[rustfmt::skip]
        let bytes: [u8; 24] = [
            // StructureSize = 24
            0x18, 0x00,
            // Reserved1 = 0xFFFF (non-zero, should be ignored)
            0xFF, 0xFF,
            // Reserved2 = 0xFFFFFFFF (non-zero, should be ignored)
            0xFF, 0xFF, 0xFF, 0xFF,
            // FileId.Persistent = 0
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            // FileId.Volatile = 0
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut cursor = ReadCursor::new(&bytes);
        let req = FlushRequest::unpack(&mut cursor).unwrap();
        assert_eq!(req.file_id, FileId::default());
    }

    // ── FlushResponse tests ────────────────────────────────────────

    super::super::trivial_message_tests!(
        FlushResponse,
        flush_response_known_bytes,
        flush_response_roundtrip,
        flush_response_wrong_structure_size,
        flush_response_too_short
    );
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::arb_file_id;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn flush_request_pack_unpack(file_id in arb_file_id()) {
            let original = FlushRequest { file_id };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = FlushRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
