//! SMB2 READ Request and Response (MS-SMB2 sections 2.2.19, 2.2.20).
//!
//! The READ request reads data from a file or named pipe.
//! The response carries the read data in a variable-length buffer.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

/// Read flag: read data directly from underlying storage (SMB 3.0.2+).
pub const SMB2_READFLAG_READ_UNBUFFERED: u8 = 0x01;

/// Read flag: request compressed response (SMB 3.1.1).
pub const SMB2_READFLAG_REQUEST_COMPRESSED: u8 = 0x02;

/// Channel value: no channel information.
pub const SMB2_CHANNEL_NONE: u32 = 0x0000_0000;

/// SMB2 READ Request (MS-SMB2 section 2.2.19).
///
/// Sent by the client to read data from a file. The fixed portion is 49 bytes
/// (StructureSize says 49 regardless of the variable buffer length):
/// - StructureSize (2 bytes, must be 49)
/// - Padding (1 byte)
/// - Flags (1 byte)
/// - Length (4 bytes)
/// - Offset (8 bytes)
/// - FileId (16 bytes)
/// - MinimumCount (4 bytes)
/// - Channel (4 bytes)
/// - RemainingBytes (4 bytes)
/// - ReadChannelInfoOffset (2 bytes)
/// - ReadChannelInfoLength (2 bytes)
/// - Buffer (variable, typically empty for basic reads)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// Requested data placement offset in the response.
    pub padding: u8,
    /// Flags for the read operation.
    pub flags: u8,
    /// Number of bytes to read.
    pub length: u32,
    /// File offset to start reading from.
    pub offset: u64,
    /// File handle to read from.
    pub file_id: FileId,
    /// Minimum number of bytes for a successful read.
    pub minimum_count: u32,
    /// Channel for RDMA operations (typically `SMB2_CHANNEL_NONE`).
    pub channel: u32,
    /// Remaining bytes in a multi-part read.
    pub remaining_bytes: u32,
    /// Variable-length read channel info buffer.
    pub read_channel_info: Vec<u8>,
}

impl ReadRequest {
    pub const STRUCTURE_SIZE: u16 = 49;
}

impl Pack for ReadRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u8(self.padding);
        cursor.write_u8(self.flags);
        cursor.write_u32_le(self.length);
        cursor.write_u64_le(self.offset);
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
        cursor.write_u32_le(self.minimum_count);
        cursor.write_u32_le(self.channel);
        cursor.write_u32_le(self.remaining_bytes);

        // ReadChannelInfoOffset/Length: relative to start of SMB2 header.
        // For packing the body alone, we store offset as 0 when empty.
        if self.read_channel_info.is_empty() {
            cursor.write_u16_le(0);
            cursor.write_u16_le(0);
        } else {
            // Offset from the SMB2 header = header (64) + fixed body (48) = 112.
            // The fixed body before Buffer is 48 bytes (StructureSize 49 minus
            // the 1 byte of Buffer that's counted in StructureSize).
            cursor.write_u16_le(0); // Caller must backpatch if needed
            cursor.write_u16_le(self.read_channel_info.len() as u16);
        }

        // Buffer: at minimum 1 byte per the StructureSize=49 contract,
        // but we write the actual channel info if present.
        if self.read_channel_info.is_empty() {
            // Write a single padding byte so the fixed part is 49 bytes
            // (StructureSize includes this 1-byte minimum buffer).
            cursor.write_u8(0);
        } else {
            cursor.write_bytes(&self.read_channel_info);
        }
    }
}

impl Unpack for ReadRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid ReadRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let padding = cursor.read_u8()?;
        let flags = cursor.read_u8()?;
        let length = cursor.read_u32_le()?;
        let offset = cursor.read_u64_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let minimum_count = cursor.read_u32_le()?;
        let channel = cursor.read_u32_le()?;
        let remaining_bytes = cursor.read_u32_le()?;
        let _read_channel_info_offset = cursor.read_u16_le()?;
        let read_channel_info_length = cursor.read_u16_le()?;

        // The buffer is at least 1 byte (per StructureSize=49).
        // Read channel info from the buffer based on the length field.
        let read_channel_info = if read_channel_info_length > 0 {
            cursor
                .read_bytes(read_channel_info_length as usize)?
                .to_vec()
        } else {
            // Skip the minimum 1-byte buffer
            cursor.skip(1)?;
            Vec::new()
        };

        Ok(ReadRequest {
            padding,
            flags,
            length,
            offset,
            file_id: FileId {
                persistent,
                volatile,
            },
            minimum_count,
            channel,
            remaining_bytes,
            read_channel_info,
        })
    }
}

/// SMB2 READ Response (MS-SMB2 section 2.2.20).
///
/// Sent by the server with the requested data. The fixed portion is 17 bytes:
/// - StructureSize (2 bytes, must be 17)
/// - DataOffset (1 byte)
/// - Reserved (1 byte)
/// - DataLength (4 bytes)
/// - DataRemaining (4 bytes)
/// - Reserved2 (4 bytes)
/// - Buffer (variable, DataLength bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResponse {
    /// Offset from the start of the SMB2 header to the data.
    pub data_offset: u8,
    /// Number of remaining bytes on the channel.
    pub data_remaining: u32,
    /// Flags/Reserved2 field (used in SMB 3.1.1, otherwise 0).
    pub flags: u32,
    /// The data that was read.
    pub data: Vec<u8>,
}

impl ReadResponse {
    pub const STRUCTURE_SIZE: u16 = 17;
}

