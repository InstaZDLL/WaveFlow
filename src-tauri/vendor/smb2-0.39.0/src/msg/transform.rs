//! SMB2 TRANSFORM_HEADER and COMPRESSION_TRANSFORM_HEADER
//! (MS-SMB2 sections 2.2.41, 2.2.42).
//!
//! These headers wrap (encrypted or compressed) SMB2 messages. They are NOT
//! SMB2 messages themselves -- they precede the actual message data.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::SessionId;
use crate::Error;

// ── Transform header protocol IDs ──────────────────────────────────────

/// Protocol identifier for the encryption transform header (0xFD 'S' 'M' 'B').
/// Note: this is NOT the normal SMB2 protocol ID (0xFE).
pub const TRANSFORM_PROTOCOL_ID: [u8; 4] = [0xFD, b'S', b'M', b'B'];

/// Protocol identifier for the compression transform header (0xFC 'S' 'M' 'B').
pub const COMPRESSION_PROTOCOL_ID: [u8; 4] = [0xFC, b'S', b'M', b'B'];

// ── Transform header flags ────────────────────────────────────────────

/// The message is encrypted.
pub const SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED: u16 = 0x0001;

// ── CompressionAlgorithm values ────────────────────────────────────────

/// No compression.
pub const COMPRESSION_ALGORITHM_NONE: u16 = 0x0000;

/// LZNT1 compression.
pub const COMPRESSION_ALGORITHM_LZNT1: u16 = 0x0001;

/// LZ77 compression.
pub const COMPRESSION_ALGORITHM_LZ77: u16 = 0x0002;

/// LZ77 with Huffman encoding.
pub const COMPRESSION_ALGORITHM_LZ77_HUFFMAN: u16 = 0x0003;

/// Pattern_V1 compression.
pub const COMPRESSION_ALGORITHM_PATTERN_V1: u16 = 0x0004;

/// LZ4 compression.
pub const COMPRESSION_ALGORITHM_LZ4: u16 = 0x0005;

// ── Compression flags ──────────────────────────────────────────────────

/// No compression flags.
pub const SMB2_COMPRESSION_FLAG_NONE: u16 = 0x0000;

/// Chained compression (multiple segments).
pub const SMB2_COMPRESSION_FLAG_CHAINED: u16 = 0x0001;

// ── TransformHeader ────────────────────────────────────────────────────

/// SMB2 TRANSFORM_HEADER (MS-SMB2 section 2.2.41).
///
/// An encryption wrapper that precedes an encrypted SMB2 message.
/// The total header is 52 bytes:
/// - ProtocolId (4 bytes, must be 0xFD 'S' 'M' 'B')
/// - Signature (16 bytes)
/// - Nonce (16 bytes -- first 11 bytes used for AES-CCM, first 12 for AES-GCM)
/// - OriginalMessageSize (4 bytes)
/// - Reserved (2 bytes)
/// - Flags (2 bytes)
/// - SessionId (8 bytes)
///
/// The encrypted message data follows immediately after this header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformHeader {
    /// 16-byte AES signature over the encrypted message.
    pub signature: [u8; 16],
    /// 16-byte nonce. Only the first 11 bytes are used for AES-CCM,
    /// and the first 12 bytes for AES-GCM. The remaining bytes must be zero.
    pub nonce: [u8; 16],
    /// Size of the original (unencrypted) SMB2 message in bytes.
    pub original_message_size: u32,
    /// Flags for the transform header. Use
    /// `SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED`.
    pub flags: u16,
    /// Session identifier for the encrypted message.
    pub session_id: SessionId,
}

impl TransformHeader {
    /// Total header size in bytes (52).
    pub const SIZE: usize = 52;
}

impl Pack for TransformHeader {
    fn pack(&self, cursor: &mut WriteCursor) {
        // ProtocolId (4 bytes)
        cursor.write_bytes(&TRANSFORM_PROTOCOL_ID);
        // Signature (16 bytes)
        cursor.write_bytes(&self.signature);
        // Nonce (16 bytes)
        cursor.write_bytes(&self.nonce);
        // OriginalMessageSize (4 bytes)
        cursor.write_u32_le(self.original_message_size);
        // Reserved (2 bytes)
        cursor.write_u16_le(0);
        // Flags (2 bytes)
        cursor.write_u16_le(self.flags);
        // SessionId (8 bytes)
        cursor.write_u64_le(self.session_id.0);
    }
}

impl Unpack for TransformHeader {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // ProtocolId (4 bytes)
        let proto = cursor.read_bytes(4)?;
        if proto != TRANSFORM_PROTOCOL_ID {
            return Err(Error::invalid_data(format!(
                "invalid transform header protocol ID: expected {:02X?}, got {:02X?}",
                TRANSFORM_PROTOCOL_ID, proto
            )));
        }

