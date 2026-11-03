//! SMB2 QUERY_INFO request and response (spec sections 2.2.37, 2.2.38).
//!
//! Used to query file, filesystem, security, or quota information.
//! The response buffer is stored as raw bytes -- parsing into specific
//! information classes is deferred.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

// ── Enums ────────────────────────────────────────────────────────────────

/// Info type for query/set info operations (MS-SMB2 2.2.37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InfoType {
    /// Query file information.
    File = 0x01,
    /// Query filesystem information.
    Filesystem = 0x02,
    /// Query security information.
    Security = 0x03,
    /// Query quota information.
    Quota = 0x04,
}

impl TryFrom<u8> for InfoType {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::File),
            0x02 => Ok(Self::Filesystem),
            0x03 => Ok(Self::Security),
            0x04 => Ok(Self::Quota),
            _ => Err(Error::invalid_data(format!(
                "invalid InfoType: 0x{:02X}",
                value
            ))),
        }
    }
}

// ── QueryInfoRequest ─────────────────────────────────────────────────────

/// SMB2 QUERY_INFO request (spec section 2.2.37).
///
/// Sent by the client to query information about a file, filesystem,
/// security descriptor, or quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryInfoRequest {
    /// The type of information being queried.
    pub info_type: InfoType,
    /// The file information class (interpretation depends on `info_type`).
    pub file_info_class: u8,
    /// Maximum number of output bytes the server may return.
    pub output_buffer_length: u32,
    /// Additional information flags (for example, security information flags).
    pub additional_information: u32,
    /// Query flags.
    pub flags: u32,
    /// Handle to the file or directory being queried.
    pub file_id: FileId,
    /// Optional input buffer (for example, for quota queries).
    pub input_buffer: Vec<u8>,
}

impl QueryInfoRequest {
    pub const STRUCTURE_SIZE: u16 = 41;
}

impl Pack for QueryInfoRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // InfoType (1 byte)
        cursor.write_u8(self.info_type as u8);
        // FileInfoClass (1 byte)
        cursor.write_u8(self.file_info_class);
        // OutputBufferLength (4 bytes)
        cursor.write_u32_le(self.output_buffer_length);
        // InputBufferOffset (2 bytes) -- placeholder
        let input_offset_pos = cursor.position();
        cursor.write_u16_le(0);
        // Reserved (2 bytes)
        cursor.write_u16_le(0);
        // InputBufferLength (4 bytes)
        cursor.write_u32_le(self.input_buffer.len() as u32);
        // AdditionalInformation (4 bytes)
        cursor.write_u32_le(self.additional_information);
        // Flags (4 bytes)
        cursor.write_u32_le(self.flags);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);

        // Buffer (variable)
        if !self.input_buffer.is_empty() {
            // Offset is from the beginning of the SMB2 header per spec.
            let buf_offset = Header::SIZE + (cursor.position() - start);
            cursor.write_bytes(&self.input_buffer);
            cursor.set_u16_le_at(input_offset_pos, buf_offset as u16);
        }
    }
}

impl Unpack for QueryInfoRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid QueryInfoRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // InfoType (1 byte)
        let info_type = InfoType::try_from(cursor.read_u8()?)?;
        // FileInfoClass (1 byte)
        let file_info_class = cursor.read_u8()?;
        // OutputBufferLength (4 bytes)
        let output_buffer_length = cursor.read_u32_le()?;
        // InputBufferOffset (2 bytes)
        let input_offset = cursor.read_u16_le()? as usize;
        // Reserved (2 bytes)
        let _reserved = cursor.read_u16_le()?;
        // InputBufferLength (4 bytes)
        let input_length = cursor.read_u32_le()? as usize;
        // AdditionalInformation (4 bytes)
        let additional_information = cursor.read_u32_le()?;
        // Flags (4 bytes)
        let flags = cursor.read_u32_le()?;
        // FileId (16 bytes)
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let file_id = FileId {
            persistent,
            volatile,
        };

        // Read input buffer
        // Offset on the wire is from beginning of SMB2 header.
        let input_buffer = if input_length > 0 {
            let current = cursor.position();
            let body_offset = input_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_bytes_bounded(input_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(QueryInfoRequest {
            info_type,
            file_info_class,
            output_buffer_length,
            additional_information,
            flags,
            file_id,
            input_buffer,
        })
    }
}

// ── QueryInfoResponse ────────────────────────────────────────────────────

/// SMB2 QUERY_INFO response (spec section 2.2.38).
///
/// Contains the queried information as raw bytes. The format depends
/// on the `InfoType` and `FileInfoClass` from the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryInfoResponse {
    /// Raw output buffer containing the queried information.
    pub output_buffer: Vec<u8>,
}

impl QueryInfoResponse {
    pub const STRUCTURE_SIZE: u16 = 9;
}

