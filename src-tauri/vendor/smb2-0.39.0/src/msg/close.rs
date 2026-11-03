//! SMB2 CLOSE Request and Response (MS-SMB2 sections 2.2.15, 2.2.16).
//!
//! The CLOSE request closes a file handle previously opened via CREATE.
//! The response optionally returns file attributes if the
//! `SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB` flag was set.

use crate::error::Result;
use crate::pack::{FileTime, Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

/// Close flag: request that the server returns file attributes in the response.
pub const SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB: u16 = 0x0001;

/// SMB2 CLOSE Request (MS-SMB2 section 2.2.15).
///
/// Sent by the client to close a file handle. The structure is 24 bytes:
/// - StructureSize (2 bytes, must be 24)
/// - Flags (2 bytes)
/// - Reserved (4 bytes)
/// - FileId (16 bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseRequest {
    /// Flags indicating how to process the close.
    /// Use `SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB` to request attributes.
    pub flags: u16,
    /// The file handle to close.
    pub file_id: FileId,
}

impl CloseRequest {
    pub const STRUCTURE_SIZE: u16 = 24;
}

impl Pack for CloseRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Flags (2 bytes)
        cursor.write_u16_le(self.flags);
        // Reserved (4 bytes)
        cursor.write_u32_le(0);
        // FileId (16 bytes): persistent + volatile
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
    }
}

impl Unpack for CloseRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid CloseRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let flags = cursor.read_u16_le()?;
        let _reserved = cursor.read_u32_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;

        Ok(CloseRequest {
            flags,
            file_id: FileId {
                persistent,
                volatile,
            },
        })
    }
}

/// SMB2 CLOSE Response (MS-SMB2 section 2.2.16).
///
/// Sent by the server to confirm a close. The structure is 60 bytes:
/// - StructureSize (2 bytes, must be 60)
/// - Flags (2 bytes)
/// - Reserved (4 bytes)
/// - CreationTime (8 bytes)
/// - LastAccessTime (8 bytes)
/// - LastWriteTime (8 bytes)
/// - ChangeTime (8 bytes)
/// - AllocationSize (8 bytes)
/// - EndOfFile (8 bytes)
/// - FileAttributes (4 bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseResponse {
    /// Flags echoed from the request. If `SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB`
    /// is set, the attribute fields below contain valid data.
    pub flags: u16,
    /// File creation time.
    pub creation_time: FileTime,
    /// Last access time.
    pub last_access_time: FileTime,
    /// Last write time.
    pub last_write_time: FileTime,
    /// Change time.
    pub change_time: FileTime,
    /// Size of allocated data in bytes.
    pub allocation_size: u64,
    /// End-of-file position in bytes.
    pub end_of_file: u64,
    /// File attributes (see MS-FSCC section 2.6).
    pub file_attributes: u32,
}

impl CloseResponse {
    pub const STRUCTURE_SIZE: u16 = 60;
}

impl Pack for CloseResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u16_le(self.flags);
        cursor.write_u32_le(0); // Reserved
        self.creation_time.pack(cursor);
        self.last_access_time.pack(cursor);
        self.last_write_time.pack(cursor);
        self.change_time.pack(cursor);
        cursor.write_u64_le(self.allocation_size);
        cursor.write_u64_le(self.end_of_file);
        cursor.write_u32_le(self.file_attributes);
    }
}

