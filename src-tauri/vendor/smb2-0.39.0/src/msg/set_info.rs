//! SMB2 SET_INFO request and response (spec sections 2.2.39, 2.2.40).
//!
//! Used to set file, filesystem, security, or quota information.
//! The request buffer contains the information to set, stored as raw bytes.
//! The response is a minimal 2-byte structure.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

// Re-use InfoType from query_info
pub use super::query_info::InfoType;

// ── SetInfoRequest ───────────────────────────────────────────────────────

/// SMB2 SET_INFO request (spec section 2.2.39).
///
/// Sent by the client to set information on a file, filesystem,
/// security descriptor, or quota.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetInfoRequest {
    /// The type of information being set.
    pub info_type: InfoType,
    /// The file information class (interpretation depends on `info_type`).
    pub file_info_class: u8,
    /// Additional information flags (for example, security information flags).
    pub additional_information: u32,
    /// Handle to the file or directory.
    pub file_id: FileId,
    /// Raw buffer containing the information to set.
    pub buffer: Vec<u8>,
}

impl SetInfoRequest {
    pub const STRUCTURE_SIZE: u16 = 33;
}

impl Pack for SetInfoRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // InfoType (1 byte)
        cursor.write_u8(self.info_type as u8);
        // FileInfoClass (1 byte)
        cursor.write_u8(self.file_info_class);
        // BufferLength (4 bytes)
        cursor.write_u32_le(self.buffer.len() as u32);
        // BufferOffset (2 bytes) -- placeholder
        let offset_pos = cursor.position();
        cursor.write_u16_le(0);
        // Reserved (2 bytes)
        cursor.write_u16_le(0);
        // AdditionalInformation (4 bytes)
        cursor.write_u32_le(self.additional_information);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);

        // Buffer (variable)
        if !self.buffer.is_empty() {
            // Offset is from the beginning of the SMB2 header per spec.
            let buf_offset = Header::SIZE + (cursor.position() - start);
            cursor.write_bytes(&self.buffer);
            cursor.set_u16_le_at(offset_pos, buf_offset as u16);
        }
    }
}

impl Unpack for SetInfoRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid SetInfoRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // InfoType (1 byte)
        let info_type = InfoType::try_from(cursor.read_u8()?)?;
        // FileInfoClass (1 byte)
        let file_info_class = cursor.read_u8()?;
        // BufferLength (4 bytes)
        let buffer_length = cursor.read_u32_le()? as usize;
        // BufferOffset (2 bytes)
        let buf_offset = cursor.read_u16_le()? as usize;
        // Reserved (2 bytes)
        let _reserved = cursor.read_u16_le()?;
        // AdditionalInformation (4 bytes)
        let additional_information = cursor.read_u32_le()?;
        // FileId (16 bytes)
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let file_id = FileId {
            persistent,
            volatile,
        };

        // Read buffer
        // Offset on the wire is from beginning of SMB2 header.
        let buffer = if buffer_length > 0 {
            let current = cursor.position();
            let body_offset = buf_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_bytes_bounded(buffer_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(SetInfoRequest {
            info_type,
            file_info_class,
            additional_information,
            file_id,
            buffer,
        })
    }
}

// ── SetInfoResponse ──────────────────────────────────────────────────────

/// SMB2 SET_INFO response (spec section 2.2.40).
///
/// A minimal response indicating that the set operation succeeded.
/// Contains only the 2-byte StructureSize field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetInfoResponse;

impl SetInfoResponse {
    pub const STRUCTURE_SIZE: u16 = 2;
}

impl Pack for SetInfoResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
    }
}

impl Unpack for SetInfoResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid SetInfoResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        Ok(SetInfoResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SetInfoRequest tests ─────────────────────────────────────────

    #[test]
    fn set_info_request_roundtrip_with_buffer() {
        let info_data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];

        let original = SetInfoRequest {
            info_type: InfoType::File,
            file_info_class: 0x04, // FileBasicInformation
            additional_information: 0,
            file_id: FileId {
                persistent: 0xAAAA_BBBB_CCCC_DDDD,
                volatile: 0x1111_2222_3333_4444,
            },
            buffer: info_data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SetInfoRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.info_type, InfoType::File);
        assert_eq!(decoded.file_info_class, 0x04);
        assert_eq!(decoded.additional_information, 0);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.buffer, info_data);
    }

    #[test]
    fn set_info_request_security_info() {
        let sd_data = vec![0x01, 0x00, 0x04, 0x80, 0x00, 0x00, 0x00, 0x00];

        let original = SetInfoRequest {
            info_type: InfoType::Security,
            file_info_class: 0,
            additional_information: 0x04, // DACL_SECURITY_INFORMATION
            file_id: FileId {
                persistent: 42,
                volatile: 99,
            },
            buffer: sd_data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SetInfoRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.info_type, InfoType::Security);
        assert_eq!(decoded.additional_information, 0x04);
        assert_eq!(decoded.buffer, sd_data);
    }

    #[test]
    fn set_info_request_structure_size() {
        let req = SetInfoRequest {
            info_type: InfoType::File,
            file_info_class: 0,
            additional_information: 0,
            file_id: FileId::default(),
            buffer: vec![0x01],
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes[0], 33);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn set_info_request_wrong_structure_size() {
        let mut buf = vec![0u8; 48];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = SetInfoRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── SetInfoResponse tests ────────────────────────────────────────

    #[test]
    fn set_info_response_roundtrip() {
        let original = SetInfoResponse;

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Only 2 bytes
        assert_eq!(bytes.len(), 2);
        assert_eq!(bytes, [0x02, 0x00]);

        let mut r = ReadCursor::new(&bytes);
        let decoded = SetInfoResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded, SetInfoResponse);
    }

    #[test]
    fn set_info_response_wrong_structure_size() {
        let bytes = [0x04, 0x00];
        let mut cursor = ReadCursor::new(&bytes);
        let result = SetInfoResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn set_info_response_too_short() {
        let bytes = [0x02];
        let mut cursor = ReadCursor::new(&bytes);
        let result = SetInfoResponse::unpack(&mut cursor);
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_bytes, arb_file_id, arb_info_type};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn set_info_request_pack_unpack(
            info_type in arb_info_type(),
            file_info_class in any::<u8>(),
            additional_information in any::<u32>(),
            file_id in arb_file_id(),
            buffer in arb_bytes(),
        ) {
            let original = SetInfoRequest {
                info_type,
                file_info_class,
                additional_information,
                file_id,
                buffer,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = SetInfoRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }
    }
}
