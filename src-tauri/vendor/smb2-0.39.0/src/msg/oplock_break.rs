//! SMB2 Oplock Break Notification, Acknowledgment, and Response
//! (MS-SMB2 sections 2.2.23, 2.2.24, 2.2.25).
//!
//! All three oplock break messages share an identical 24-byte wire format:
//! - StructureSize (2 bytes, must be 24)
//! - OplockLevel (1 byte)
//! - Reserved (1 byte)
//! - Reserved2 (4 bytes)
//! - FileId (16 bytes)
//!
//! We define one shared struct and provide type aliases for each role.
//!
//! Note: Lease break notification/acknowledgment/response (sections 2.2.23.2,
//! 2.2.24.2, 2.2.25.2) use a different structure with LeaseKey, LeaseState,
//! etc. Lease break handling is deferred to a future implementation.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::{FileId, OplockLevel};
use crate::Error;

// ── OplockBreak (shared struct) ────────────────────────────────────────

/// Shared wire format for oplock break notification, acknowledgment, and
/// response messages (MS-SMB2 sections 2.2.23, 2.2.24, 2.2.25).
///
/// All three messages have an identical 24-byte layout. The message's role
/// (notification vs acknowledgment vs response) is determined by the header's
/// command code and flags, not by this structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OplockBreak {
    /// The oplock level.
    pub oplock_level: OplockLevel,
    /// The file handle associated with the oplock.
    pub file_id: FileId,
}

impl OplockBreak {
    pub const STRUCTURE_SIZE: u16 = 24;
}

impl Pack for OplockBreak {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // OplockLevel (1 byte)
        cursor.write_u8(self.oplock_level as u8);
        // Reserved (1 byte)
        cursor.write_u8(0);
        // Reserved2 (4 bytes)
        cursor.write_u32_le(0);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
    }
}

impl Unpack for OplockBreak {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid OplockBreak structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let oplock_level = OplockLevel::try_from(cursor.read_u8()?)?;
        let _reserved = cursor.read_u8()?;
        let _reserved2 = cursor.read_u32_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;

        Ok(OplockBreak {
            oplock_level,
            file_id: FileId {
                persistent,
                volatile,
            },
        })
    }
}

/// Oplock break notification (server to client, MS-SMB2 section 2.2.23).
///
/// Arrives with `MessageId = 0xFFFFFFFFFFFFFFFF` (unsolicited).
pub type OplockBreakNotification = OplockBreak;

/// Oplock break acknowledgment (client to server, MS-SMB2 section 2.2.24).
pub type OplockBreakAcknowledgment = OplockBreak;

/// Oplock break response (server to client after ack, MS-SMB2 section 2.2.25).
pub type OplockBreakResponse = OplockBreak;

#[cfg(test)]
mod tests {
    use super::*;

    // ── OplockBreakNotification tests ─────────────────────────────────

    #[test]
    fn oplock_break_notification_roundtrip() {
        let original = OplockBreakNotification {
            oplock_level: OplockLevel::LevelII,
            file_id: FileId {
                persistent: 0x1122_3344_5566_7788,
                volatile: 0xAABB_CCDD_EEFF_0011,
            },
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed 24 bytes
        assert_eq!(bytes.len(), 24);

        let mut r = ReadCursor::new(&bytes);
        let decoded = OplockBreakNotification::unpack(&mut r).unwrap();

        assert_eq!(decoded.oplock_level, OplockLevel::LevelII);
        assert_eq!(decoded.file_id, original.file_id);
    }

    #[test]
    fn oplock_break_notification_exclusive_level() {
        let original = OplockBreakNotification {
            oplock_level: OplockLevel::Exclusive,
            file_id: FileId {
                persistent: 0x42,
                volatile: 0x99,
            },
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = OplockBreakNotification::unpack(&mut r).unwrap();

        assert_eq!(decoded.oplock_level, OplockLevel::Exclusive);
        assert_eq!(decoded.file_id.persistent, 0x42);
        assert_eq!(decoded.file_id.volatile, 0x99);
    }

    // ── OplockBreakAcknowledgment tests ───────────────────────────────

    #[test]
    fn oplock_break_acknowledgment_roundtrip() {
        let original = OplockBreakAcknowledgment {
            oplock_level: OplockLevel::None,
            file_id: FileId {
                persistent: 0xDEAD,
                volatile: 0xBEEF,
            },
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), 24);

        let mut r = ReadCursor::new(&bytes);
        let decoded = OplockBreakAcknowledgment::unpack(&mut r).unwrap();

        assert_eq!(decoded.oplock_level, OplockLevel::None);
        assert_eq!(decoded.file_id, original.file_id);
    }

    // ── OplockBreakResponse tests ─────────────────────────────────────

    #[test]
    fn oplock_break_response_roundtrip() {
        let original = OplockBreakResponse {
            oplock_level: OplockLevel::Batch,
            file_id: FileId {
                persistent: 0xCAFE,
                volatile: 0xFACE,
            },
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), 24);

        let mut r = ReadCursor::new(&bytes);
        let decoded = OplockBreakResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.oplock_level, OplockLevel::Batch);
        assert_eq!(decoded.file_id, original.file_id);
    }

    // ── Error tests ───────────────────────────────────────────────────

    #[test]
    fn oplock_break_wrong_structure_size() {
        let mut buf = [0u8; 24];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = OplockBreak::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // Roundtrip property tests live in `roundtrip_props` at file end.

    #[test]
    fn oplock_break_reserved_fields_ignored() {
        let mut buf = [0u8; 24];
        // StructureSize = 24
        buf[0..2].copy_from_slice(&24u16.to_le_bytes());
        // OplockLevel = LEVEL_II
        buf[2] = OplockLevel::LevelII as u8;
        // Reserved = 0xFF (should be ignored)
        buf[3] = 0xFF;
        // Reserved2 = 0xDEADBEEF (should be ignored)
        buf[4..8].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
        // FileId persistent = 1
        buf[8..16].copy_from_slice(&1u64.to_le_bytes());
        // FileId volatile = 2
        buf[16..24].copy_from_slice(&2u64.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let decoded = OplockBreak::unpack(&mut cursor).unwrap();

        assert_eq!(decoded.oplock_level, OplockLevel::LevelII);
        assert_eq!(decoded.file_id.persistent, 1);
        assert_eq!(decoded.file_id.volatile, 2);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_file_id, arb_oplock_level};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn oplock_break_pack_unpack(
            oplock_level in arb_oplock_level(),
            file_id in arb_file_id(),
        ) {
            let original = OplockBreak { oplock_level, file_id };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = OplockBreak::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