impl Pack for ReadResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u8(self.data_offset);
        cursor.write_u8(0); // Reserved
        cursor.write_u32_le(self.data.len() as u32);
        cursor.write_u32_le(self.data_remaining);
        cursor.write_u32_le(self.flags); // Reserved2/Flags
        cursor.write_bytes(&self.data);
    }
}

impl Unpack for ReadResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid ReadResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let data_offset = cursor.read_u8()?;
        let _reserved = cursor.read_u8()?;
        let data_length = cursor.read_u32_le()?;
        let data_remaining = cursor.read_u32_le()?;
        let flags = cursor.read_u32_le()?;

        let data = if data_length > 0 {
            cursor.read_bytes_bounded(data_length as usize)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(ReadResponse {
            data_offset,
            data_remaining,
            flags,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ReadRequest tests ──────────────────────────────────────────

    #[test]
    fn read_request_roundtrip() {
        let original = ReadRequest {
            padding: 0x50,
            flags: SMB2_READFLAG_READ_UNBUFFERED,
            length: 65536,
            offset: 0x1000,
            file_id: FileId {
                persistent: 0xAAAA_BBBB_CCCC_DDDD,
                volatile: 0x1111_2222_3333_4444,
            },
            minimum_count: 1024,
            channel: SMB2_CHANNEL_NONE,
            remaining_bytes: 0,
            read_channel_info: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 48 bytes + 1-byte minimum buffer = 49 bytes
        assert_eq!(bytes.len(), 49);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ReadRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.padding, original.padding);
        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.length, original.length);
        assert_eq!(decoded.offset, original.offset);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.minimum_count, original.minimum_count);
        assert_eq!(decoded.channel, original.channel);
        assert_eq!(decoded.remaining_bytes, original.remaining_bytes);
        assert!(decoded.read_channel_info.is_empty());
    }

    #[test]
    fn read_request_with_channel_info_roundtrip() {
        let channel_data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let original = ReadRequest {
            padding: 0,
            flags: 0,
            length: 4096,
            offset: 0,
            file_id: FileId {
                persistent: 1,
                volatile: 2,
            },
            minimum_count: 0,
            channel: 0x0000_0001, // SMB2_CHANNEL_RDMA_V1
            remaining_bytes: 4096,
            read_channel_info: channel_data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 48 bytes + 4-byte channel info = 52 bytes
        assert_eq!(bytes.len(), 52);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ReadRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.read_channel_info, channel_data);
        assert_eq!(decoded.channel, 0x0000_0001);
    }

    #[test]
    fn read_request_wrong_structure_size() {
        let mut buf = [0u8; 49];
        buf[0..2].copy_from_slice(&50u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = ReadRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── ReadResponse tests ─────────────────────────────────────────

    #[test]
    fn read_response_roundtrip() {
        let original = ReadResponse {
            data_offset: 0x50, // typical: 64 (header) + 16 (body fixed) = 80 = 0x50
            data_remaining: 0,
            flags: 0,
            data: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 16 bytes + 5 bytes data = 21 bytes
        assert_eq!(bytes.len(), 21);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ReadResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.data_offset, original.data_offset);
        assert_eq!(decoded.data_remaining, original.data_remaining);
        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.data, original.data);
    }

    #[test]
    fn read_response_empty_data() {
        let original = ReadResponse {
            data_offset: 0,
            data_remaining: 0,
            flags: 0,
            data: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 16 bytes, no data
        assert_eq!(bytes.len(), 16);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ReadResponse::unpack(&mut r).unwrap();

        assert!(decoded.data.is_empty());
    }

    #[test]
    fn read_response_known_bytes() {
        let mut buf = Vec::new();
        // StructureSize = 17
        buf.extend_from_slice(&17u16.to_le_bytes());
        // DataOffset = 0x50
        buf.push(0x50);
        // Reserved = 0
        buf.push(0x00);
        // DataLength = 3
        buf.extend_from_slice(&3u32.to_le_bytes());
        // DataRemaining = 0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // Reserved2/Flags = 0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // Buffer = [0xAA, 0xBB, 0xCC]
        buf.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        let mut cursor = ReadCursor::new(&buf);
        let resp = ReadResponse::unpack(&mut cursor).unwrap();

        assert_eq!(resp.data_offset, 0x50);
        assert_eq!(resp.data, vec![0xAA, 0xBB, 0xCC]);
        assert_eq!(resp.data_remaining, 0);
        assert_eq!(resp.flags, 0);
    }

    #[test]
    fn read_response_wrong_structure_size() {
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = ReadResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_bytes, arb_file_id, arb_small_bytes};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn read_request_pack_unpack(
            padding in any::<u8>(),
            flags in any::<u8>(),
            length in any::<u32>(),
            offset in any::<u64>(),
            file_id in arb_file_id(),
            minimum_count in any::<u32>(),
            channel in any::<u32>(),
            remaining_bytes in any::<u32>(),
            read_channel_info in arb_small_bytes(),
        ) {
            let original = ReadRequest {
                padding,
                flags,
                length,
                offset,
                file_id,
                minimum_count,
                channel,
                remaining_bytes,
                read_channel_info,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = ReadRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn read_response_pack_unpack(
            data_offset in any::<u8>(),
            data_remaining in any::<u32>(),
            flags in any::<u32>(),
            data in arb_bytes(),
        ) {
            let original = ReadResponse {
                data_offset,
                data_remaining,
                flags,
                data,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = ReadResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
