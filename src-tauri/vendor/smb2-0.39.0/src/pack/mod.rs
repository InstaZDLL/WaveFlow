//! Binary serialization/deserialization primitives for SMB2.
//!
//! Provides [`ReadCursor`] and [`WriteCursor`] for reading and writing
//! little-endian binary data, plus [`Pack`] and [`Unpack`] traits for
//! structured types.
//!
//! Most users don't need this module directly -- use [`SmbClient`](crate::SmbClient)
//! for high-level file operations.

pub mod filetime;
pub mod guid;

pub use filetime::FileTime;
pub use guid::Guid;

use crate::error::Result;
use crate::Error;

/// Trait for types that can serialize themselves into binary format.
pub trait Pack: Send + Sync {
    /// Write this value into the cursor.
    fn pack(&self, cursor: &mut WriteCursor);
}

/// Trait for types that can deserialize themselves from binary format.
pub trait Unpack: Sized {
    /// Read a value from the cursor, advancing its position.
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self>;
}

// ---------------------------------------------------------------------------
// ReadCursor
// ---------------------------------------------------------------------------

/// A cursor for reading little-endian binary data from a byte slice.
///
/// Tracks the current read position and returns errors on buffer overruns
/// rather than panicking.
pub struct ReadCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ReadCursor<'a> {
    /// Create a new read cursor starting at position 0.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Read a single byte.
    pub fn read_u8(&mut self) -> Result<u8> {
        self.ensure(1)?;
        let val = self.data[self.pos];
        self.pos += 1;
        Ok(val)
    }

    /// Read a little-endian `u16`.
    pub fn read_u16_le(&mut self) -> Result<u16> {
        let bytes = self.read_array::<2>()?;
        Ok(u16::from_le_bytes(bytes))
    }

    /// Read a little-endian `u32`.
    pub fn read_u32_le(&mut self) -> Result<u32> {
        let bytes = self.read_array::<4>()?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Read a little-endian `u64`.
    pub fn read_u64_le(&mut self) -> Result<u64> {
        let bytes = self.read_array::<8>()?;
        Ok(u64::from_le_bytes(bytes))
    }

    /// Read a little-endian `u128`.
    pub fn read_u128_le(&mut self) -> Result<u128> {
        let bytes = self.read_array::<16>()?;
        Ok(u128::from_le_bytes(bytes))
    }

    /// Read exactly `n` bytes, returning a sub-slice.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.ensure(n)?;
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Read `byte_len` bytes of UTF-16LE data and decode to a [`String`].
    ///
    /// `byte_len` must be even (each code unit is 2 bytes).
    pub fn read_utf16_le(&mut self, byte_len: usize) -> Result<String> {
        if !byte_len.is_multiple_of(2) {
            return Err(Error::invalid_data(format!(
                "UTF-16LE byte length must be even, got {}",
                byte_len
            )));
        }
        let raw = self.read_bytes(byte_len)?;
        let code_units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect();
        String::from_utf16(&code_units)
            .map_err(|_| Error::invalid_data("invalid UTF-16LE encoding"))
    }

    /// Skip `n` bytes without reading them.
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.ensure(n)?;
        self.pos += n;
        Ok(())
    }

    /// Return the number of bytes remaining.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Return the current byte position.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Return `true` if no bytes remain.
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Maximum buffer size we'll allocate from untrusted data (16 MB).
    pub const MAX_UNPACK_BUFFER: usize = 16 * 1024 * 1024;

    /// Read `n` bytes, but refuse if `n` exceeds [`Self::MAX_UNPACK_BUFFER`].
    pub fn read_bytes_bounded(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > Self::MAX_UNPACK_BUFFER {
            return Err(Error::invalid_data(format!(
                "buffer size {} exceeds maximum {} bytes",
                n,
                Self::MAX_UNPACK_BUFFER
            )));
        }
        self.read_bytes(n)
    }

    // -- private helpers --

    fn ensure(&self, n: usize) -> Result<()> {
        if self.remaining() < n {
            Err(Error::invalid_data(format!(
                "need {} bytes but only {} remain at offset {}",
                n,
                self.remaining(),
                self.pos
            )))
        } else {
            Ok(())
        }
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.ensure(N)?;
        let mut arr = [0u8; N];
        arr.copy_from_slice(&self.data[self.pos..self.pos + N]);
        self.pos += N;
        Ok(arr)
    }
}

// ---------------------------------------------------------------------------
// WriteCursor
// ---------------------------------------------------------------------------

/// A cursor for writing little-endian binary data into a growable buffer.
pub struct WriteCursor {
    buf: Vec<u8>,
}

impl WriteCursor {
    /// Create an empty write cursor.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Create a write cursor with pre-allocated capacity.
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    /// Write a single byte.
    pub fn write_u8(&mut self, val: u8) {
        self.buf.push(val);
    }

