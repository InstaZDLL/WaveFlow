//! SMB2 NEGOTIATE request and response (spec sections 2.2.3, 2.2.4)
//! and negotiate context structures (spec section 2.2.3.1).
//!
//! Negotiate is the first exchange between client and server. The client
//! advertises which dialects and capabilities it supports, and the server
//! picks the highest mutually supported dialect and returns its own
//! capabilities.
//!
//! For SMB 3.1.1 (dialect 0x0311), both request and response carry a
//! variable-length list of negotiate contexts that negotiate features
//! such as preauthentication integrity, encryption, compression, and
//! signing algorithms.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{Guid, Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::flags::{Capabilities, SecurityMode};
use crate::types::Dialect;
use crate::Error;

// ── Negotiate context type constants ───────────────────────────────────

/// Preauthentication integrity capabilities context type.
pub const NEGOTIATE_CONTEXT_PREAUTH_INTEGRITY: u16 = 0x0001;
/// Encryption capabilities context type.
pub const NEGOTIATE_CONTEXT_ENCRYPTION: u16 = 0x0002;
/// Compression capabilities context type.
pub const NEGOTIATE_CONTEXT_COMPRESSION: u16 = 0x0003;
/// Signing capabilities context type.
pub const NEGOTIATE_CONTEXT_SIGNING: u16 = 0x0008;

// ── Hash algorithm IDs (2.2.3.1.1) ────────────────────────────────────

/// SHA-512 hash algorithm for preauthentication integrity.
pub const HASH_ALGORITHM_SHA512: u16 = 0x0001;

// ── Encryption cipher IDs (2.2.3.1.2) ─────────────────────────────────

/// AES-128-CCM cipher.
pub const CIPHER_AES_128_CCM: u16 = 0x0001;
/// AES-128-GCM cipher.
pub const CIPHER_AES_128_GCM: u16 = 0x0002;
/// AES-256-CCM cipher.
pub const CIPHER_AES_256_CCM: u16 = 0x0003;
/// AES-256-GCM cipher.
pub const CIPHER_AES_256_GCM: u16 = 0x0004;

// ── Signing algorithm IDs (2.2.3.1.7) ─────────────────────────────────

/// HMAC-SHA256 signing algorithm.
pub const SIGNING_HMAC_SHA256: u16 = 0x0000;
/// AES-CMAC signing algorithm.
pub const SIGNING_AES_CMAC: u16 = 0x0001;
/// AES-GMAC signing algorithm.
pub const SIGNING_AES_GMAC: u16 = 0x0002;

// ── Compression algorithm IDs (2.2.3.1.3) ─────────────────────────────

/// No compression.
pub const COMPRESSION_NONE: u16 = 0x0000;
/// LZNT1 compression algorithm.
pub const COMPRESSION_LZNT1: u16 = 0x0001;
/// LZ77 compression algorithm.
pub const COMPRESSION_LZ77: u16 = 0x0002;
/// LZ77+Huffman compression algorithm.
pub const COMPRESSION_LZ77_HUFFMAN: u16 = 0x0003;
/// Pattern scanning algorithm.
pub const COMPRESSION_PATTERN_V1: u16 = 0x0004;
/// LZ4 compression algorithm.
pub const COMPRESSION_LZ4: u16 = 0x0005;

// ── Compression capability flags ───────────────────────────────────────

/// Chained compression is not supported.
pub const COMPRESSION_FLAG_NONE: u32 = 0x0000_0000;
/// Chained compression is supported.
pub const COMPRESSION_FLAG_CHAINED: u32 = 0x0000_0001;

// ── NegotiateContext ───────────────────────────────────────────────────

/// A single negotiate context entry (spec section 2.2.3.1).
///
/// Each context has a type, reserved field, and type-specific data.
/// The four most important types are represented as dedicated variants;
/// unknown types are stored as raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegotiateContext {
    /// Preauthentication integrity capabilities (type 0x0001).
    PreauthIntegrity {
        /// Supported hash algorithm IDs.
        hash_algorithms: Vec<u16>,
        /// Salt value.
        salt: Vec<u8>,
    },
    /// Encryption capabilities (type 0x0002).
    Encryption {
        /// Supported cipher IDs in preference order.
        ciphers: Vec<u16>,
    },
    /// Compression capabilities (type 0x0003).
    Compression {
        /// Compression capability flags.
        flags: u32,
        /// Supported compression algorithm IDs in preference order.
        algorithms: Vec<u16>,
    },
    /// Signing capabilities (type 0x0008).
    Signing {
        /// Supported signing algorithm IDs in preference order.
        algorithms: Vec<u16>,
    },
    /// Unknown or unsupported context type, stored as raw bytes.
    Unknown {
        /// The context type identifier.
        context_type: u16,
        /// The raw data bytes.
        data: Vec<u8>,
    },
}