impl Unpack for CloseResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid CloseResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let flags = cursor.read_u16_le()?;
        let _reserved = cursor.read_u32_le()?;
        let creation_time = FileTime::unpack(cursor)?;
        let last_access_time = FileTime::unpack(cursor)?;
        let last_write_time = FileTime::unpack(cursor)?;
        let change_time = FileTime::unpack(cursor)?;
        let allocation_size = cursor.read_u64_le()?;
        let end_of_file = cursor.read_u64_le()?;
        let file_attributes = cursor.read_u32_le()?;

        Ok(CloseResponse {
            flags,
            creation_time,
            last_access_time,
            last_write_time,
            change_time,
            allocation_size,
            end_of_file,
            file_attributes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CloseRequest tests ─────────────────────────────────────────

    #[test]
    fn close_request_roundtrip() {
        let original = CloseRequest {
            flags: SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB,
            file_id: FileId {
                persistent: 0x1122_3344_5566_7788,
                volatile: 0xAABB_CCDD_EEFF_0011,
            },
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // 2 + 2 + 4 + 16 = 24 bytes
        assert_eq!(bytes.len(), 24);

        let mut r = ReadCursor::new(&bytes);
        let decoded = CloseRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.file_id, original.file_id);
    }

    #[test]
    fn close_request_known_bytes() {
        let mut buf = [0u8; 24];
        // StructureSize = 24
        buf[0..2].copy_from_slice(&24u16.to_le_bytes());
        // Flags = 0x0001
        buf[2..4].copy_from_slice(&1u16.to_le_bytes());
        // Reserved = 0
        buf[4..8].copy_from_slice(&0u32.to_le_bytes());
        // FileId persistent = 0x42
        buf[8..16].copy_from_slice(&0x42u64.to_le_bytes());
        // FileId volatile = 0x99
        buf[16..24].copy_from_slice(&0x99u64.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let req = CloseRequest::unpack(&mut cursor).unwrap();

        assert_eq!(req.flags, SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB);
        assert_eq!(req.file_id.persistent, 0x42);
        assert_eq!(req.file_id.volatile, 0x99);
    }

    #[test]
    fn close_request_wrong_structure_size() {
        let mut buf = [0u8; 24];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = CloseRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── CloseResponse tests ────────────────────────────────────────

    #[test]
    fn close_response_roundtrip() {
        let original = CloseResponse {
            flags: SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB,
            creation_time: FileTime(0x01D8_AAAA_BBBB_CCCC),
            last_access_time: FileTime(0x01D8_DDDD_EEEE_FFFF),
            last_write_time: FileTime(0x01D8_1111_2222_3333),
            change_time: FileTime(0x01D8_4444_5555_6666),
            allocation_size: 4096,
            end_of_file: 2048,
            file_attributes: 0x20, // FILE_ATTRIBUTE_ARCHIVE
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // 2 + 2 + 4 + 8*6 + 4 = 60 bytes
        assert_eq!(bytes.len(), 60);

        let mut r = ReadCursor::new(&bytes);
        let decoded = CloseResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.creation_time, original.creation_time);
        assert_eq!(decoded.last_access_time, original.last_access_time);
        assert_eq!(decoded.last_write_time, original.last_write_time);
        assert_eq!(decoded.change_time, original.change_time);
        assert_eq!(decoded.allocation_size, original.allocation_size);
        assert_eq!(decoded.end_of_file, original.end_of_file);
        assert_eq!(decoded.file_attributes, original.file_attributes);
    }

    #[test]
    fn close_response_known_bytes() {
        let mut buf = [0u8; 60];
        // StructureSize = 60
        buf[0..2].copy_from_slice(&60u16.to_le_bytes());
        // Flags = 0x0001
        buf[2..4].copy_from_slice(&1u16.to_le_bytes());
        // Reserved = 0
        buf[4..8].copy_from_slice(&0u32.to_le_bytes());
        // CreationTime = 100
        buf[8..16].copy_from_slice(&100u64.to_le_bytes());
        // LastAccessTime = 200
        buf[16..24].copy_from_slice(&200u64.to_le_bytes());
        // LastWriteTime = 300
        buf[24..32].copy_from_slice(&300u64.to_le_bytes());
        // ChangeTime = 400
        buf[32..40].copy_from_slice(&400u64.to_le_bytes());
        // AllocationSize = 8192
        buf[40..48].copy_from_slice(&8192u64.to_le_bytes());
        // EndOfFile = 1024
        buf[48..56].copy_from_slice(&1024u64.to_le_bytes());
        // FileAttributes = 0x10 (directory)
        buf[56..60].copy_from_slice(&0x10u32.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let resp = CloseResponse::unpack(&mut cursor).unwrap();

        assert_eq!(resp.flags, SMB2_CLOSE_FLAG_POSTQUERY_ATTRIB);
        assert_eq!(resp.creation_time, FileTime(100));
        assert_eq!(resp.last_access_time, FileTime(200));
        assert_eq!(resp.last_write_time, FileTime(300));
        assert_eq!(resp.change_time, FileTime(400));
        assert_eq!(resp.allocation_size, 8192);
        assert_eq!(resp.end_of_file, 1024);
        assert_eq!(resp.file_attributes, 0x10);
    }

    #[test]
    fn close_response_wrong_structure_size() {
        let mut buf = [0u8; 60];
        buf[0..2].copy_from_slice(&42u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = CloseResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn close_response_zero_flags_has_zeroed_attributes() {
        let original = CloseResponse {
            flags: 0,
            creation_time: FileTime::ZERO,
            last_access_time: FileTime::ZERO,
            last_write_time: FileTime::ZERO,
            change_time: FileTime::ZERO,
            allocation_size: 0,
            end_of_file: 0,
            file_attributes: 0,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = CloseResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.creation_time, FileTime::ZERO);
        assert_eq!(decoded.file_attributes, 0);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_file_id, arb_file_time};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn close_request_pack_unpack(
            flags in any::<u16>(),
            file_id in arb_file_id(),
        ) {
            let original = CloseRequest { flags, file_id };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = CloseRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn close_response_pack_unpack(
            flags in any::<u16>(),
            creation_time in arb_file_time(),
            last_access_time in arb_file_time(),
            last_write_time in arb_file_time(),
            change_time in arb_file_time(),
            allocation_size in any::<u64>(),
            end_of_file in any::<u64>(),
            file_attributes in any::<u32>(),
        ) {
            let original = CloseResponse {
                flags,
                creation_time,
                last_access_time,
                last_write_time,
                change_time,
                allocation_size,
                end_of_file,
                file_attributes,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = CloseResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
