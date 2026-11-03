//! SMB2 QUERY_DIRECTORY request and response (spec sections 2.2.33, 2.2.34).
//!
//! Used by the client to enumerate directory contents. The request specifies
//! a search pattern (typically `"*"`) and the response contains directory
//! entries in the requested information class format.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

// ── Enums / flags ────────────────────────────────────────────────────────

/// File information class for directory queries (MS-SMB2 2.2.33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FileInformationClass {
    /// Basic directory information.
    FileDirectoryInformation = 0x01,
    /// Full directory information.
    FileFullDirectoryInformation = 0x02,
    /// Both short and long name information.
    FileBothDirectoryInformation = 0x03,
    /// File names only.
    FileNamesInformation = 0x0C,
    /// Both short and long name information with file IDs.
    FileIdBothDirectoryInformation = 0x25,
    /// Full directory information with file IDs.
    FileIdFullDirectoryInformation = 0x26,
}

impl TryFrom<u8> for FileInformationClass {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::FileDirectoryInformation),
            0x02 => Ok(Self::FileFullDirectoryInformation),
            0x03 => Ok(Self::FileBothDirectoryInformation),
            0x0C => Ok(Self::FileNamesInformation),
            0x25 => Ok(Self::FileIdBothDirectoryInformation),
            0x26 => Ok(Self::FileIdFullDirectoryInformation),
            _ => Err(Error::invalid_data(format!(
                "invalid FileInformationClass: 0x{:02X}",
                value
            ))),
        }
    }
}

/// Query directory flags (MS-SMB2 2.2.33).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueryDirectoryFlags(pub u8);

impl QueryDirectoryFlags {
    /// Restart the enumeration from the beginning.
    pub const RESTART_SCANS: u8 = 0x01;
    /// Return only a single entry.
    pub const RETURN_SINGLE_ENTRY: u8 = 0x02;
    /// Resume from the specified file index.
    pub const INDEX_SPECIFIED: u8 = 0x04;
    /// Reopen the directory and change the search pattern.
    pub const REOPEN: u8 = 0x10;
}

// ── QueryDirectoryRequest ────────────────────────────────────────────────

/// SMB2 QUERY_DIRECTORY request (spec section 2.2.33).
///
/// Sent by the client to enumerate files in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDirectoryRequest {
    /// The type of information to return for each directory entry.
    pub file_information_class: FileInformationClass,
    /// Flags controlling the query behavior.
    pub flags: QueryDirectoryFlags,
    /// Byte offset within the directory to resume enumeration from.
    pub file_index: u32,
    /// Handle to the directory being queried.
    pub file_id: FileId,
    /// Maximum number of bytes the server can return.
    pub output_buffer_length: u32,
    /// Search pattern (for example, `"*"` for all files).
    pub file_name: String,
}

impl QueryDirectoryRequest {
    pub const STRUCTURE_SIZE: u16 = 33;
}

impl Pack for QueryDirectoryRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // FileInformationClass (1 byte)
        cursor.write_u8(self.file_information_class as u8);
        // Flags (1 byte)
        cursor.write_u8(self.flags.0);
        // FileIndex (4 bytes)
        cursor.write_u32_le(self.file_index);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
        // FileNameOffset (2 bytes) -- placeholder
        let name_offset_pos = cursor.position();
        cursor.write_u16_le(0);
        // FileNameLength (2 bytes) -- placeholder
        let name_length_pos = cursor.position();
        cursor.write_u16_le(0);
        // OutputBufferLength (4 bytes)
        cursor.write_u32_le(self.output_buffer_length);

        if self.file_name.is_empty() {
            // No search pattern: FileNameOffset and FileNameLength stay 0
            // per spec section 2.2.33. Write 1 padding byte to satisfy
            // StructureSize=33 (32 fixed + 1 byte buffer minimum).
            cursor.write_u8(0);
        } else {
            // Buffer: filename pattern in UTF-16LE.
            // Offset is from the beginning of the SMB2 header per spec.
            let name_offset = Header::SIZE + (cursor.position() - start);
            let name_start = cursor.position();
            cursor.write_utf16_le(&self.file_name);
            let name_byte_len = cursor.position() - name_start;

            // Backpatch
            cursor.set_u16_le_at(name_offset_pos, name_offset as u16);
            cursor.set_u16_le_at(name_length_pos, name_byte_len as u16);
        }
    }
}

impl Unpack for QueryDirectoryRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid QueryDirectoryRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // FileInformationClass (1 byte)
        let info_class = FileInformationClass::try_from(cursor.read_u8()?)?;
        // Flags (1 byte)
        let flags = QueryDirectoryFlags(cursor.read_u8()?);
        // FileIndex (4 bytes)
        let file_index = cursor.read_u32_le()?;
        // FileId (16 bytes)
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let file_id = FileId {
            persistent,
            volatile,
        };
        // FileNameOffset (2 bytes)
        let name_offset = cursor.read_u16_le()? as usize;
        // FileNameLength (2 bytes)
        let name_length = cursor.read_u16_le()? as usize;
        // OutputBufferLength (4 bytes)
        let output_buffer_length = cursor.read_u32_le()?;

        // Read filename
        // Offset on the wire is from beginning of SMB2 header.
        let file_name = if name_length > 0 {
            let current = cursor.position();
            let body_offset = name_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_utf16_le(name_length)?
        } else {
            String::new()
        };

        Ok(QueryDirectoryRequest {
            file_information_class: info_class,
            flags,
            file_index,
            file_id,
            output_buffer_length,
            file_name,
        })
    }
}