    /// Write a little-endian `u16`.
    pub fn write_u16_le(&mut self, val: u16) {
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    /// Write a little-endian `u32`.
    pub fn write_u32_le(&mut self, val: u32) {
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    /// Write a little-endian `u64`.
    pub fn write_u64_le(&mut self, val: u64) {
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    /// Write a little-endian `u128`.
    pub fn write_u128_le(&mut self, val: u128) {
        self.buf.extend_from_slice(&val.to_le_bytes());
    }

    /// Write a raw byte slice.
    pub fn write_bytes(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Encode a string as UTF-16LE and write the bytes.
    pub fn write_utf16_le(&mut self, s: &str) {
        for code_unit in s.encode_utf16() {
            self.buf.extend_from_slice(&code_unit.to_le_bytes());
        }
    }

    /// Write `n` zero bytes.
    pub fn write_zeros(&mut self, n: usize) {
        self.buf.resize(self.buf.len() + n, 0);
    }

    /// Pad with zero bytes until the position is a multiple of `alignment`.
    ///
    /// Does nothing if `alignment` is 0 or 1, or if already aligned.
    pub fn align_to(&mut self, alignment: usize) {
        if alignment <= 1 {
            return;
        }
        let remainder = self.buf.len() % alignment;
        if remainder != 0 {
            self.write_zeros(alignment - remainder);
        }
    }

    /// Return the current write position (number of bytes written so far).
    pub fn position(&self) -> usize {
        self.buf.len()
    }

    /// Overwrite a `u16` at a previous position (little-endian).
    ///
    /// # Panics
    ///
    /// Panics if `pos + 2 > self.position()`.
    pub fn set_u16_le_at(&mut self, pos: usize, val: u16) {
        self.buf[pos..pos + 2].copy_from_slice(&val.to_le_bytes());
    }

    /// Overwrite a `u32` at a previous position (little-endian).
    ///
    /// # Panics
    ///
    /// Panics if `pos + 4 > self.position()`.
    pub fn set_u32_le_at(&mut self, pos: usize, val: u32) {
        self.buf[pos..pos + 4].copy_from_slice(&val.to_le_bytes());
    }

    /// Consume the cursor and return the underlying buffer.
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    /// Return a reference to the bytes written so far.
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }
}

impl Default for WriteCursor {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // -- ReadCursor tests --

    #[test]
    fn read_u8_from_known_bytes() {
        let data = [0x42];
        let mut cursor = ReadCursor::new(&data);
        assert_eq!(cursor.read_u8().unwrap(), 0x42);
        assert!(cursor.is_empty());
    }

    #[test]
    fn read_u16_le_from_known_bytes() {
        let data = [0x34, 0x12];
        let mut cursor = ReadCursor::new(&data);
        assert_eq!(cursor.read_u16_le().unwrap(), 0x1234);
    }

    #[test]
    fn read_u32_le_from_known_bytes() {
        let data = [0x78, 0x56, 0x34, 0x12];
        let mut cursor = ReadCursor::new(&data);
        assert_eq!(cursor.read_u32_le().unwrap(), 0x12345678);
    }

    #[test]
    fn read_u64_le_from_known_bytes() {
        let data = [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
        let mut cursor = ReadCursor::new(&data);
        assert_eq!(cursor.read_u64_le().unwrap(), 0x0102030405060708);
    }

    #[test]
    fn read_u128_le_from_known_bytes() {
        let mut data = [0u8; 16];
        data[0] = 0x01;
        data[15] = 0x80;
        let mut cursor = ReadCursor::new(&data);
        let val = cursor.read_u128_le().unwrap();
        assert_eq!(val, 0x80000000_00000000_00000000_00000001);
    }

    #[test]
    fn read_past_end_returns_error() {
        let data = [0x00];
        let mut cursor = ReadCursor::new(&data);
        assert!(cursor.read_u16_le().is_err());

        let empty: &[u8] = &[];
        let mut cursor = ReadCursor::new(empty);
        assert!(cursor.read_u8().is_err());
    }

    #[test]
    fn remaining_and_position_track_correctly() {
        let data = [0x01, 0x02, 0x03, 0x04, 0x05];
        let mut cursor = ReadCursor::new(&data);
        assert_eq!(cursor.position(), 0);
        assert_eq!(cursor.remaining(), 5);

        cursor.read_u8().unwrap();
        assert_eq!(cursor.position(), 1);
        assert_eq!(cursor.remaining(), 4);

        cursor.read_u16_le().unwrap();
        assert_eq!(cursor.position(), 3);
        assert_eq!(cursor.remaining(), 2);
    }

    #[test]
    fn skip_advances_position() {
        let data = [0x01, 0x02, 0x03, 0x04];
        let mut cursor = ReadCursor::new(&data);
        cursor.skip(2).unwrap();
        assert_eq!(cursor.position(), 2);
        assert_eq!(cursor.read_u8().unwrap(), 0x03);

        // Skip past end is error
        assert!(cursor.skip(10).is_err());
    }

    #[test]
    fn read_bytes_returns_correct_slice() {
        let data = [0x0A, 0x0B, 0x0C, 0x0D];
        let mut cursor = ReadCursor::new(&data);
        cursor.skip(1).unwrap();
        let slice = cursor.read_bytes(2).unwrap();
        assert_eq!(slice, &[0x0B, 0x0C]);
        assert_eq!(cursor.position(), 3);
    }

    #[test]
    fn read_utf16_le_decodes_hello() {
        // "hello" in UTF-16LE
        let data = [0x68, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x6C, 0x00, 0x6F, 0x00];
        let mut cursor = ReadCursor::new(&data);
        let s = cursor.read_utf16_le(10).unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn read_utf16_le_odd_byte_len_is_error() {
        let data = [0x68, 0x00, 0x65];
        let mut cursor = ReadCursor::new(&data);
        assert!(cursor.read_utf16_le(3).is_err());
    }

    // -- WriteCursor tests --

    #[test]
    fn write_u8_produces_correct_byte() {
        let mut cursor = WriteCursor::new();
        cursor.write_u8(0xFF);
        assert_eq!(cursor.as_bytes(), &[0xFF]);
    }

    #[test]
    fn write_u16_le_produces_correct_bytes() {
        let mut cursor = WriteCursor::new();
        cursor.write_u16_le(0x1234);
        assert_eq!(cursor.as_bytes(), &[0x34, 0x12]);
    }

    #[test]
    fn write_u32_le_produces_correct_bytes() {
        let mut cursor = WriteCursor::new();
        cursor.write_u32_le(0x12345678);
        assert_eq!(cursor.as_bytes(), &[0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn write_u64_le_produces_correct_bytes() {
        let mut cursor = WriteCursor::new();
        cursor.write_u64_le(0x0102030405060708);
        assert_eq!(
            cursor.as_bytes(),
            &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
    }

    #[test]
    fn write_u128_le_produces_correct_bytes() {
        let mut cursor = WriteCursor::new();
        cursor.write_u128_le(0x01);
        let bytes = cursor.as_bytes();
        assert_eq!(bytes.len(), 16);
        assert_eq!(bytes[0], 0x01);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn align_to_pads_correctly() {
        // From position 0 -> already aligned
        let mut cursor = WriteCursor::new();
        cursor.align_to(8);
        assert_eq!(cursor.position(), 0);

        // From position 3 -> pad to 8
        let mut cursor = WriteCursor::new();
        cursor.write_bytes(&[0x01, 0x02, 0x03]);
        cursor.align_to(8);
        assert_eq!(cursor.position(), 8);
        // Padding bytes should be zeros
        assert_eq!(&cursor.as_bytes()[3..8], &[0, 0, 0, 0, 0]);

        // From position 8 -> already aligned
        cursor.align_to(8);
        assert_eq!(cursor.position(), 8);

        // From position 1 -> pad to 4
        let mut cursor = WriteCursor::new();
        cursor.write_u8(0xAA);
        cursor.align_to(4);
        assert_eq!(cursor.position(), 4);
    }

    #[test]
    fn set_u32_le_at_backpatches_correctly() {
        let mut cursor = WriteCursor::new();
        cursor.write_u32_le(0); // placeholder
        cursor.write_u32_le(0xDEADBEEF);
        cursor.set_u32_le_at(0, 0x12345678);
        assert_eq!(
            cursor.as_bytes(),
            &[0x78, 0x56, 0x34, 0x12, 0xEF, 0xBE, 0xAD, 0xDE]
        );
    }

    #[test]
    fn set_u16_le_at_backpatches_correctly() {
        let mut cursor = WriteCursor::new();
        cursor.write_u16_le(0);
        cursor.write_u16_le(0xBEEF);
        cursor.set_u16_le_at(0, 0x1234);
        assert_eq!(cursor.as_bytes(), &[0x34, 0x12, 0xEF, 0xBE]);
    }

    #[test]
    fn write_utf16_le_encodes_correctly() {
        let mut cursor = WriteCursor::new();
        cursor.write_utf16_le("hello");
        assert_eq!(
            cursor.as_bytes(),
            &[0x68, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x6C, 0x00, 0x6F, 0x00]
        );
    }

    #[test]
    fn write_zeros_produces_correct_count() {
        let mut cursor = WriteCursor::new();
        cursor.write_zeros(5);
        assert_eq!(cursor.as_bytes(), &[0, 0, 0, 0, 0]);
        assert_eq!(cursor.position(), 5);
    }

    #[test]
    fn into_inner_returns_buffer() {
        let mut cursor = WriteCursor::new();
        cursor.write_u8(0x42);
        let buf = cursor.into_inner();
        assert_eq!(buf, vec![0x42]);
    }

    #[test]
    fn with_capacity_works() {
        let cursor = WriteCursor::with_capacity(1024);
        assert_eq!(cursor.position(), 0);
    }

    // -- Roundtrip tests --

    #[test]
    fn roundtrip_u8() {
        let mut w = WriteCursor::new();
        w.write_u8(0xAB);
        let mut r = ReadCursor::new(w.as_bytes());
        assert_eq!(r.read_u8().unwrap(), 0xAB);
    }

    #[test]
    fn roundtrip_u16() {
        let mut w = WriteCursor::new();
        w.write_u16_le(0xCAFE);
        let mut r = ReadCursor::new(w.as_bytes());
        assert_eq!(r.read_u16_le().unwrap(), 0xCAFE);
    }

    #[test]
    fn roundtrip_u32() {
        let mut w = WriteCursor::new();
        w.write_u32_le(0xDEADBEEF);
        let mut r = ReadCursor::new(w.as_bytes());
        assert_eq!(r.read_u32_le().unwrap(), 0xDEADBEEF);
    }

    #[test]
    fn roundtrip_u64() {
        let mut w = WriteCursor::new();
        w.write_u64_le(0x0102030405060708);
        let mut r = ReadCursor::new(w.as_bytes());
        assert_eq!(r.read_u64_le().unwrap(), 0x0102030405060708);
    }

    #[test]
    fn roundtrip_u128() {
        let val: u128 = 0x0102030405060708090A0B0C0D0E0F10;
        let mut w = WriteCursor::new();
        w.write_u128_le(val);
        let mut r = ReadCursor::new(w.as_bytes());
        assert_eq!(r.read_u128_le().unwrap(), val);
    }

    #[test]
    fn roundtrip_utf16_le() {
        let mut w = WriteCursor::new();
        w.write_utf16_le("Hello, world!");
        let bytes = w.into_inner();
        let mut r = ReadCursor::new(&bytes);
        let s = r.read_utf16_le(bytes.len()).unwrap();
        assert_eq!(s, "Hello, world!");
    }

    #[test]
    fn roundtrip_utf16_le_emoji() {
        let mut w = WriteCursor::new();
        w.write_utf16_le("\u{1F600}");
        let bytes = w.into_inner();
        let mut r = ReadCursor::new(&bytes);
        let s = r.read_utf16_le(bytes.len()).unwrap();
        assert_eq!(s, "\u{1F600}");
    }

    // -- Property-based tests --

    fn valid_utf16_string() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::char::range('\u{0000}', '\u{D7FF}')
                .prop_union(prop::char::range('\u{E000}', '\u{FFFF}')),
            0..100,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #[test]
        fn prop_roundtrip_u8(val: u8) {
            let mut w = WriteCursor::new();
            w.write_u8(val);
            let mut r = ReadCursor::new(w.as_bytes());
            prop_assert_eq!(r.read_u8().unwrap(), val);
        }

        #[test]
        fn prop_roundtrip_u16(val: u16) {
            let mut w = WriteCursor::new();
            w.write_u16_le(val);
            let mut r = ReadCursor::new(w.as_bytes());
            prop_assert_eq!(r.read_u16_le().unwrap(), val);
        }

        #[test]
        fn prop_roundtrip_u32(val: u32) {
            let mut w = WriteCursor::new();
            w.write_u32_le(val);
            let mut r = ReadCursor::new(w.as_bytes());
            prop_assert_eq!(r.read_u32_le().unwrap(), val);
        }

        #[test]
        fn prop_roundtrip_u64(val: u64) {
            let mut w = WriteCursor::new();
            w.write_u64_le(val);
            let mut r = ReadCursor::new(w.as_bytes());
            prop_assert_eq!(r.read_u64_le().unwrap(), val);
        }

        #[test]
        fn prop_roundtrip_u128(val: u128) {
            let mut w = WriteCursor::new();
            w.write_u128_le(val);
            let mut r = ReadCursor::new(w.as_bytes());
            prop_assert_eq!(r.read_u128_le().unwrap(), val);
        }

        #[test]
        fn prop_roundtrip_utf16_le(s in valid_utf16_string()) {
            let mut w = WriteCursor::new();
            w.write_utf16_le(&s);
            let bytes = w.into_inner();
            let mut r = ReadCursor::new(&bytes);
            let decoded = r.read_utf16_le(bytes.len()).unwrap();
            prop_assert_eq!(decoded, s);
        }
    }
}