/// Pack a single negotiate context's data (without the header).
fn pack_context_data(ctx: &NegotiateContext, cursor: &mut WriteCursor) {
    match ctx {
        NegotiateContext::PreauthIntegrity {
            hash_algorithms,
            salt,
        } => {
            // HashAlgorithmCount (2)
            cursor.write_u16_le(hash_algorithms.len() as u16);
            // SaltLength (2)
            cursor.write_u16_le(salt.len() as u16);
            // HashAlgorithms (variable)
            for &alg in hash_algorithms {
                cursor.write_u16_le(alg);
            }
            // Salt (variable)
            cursor.write_bytes(salt);
        }
        NegotiateContext::Encryption { ciphers } => {
            // CipherCount (2)
            cursor.write_u16_le(ciphers.len() as u16);
            // Ciphers (variable)
            for &c in ciphers {
                cursor.write_u16_le(c);
            }
        }
        NegotiateContext::Compression { flags, algorithms } => {
            // CompressionAlgorithmCount (2)
            cursor.write_u16_le(algorithms.len() as u16);
            // Padding (2)
            cursor.write_u16_le(0);
            // Flags (4)
            cursor.write_u32_le(*flags);
            // CompressionAlgorithms (variable)
            for &a in algorithms {
                cursor.write_u16_le(a);
            }
        }
        NegotiateContext::Signing { algorithms } => {
            // SigningAlgorithmCount (2)
            cursor.write_u16_le(algorithms.len() as u16);
            // SigningAlgorithms (variable)
            for &a in algorithms {
                cursor.write_u16_le(a);
            }
        }
        NegotiateContext::Unknown { data, .. } => {
            cursor.write_bytes(data);
        }
    }
}

/// Return the context type ID for a negotiate context.
fn context_type_id(ctx: &NegotiateContext) -> u16 {
    match ctx {
        NegotiateContext::PreauthIntegrity { .. } => NEGOTIATE_CONTEXT_PREAUTH_INTEGRITY,
        NegotiateContext::Encryption { .. } => NEGOTIATE_CONTEXT_ENCRYPTION,
        NegotiateContext::Compression { .. } => NEGOTIATE_CONTEXT_COMPRESSION,
        NegotiateContext::Signing { .. } => NEGOTIATE_CONTEXT_SIGNING,
        NegotiateContext::Unknown { context_type, .. } => *context_type,
    }
}

/// Compute the data length of a single negotiate context (without the 8-byte header).
fn context_data_len(ctx: &NegotiateContext) -> usize {
    match ctx {
        NegotiateContext::PreauthIntegrity {
            hash_algorithms,
            salt,
        } => 2 + 2 + hash_algorithms.len() * 2 + salt.len(),
        NegotiateContext::Encryption { ciphers } => 2 + ciphers.len() * 2,
        NegotiateContext::Compression { algorithms, .. } => 2 + 2 + 4 + algorithms.len() * 2,
        NegotiateContext::Signing { algorithms } => 2 + algorithms.len() * 2,
        NegotiateContext::Unknown { data, .. } => data.len(),
    }
}

/// Pack a list of negotiate contexts, each preceded by its header and
/// 8-byte aligned.
fn pack_negotiate_contexts(contexts: &[NegotiateContext], cursor: &mut WriteCursor) {
    for (i, ctx) in contexts.iter().enumerate() {
        // Pad to 8-byte alignment before each context (except the first,
        // which should already be aligned by the caller).
        if i > 0 {
            cursor.align_to(8);
        }

        // ContextType (2)
        cursor.write_u16_le(context_type_id(ctx));
        // DataLength (2)
        cursor.write_u16_le(context_data_len(ctx) as u16);
        // Reserved (4)
        cursor.write_u32_le(0);
        // Data (variable)
        pack_context_data(ctx, cursor);
    }
}

