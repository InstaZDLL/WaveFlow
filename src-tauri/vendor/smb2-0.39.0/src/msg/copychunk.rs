//! Server-side copy wire structures (MS-SMB2 sections 2.2.31.1, 2.2.32.1,
//! 2.2.32.3).
//!
//! Server-side copy lets the server copy byte ranges between two of its own
//! open files, so the data never crosses the wire. It is a two-step protocol
//! carried inside [`IoctlRequest`](super::ioctl::IoctlRequest) /
//! [`IoctlResponse`](super::ioctl::IoctlResponse) buffers:
//!
//! 1. The client asks the server for an opaque 24-byte *resume key* that names
//!    the source open ([`FSCTL_SRV_REQUEST_RESUME_KEY`](super::ioctl::FSCTL_SRV_REQUEST_RESUME_KEY)),
//!    parsing a [`SrvRequestResumeKeyResponse`] out of the IOCTL output buffer.
//! 2. Against the *destination* open, the client sends a [`SrvCopychunkCopy`]
//!    (the resume key plus an array of source->target ranges) via
//!    [`FSCTL_SRV_COPYCHUNK`](super::ioctl::FSCTL_SRV_COPYCHUNK) and reads back a
//!    [`SrvCopychunkResponse`] telling it how much was copied -- or, when the
//!    request exceeds the server's per-request limits, the limits themselves.
//!
//! These structs are the wire layer only. The high-level, batching client API
//! lives in [`client::copy`](crate::client::copy).

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::Error;

/// Length in bytes of a server-side copy resume key (MS-SMB2 2.2.32.3).
pub const RESUME_KEY_LEN: usize = 24;

// -- SrvCopychunk -------------------------------------------------------

/// One source->target byte range in a [`SrvCopychunkCopy`] (MS-SMB2 2.2.31.1.1).
///
/// Fixed 24-byte layout: `SourceOffset` (8) + `TargetOffset` (8) + `Length` (4)
/// + `Reserved` (4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SrvCopychunk {
    /// Offset in the source file to copy from.
    pub source_offset: u64,
    /// Offset in the destination file to copy to.
    pub target_offset: u64,
    /// Number of bytes to copy.
    pub length: u32,
}

impl SrvCopychunk {
    /// Wire size of one chunk descriptor (24 bytes).
    pub const SIZE: usize = 24;
}

impl Pack for SrvCopychunk {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u64_le(self.source_offset);
        cursor.write_u64_le(self.target_offset);
        cursor.write_u32_le(self.length);
        cursor.write_u32_le(0); // Reserved, MUST be 0.
    }
}

impl Unpack for SrvCopychunk {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let source_offset = cursor.read_u64_le()?;
        let target_offset = cursor.read_u64_le()?;
        let length = cursor.read_u32_le()?;
        let _reserved = cursor.read_u32_le()?;
        Ok(SrvCopychunk {
            source_offset,
            target_offset,
            length,
        })
    }
}

// -- SrvCopychunkCopy ---------------------------------------------------

/// The input buffer of an `FSCTL_SRV_COPYCHUNK` IOCTL request (MS-SMB2
/// 2.2.31.1): a source resume key plus the array of ranges to copy.
///
/// Layout: `SourceKey` (24) + `ChunkCount` (4) + `Reserved` (4) + `Chunks`
/// (`ChunkCount` x 24). `ChunkCount` is derived from `chunks.len()` on pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrvCopychunkCopy {
    /// The opaque 24-byte key naming the source open, from a
    /// [`SrvRequestResumeKeyResponse`].
    pub source_key: [u8; RESUME_KEY_LEN],
    /// The ranges to copy.
    pub chunks: Vec<SrvCopychunk>,
}

impl SrvCopychunkCopy {
    /// Fixed header size before the chunk array (32 bytes).
    pub const HEADER_SIZE: usize = RESUME_KEY_LEN + 8;

    /// Packed size for `chunk_count` chunks.
    #[must_use]
    pub fn packed_size(chunk_count: usize) -> usize {
        Self::HEADER_SIZE + chunk_count * SrvCopychunk::SIZE
    }
}

