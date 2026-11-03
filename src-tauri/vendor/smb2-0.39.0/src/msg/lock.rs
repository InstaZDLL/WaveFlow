//! SMB2 LOCK Request and Response (MS-SMB2 sections 2.2.26, 2.2.27).
//!
//! The LOCK request locks or unlocks byte ranges within a file.
//! Multiple ranges can be locked/unlocked in a single request.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

/// Lock flag: shared lock (allows other readers).
pub const SMB2_LOCKFLAG_SHARED_LOCK: u32 = 0x0000_0001;

/// Lock flag: exclusive lock (no other readers or writers).
pub const SMB2_LOCKFLAG_EXCLUSIVE_LOCK: u32 = 0x0000_0002;

/// Lock flag: unlock a previously locked range.
pub const SMB2_LOCKFLAG_UNLOCK: u32 = 0x0000_0004;

/// Lock flag: fail immediately if the lock conflicts.
pub const SMB2_LOCKFLAG_FAIL_IMMEDIATELY: u32 = 0x0000_0010;

/// A single lock element describing a byte range to lock or unlock.
///
/// Each element is 24 bytes on the wire:
/// - Offset (8 bytes)
/// - Length (8 bytes)
/// - Flags (4 bytes)
/// - Reserved (4 bytes)
///
/// Reference: MS-SMB2 section 2.2.26.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockElement {
    /// Starting offset in bytes from where the range begins.
    pub offset: u64,
    /// Length of the range in bytes.
    pub length: u64,
    /// Flags describing how the range is locked or unlocked.
    pub flags: u32,
}

impl LockElement {
    /// Wire size of a single lock element.
    pub const SIZE: usize = 24;
}

impl Pack for LockElement {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u64_le(self.offset);
        cursor.write_u64_le(self.length);
        cursor.write_u32_le(self.flags);
        cursor.write_u32_le(0); // Reserved
    }
}

impl Unpack for LockElement {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let offset = cursor.read_u64_le()?;
        let length = cursor.read_u64_le()?;
        let flags = cursor.read_u32_le()?;
        let _reserved = cursor.read_u32_le()?;

        Ok(LockElement {
            offset,
            length,
            flags,
        })
    }
}

/// SMB2 LOCK Request (MS-SMB2 section 2.2.26).
///
/// Sent by the client to lock or unlock byte ranges. The fixed portion
/// is 48 bytes (StructureSize=48, which includes one `LockElement`):
/// - StructureSize (2 bytes, must be 48)
/// - LockCount (2 bytes)
/// - LockSequenceNumber/Index (4 bytes)
/// - FileId (16 bytes)
/// - Locks (variable, LockCount x 24 bytes each)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockRequest {
    /// Combined lock sequence number (4 bits) and index (28 bits).
    /// In SMB 2.0.2 this field is reserved (0).
    pub lock_sequence: u32,
    /// File handle to lock ranges on.
    pub file_id: FileId,
    /// Array of lock elements. Must contain at least one element.
    pub locks: Vec<LockElement>,
}

impl LockRequest {
    pub const STRUCTURE_SIZE: u16 = 48;
}

impl Pack for LockRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u16_le(self.locks.len() as u16); // LockCount
        cursor.write_u32_le(self.lock_sequence);
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
        for lock in &self.locks {
            lock.pack(cursor);
        }
    }
}

impl Unpack for LockRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid LockRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let lock_count = cursor.read_u16_le()?;
        let lock_sequence = cursor.read_u32_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;

        let mut locks = Vec::with_capacity(lock_count as usize);
        for _ in 0..lock_count {
            locks.push(LockElement::unpack(cursor)?);
        }

        Ok(LockRequest {
            lock_sequence,
            file_id: FileId {
                persistent,
                volatile,
            },
            locks,
        })
    }
}

/// SMB2 LOCK Response (MS-SMB2 section 2.2.27).
///
/// Sent by the server to confirm a lock operation. The structure is 4 bytes:
/// - StructureSize (2 bytes, must be 4)
/// - Reserved (2 bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockResponse;

impl LockResponse {
    pub const STRUCTURE_SIZE: u16 = 4;
}

impl Pack for LockResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        cursor.write_u16_le(0); // Reserved
    }
}