        // Signature (16 bytes)
        let sig_bytes = cursor.read_bytes(16)?;
        let mut signature = [0u8; 16];
        signature.copy_from_slice(sig_bytes);

        // Nonce (16 bytes)
        let nonce_bytes = cursor.read_bytes(16)?;
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(nonce_bytes);

        // OriginalMessageSize (4 bytes)
        let original_message_size = cursor.read_u32_le()?;

        // Reserved (2 bytes)
        let _reserved = cursor.read_u16_le()?;

        // Flags (2 bytes)
        let flags = cursor.read_u16_le()?;

        // SessionId (8 bytes)
        let session_id = SessionId(cursor.read_u64_le()?);

        Ok(TransformHeader {
            signature,
            nonce,
            original_message_size,
            flags,
            session_id,
        })
    }
}

// ── CompressionTransformHeader ─────────────────────────────────────────

/// SMB2 COMPRESSION_TRANSFORM_HEADER (MS-SMB2 section 2.2.42).
///
/// A compression wrapper that precedes a compressed SMB2 message.
/// This implements the unchained variant (Flags = 0) only. The total
/// header is 16 bytes:
/// - ProtocolId (4 bytes, must be 0xFC 'S' 'M' 'B')
/// - OriginalCompressedSegmentSize (4 bytes)
/// - CompressionAlgorithm (2 bytes)
/// - Flags (2 bytes)
/// - Offset (4 bytes) -- offset from the end of this header to the
///   start of compressed data
///
/// Note: The chained variant (Flags = SMB2_COMPRESSION_FLAG_CHAINED)
/// interprets the last 4 bytes as Length instead of Offset. Chained
/// compression is deferred to a future implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressionTransformHeader {
    /// Size of the original uncompressed data segment.
    pub original_compressed_segment_size: u32,
    /// The compression algorithm used.
    pub compression_algorithm: u16,
    /// Compression flags. Currently only unchained (0x0000) is supported.
    pub flags: u16,
    /// For unchained: offset from end of this header to the start of
    /// compressed data. For chained: length of the original uncompressed
    /// segment (chained is not yet implemented).
    pub offset_or_length: u32,
}

impl CompressionTransformHeader {
    /// Total header size in bytes (16).
    pub const SIZE: usize = 16;
}

impl Pack for CompressionTransformHeader {
    fn pack(&self, cursor: &mut WriteCursor) {
        // ProtocolId (4 bytes)
        cursor.write_bytes(&COMPRESSION_PROTOCOL_ID);
        // OriginalCompressedSegmentSize (4 bytes)
        cursor.write_u32_le(self.original_compressed_segment_size);
        // CompressionAlgorithm (2 bytes)
        cursor.write_u16_le(self.compression_algorithm);
        // Flags (2 bytes)
        cursor.write_u16_le(self.flags);
        // Offset/Length (4 bytes)
        cursor.write_u32_le(self.offset_or_length);
    }
}

impl Unpack for CompressionTransformHeader {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // ProtocolId (4 bytes)
        let proto = cursor.read_bytes(4)?;
        if proto != COMPRESSION_PROTOCOL_ID {
            return Err(Error::invalid_data(format!(
                "invalid compression transform header protocol ID: expected {:02X?}, got {:02X?}",
                COMPRESSION_PROTOCOL_ID, proto
            )));
        }

        // OriginalCompressedSegmentSize (4 bytes)
        let original_compressed_segment_size = cursor.read_u32_le()?;

        // CompressionAlgorithm (2 bytes)
        let compression_algorithm = cursor.read_u16_le()?;

        // Flags (2 bytes)
        let flags = cursor.read_u16_le()?;

        // Offset/Length (4 bytes)
        let offset_or_length = cursor.read_u32_le()?;

