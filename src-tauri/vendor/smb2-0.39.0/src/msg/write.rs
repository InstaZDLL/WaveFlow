//! SMB2 WRITE Request and Response (MS-SMB2 sections 2.2.21, 2.2.22).
//!
//! The WRITE request writes data to a file or named pipe.
//! The response reports how many bytes were written.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

/// Write flag: server performs write-through (SMB 2.1+).
///
/// ❌ Not on its own on 3.0.2 / 3.1.1: there the server MUST refuse it with
/// `STATUS_INVALID_PARAMETER` unless [`SMB2_WRITEFLAG_WRITE_UNBUFFERED`] is
/// set too or the open carries `FILE_NO_INTERMEDIATE_BUFFERING` (MS-SMB2
/// § 3.3.5.13). Windows Server 2022 does exactly that (2026-09-23). To make
/// written data durable, send a FLUSH instead, which is what the write paths
/// here do.
pub const SMB2_WRITEFLAG_WRITE_THROUGH: u32 = 0x0000_0001;

/// Write flag: file buffering is not performed (SMB 3.0.2+).
pub const SMB2_WRITEFLAG_WRITE_UNBUFFERED: u32 = 0x0000_0002;

/// SMB2 WRITE Request (MS-SMB2 section 2.2.21).
///
/// Sent by the client to write data to a file. The fixed portion is 49 bytes
/// (StructureSize says 49 regardless of the variable buffer length):
/// - StructureSize (2 bytes, must be 49)
/// - DataOffset (2 bytes)
/// - Length (4 bytes)
/// - Offset (8 bytes)
/// - FileId (16 bytes)
/// - Channel (4 bytes)
/// - RemainingBytes (4 bytes)
/// - WriteChannelInfoOffset (2 bytes)
/// - WriteChannelInfoLength (2 bytes)
/// - Flags (4 bytes)
/// - Buffer (variable, Length bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    /// Offset from the beginning of the SMB2 header to the write data.
    pub data_offset: u16,
    /// File offset to start writing at.
    pub offset: u64,
    /// File handle to write to.
    pub file_id: FileId,
    /// Channel for RDMA operations (typically 0 = SMB2_CHANNEL_NONE).
    pub channel: u32,
    /// Remaining bytes in a multi-part write.
    pub remaining_bytes: u32,
    /// Write channel info offset (typically 0).
    pub write_channel_info_offset: u16,
    /// Write channel info length (typically 0).
    pub write_channel_info_length: u16,
    /// Flags for the write operation.
    pub flags: u32,
    /// The data to write.
    pub data: Vec<u8>,
}

impl WriteRequest {
    pub const STRUCTURE_SIZE: u16 = 49;
}

impl Pack for WriteRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u16_le(self.data_offset);
        cursor.write_u32_le(self.data.len() as u32); // Length
        cursor.write_u64_le(self.offset);
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
        cursor.write_u32_le(self.channel);
        cursor.write_u32_le(self.remaining_bytes);
        cursor.write_u16_le(self.write_channel_info_offset);
        cursor.write_u16_le(self.write_channel_info_length);
        cursor.write_u32_le(self.flags);

        // Buffer: write the data (may be empty for zero-length writes).
        // Per StructureSize=49 contract, at least 1 byte is implied.
        if self.data.is_empty() {
            cursor.write_u8(0);
        } else {
            cursor.write_bytes(&self.data);
        }
    }
}

impl Unpack for WriteRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid WriteRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let data_offset = cursor.read_u16_le()?;
        let length = cursor.read_u32_le()?;
        let offset = cursor.read_u64_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let channel = cursor.read_u32_le()?;
        let remaining_bytes = cursor.read_u32_le()?;
        let write_channel_info_offset = cursor.read_u16_le()?;
        let write_channel_info_length = cursor.read_u16_le()?;
        let flags = cursor.read_u32_le()?;

        let data = if length > 0 {
            cursor.read_bytes_bounded(length as usize)?.to_vec()
        } else {
            // Skip the minimum 1-byte buffer
            cursor.skip(1)?;
            Vec::new()
        };

        Ok(WriteRequest {
            data_offset,
            offset,
            file_id: FileId {
                persistent,
                volatile,
            },
            channel,
            remaining_bytes,
            write_channel_info_offset,
            write_channel_info_length,
            flags,
            data,
        })
    }
}