/// Unpack a single negotiate context from the cursor.
fn unpack_negotiate_context(cursor: &mut ReadCursor<'_>) -> Result<NegotiateContext> {
    // ContextType (2)
    let context_type = cursor.read_u16_le()?;
    // DataLength (2)
    let data_length = cursor.read_u16_le()? as usize;
    // Reserved (4)
    let _reserved = cursor.read_u32_le()?;

    match context_type {
        NEGOTIATE_CONTEXT_PREAUTH_INTEGRITY => {
            let hash_count = cursor.read_u16_le()? as usize;
            let salt_length = cursor.read_u16_le()? as usize;
            let mut hash_algorithms = Vec::with_capacity(hash_count);
            for _ in 0..hash_count {
                hash_algorithms.push(cursor.read_u16_le()?);
            }
            let salt = cursor.read_bytes_bounded(salt_length)?.to_vec();
            Ok(NegotiateContext::PreauthIntegrity {
                hash_algorithms,
                salt,
            })
        }
        NEGOTIATE_CONTEXT_ENCRYPTION => {
            let cipher_count = cursor.read_u16_le()? as usize;
            let mut ciphers = Vec::with_capacity(cipher_count);
            for _ in 0..cipher_count {
                ciphers.push(cursor.read_u16_le()?);
            }
            Ok(NegotiateContext::Encryption { ciphers })
        }
        NEGOTIATE_CONTEXT_COMPRESSION => {
            let alg_count = cursor.read_u16_le()? as usize;
            let _padding = cursor.read_u16_le()?;
            let flags = cursor.read_u32_le()?;
            let mut algorithms = Vec::with_capacity(alg_count);
            for _ in 0..alg_count {
                algorithms.push(cursor.read_u16_le()?);
            }
            Ok(NegotiateContext::Compression { flags, algorithms })
        }
        NEGOTIATE_CONTEXT_SIGNING => {
            let alg_count = cursor.read_u16_le()? as usize;
            let mut algorithms = Vec::with_capacity(alg_count);
            for _ in 0..alg_count {
                algorithms.push(cursor.read_u16_le()?);
            }
            Ok(NegotiateContext::Signing { algorithms })
        }
        _ => {
            let data = cursor.read_bytes_bounded(data_length)?.to_vec();
            Ok(NegotiateContext::Unknown { context_type, data })
        }
    }
}

/// Unpack a list of negotiate contexts.
fn unpack_negotiate_contexts(
    cursor: &mut ReadCursor<'_>,
    count: usize,
) -> Result<Vec<NegotiateContext>> {
    let mut contexts = Vec::with_capacity(count);
    for i in 0..count {
        // Each context after the first must be 8-byte aligned.
        if i > 0 {
            let pos = cursor.position();
            let remainder = pos % 8;
            if remainder != 0 {
                cursor.skip(8 - remainder)?;
            }
        }
        contexts.push(unpack_negotiate_context(cursor)?);
    }
    Ok(contexts)
}

// ── NegotiateRequest ───────────────────────────────────────────────────

/// SMB2 NEGOTIATE request (spec section 2.2.3).
///
/// Sent by the client to advertise which dialects and capabilities it
/// supports. For SMB 3.1.1, includes negotiate contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiateRequest {
    /// Security mode indicating signing requirements.
    pub security_mode: SecurityMode,
    /// Client capabilities.
    pub capabilities: Capabilities,
    /// Client GUID for identification.
    pub client_guid: Guid,
    /// Supported dialect revision numbers.
    pub dialects: Vec<Dialect>,
    /// Negotiate contexts (only for SMB 3.1.1).
    pub negotiate_contexts: Vec<NegotiateContext>,
}

impl NegotiateRequest {
    pub const STRUCTURE_SIZE: u16 = 36;

    /// Returns `true` if the dialects list includes SMB 3.1.1.
    fn has_smb311(&self) -> bool {
        self.dialects.contains(&Dialect::Smb3_1_1)
    }
}

impl Pack for NegotiateRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // DialectCount (2)
        cursor.write_u16_le(self.dialects.len() as u16);
        // SecurityMode (2)
        cursor.write_u16_le(self.security_mode.bits());
        // Reserved (2)
        cursor.write_u16_le(0);
        // Capabilities (4)
        cursor.write_u32_le(self.capabilities.bits());
        // ClientGuid (16)
        self.client_guid.pack(cursor);

        if self.has_smb311() {
            // NegotiateContextOffset (4) -- will be backpatched
            let ctx_offset_pos = cursor.position();
            cursor.write_u32_le(0); // placeholder
                                    // NegotiateContextCount (2)
            cursor.write_u16_le(self.negotiate_contexts.len() as u16);
            // Reserved2 (2)
            cursor.write_u16_le(0);

            // Dialects array
            for &d in &self.dialects {
                cursor.write_u16_le(d.into());
            }

            // Pad to 8-byte alignment (from start of SMB2 header).
            // The offset is measured from the header start, so we align
            // (Header::SIZE + current_struct_pos).
            let abs_pos = Header::SIZE + (cursor.position() - start);
            let remainder = abs_pos % 8;
            if remainder != 0 {
                cursor.write_zeros(8 - remainder);
            }

            // Backpatch NegotiateContextOffset (from header start)
            let ctx_offset = Header::SIZE + (cursor.position() - start);
            cursor.set_u32_le_at(ctx_offset_pos, ctx_offset as u32);

            // Write negotiate contexts
            pack_negotiate_contexts(&self.negotiate_contexts, cursor);
        } else {
            // ClientStartTime (8 bytes, must be 0)
            cursor.write_u64_le(0);

            // Dialects array
            for &d in &self.dialects {
                cursor.write_u16_le(d.into());
            }
        }
    }
}

