//! SMB2 LZ4 compression for unchained mode (MS-SMB2 section 3.1.4.4).
//!
//! In unchained mode, the `CompressionTransformHeader` has `Flags = 0x0000`.
//! The `Offset` field indicates where compressed data starts relative to the
//! original message. Bytes before the offset are sent uncompressed (the
//! "uncompressed prefix"), while bytes from the offset onward are
//! LZ4-compressed.
//!
//! This allows the SMB2 header to remain uncompressed for routing while the
//! payload is compressed.

/// Maximum decompressed size we allow (16 MB). Prevents decompression bombs.
const MAX_DECOMPRESSED_SIZE: u32 = 16 * 1024 * 1024;

/// The result of compressing an SMB2 message (unchained mode).
#[derive(Debug, Clone)]
pub struct CompressedMessage {
    /// The original uncompressed size of the compressed portion.
    pub original_size: u32,
    /// Bytes before the compression offset (sent as-is).
    pub uncompressed_prefix: Vec<u8>,
    /// The LZ4-compressed data.
    pub compressed_data: Vec<u8>,
    /// The offset that was used (same as input offset).
    pub offset: u32,
}

/// Compress an SMB2 message using LZ4 (unchained mode).
///
/// `offset` indicates where compression starts in the original message.
/// Bytes before `offset` are kept as-is (uncompressed prefix).
/// Bytes from `offset` onward are LZ4-compressed.
///
/// Returns `None` if compression doesn't reduce the size (not worth it),
/// or if there is nothing to compress (offset >= message length).
pub fn compress_message(message: &[u8], offset: usize) -> Option<CompressedMessage> {
    // Nothing to compress if offset is at or beyond the end.
    if offset >= message.len() {
        return None;
    }

    let prefix = &message[..offset];
    let to_compress = &message[offset..];

    let compressed = lz4_flex::block::compress(to_compress);

    // Only use compression if it actually reduces size.
    if compressed.len() >= to_compress.len() {
        return None;
    }

    Some(CompressedMessage {
        original_size: to_compress.len() as u32,
        uncompressed_prefix: prefix.to_vec(),
        compressed_data: compressed,
        offset: offset as u32,
    })
}