/// SMB2 WRITE Response (MS-SMB2 section 2.2.22).
///
/// Sent by the server to confirm a write. The structure is 17 bytes:
/// - StructureSize (2 bytes, must be 17)
/// - Reserved (2 bytes)
/// - Count (4 bytes)
/// - Remaining (4 bytes)
/// - WriteChannelInfoOffset (2 bytes)
/// - WriteChannelInfoLength (2 bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResponse {
    /// Number of bytes written.
    pub count: u32,
    /// Reserved remaining field (must be 0).
    pub remaining: u32,
    /// Reserved write channel info offset (must be 0).
    pub write_channel_info_offset: u16,
    /// Reserved write channel info length (must be 0).
    pub write_channel_info_length: u16,
}

impl WriteResponse {
    pub const STRUCTURE_SIZE: u16 = 17;
}

impl Pack for WriteResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u16_le(0); // Reserved
        cursor.write_u32_le(self.count);
        cursor.write_u32_le(self.remaining);
        cursor.write_u16_le(self.write_channel_info_offset);
        cursor.write_u16_le(self.write_channel_info_length);
    }
}

impl Unpack for WriteResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid WriteResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let _reserved = cursor.read_u16_le()?;
        let count = cursor.read_u32_le()?;
        let remaining = cursor.read_u32_le()?;
        let write_channel_info_offset = cursor.read_u16_le()?;
        let write_channel_info_length = cursor.read_u16_le()?;

        Ok(WriteResponse {
            count,
            remaining,
            write_channel_info_offset,
            write_channel_info_length,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── WriteRequest tests ─────────────────────────────────────────

    #[test]
    fn write_request_roundtrip() {
        let original = WriteRequest {
            data_offset: 0x70, // 64 (header) + 48 (fixed body) = 112 = 0x70
            offset: 0x2000,
            file_id: FileId {
                persistent: 0xAAAA_BBBB_CCCC_DDDD,
                volatile: 0x1111_2222_3333_4444,
            },
            channel: 0,
            remaining_bytes: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
            flags: SMB2_WRITEFLAG_WRITE_THROUGH,
            data: vec![0x48, 0x65, 0x6C, 0x6C, 0x6F], // "Hello"
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 48 bytes + 5 bytes data = 53 bytes
        assert_eq!(bytes.len(), 53);

        let mut r = ReadCursor::new(&bytes);
        let decoded = WriteRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.data_offset, original.data_offset);
        assert_eq!(decoded.offset, original.offset);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.channel, original.channel);
        assert_eq!(decoded.remaining_bytes, original.remaining_bytes);
        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.data, original.data);
    }

    #[test]
    fn write_request_empty_data_roundtrip() {
        let original = WriteRequest {
            data_offset: 0x70,
            offset: 0,
            file_id: FileId {
                persistent: 1,
                volatile: 2,
            },
            channel: 0,
            remaining_bytes: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
            flags: 0,
            data: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 48 bytes + 1-byte minimum buffer = 49 bytes
        assert_eq!(bytes.len(), 49);

        let mut r = ReadCursor::new(&bytes);
        let decoded = WriteRequest::unpack(&mut r).unwrap();

        assert!(decoded.data.is_empty());
        assert_eq!(decoded.file_id, original.file_id);
    }

    #[test]
    fn write_request_wrong_structure_size() {
        let mut buf = [0u8; 49];
        buf[0..2].copy_from_slice(&48u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = WriteRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn write_request_known_bytes() {
        let mut buf = Vec::new();
        // StructureSize = 49
        buf.extend_from_slice(&49u16.to_le_bytes());
        // DataOffset = 0x70
        buf.extend_from_slice(&0x70u16.to_le_bytes());
        // Length = 2
        buf.extend_from_slice(&2u32.to_le_bytes());
        // Offset = 0
        buf.extend_from_slice(&0u64.to_le_bytes());
        // FileId persistent = 0x10
        buf.extend_from_slice(&0x10u64.to_le_bytes());
        // FileId volatile = 0x20
        buf.extend_from_slice(&0x20u64.to_le_bytes());
        // Channel = 0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // RemainingBytes = 0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // WriteChannelInfoOffset = 0
        buf.extend_from_slice(&0u16.to_le_bytes());
        // WriteChannelInfoLength = 0
        buf.extend_from_slice(&0u16.to_le_bytes());
        // Flags = WRITE_THROUGH
        buf.extend_from_slice(&1u32.to_le_bytes());
        // Buffer = [0xAA, 0xBB]
        buf.extend_from_slice(&[0xAA, 0xBB]);

        let mut cursor = ReadCursor::new(&buf);
        let req = WriteRequest::unpack(&mut cursor).unwrap();

        assert_eq!(req.data_offset, 0x70);
        assert_eq!(req.file_id.persistent, 0x10);
        assert_eq!(req.file_id.volatile, 0x20);
        assert_eq!(req.flags, SMB2_WRITEFLAG_WRITE_THROUGH);
        assert_eq!(req.data, vec![0xAA, 0xBB]);
    }

    // ── WriteResponse tests ────────────────────────────────────────

    #[test]
    fn write_response_roundtrip() {
        let original = WriteResponse {
            count: 65536,
            remaining: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // 2 + 2 + 4 + 4 + 2 + 2 = 16 bytes
        assert_eq!(bytes.len(), 16);

        let mut r = ReadCursor::new(&bytes);
        let decoded = WriteResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.count, original.count);
        assert_eq!(decoded.remaining, original.remaining);
        assert_eq!(
            decoded.write_channel_info_offset,
            original.write_channel_info_offset
        );
        assert_eq!(
            decoded.write_channel_info_length,
            original.write_channel_info_length
        );
    }

    #[test]
    fn write_response_known_bytes() {
        let mut buf = Vec::new();
        // StructureSize = 17
        buf.extend_from_slice(&17u16.to_le_bytes());
        // Reserved = 0
        buf.extend_from_slice(&0u16.to_le_bytes());
        // Count = 1024
        buf.extend_from_slice(&1024u32.to_le_bytes());
        // Remaining = 0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // WriteChannelInfoOffset = 0
        buf.extend_from_slice(&0u16.to_le_bytes());
        // WriteChannelInfoLength = 0
        buf.extend_from_slice(&0u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let resp = WriteResponse::unpack(&mut cursor).unwrap();

        assert_eq!(resp.count, 1024);
        assert_eq!(resp.remaining, 0);
    }

    #[test]
    fn write_response_wrong_structure_size() {
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&16u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = WriteResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_bytes, arb_file_id};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn write_request_pack_unpack(
            data_offset in any::<u16>(),
            offset in any::<u64>(),
            file_id in arb_file_id(),
            channel in any::<u32>(),
            remaining_bytes in any::<u32>(),
            write_channel_info_offset in any::<u16>(),
            write_channel_info_length in any::<u16>(),
            flags in any::<u32>(),
            data in arb_bytes(),
        ) {
            let original = WriteRequest {
                data_offset,
                offset,
                file_id,
                channel,
                remaining_bytes,
                write_channel_info_offset,
                write_channel_info_length,
                flags,
                data,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = WriteRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn write_response_pack_unpack(
            count in any::<u32>(),
            remaining in any::<u32>(),
            write_channel_info_offset in any::<u16>(),
            write_channel_info_length in any::<u16>(),
        ) {
            let original = WriteResponse {
                count,
                remaining,
                write_channel_info_offset,
                write_channel_info_length,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = WriteResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