impl Unpack for NegotiateRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid NegotiateRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // DialectCount (2)
        let dialect_count = cursor.read_u16_le()? as usize;
        // SecurityMode (2)
        let security_mode = SecurityMode::new(cursor.read_u16_le()?);
        // Reserved (2)
        let _reserved = cursor.read_u16_le()?;
        // Capabilities (4)
        let capabilities = Capabilities::new(cursor.read_u32_le()?);
        // ClientGuid (16)
        let client_guid = Guid::unpack(cursor)?;

        // Read the 8-byte field that is either (offset, count, reserved2)
        // or ClientStartTime -- we need to peek at the dialects to know.
        let raw_8 = cursor.read_bytes(8)?;

        // Dialects array
        let mut dialects = Vec::with_capacity(dialect_count);
        for _ in 0..dialect_count {
            let d = cursor.read_u16_le()?;
            dialects.push(
                Dialect::try_from(d)
                    .map_err(|_| Error::invalid_data(format!("invalid dialect: 0x{:04X}", d)))?,
            );
        }

        let has_311 = dialects.contains(&Dialect::Smb3_1_1);

        let negotiate_contexts = if has_311 {
            // Parse the 8-byte field as (offset, count, reserved2)
            let ctx_offset = u32::from_le_bytes([raw_8[0], raw_8[1], raw_8[2], raw_8[3]]) as usize;
            let ctx_count = u16::from_le_bytes([raw_8[4], raw_8[5]]) as usize;

            // Skip padding to reach the negotiate context list.
            let current_abs = Header::SIZE + (cursor.position() - start);
            if ctx_offset > current_abs {
                cursor.skip(ctx_offset - current_abs)?;
            }

            unpack_negotiate_contexts(cursor, ctx_count)?
        } else {
            Vec::new()
        };

        Ok(NegotiateRequest {
            security_mode,
            capabilities,
            client_guid,
            dialects,
            negotiate_contexts,
        })
    }
}

// ── NegotiateResponse ──────────────────────────────────────────────────

/// SMB2 NEGOTIATE response (spec section 2.2.4).
///
/// Sent by the server to indicate the selected dialect, server capabilities,
/// and (for SMB 3.1.1) negotiate contexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiateResponse {
    /// Server security mode.
    pub security_mode: SecurityMode,
    /// Selected dialect revision.
    pub dialect_revision: Dialect,
    /// Server GUID.
    pub server_guid: Guid,
    /// Server capabilities.
    pub capabilities: Capabilities,
    /// Maximum transact buffer size.
    pub max_transact_size: u32,
    /// Maximum read size.
    pub max_read_size: u32,
    /// Maximum write size.
    pub max_write_size: u32,
    /// Server system time as raw FILETIME value.
    pub system_time: u64,
    /// Server start time as raw FILETIME value.
    pub server_start_time: u64,
    /// Security buffer (GSS token).
    pub security_buffer: Vec<u8>,
    /// Negotiate contexts (only for SMB 3.1.1).
    pub negotiate_contexts: Vec<NegotiateContext>,
}

impl NegotiateResponse {
    pub const STRUCTURE_SIZE: u16 = 65;

    /// Returns `true` if the negotiated dialect is SMB 3.1.1.
    fn is_smb311(&self) -> bool {
        self.dialect_revision == Dialect::Smb3_1_1
    }
}

impl Pack for NegotiateResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // SecurityMode (2)
        cursor.write_u16_le(self.security_mode.bits());
        // DialectRevision (2)
        cursor.write_u16_le(self.dialect_revision.into());
        // NegotiateContextCount/Reserved (2)
        if self.is_smb311() {
            cursor.write_u16_le(self.negotiate_contexts.len() as u16);
        } else {
            cursor.write_u16_le(0);
        }
        // ServerGuid (16)
        self.server_guid.pack(cursor);
        // Capabilities (4)
        cursor.write_u32_le(self.capabilities.bits());
        // MaxTransactSize (4)
        cursor.write_u32_le(self.max_transact_size);
        // MaxReadSize (4)
        cursor.write_u32_le(self.max_read_size);
        // MaxWriteSize (4)
        cursor.write_u32_le(self.max_write_size);
        // SystemTime (8)
        cursor.write_u64_le(self.system_time);
        // ServerStartTime (8)
        cursor.write_u64_le(self.server_start_time);

        // SecurityBufferOffset (2) -- offset from header start to the buffer.
        // Fixed part of the response struct is 64 bytes (fields above), so
        // the buffer starts at Header::SIZE + 64.
        let sec_buf_offset = (Header::SIZE + 64) as u16;
        cursor.write_u16_le(sec_buf_offset);
        // SecurityBufferLength (2)
        cursor.write_u16_le(self.security_buffer.len() as u16);

        // NegotiateContextOffset/Reserved2 (4)
        let ctx_offset_pos = cursor.position();
        cursor.write_u32_le(0); // placeholder (will backpatch for 3.1.1)

        // SecurityBuffer (variable)
        cursor.write_bytes(&self.security_buffer);

        if self.is_smb311() && !self.negotiate_contexts.is_empty() {
            // Pad to 8-byte alignment from header start
            let abs_pos = Header::SIZE + (cursor.position() - start);
            let remainder = abs_pos % 8;
            if remainder != 0 {
                cursor.write_zeros(8 - remainder);
            }

            // Backpatch NegotiateContextOffset
            let ctx_offset = Header::SIZE + (cursor.position() - start);
            cursor.set_u32_le_at(ctx_offset_pos, ctx_offset as u32);

            // Write negotiate contexts
            pack_negotiate_contexts(&self.negotiate_contexts, cursor);
        }
    }
}

