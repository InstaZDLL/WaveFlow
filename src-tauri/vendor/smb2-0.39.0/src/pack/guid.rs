//! GUID (Globally Unique Identifier) type for SMB2.
//!
//! GUIDs follow the mixed-endian layout defined in MS-DTYP section 2.3.4:
//! - Bytes 0-3: `data1` (`u32`, little-endian)
//! - Bytes 4-5: `data2` (`u16`, little-endian)
//! - Bytes 6-7: `data3` (`u16`, little-endian)
//! - Bytes 8-15: `data4` (8 raw bytes, big-endian order)

use std::fmt;

use super::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::error::Result;

/// A 128-bit GUID in mixed-endian wire format (MS-DTYP 2.3.4).
///
/// With the `serde` feature on, the JSON form mirrors the in-memory
/// field shape (`{data1, data2, data3, data4}`), **not** the wire byte
/// order — the wire layout is mixed-endian and round-tripping it through
/// JSON would just be confusing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Guid {
    /// First component (bytes 0-3, little-endian on wire).
    pub data1: u32,
    /// Second component (bytes 4-5, little-endian on wire).
    pub data2: u16,
    /// Third component (bytes 6-7, little-endian on wire).
    pub data3: u16,
    /// Fourth component (bytes 8-15, raw byte order on wire).
    pub data4: [u8; 8],
}

impl Guid {
    /// The NULL GUID: `{00000000-0000-0000-0000-000000000000}`.
    pub const ZERO: Self = Self {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0; 8],
    };
}

impl Pack for Guid {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u32_le(self.data1);
        cursor.write_u16_le(self.data2);
        cursor.write_u16_le(self.data3);
        cursor.write_bytes(&self.data4);
    }
}

impl Unpack for Guid {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let data1 = cursor.read_u32_le()?;
        let data2 = cursor.read_u16_le()?;
        let data3 = cursor.read_u16_le()?;
        let raw = cursor.read_bytes(8)?;
        let mut data4 = [0u8; 8];
        data4.copy_from_slice(raw);
        Ok(Self {
            data1,
            data2,
            data3,
            data4,
        })
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
            self.data1,
            self.data2,
            self.data3,
            self.data4[0],
            self.data4[1],
            self.data4[2],
            self.data4[3],
            self.data4[4],
            self.data4[5],
            self.data4[6],
            self.data4[7],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_null_guid() {
        let bytes = [0u8; 16];
        let mut cursor = ReadCursor::new(&bytes);
        let guid = Guid::unpack(&mut cursor).unwrap();
        assert_eq!(guid, Guid::ZERO);
    }

    #[test]
    fn pack_null_guid() {
        let mut cursor = WriteCursor::new();
        Guid::ZERO.pack(&mut cursor);
        assert_eq!(cursor.as_bytes(), &[0u8; 16]);
    }

    #[test]
    fn roundtrip_known_guid() {
        let guid = Guid {
            data1: 0x6BA7B810,
            data2: 0x9DAD,
            data3: 0x11D1,
            data4: [0x80, 0xB4, 0x00, 0xC0, 0x4F, 0xD4, 0x30, 0xC8],
        };

        let mut w = WriteCursor::new();
        guid.pack(&mut w);
        let mut r = ReadCursor::new(w.as_bytes());
        let unpacked = Guid::unpack(&mut r).unwrap();
        assert_eq!(unpacked, guid);
    }

    #[test]
    fn display_format() {
        let guid = Guid {
            data1: 0x6BA7B810,
            data2: 0x9DAD,
            data3: 0x11D1,
            data4: [0x80, 0xB4, 0x00, 0xC0, 0x4F, 0xD4, 0x30, 0xC8],
        };
        assert_eq!(guid.to_string(), "{6ba7b810-9dad-11d1-80b4-00c04fd430c8}");
    }

    #[test]
    fn display_null_guid() {
        assert_eq!(
            Guid::ZERO.to_string(),
            "{00000000-0000-0000-0000-000000000000}"
        );
    }

    #[test]
    fn mixed_endian_byte_ordering() {
        // Build a GUID with known values and verify the wire bytes directly.
        let guid = Guid {
            data1: 0x04030201,
            data2: 0x0605,
            data3: 0x0807,
            data4: [0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10],
        };

        let mut w = WriteCursor::new();
        guid.pack(&mut w);
        let bytes = w.as_bytes();

        // data1: u32 LE -> 01 02 03 04
        assert_eq!(&bytes[0..4], &[0x01, 0x02, 0x03, 0x04]);
        // data2: u16 LE -> 05 06
        assert_eq!(&bytes[4..6], &[0x05, 0x06]);
        // data3: u16 LE -> 07 08
        assert_eq!(&bytes[6..8], &[0x07, 0x08]);
        // data4: raw bytes -> 09 0A 0B 0C 0D 0E 0F 10
        assert_eq!(
            &bytes[8..16],
            &[0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10]
        );
    }

    #[test]
    fn unpack_insufficient_bytes() {
        let bytes = [0u8; 10]; // need 16
        let mut cursor = ReadCursor::new(&bytes);
        assert!(Guid::unpack(&mut cursor).is_err());
    }
}
