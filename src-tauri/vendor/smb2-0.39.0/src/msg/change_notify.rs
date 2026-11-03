//! SMB2 CHANGE_NOTIFY Request and Response (MS-SMB2 sections 2.2.35, 2.2.36).
//!
//! The CHANGE_NOTIFY request registers for change notifications on a
//! directory. The response returns FILE_NOTIFY_INFORMATION entries
//! describing the changes that occurred.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

// ── Change Notify flags ────────────────────────────────────────────────

/// Watch the entire subtree (recursive).
pub const SMB2_WATCH_TREE: u16 = 0x0001;

// ── CompletionFilter values ────────────────────────────────────────────

/// Notify when a file name changes.
pub const FILE_NOTIFY_CHANGE_FILE_NAME: u32 = 0x0000_0001;

/// Notify when a directory name changes.
pub const FILE_NOTIFY_CHANGE_DIR_NAME: u32 = 0x0000_0002;

/// Notify when file attributes change.
pub const FILE_NOTIFY_CHANGE_ATTRIBUTES: u32 = 0x0000_0004;

/// Notify when the file size changes.
pub const FILE_NOTIFY_CHANGE_SIZE: u32 = 0x0000_0008;

/// Notify when the last write time changes.
pub const FILE_NOTIFY_CHANGE_LAST_WRITE: u32 = 0x0000_0010;

/// Notify when the last access time changes.
pub const FILE_NOTIFY_CHANGE_LAST_ACCESS: u32 = 0x0000_0020;

/// Notify when the creation time changes.
pub const FILE_NOTIFY_CHANGE_CREATION: u32 = 0x0000_0040;

/// Notify when extended attributes change.
pub const FILE_NOTIFY_CHANGE_EA: u32 = 0x0000_0080;

/// Notify when the security descriptor changes.
pub const FILE_NOTIFY_CHANGE_SECURITY: u32 = 0x0000_0100;

/// Notify when a stream name changes.
pub const FILE_NOTIFY_CHANGE_STREAM_NAME: u32 = 0x0000_0200;

/// Notify when a stream size changes.
pub const FILE_NOTIFY_CHANGE_STREAM_SIZE: u32 = 0x0000_0400;

/// Notify when stream data is written.
pub const FILE_NOTIFY_CHANGE_STREAM_WRITE: u32 = 0x0000_0800;

// ── ChangeNotifyRequest ────────────────────────────────────────────────

/// SMB2 CHANGE_NOTIFY Request (MS-SMB2 section 2.2.35).
///
/// Registers for directory change notifications. The structure is 32 bytes:
/// - StructureSize (2 bytes, must be 32)
/// - Flags (2 bytes)
/// - OutputBufferLength (4 bytes)
/// - FileId (16 bytes)
/// - CompletionFilter (4 bytes)
/// - Reserved (4 bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeNotifyRequest {
    /// Flags controlling the notification. Use `SMB2_WATCH_TREE` for recursive.
    pub flags: u16,
    /// Maximum size of the output buffer for notification data.
    pub output_buffer_length: u32,
    /// The directory handle to watch.
    pub file_id: FileId,
    /// Bitmask of change types to watch for.
    pub completion_filter: u32,
}

impl ChangeNotifyRequest {
    pub const STRUCTURE_SIZE: u16 = 32;
}

impl Pack for ChangeNotifyRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Flags (2 bytes)
        cursor.write_u16_le(self.flags);
        // OutputBufferLength (4 bytes)
        cursor.write_u32_le(self.output_buffer_length);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
        // CompletionFilter (4 bytes)
        cursor.write_u32_le(self.completion_filter);
        // Reserved (4 bytes)
        cursor.write_u32_le(0);
    }
}

impl Unpack for ChangeNotifyRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid ChangeNotifyRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let flags = cursor.read_u16_le()?;
        let output_buffer_length = cursor.read_u32_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let completion_filter = cursor.read_u32_le()?;
        let _reserved = cursor.read_u32_le()?;

        Ok(ChangeNotifyRequest {
            flags,
            output_buffer_length,
            file_id: FileId {
                persistent,
                volatile,
            },
            completion_filter,
        })
    }
}

// ── ChangeNotifyResponse ───────────────────────────────────────────────