impl Unpack for LockResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid LockResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let _reserved = cursor.read_u16_le()?;

        Ok(LockResponse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── LockElement tests ──────────────────────────────────────────

    #[test]
    fn lock_element_roundtrip() {
        let original = LockElement {
            offset: 0x1000,
            length: 0x2000,
            flags: SMB2_LOCKFLAG_EXCLUSIVE_LOCK | SMB2_LOCKFLAG_FAIL_IMMEDIATELY,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), LockElement::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = LockElement::unpack(&mut r).unwrap();

        assert_eq!(decoded, original);
    }

    // ── LockRequest tests ──────────────────────────────────────────

    #[test]
    fn lock_request_single_lock_roundtrip() {
        let original = LockRequest {
            lock_sequence: 0,
            file_id: FileId {
                persistent: 0xDEAD,
                volatile: 0xBEEF,
            },
            locks: vec![LockElement {
                offset: 0,
                length: 4096,
                flags: SMB2_LOCKFLAG_SHARED_LOCK,
            }],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 24 bytes + 1 lock element (24 bytes) = 48 bytes
        assert_eq!(bytes.len(), 48);

        let mut r = ReadCursor::new(&bytes);
        let decoded = LockRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.lock_sequence, original.lock_sequence);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.locks.len(), 1);
        assert_eq!(decoded.locks[0], original.locks[0]);
    }

    #[test]
    fn lock_request_multiple_locks_roundtrip() {
        let original = LockRequest {
            lock_sequence: 0x1234_5678,
            file_id: FileId {
                persistent: 0x1111,
                volatile: 0x2222,
            },
            locks: vec![
                LockElement {
                    offset: 0,
                    length: 1024,
                    flags: SMB2_LOCKFLAG_EXCLUSIVE_LOCK | SMB2_LOCKFLAG_FAIL_IMMEDIATELY,
                },
                LockElement {
                    offset: 4096,
                    length: 2048,
                    flags: SMB2_LOCKFLAG_SHARED_LOCK,
                },
                LockElement {
                    offset: 8192,
                    length: 512,
                    flags: SMB2_LOCKFLAG_UNLOCK,
                },
            ],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed: 24 bytes + 3 lock elements (3 * 24) = 96 bytes
        assert_eq!(bytes.len(), 96);

        let mut r = ReadCursor::new(&bytes);
        let decoded = LockRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.lock_sequence, original.lock_sequence);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.locks.len(), 3);
        assert_eq!(decoded.locks[0], original.locks[0]);
        assert_eq!(decoded.locks[1], original.locks[1]);
        assert_eq!(decoded.locks[2], original.locks[2]);
    }

    #[test]
    fn lock_request_known_bytes() {
        let mut buf = Vec::new();
        // StructureSize = 48
        buf.extend_from_slice(&48u16.to_le_bytes());
        // LockCount = 1
        buf.extend_from_slice(&1u16.to_le_bytes());
        // LockSequence = 0
        buf.extend_from_slice(&0u32.to_le_bytes());
        // FileId persistent = 0x10
        buf.extend_from_slice(&0x10u64.to_le_bytes());
        // FileId volatile = 0x20
        buf.extend_from_slice(&0x20u64.to_le_bytes());
        // LockElement: offset = 0, length = 100, flags = SHARED (1), reserved = 0
        buf.extend_from_slice(&0u64.to_le_bytes());
        buf.extend_from_slice(&100u64.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let req = LockRequest::unpack(&mut cursor).unwrap();

        assert_eq!(req.file_id.persistent, 0x10);
        assert_eq!(req.file_id.volatile, 0x20);
        assert_eq!(req.locks.len(), 1);
        assert_eq!(req.locks[0].offset, 0);
        assert_eq!(req.locks[0].length, 100);
        assert_eq!(req.locks[0].flags, SMB2_LOCKFLAG_SHARED_LOCK);
    }

    #[test]
    fn lock_request_wrong_structure_size() {
        let mut buf = [0u8; 48];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = LockRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── LockResponse tests ─────────────────────────────────────────

    #[test]
    fn lock_response_roundtrip() {
        let original = LockResponse;

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // 2 + 2 = 4 bytes
        assert_eq!(bytes.len(), 4);

        let mut r = ReadCursor::new(&bytes);
        let _decoded = LockResponse::unpack(&mut r).unwrap();
    }

    #[test]
    fn lock_response_known_bytes() {
        let mut buf = [0u8; 4];
        buf[0..2].copy_from_slice(&4u16.to_le_bytes());
        buf[2..4].copy_from_slice(&0u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let _resp = LockResponse::unpack(&mut cursor).unwrap();
    }

    #[test]
    fn lock_response_wrong_structure_size() {
        let mut buf = [0u8; 4];
        buf[0..2].copy_from_slice(&8u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = LockResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn lock_flags_combinations() {
        // Verify flag constants are distinct and correct
        assert_eq!(SMB2_LOCKFLAG_SHARED_LOCK, 0x01);
        assert_eq!(SMB2_LOCKFLAG_EXCLUSIVE_LOCK, 0x02);
        assert_eq!(SMB2_LOCKFLAG_UNLOCK, 0x04);
        assert_eq!(SMB2_LOCKFLAG_FAIL_IMMEDIATELY, 0x10);

        // Shared + fail immediately
        let combined = SMB2_LOCKFLAG_SHARED_LOCK | SMB2_LOCKFLAG_FAIL_IMMEDIATELY;
        assert_eq!(combined, 0x11);

        // Exclusive + fail immediately
        let combined = SMB2_LOCKFLAG_EXCLUSIVE_LOCK | SMB2_LOCKFLAG_FAIL_IMMEDIATELY;
        assert_eq!(combined, 0x12);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::arb_file_id;
    use proptest::prelude::*;

    fn arb_lock_element() -> impl Strategy<Value = LockElement> {
        (any::<u64>(), any::<u64>(), any::<u32>()).prop_map(|(offset, length, flags)| LockElement {
            offset,
            length,
            flags,
        })
    }

    proptest! {
        #[test]
        fn lock_element_pack_unpack(elem in arb_lock_element()) {
            let mut w = WriteCursor::new();
            elem.pack(&mut w);
            let bytes = w.into_inner();
            prop_assert_eq!(bytes.len(), LockElement::SIZE);

            let mut r = ReadCursor::new(&bytes);
            let decoded = LockElement::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, elem);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn lock_request_pack_unpack(
            lock_sequence in any::<u32>(),
            file_id in arb_file_id(),
            // MS-SMB2: LockCount must be >= 1, so generate 1..=8.
            locks in prop::collection::vec(arb_lock_element(), 1..=8),
        ) {
            let original = LockRequest {
                lock_sequence,
                file_id,
                locks,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = LockRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn lock_response_pack_unpack(_ in any::<bool>()) {
            // LockResponse is a unit struct; there's nothing to vary, but
            // running it through the proptest harness keeps the coverage
            // map uniform.
            let original = LockResponse;
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = LockResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