impl Unpack for NegotiateResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid NegotiateResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // SecurityMode (2)
        let security_mode = SecurityMode::new(cursor.read_u16_le()?);
        // DialectRevision (2)
        let dialect_raw = cursor.read_u16_le()?;
        let dialect_revision = Dialect::try_from(dialect_raw).map_err(|_| {
            Error::invalid_data(format!("invalid dialect revision: 0x{:04X}", dialect_raw))
        })?;
        // NegotiateContextCount/Reserved (2)
        let negotiate_context_count = cursor.read_u16_le()? as usize;
        // ServerGuid (16)
        let server_guid = Guid::unpack(cursor)?;
        // Capabilities (4)
        let capabilities = Capabilities::new(cursor.read_u32_le()?);
        // MaxTransactSize (4)
        let max_transact_size = cursor.read_u32_le()?;
        // MaxReadSize (4)
        let max_read_size = cursor.read_u32_le()?;
        // MaxWriteSize (4)
        let max_write_size = cursor.read_u32_le()?;
        // SystemTime (8)
        let system_time = cursor.read_u64_le()?;
        // ServerStartTime (8)
        let server_start_time = cursor.read_u64_le()?;
        // SecurityBufferOffset (2)
        let _sec_buf_offset = cursor.read_u16_le()?;
        // SecurityBufferLength (2)
        let sec_buf_length = cursor.read_u16_le()? as usize;
        // NegotiateContextOffset/Reserved2 (4)
        let negotiate_context_offset = cursor.read_u32_le()? as usize;

        // SecurityBuffer (variable)
        let security_buffer = if sec_buf_length > 0 {
            cursor.read_bytes_bounded(sec_buf_length)?.to_vec()
        } else {
            Vec::new()
        };

        // Negotiate contexts (only for 3.1.1)
        let negotiate_contexts =
            if dialect_revision == Dialect::Smb3_1_1 && negotiate_context_count > 0 {
                // Skip padding to reach the context list
                let current_abs = Header::SIZE + (cursor.position() - start);
                if negotiate_context_offset > current_abs {
                    cursor.skip(negotiate_context_offset - current_abs)?;
                }
                unpack_negotiate_contexts(cursor, negotiate_context_count)?
            } else {
                Vec::new()
            };

        Ok(NegotiateResponse {
            security_mode,
            dialect_revision,
            server_guid,
            capabilities,
            max_transact_size,
            max_read_size,
            max_write_size,
            system_time,
            server_start_time,
            security_buffer,
            negotiate_contexts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ────────────────────────────────────────────────────

    fn sample_guid() -> Guid {
        Guid {
            data1: 0x6BA7B810,
            data2: 0x9DAD,
            data3: 0x11D1,
            data4: [0x80, 0xB4, 0x00, 0xC0, 0x4F, 0xD4, 0x30, 0xC8],
        }
    }

    // ── NegotiateRequest tests ─────────────────────────────────────

    #[test]
    fn negotiate_request_roundtrip_without_contexts() {
        let original = NegotiateRequest {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::new(Capabilities::DFS | Capabilities::LARGE_MTU),
            client_guid: sample_guid(),
            dialects: vec![Dialect::Smb2_0_2, Dialect::Smb2_1, Dialect::Smb3_0],
            negotiate_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.security_mode.bits(), original.security_mode.bits());
        assert_eq!(decoded.capabilities.bits(), original.capabilities.bits());
        assert_eq!(decoded.client_guid, original.client_guid);
        assert_eq!(decoded.dialects, original.dialects);
        assert!(decoded.negotiate_contexts.is_empty());
    }

    #[test]
    fn negotiate_request_roundtrip_with_contexts() {
        let original = NegotiateRequest {
            security_mode: SecurityMode::new(
                SecurityMode::SIGNING_ENABLED | SecurityMode::SIGNING_REQUIRED,
            ),
            capabilities: Capabilities::new(
                Capabilities::DFS
                    | Capabilities::LEASING
                    | Capabilities::LARGE_MTU
                    | Capabilities::ENCRYPTION,
            ),
            client_guid: sample_guid(),
            dialects: vec![
                Dialect::Smb2_0_2,
                Dialect::Smb2_1,
                Dialect::Smb3_0,
                Dialect::Smb3_0_2,
                Dialect::Smb3_1_1,
            ],
            negotiate_contexts: vec![
                NegotiateContext::PreauthIntegrity {
                    hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                    salt: vec![0xDE, 0xAD, 0xBE, 0xEF],
                },
                NegotiateContext::Encryption {
                    ciphers: vec![CIPHER_AES_128_GCM, CIPHER_AES_128_CCM],
                },
                NegotiateContext::Signing {
                    algorithms: vec![SIGNING_AES_GMAC, SIGNING_AES_CMAC],
                },
                NegotiateContext::Compression {
                    flags: COMPRESSION_FLAG_CHAINED,
                    algorithms: vec![COMPRESSION_LZ77, COMPRESSION_LZNT1],
                },
            ],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.security_mode.bits(), original.security_mode.bits());
        assert_eq!(decoded.capabilities.bits(), original.capabilities.bits());
        assert_eq!(decoded.client_guid, original.client_guid);
        assert_eq!(decoded.dialects, original.dialects);
        assert_eq!(decoded.negotiate_contexts.len(), 4);
        assert_eq!(decoded.negotiate_contexts, original.negotiate_contexts);
    }

    #[test]
    fn negotiate_request_structure_size_field() {
        let req = NegotiateRequest {
            security_mode: SecurityMode::default(),
            capabilities: Capabilities::default(),
            client_guid: Guid::ZERO,
            dialects: vec![Dialect::Smb2_0_2],
            negotiate_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 36);
    }

    #[test]
    fn negotiate_request_wrong_structure_size() {
        let mut buf = [0u8; 48];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = NegotiateRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn negotiate_request_single_dialect() {
        let original = NegotiateRequest {
            security_mode: SecurityMode::default(),
            capabilities: Capabilities::default(),
            client_guid: Guid::ZERO,
            dialects: vec![Dialect::Smb3_0_2],
            negotiate_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.dialects, vec![Dialect::Smb3_0_2]);
    }

    #[test]
    fn negotiate_request_smb311_only() {
        let original = NegotiateRequest {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::default(),
            client_guid: sample_guid(),
            dialects: vec![Dialect::Smb3_1_1],
            negotiate_contexts: vec![NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            }],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.dialects, vec![Dialect::Smb3_1_1]);
        assert_eq!(decoded.negotiate_contexts.len(), 1);
        assert_eq!(
            decoded.negotiate_contexts[0],
            original.negotiate_contexts[0]
        );
    }

    // ── NegotiateResponse tests ────────────────────────────────────

    #[test]
    fn negotiate_response_roundtrip_no_contexts() {
        let original = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: Dialect::Smb3_0,
            server_guid: sample_guid(),
            capabilities: Capabilities::new(
                Capabilities::DFS | Capabilities::LEASING | Capabilities::LARGE_MTU,
            ),
            max_transact_size: 8_388_608,
            max_read_size: 8_388_608,
            max_write_size: 8_388_608,
            system_time: 133_485_408_000_000_000,
            server_start_time: 0,
            security_buffer: vec![0x60, 0x28, 0x06, 0x06],
            negotiate_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.security_mode.bits(), original.security_mode.bits());
        assert_eq!(decoded.dialect_revision, Dialect::Smb3_0);
        assert_eq!(decoded.server_guid, original.server_guid);
        assert_eq!(decoded.capabilities.bits(), original.capabilities.bits());
        assert_eq!(decoded.max_transact_size, 8_388_608);
        assert_eq!(decoded.max_read_size, 8_388_608);
        assert_eq!(decoded.max_write_size, 8_388_608);
        assert_eq!(decoded.system_time, original.system_time);
        assert_eq!(decoded.server_start_time, 0);
        assert_eq!(decoded.security_buffer, original.security_buffer);
        assert!(decoded.negotiate_contexts.is_empty());
    }

    #[test]
    fn negotiate_response_roundtrip_with_contexts() {
        let original = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: Dialect::Smb3_1_1,
            server_guid: sample_guid(),
            capabilities: Capabilities::new(Capabilities::DFS | Capabilities::ENCRYPTION),
            max_transact_size: 1_048_576,
            max_read_size: 1_048_576,
            max_write_size: 1_048_576,
            system_time: 133_485_408_000_000_000,
            server_start_time: 133_000_000_000_000_000,
            security_buffer: vec![0x60, 0x28],
            negotiate_contexts: vec![
                NegotiateContext::PreauthIntegrity {
                    hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                    salt: vec![0xAA, 0xBB, 0xCC, 0xDD],
                },
                NegotiateContext::Encryption {
                    ciphers: vec![CIPHER_AES_128_GCM],
                },
                NegotiateContext::Signing {
                    algorithms: vec![SIGNING_AES_GMAC],
                },
            ],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.dialect_revision, Dialect::Smb3_1_1);
        assert_eq!(decoded.negotiate_contexts.len(), 3);
        assert_eq!(decoded.negotiate_contexts, original.negotiate_contexts);
        assert_eq!(decoded.security_buffer, original.security_buffer);
    }

    #[test]
    fn negotiate_response_structure_size_field() {
        let resp = NegotiateResponse {
            security_mode: SecurityMode::default(),
            dialect_revision: Dialect::Smb2_0_2,
            server_guid: Guid::ZERO,
            capabilities: Capabilities::default(),
            max_transact_size: 0,
            max_read_size: 0,
            max_write_size: 0,
            system_time: 0,
            server_start_time: 0,
            security_buffer: Vec::new(),
            negotiate_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 65);
    }

    #[test]
    fn negotiate_response_wrong_structure_size() {
        let mut buf = [0u8; 70];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = NegotiateResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn negotiate_response_empty_security_buffer() {
        let original = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: Dialect::Smb2_1,
            server_guid: Guid::ZERO,
            capabilities: Capabilities::default(),
            max_transact_size: 65536,
            max_read_size: 65536,
            max_write_size: 65536,
            system_time: 0,
            server_start_time: 0,
            security_buffer: Vec::new(),
            negotiate_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = NegotiateResponse::unpack(&mut r).unwrap();

        assert!(decoded.security_buffer.is_empty());
        assert_eq!(decoded.dialect_revision, Dialect::Smb2_1);
    }

    // ── Negotiate context roundtrip tests ──────────────────────────

    #[test]
    fn context_preauth_integrity_roundtrip() {
        let ctx = NegotiateContext::PreauthIntegrity {
            hash_algorithms: vec![HASH_ALGORITHM_SHA512],
            salt: vec![0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
        };

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(std::slice::from_ref(&ctx), &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 1).unwrap();

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], ctx);
    }

    #[test]
    fn context_encryption_roundtrip() {
        let ctx = NegotiateContext::Encryption {
            ciphers: vec![
                CIPHER_AES_128_GCM,
                CIPHER_AES_128_CCM,
                CIPHER_AES_256_GCM,
                CIPHER_AES_256_CCM,
            ],
        };

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(std::slice::from_ref(&ctx), &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 1).unwrap();

        assert_eq!(decoded[0], ctx);
    }

    #[test]
    fn context_signing_roundtrip() {
        let ctx = NegotiateContext::Signing {
            algorithms: vec![SIGNING_AES_GMAC, SIGNING_AES_CMAC, SIGNING_HMAC_SHA256],
        };

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(std::slice::from_ref(&ctx), &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 1).unwrap();

        assert_eq!(decoded[0], ctx);
    }

    #[test]
    fn context_compression_roundtrip() {
        let ctx = NegotiateContext::Compression {
            flags: COMPRESSION_FLAG_CHAINED,
            algorithms: vec![COMPRESSION_LZ77, COMPRESSION_LZNT1, COMPRESSION_LZ4],
        };

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(std::slice::from_ref(&ctx), &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 1).unwrap();

        assert_eq!(decoded[0], ctx);
    }

    #[test]
    fn context_unknown_roundtrip() {
        let ctx = NegotiateContext::Unknown {
            context_type: 0x00FF,
            data: vec![0x01, 0x02, 0x03, 0x04],
        };

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(std::slice::from_ref(&ctx), &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 1).unwrap();

        assert_eq!(decoded[0], ctx);
    }

    #[test]
    fn multiple_contexts_roundtrip() {
        let contexts = vec![
            NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0xAA; 32],
            },
            NegotiateContext::Encryption {
                ciphers: vec![CIPHER_AES_128_GCM],
            },
            NegotiateContext::Compression {
                flags: COMPRESSION_FLAG_NONE,
                algorithms: vec![COMPRESSION_NONE],
            },
            NegotiateContext::Signing {
                algorithms: vec![SIGNING_HMAC_SHA256],
            },
        ];

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(&contexts, &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 4).unwrap();

        assert_eq!(decoded, contexts);
    }

    #[test]
    fn context_alignment_is_8_bytes() {
        // A PreauthIntegrity context with a 3-byte salt creates a data section
        // that isn't 8-byte aligned. The next context should be padded.
        let contexts = vec![
            NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0x01, 0x02, 0x03], // 3 bytes -> total data = 2+2+2+3 = 9
            },
            NegotiateContext::Encryption {
                ciphers: vec![CIPHER_AES_128_GCM],
            },
        ];

        let mut w = WriteCursor::new();
        pack_negotiate_contexts(&contexts, &mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = unpack_negotiate_contexts(&mut r, 2).unwrap();

        assert_eq!(decoded, contexts);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{
        arb_capabilities, arb_dialect, arb_guid, arb_security_mode, arb_small_bytes,
    };
    use proptest::prelude::*;

    fn arb_u16_vec(max: usize) -> impl Strategy<Value = Vec<u16>> {
        prop::collection::vec(any::<u16>(), 0..=max)
    }

    /// Generate a `NegotiateContext`. The generator avoids the Unknown variant
    /// with a context type that collides with a known one, since the decoder
    /// would demote it back to the typed form and roundtrip would fail.
    fn arb_negotiate_context() -> impl Strategy<Value = NegotiateContext> {
        let known_types = [
            NEGOTIATE_CONTEXT_PREAUTH_INTEGRITY,
            NEGOTIATE_CONTEXT_ENCRYPTION,
            NEGOTIATE_CONTEXT_COMPRESSION,
            NEGOTIATE_CONTEXT_SIGNING,
        ];

        let preauth = (arb_u16_vec(8), prop::collection::vec(any::<u8>(), 0..=64)).prop_map(
            |(hash_algorithms, salt)| NegotiateContext::PreauthIntegrity {
                hash_algorithms,
                salt,
            },
        );
        let encryption =
            arb_u16_vec(8).prop_map(|ciphers| NegotiateContext::Encryption { ciphers });
        let compression = (any::<u32>(), arb_u16_vec(8))
            .prop_map(|(flags, algorithms)| NegotiateContext::Compression { flags, algorithms });
        let signing =
            arb_u16_vec(8).prop_map(|algorithms| NegotiateContext::Signing { algorithms });
        let unknown = (any::<u16>(), prop::collection::vec(any::<u8>(), 0..=64))
            .prop_filter(
                "type must not collide with a known variant",
                move |(t, _)| !known_types.contains(t),
            )
            .prop_map(|(context_type, data)| NegotiateContext::Unknown { context_type, data });

        prop_oneof![preauth, encryption, compression, signing, unknown]
    }

    /// Generate the tuple `(dialects, negotiate_contexts)` in a mutually
    /// consistent way: contexts are present iff 3.1.1 is in the dialect list.
    fn arb_dialects_and_contexts() -> impl Strategy<Value = (Vec<Dialect>, Vec<NegotiateContext>)> {
        prop::collection::vec(arb_dialect(), 1..=5).prop_flat_map(|dialects| {
            let has_311 = dialects.contains(&Dialect::Smb3_1_1);
            let ctx_strat: BoxedStrategy<Vec<NegotiateContext>> = if has_311 {
                prop::collection::vec(arb_negotiate_context(), 0..=4).boxed()
            } else {
                Just(Vec::new()).boxed()
            };
            (Just(dialects), ctx_strat)
        })
    }

    proptest! {
        #[test]
        fn negotiate_context_list_roundtrip(
            contexts in prop::collection::vec(arb_negotiate_context(), 0..=6),
        ) {
            let mut w = WriteCursor::new();
            pack_negotiate_contexts(&contexts, &mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = unpack_negotiate_contexts(&mut r, contexts.len()).unwrap();
            prop_assert_eq!(decoded, contexts);
        }

        #[test]
        fn negotiate_request_pack_unpack(
            security_mode in arb_security_mode(),
            capabilities in arb_capabilities(),
            client_guid in arb_guid(),
            (dialects, negotiate_contexts) in arb_dialects_and_contexts(),
        ) {
            let original = NegotiateRequest {
                security_mode,
                capabilities,
                client_guid,
                dialects,
                negotiate_contexts,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = NegotiateRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }

        #[test]
        fn negotiate_response_pack_unpack(
            security_mode in arb_security_mode(),
            server_guid in arb_guid(),
            capabilities in arb_capabilities(),
            max_transact_size in any::<u32>(),
            max_read_size in any::<u32>(),
            max_write_size in any::<u32>(),
            system_time in any::<u64>(),
            server_start_time in any::<u64>(),
            security_buffer in arb_small_bytes(),
            dialect_revision in arb_dialect(),
            contexts_if_311 in prop::collection::vec(arb_negotiate_context(), 0..=4),
        ) {
            // Contexts only present for SMB 3.1.1, per the spec / encoder.
            let negotiate_contexts = if dialect_revision == Dialect::Smb3_1_1 {
                contexts_if_311
            } else {
                Vec::new()
            };
            let original = NegotiateResponse {
                security_mode,
                dialect_revision,
                server_guid,
                capabilities,
                max_transact_size,
                max_read_size,
                max_write_size,
                system_time,
                server_start_time,
                security_buffer,
                negotiate_contexts,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = NegotiateResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }
    }
}