impl Pack for QueryInfoResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // OutputBufferOffset (2 bytes) -- placeholder
        let offset_pos = cursor.position();
        cursor.write_u16_le(0);
        // OutputBufferLength (4 bytes)
        cursor.write_u32_le(self.output_buffer.len() as u32);

        // Buffer
        if !self.output_buffer.is_empty() {
            // Offset is from the beginning of the SMB2 header per spec.
            let buf_offset = Header::SIZE + (cursor.position() - start);
            cursor.write_bytes(&self.output_buffer);
            cursor.set_u16_le_at(offset_pos, buf_offset as u16);
        }
    }
}

impl Unpack for QueryInfoResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid QueryInfoResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // OutputBufferOffset (2 bytes)
        let buf_offset = cursor.read_u16_le()? as usize;
        // OutputBufferLength (4 bytes)
        let buf_length = cursor.read_u32_le()? as usize;

        // Read buffer
        // Offset on the wire is from beginning of SMB2 header.
        let output_buffer = if buf_length > 0 {
            let current = cursor.position();
            let body_offset = buf_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_bytes_bounded(buf_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(QueryInfoResponse { output_buffer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── QueryInfoRequest tests ───────────────────────────────────────

    #[test]
    fn query_info_request_roundtrip_file_info() {
        let original = QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: 0x12, // FileAllInformation
            output_buffer_length: 4096,
            additional_information: 0,
            flags: 0,
            file_id: FileId {
                persistent: 0xDEAD_BEEF_CAFE_BABE,
                volatile: 0x1234_5678_9ABC_DEF0,
            },
            input_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryInfoRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.info_type, InfoType::File);
        assert_eq!(decoded.file_info_class, 0x12);
        assert_eq!(decoded.output_buffer_length, 4096);
        assert_eq!(decoded.additional_information, 0);
        assert_eq!(decoded.flags, 0);
        assert_eq!(decoded.file_id, original.file_id);
        assert!(decoded.input_buffer.is_empty());
    }

    #[test]
    fn query_info_request_with_input_buffer() {
        let input = vec![0x01, 0x02, 0x03, 0x04];
        let original = QueryInfoRequest {
            info_type: InfoType::Quota,
            file_info_class: 0x20,
            output_buffer_length: 8192,
            additional_information: 0x04, // SACL_SECURITY_INFORMATION
            flags: 0,
            file_id: FileId {
                persistent: 1,
                volatile: 2,
            },
            input_buffer: input.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryInfoRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.info_type, InfoType::Quota);
        assert_eq!(decoded.input_buffer, input);
    }

    #[test]
    fn query_info_request_structure_size() {
        let req = QueryInfoRequest {
            info_type: InfoType::File,
            file_info_class: 0,
            output_buffer_length: 0,
            additional_information: 0,
            flags: 0,
            file_id: FileId::default(),
            input_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes[0], 41);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn query_info_request_wrong_structure_size() {
        let mut buf = vec![0u8; 48];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = QueryInfoRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── QueryInfoResponse tests ──────────────────────────────────────

    #[test]
    fn query_info_response_roundtrip_with_data() {
        let info_data = vec![
            0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xA0, 0xB0, 0xC0,
        ];

        let original = QueryInfoResponse {
            output_buffer: info_data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryInfoResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.output_buffer, info_data);
    }

    #[test]
    fn query_info_response_empty() {
        let original = QueryInfoResponse {
            output_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // StructureSize(2) + Offset(2) + Length(4) = 8
        assert_eq!(bytes.len(), 8);

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryInfoResponse::unpack(&mut r).unwrap();

        assert!(decoded.output_buffer.is_empty());
    }

    #[test]
    fn query_info_response_structure_size() {
        let resp = QueryInfoResponse {
            output_buffer: vec![0xFF],
        };

        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes[0], 9);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn query_info_response_wrong_structure_size() {
        let mut buf = vec![0u8; 16];
        buf[0..2].copy_from_slice(&42u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = QueryInfoResponse::unpack(&mut cursor);
        assert!(result.is_err());
    }

    // ── Enum tests ───────────────────────────────────────────────────

    #[test]
    fn info_type_roundtrip() {
        for &it in &[
            InfoType::File,
            InfoType::Filesystem,
            InfoType::Security,
            InfoType::Quota,
        ] {
            let raw = it as u8;
            let decoded = InfoType::try_from(raw).unwrap();
            assert_eq!(decoded, it);
        }
    }

    #[test]
    fn info_type_invalid() {
        assert!(InfoType::try_from(0x00).is_err());
        assert!(InfoType::try_from(0x05).is_err());
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_bytes, arb_file_id, arb_info_type};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn query_info_request_pack_unpack(
            info_type in arb_info_type(),
            file_info_class in any::<u8>(),
            output_buffer_length in any::<u32>(),
            additional_information in any::<u32>(),
            flags in any::<u32>(),
            file_id in arb_file_id(),
            input_buffer in arb_bytes(),
        ) {
            let original = QueryInfoRequest {
                info_type,
                file_info_class,
                output_buffer_length,
                additional_information,
                flags,
                file_id,
                input_buffer,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = QueryInfoRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }

        #[test]
        fn query_info_response_pack_unpack(output_buffer in arb_bytes()) {
            let original = QueryInfoResponse { output_buffer };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = QueryInfoResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }
    }
}