/// Decompress an SMB2 message (unchained mode).
///
/// `uncompressed_prefix` is the data before the compression offset.
/// `compressed_data` is the LZ4-compressed portion.
/// `original_size` is the expected decompressed size of the compressed portion.
///
/// Returns the full reconstructed message (prefix + decompressed data).
pub fn decompress_message(
    uncompressed_prefix: &[u8],
    compressed_data: &[u8],
    original_size: u32,
) -> Result<Vec<u8>, crate::Error> {
    // Validate original_size to prevent decompression bombs.
    if original_size > MAX_DECOMPRESSED_SIZE {
        return Err(crate::Error::invalid_data(format!(
            "decompressed size {} exceeds maximum allowed size {}",
            original_size, MAX_DECOMPRESSED_SIZE
        )));
    }

    let decompressed = lz4_flex::block::decompress(compressed_data, original_size as usize)
        .map_err(|e| crate::Error::invalid_data(format!("LZ4 decompression failed: {e}")))?;

    let mut result = Vec::with_capacity(uncompressed_prefix.len() + decompressed.len());
    result.extend_from_slice(uncompressed_prefix);
    result.extend_from_slice(&decompressed);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_and_decompress_roundtrip() {
        // Compressible data: repeated pattern.
        let message: Vec<u8> = b"ABCDEFGH".iter().copied().cycle().take(1024).collect();

        let compressed = compress_message(&message, 0).expect("should compress");
        assert!(compressed.compressed_data.len() < message.len());
        assert_eq!(compressed.original_size, message.len() as u32);
        assert!(compressed.uncompressed_prefix.is_empty());
        assert_eq!(compressed.offset, 0);

        let decompressed = decompress_message(
            &compressed.uncompressed_prefix,
            &compressed.compressed_data,
            compressed.original_size,
        )
        .expect("should decompress");

        assert_eq!(decompressed, message);
    }

    #[test]
    fn compress_with_offset_preserves_prefix() {
        // Simulate a 64-byte SMB2 header + compressible payload.
        let mut message = vec![0xFE; 64]; // "header" bytes
        let payload: Vec<u8> = b"HelloWorld".iter().copied().cycle().take(2048).collect();
        message.extend_from_slice(&payload);

        let compressed = compress_message(&message, 64).expect("should compress");
        assert_eq!(compressed.offset, 64);
        assert_eq!(compressed.uncompressed_prefix, &message[..64]);
        assert_eq!(compressed.original_size, payload.len() as u32);
        assert!(compressed.compressed_data.len() < payload.len());

        let decompressed = decompress_message(
            &compressed.uncompressed_prefix,
            &compressed.compressed_data,
            compressed.original_size,
        )
        .expect("should decompress");

        assert_eq!(decompressed, message);
    }

    #[test]
    fn compress_with_offset_zero_compresses_entire_message() {
        let message: Vec<u8> = vec![42u8; 4096];

        let compressed = compress_message(&message, 0).expect("should compress");
        assert_eq!(compressed.offset, 0);
        assert!(compressed.uncompressed_prefix.is_empty());
        assert_eq!(compressed.original_size, 4096);

        let decompressed = decompress_message(
            &compressed.uncompressed_prefix,
            &compressed.compressed_data,
            compressed.original_size,
        )
        .expect("should decompress");

        assert_eq!(decompressed, message);
    }

    #[test]
    fn compress_empty_message_returns_none() {
        let message: &[u8] = &[];
        assert!(compress_message(message, 0).is_none());
    }

    #[test]
    fn compress_offset_at_end_returns_none() {
        let message = b"short";
        assert!(compress_message(message, 5).is_none());
        assert!(compress_message(message, 100).is_none());
    }

    #[test]
    fn incompressible_data_returns_none() {
        // Random-ish bytes that LZ4 cannot compress (will likely grow).
        let mut message = Vec::with_capacity(256);
        for i in 0u16..256 {
            // Use a simple PRNG-like pattern that doesn't compress well.
            message.push(((i.wrapping_mul(137).wrapping_add(53)) & 0xFF) as u8);
        }

        // Small incompressible data should return None.
        assert!(
            compress_message(&message, 0).is_none(),
            "incompressible data should return None"
        );
    }

    #[test]
    fn large_message_compresses_well() {
        // 1 MB of repeated pattern -- should compress very well.
        let message: Vec<u8> = b"SMB2 compression test data! "
            .iter()
            .copied()
            .cycle()
            .take(1024 * 1024)
            .collect();

        let compressed = compress_message(&message, 0).expect("should compress large message");

        // LZ4 should achieve at least 4:1 on highly repetitive data.
        let ratio = message.len() as f64 / compressed.compressed_data.len() as f64;
        assert!(
            ratio > 4.0,
            "compression ratio {ratio:.1} is too low for repetitive data"
        );

        let decompressed = decompress_message(
            &compressed.uncompressed_prefix,
            &compressed.compressed_data,
            compressed.original_size,
        )
        .expect("should decompress");

        assert_eq!(decompressed.len(), message.len());
        assert_eq!(decompressed, message);
    }

    #[test]
    fn decompress_with_wrong_original_size_fails() {
        let message: Vec<u8> = vec![0xAA; 1024];
        let compressed = compress_message(&message, 0).expect("should compress");

        // Use a wrong (smaller) original_size -- decompression should fail
        // because LZ4 validates the output size.
        let result = decompress_message(&[], &compressed.compressed_data, 512);
        assert!(result.is_err(), "wrong original_size should cause an error");
    }

    #[test]
    fn decompress_rejects_oversized_original_size() {
        // Attempt to decompress with original_size exceeding 16 MB limit.
        let bogus_compressed = vec![0u8; 10];
        let result = decompress_message(&[], &bogus_compressed, MAX_DECOMPRESSED_SIZE + 1);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds maximum"),
            "error should mention size limit, got: {err_msg}"
        );
    }

    #[test]
    fn decompress_with_exact_max_size_is_allowed() {
        // original_size == MAX_DECOMPRESSED_SIZE should not be rejected
        // by the size check (it will fail on actual decompression since the
        // data is bogus, but that's a different error).
        let bogus_compressed = vec![0u8; 10];
        let result = decompress_message(&[], &bogus_compressed, MAX_DECOMPRESSED_SIZE);

        // Should fail on decompression, not on size validation.
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("decompression failed"),
            "should fail on decompression, not size check, got: {err_msg}"
        );
    }

    #[test]
    fn decompress_corrupt_data_fails() {
        let corrupt = vec![0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
        let result = decompress_message(&[], &corrupt, 1024);
        assert!(result.is_err());

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("decompression failed"),
            "error should mention decompression failure, got: {err_msg}"
        );
    }

    #[test]
    fn decompress_preserves_prefix_in_output() {
        let prefix = b"PREFIX_DATA";
        let payload: Vec<u8> = vec![0x42; 2048];
        let compressed_payload = compress_message(&payload, 0).expect("should compress payload");

        let result = decompress_message(
            prefix,
            &compressed_payload.compressed_data,
            compressed_payload.original_size,
        )
        .expect("should decompress");

        assert_eq!(&result[..prefix.len()], prefix);
        assert_eq!(&result[prefix.len()..], &payload);
    }
}