/// SMB2 CHANGE_NOTIFY Response (MS-SMB2 section 2.2.36).
///
/// Returns FILE_NOTIFY_INFORMATION entries describing directory changes.
/// The buffer contains raw FILE_NOTIFY_INFORMATION entries; parsing those
/// is left to the caller for now.
///
/// Layout:
/// - StructureSize (2 bytes, must be 9)
/// - OutputBufferOffset (2 bytes)
/// - OutputBufferLength (4 bytes)
/// - Buffer (variable, OutputBufferLength bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeNotifyResponse {
    /// Raw FILE_NOTIFY_INFORMATION data. Parsing individual entries is
    /// deferred to a higher layer.
    pub output_data: Vec<u8>,
}

impl ChangeNotifyResponse {
    pub const STRUCTURE_SIZE: u16 = 9;

    /// Fixed header size before the variable buffer (8 bytes).
    const FIXED_SIZE: u32 = 8;
}

impl Pack for ChangeNotifyResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);

        let output_len = self.output_data.len() as u32;
        // Offset is from the beginning of the SMB2 header per spec.
        let output_offset = if output_len > 0 {
            (start as u32) + Self::FIXED_SIZE
        } else {
            0
        };

        // OutputBufferOffset (2 bytes)
        cursor.write_u16_le(output_offset as u16);
        // OutputBufferLength (4 bytes)
        cursor.write_u32_le(output_len);
        // Buffer (variable)
        cursor.write_bytes(&self.output_data);
    }
}

impl Unpack for ChangeNotifyResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid ChangeNotifyResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let _output_buffer_offset = cursor.read_u16_le()?;
        let output_buffer_length = cursor.read_u32_le()?;

        let output_data = if output_buffer_length > 0 {
            cursor
                .read_bytes_bounded(output_buffer_length as usize)?
                .to_vec()
        } else {
            Vec::new()
        };

        Ok(ChangeNotifyResponse { output_data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ChangeNotifyRequest tests ─────────────────────────────────────

    #[test]
    fn change_notify_request_roundtrip_recursive() {
        let original = ChangeNotifyRequest {
            flags: SMB2_WATCH_TREE,
            output_buffer_length: 65536,
            file_id: FileId {
                persistent: 0x1122_3344_5566_7788,
                volatile: 0xAABB_CCDD_EEFF_0011,
            },
            completion_filter: FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_LAST_WRITE,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed 32 bytes, no variable data
        assert_eq!(bytes.len(), 32);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ChangeNotifyRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags, SMB2_WATCH_TREE);
        assert_eq!(decoded.output_buffer_length, 65536);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(
            decoded.completion_filter,
            FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_LAST_WRITE
        );
    }

    #[test]
    fn change_notify_request_wrong_structure_size() {
        let mut buf = [0u8; 32];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = ChangeNotifyRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── ChangeNotifyResponse tests ────────────────────────────────────

    #[test]
    fn change_notify_response_roundtrip_with_data() {
        let notify_data = vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let original = ChangeNotifyResponse {
            output_data: notify_data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed 8 bytes + 8 bytes data
        assert_eq!(bytes.len(), 16);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ChangeNotifyResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.output_data, notify_data);
    }

    #[test]
    fn change_notify_response_roundtrip_empty() {
        let original = ChangeNotifyResponse {
            output_data: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), 8);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ChangeNotifyResponse::unpack(&mut r).unwrap();

        assert!(decoded.output_data.is_empty());
    }

    #[test]
    fn change_notify_response_wrong_structure_size() {
        let mut buf = [0u8; 8];
        buf[0..2].copy_from_slice(&42u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = ChangeNotifyResponse::unpack(&mut cursor);
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
        fn change_notify_request_pack_unpack(
            flags in any::<u16>(),
            output_buffer_length in any::<u32>(),
            file_id in arb_file_id(),
            completion_filter in any::<u32>(),
        ) {
            let original = ChangeNotifyRequest {
                flags,
                output_buffer_length,
                file_id,
                completion_filter,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = ChangeNotifyRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn change_notify_response_pack_unpack(output_data in arb_bytes()) {
            let original = ChangeNotifyResponse { output_data };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = ChangeNotifyResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