impl Pack for SrvCopychunkCopy {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_bytes(&self.source_key);
        cursor.write_u32_le(self.chunks.len() as u32);
        cursor.write_u32_le(0); // Reserved, MUST be 0.
        for chunk in &self.chunks {
            chunk.pack(cursor);
        }
    }
}

impl Unpack for SrvCopychunkCopy {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let key_bytes = cursor.read_bytes(RESUME_KEY_LEN)?;
        let mut source_key = [0u8; RESUME_KEY_LEN];
        source_key.copy_from_slice(key_bytes);

        let chunk_count = cursor.read_u32_le()?;
        let _reserved = cursor.read_u32_le()?;

        let mut chunks = Vec::with_capacity(chunk_count as usize);
        for _ in 0..chunk_count {
            chunks.push(SrvCopychunk::unpack(cursor)?);
        }

        Ok(SrvCopychunkCopy { source_key, chunks })
    }
}

// -- SrvCopychunkResponse -----------------------------------------------

/// The output buffer of an `FSCTL_SRV_COPYCHUNK` IOCTL response (MS-SMB2
/// 2.2.32.1). Fixed 12 bytes.
///
/// The meaning of the three fields depends on the response's NTSTATUS:
///
/// - On success, they report the work done: `chunks_written` chunks processed,
///   `chunk_bytes_written` bytes of a trailing partial write, `total_bytes_written`
///   bytes copied in all.
/// - On `STATUS_INVALID_PARAMETER` (the server rejected the request as too
///   large), they instead carry the server's per-request *limits*:
///   `chunks_written` is the max chunk count, `chunk_bytes_written` the max
///   bytes per chunk, `total_bytes_written` the max bytes per request. The
///   client re-batches within these and retries (MS-SMB2 3.2.5.14.3).
///
/// This struct is a plain field carrier; interpreting the fields against the
/// status is the caller's job (see [`client::copy`](crate::client::copy)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SrvCopychunkResponse {
    /// Chunks written (success), or max chunks per request (invalid-parameter).
    pub chunks_written: u32,
    /// Trailing partial-write bytes (success), or max bytes per chunk
    /// (invalid-parameter).
    pub chunk_bytes_written: u32,
    /// Total bytes copied (success), or max bytes per request (invalid-parameter).
    pub total_bytes_written: u32,
}

impl SrvCopychunkResponse {
    /// Wire size (12 bytes). Also the `MaxOutputResponse` a client sets on the
    /// copychunk IOCTL request.
    pub const SIZE: usize = 12;
}

impl Pack for SrvCopychunkResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_u32_le(self.chunks_written);
        cursor.write_u32_le(self.chunk_bytes_written);
        cursor.write_u32_le(self.total_bytes_written);
    }
}

impl Unpack for SrvCopychunkResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let chunks_written = cursor.read_u32_le()?;
        let chunk_bytes_written = cursor.read_u32_le()?;
        let total_bytes_written = cursor.read_u32_le()?;
        Ok(SrvCopychunkResponse {
            chunks_written,
            chunk_bytes_written,
            total_bytes_written,
        })
    }
}

// -- SrvRequestResumeKeyResponse ----------------------------------------

/// The output buffer of an `FSCTL_SRV_REQUEST_RESUME_KEY` IOCTL response
/// (MS-SMB2 2.2.32.3): a 24-byte opaque resume key, followed by an unused
/// `ContextLength` (4) and variable `Context` the client MUST ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SrvRequestResumeKeyResponse {
    /// The opaque 24-byte key naming the source open. Pass it to a
    /// [`SrvCopychunkCopy`]; attach no interpretation to its contents.
    pub resume_key: [u8; RESUME_KEY_LEN],
}

impl Pack for SrvRequestResumeKeyResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        cursor.write_bytes(&self.resume_key);
        cursor.write_u32_le(0); // ContextLength, MUST be 0.
    }
}

impl Unpack for SrvRequestResumeKeyResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let key_bytes = cursor.read_bytes(RESUME_KEY_LEN).map_err(|_| {
            Error::invalid_data("SRV_REQUEST_RESUME_KEY response shorter than 24-byte resume key")
        })?;
        let mut resume_key = [0u8; RESUME_KEY_LEN];
        resume_key.copy_from_slice(key_bytes);
        // ContextLength + Context follow but are unused (server sets length 0,
        // client MUST ignore); we don't read past the key.
        Ok(SrvRequestResumeKeyResponse { resume_key })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copychunk_roundtrip() {
        let original = SrvCopychunk {
            source_offset: 0x1122_3344_5566_7788,
            target_offset: 0x00AA_BB00_CC00_DD00,
            length: 0x0010_0000,
        };
        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), SrvCopychunk::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = SrvCopychunk::unpack(&mut r).unwrap();
        assert_eq!(decoded, original);
        assert!(r.is_empty());
    }

    #[test]
    fn copychunk_copy_roundtrip_multi_chunk() {
        let original = SrvCopychunkCopy {
            source_key: [0x5A; RESUME_KEY_LEN],
            chunks: vec![
                SrvCopychunk {
                    source_offset: 0,
                    target_offset: 0,
                    length: 1024,
                },
                SrvCopychunk {
                    source_offset: 1024,
                    target_offset: 4096,
                    length: 2048,
                },
            ],
        };
        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), SrvCopychunkCopy::packed_size(2));

        let mut r = ReadCursor::new(&bytes);
        let decoded = SrvCopychunkCopy::unpack(&mut r).unwrap();
        assert_eq!(decoded, original);
        assert!(r.is_empty());
    }

    #[test]
    fn copychunk_copy_chunk_count_is_derived_from_len() {
        let copy = SrvCopychunkCopy {
            source_key: [0; RESUME_KEY_LEN],
            chunks: vec![SrvCopychunk {
                source_offset: 0,
                target_offset: 0,
                length: 8,
            }],
        };
        let mut w = WriteCursor::new();
        copy.pack(&mut w);
        let bytes = w.into_inner();
        // ChunkCount lives at bytes [24..28].
        let chunk_count = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        assert_eq!(chunk_count, 1);
    }

    #[test]
    fn copychunk_response_roundtrip() {
        let original = SrvCopychunkResponse {
            chunks_written: 3,
            chunk_bytes_written: 0,
            total_bytes_written: 3 * 1024 * 1024,
        };
        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();
        assert_eq!(bytes.len(), SrvCopychunkResponse::SIZE);

        let mut r = ReadCursor::new(&bytes);
        let decoded = SrvCopychunkResponse::unpack(&mut r).unwrap();
        assert_eq!(decoded, original);
        assert!(r.is_empty());
    }

    #[test]
    fn resume_key_response_roundtrip_ignores_trailing_context() {
        let key = [0x37; RESUME_KEY_LEN];
        // A well-formed response with a zero ContextLength.
        let mut w = WriteCursor::new();
        SrvRequestResumeKeyResponse { resume_key: key }.pack(&mut w);
        let mut bytes = w.into_inner();
        // Append stray context bytes some servers pad with; unpack must ignore.
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let mut r = ReadCursor::new(&bytes);
        let decoded = SrvRequestResumeKeyResponse::unpack(&mut r).unwrap();
        assert_eq!(decoded.resume_key, key);
    }

    #[test]
    fn resume_key_response_rejects_short_buffer() {
        let short = [0u8; RESUME_KEY_LEN - 1];
        let mut r = ReadCursor::new(&short);
        assert!(SrvRequestResumeKeyResponse::unpack(&mut r).is_err());
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use proptest::prelude::*;

    fn arb_chunk() -> impl Strategy<Value = SrvCopychunk> {
        (any::<u64>(), any::<u64>(), any::<u32>()).prop_map(
            |(source_offset, target_offset, length)| SrvCopychunk {
                source_offset,
                target_offset,
                length,
            },
        )
    }

    proptest! {
        #[test]
        fn copychunk_copy_pack_unpack(
            source_key in any::<[u8; RESUME_KEY_LEN]>(),
            chunks in prop::collection::vec(arb_chunk(), 0..8),
        ) {
            let original = SrvCopychunkCopy { source_key, chunks };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = SrvCopychunkCopy::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn copychunk_response_pack_unpack(
            chunks_written in any::<u32>(),
            chunk_bytes_written in any::<u32>(),
            total_bytes_written in any::<u32>(),
        ) {
            let original = SrvCopychunkResponse {
                chunks_written,
                chunk_bytes_written,
                total_bytes_written,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = SrvCopychunkResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