// ── QueryDirectoryResponse ───────────────────────────────────────────────

/// SMB2 QUERY_DIRECTORY response (spec section 2.2.34).
///
/// Contains directory enumeration data as raw bytes. The format depends
/// on the `FileInformationClass` from the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryDirectoryResponse {
    /// Raw output buffer containing directory entries.
    pub output_buffer: Vec<u8>,
}

impl QueryDirectoryResponse {
    pub const STRUCTURE_SIZE: u16 = 9;
}

impl Pack for QueryDirectoryResponse {
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

impl Unpack for QueryDirectoryResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid QueryDirectoryResponse structure size: expected {}, got {}",
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

        Ok(QueryDirectoryResponse { output_buffer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── QueryDirectoryRequest tests ──────────────────────────────────

    #[test]
    fn query_directory_request_roundtrip_star_pattern() {
        let original = QueryDirectoryRequest {
            file_information_class: FileInformationClass::FileBothDirectoryInformation,
            flags: QueryDirectoryFlags(QueryDirectoryFlags::RESTART_SCANS),
            file_index: 0,
            file_id: FileId {
                persistent: 0xAAAA_BBBB_CCCC_DDDD,
                volatile: 0x1111_2222_3333_4444,
            },
            output_buffer_length: 65536,
            file_name: "*".to_string(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryDirectoryRequest::unpack(&mut r).unwrap();

        assert_eq!(
            decoded.file_information_class,
            FileInformationClass::FileBothDirectoryInformation
        );
        assert_eq!(decoded.flags.0, QueryDirectoryFlags::RESTART_SCANS);
        assert_eq!(decoded.file_index, 0);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.output_buffer_length, 65536);
        assert_eq!(decoded.file_name, "*");
    }

    #[test]
    fn query_directory_request_structure_size() {
        let req = QueryDirectoryRequest {
            file_information_class: FileInformationClass::FileDirectoryInformation,
            flags: QueryDirectoryFlags::default(),
            file_index: 0,
            file_id: FileId::default(),
            output_buffer_length: 1024,
            file_name: "*".to_string(),
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes[0], 33);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn query_directory_request_wrong_structure_size() {
        let mut buf = vec![0u8; 40];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = QueryDirectoryRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── QueryDirectoryResponse tests ─────────────────────────────────

    #[test]
    fn query_directory_response_roundtrip_with_buffer() {
        // Simulate raw directory entry data
        let raw_entries = vec![
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x10,
        ];

        let original = QueryDirectoryResponse {
            output_buffer: raw_entries.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryDirectoryResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.output_buffer, raw_entries);
    }

    #[test]
    fn query_directory_response_empty_buffer() {
        let original = QueryDirectoryResponse {
            output_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // StructureSize(2) + Offset(2) + Length(4) = 8 bytes
        assert_eq!(bytes.len(), 8);

        let mut r = ReadCursor::new(&bytes);
        let decoded = QueryDirectoryResponse::unpack(&mut r).unwrap();

        assert!(decoded.output_buffer.is_empty());
    }

    #[test]
    fn query_directory_response_structure_size() {
        let resp = QueryDirectoryResponse {
            output_buffer: vec![0xFF],
        };

        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes[0], 9);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn query_directory_response_wrong_structure_size() {
        let mut buf = vec![0u8; 16];
        buf[0..2].copy_from_slice(&42u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = QueryDirectoryResponse::unpack(&mut cursor);
        assert!(result.is_err());
    }

    // ── Enum tests ───────────────────────────────────────────────────

    #[test]
    fn file_information_class_roundtrip() {
        for &class in &[
            FileInformationClass::FileDirectoryInformation,
            FileInformationClass::FileFullDirectoryInformation,
            FileInformationClass::FileBothDirectoryInformation,
            FileInformationClass::FileNamesInformation,
            FileInformationClass::FileIdFullDirectoryInformation,
            FileInformationClass::FileIdBothDirectoryInformation,
        ] {
            let raw = class as u8;
            let decoded = FileInformationClass::try_from(raw).unwrap();
            assert_eq!(decoded, class);
        }
    }

    #[test]
    fn file_information_class_invalid() {
        assert!(FileInformationClass::try_from(0xFF).is_err());
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{
        arb_bytes, arb_file_id, arb_file_information_class, arb_utf16_string,
    };
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn query_directory_request_pack_unpack(
            file_information_class in arb_file_information_class(),
            flags_raw in any::<u8>(),
            file_index in any::<u32>(),
            file_id in arb_file_id(),
            output_buffer_length in any::<u32>(),
            // Search pattern is UTF-16LE on the wire. Allow empty + typical.
            file_name in arb_utf16_string(128),
        ) {
            let original = QueryDirectoryRequest {
                file_information_class,
                flags: QueryDirectoryFlags(flags_raw),
                file_index,
                file_id,
                output_buffer_length,
                file_name,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = QueryDirectoryRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }

        #[test]
        fn query_directory_response_pack_unpack(output_buffer in arb_bytes()) {
            let original = QueryDirectoryResponse { output_buffer };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = QueryDirectoryResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }
    }
}