        Ok(CompressionTransformHeader {
            original_compressed_segment_size,
            compression_algorithm,
            flags,
            offset_or_length,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TransformHeader tests ─────────────────────────────────────────

    #[test]
    fn transform_header_roundtrip() {
        let mut nonce = [0u8; 16];
        nonce[0..12].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

        let original = TransformHeader {
            signature: [
                0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
                0x99, 0x00,
            ],
            nonce,
            original_message_size: 1024,
            flags: SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED,
            session_id: SessionId(0xDEAD_BEEF_CAFE_FACE),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), TransformHeader::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = TransformHeader::unpack(&mut r).unwrap();

        assert_eq!(decoded.signature, original.signature);
        assert_eq!(decoded.nonce, original.nonce);
        assert_eq!(decoded.original_message_size, 1024);
        assert_eq!(decoded.flags, SMB2_TRANSFORM_HEADER_FLAG_ENCRYPTED);
        assert_eq!(decoded.session_id, SessionId(0xDEAD_BEEF_CAFE_FACE));
    }

    #[test]
    fn transform_header_protocol_id_is_0xfd() {
        let original = TransformHeader {
            signature: [0u8; 16],
            nonce: [0u8; 16],
            original_message_size: 0,
            flags: 0,
            session_id: SessionId(0),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // First 4 bytes must be 0xFD 'S' 'M' 'B', NOT 0xFE
        assert_eq!(bytes[0], 0xFD);
        assert_eq!(bytes[1], b'S');
        assert_eq!(bytes[2], b'M');
        assert_eq!(bytes[3], b'B');
        assert_ne!(bytes[0], 0xFE, "transform header must use 0xFD, not 0xFE");
    }

    #[test]
    fn transform_header_wrong_protocol_id() {
        let mut buf = [0u8; TransformHeader::SIZE];
        // Use the normal SMB2 protocol ID (0xFE) instead of 0xFD
        buf[0..4].copy_from_slice(&[0xFE, b'S', b'M', b'B']);

        let mut cursor = ReadCursor::new(&buf);
        let result = TransformHeader::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("protocol ID"), "error was: {err}");
    }

    // ── CompressionTransformHeader tests ──────────────────────────────

    #[test]
    fn compression_transform_header_roundtrip_unchained() {
        let original = CompressionTransformHeader {
            original_compressed_segment_size: 4096,
            compression_algorithm: COMPRESSION_ALGORITHM_LZ77,
            flags: SMB2_COMPRESSION_FLAG_NONE,
            offset_or_length: 64,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), CompressionTransformHeader::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = CompressionTransformHeader::unpack(&mut r).unwrap();

        assert_eq!(decoded.original_compressed_segment_size, 4096);
        assert_eq!(decoded.compression_algorithm, COMPRESSION_ALGORITHM_LZ77);
        assert_eq!(decoded.flags, SMB2_COMPRESSION_FLAG_NONE);
        assert_eq!(decoded.offset_or_length, 64);
    }

    #[test]
    fn compression_transform_header_protocol_id_is_0xfc() {
        let original = CompressionTransformHeader {
            original_compressed_segment_size: 0,
            compression_algorithm: COMPRESSION_ALGORITHM_NONE,
            flags: SMB2_COMPRESSION_FLAG_NONE,
            offset_or_length: 0,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // First 4 bytes must be 0xFC 'S' 'M' 'B'
        assert_eq!(bytes[0], 0xFC);
        assert_eq!(bytes[1], b'S');
        assert_eq!(bytes[2], b'M');
        assert_eq!(bytes[3], b'B');
        assert_ne!(
            bytes[0], 0xFE,
            "compression transform header must use 0xFC, not 0xFE"
        );
    }

    #[test]
    fn compression_transform_header_wrong_protocol_id() {
        let mut buf = [0u8; CompressionTransformHeader::SIZE];
        // Use wrong protocol ID
        buf[0..4].copy_from_slice(&[0xFE, b'S', b'M', b'B']);

        let mut cursor = ReadCursor::new(&buf);
        let result = CompressionTransformHeader::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("protocol ID"), "error was: {err}");
    }

    #[test]
    fn compression_transform_header_lz77_huffman() {
        let original = CompressionTransformHeader {
            original_compressed_segment_size: 8192,
            compression_algorithm: COMPRESSION_ALGORITHM_LZ77_HUFFMAN,
            flags: SMB2_COMPRESSION_FLAG_NONE,
            offset_or_length: 128,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = CompressionTransformHeader::unpack(&mut r).unwrap();

        assert_eq!(
            decoded.compression_algorithm,
            COMPRESSION_ALGORITHM_LZ77_HUFFMAN
        );
        assert_eq!(decoded.original_compressed_segment_size, 8192);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::arb_session_id;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn transform_header_pack_unpack(
            signature in any::<[u8; 16]>(),
            nonce in any::<[u8; 16]>(),
            original_message_size in any::<u32>(),
            flags in any::<u16>(),
            session_id in arb_session_id(),
        ) {
            let original = TransformHeader {
                signature,
                nonce,
                original_message_size,
                flags,
                session_id,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();
            prop_assert_eq!(bytes.len(), TransformHeader::SIZE);

            let mut r = ReadCursor::new(&bytes);
            let decoded = TransformHeader::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn compression_transform_header_pack_unpack(
            original_compressed_segment_size in any::<u32>(),
            compression_algorithm in any::<u16>(),
            flags in any::<u16>(),
            offset_or_length in any::<u32>(),
        ) {
            let original = CompressionTransformHeader {
                original_compressed_segment_size,
                compression_algorithm,
                flags,
                offset_or_length,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();
            prop_assert_eq!(bytes.len(), CompressionTransformHeader::SIZE);

            let mut r = ReadCursor::new(&bytes);
            let decoded = CompressionTransformHeader::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
