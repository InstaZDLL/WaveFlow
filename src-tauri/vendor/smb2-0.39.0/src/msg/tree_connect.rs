//! SMB2 TREE_CONNECT request and response (spec sections 2.2.9, 2.2.10).
//!
//! Tree connect messages establish access to a share on the server.
//! The request contains a UTF-16LE encoded share path (for example,
//! `\\server\share`), and the response contains share metadata such as
//! the share type, flags, capabilities, and maximal access rights.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::flags::{ShareCapabilities, ShareFlags};
use crate::Error;

// ── Share type ─────────────────────────────────────────────────────────

/// Type of share being accessed (spec section 2.2.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShareType {
    /// Physical disk share.
    Disk = 0x01,
    /// Named pipe share.
    Pipe = 0x02,
    /// Printer share.
    Print = 0x03,
}

impl ShareType {
    /// Try to convert a raw `u8` to a `ShareType`.
    pub fn try_from_u8(val: u8) -> Result<Self> {
        match val {
            0x01 => Ok(ShareType::Disk),
            0x02 => Ok(ShareType::Pipe),
            0x03 => Ok(ShareType::Print),
            other => Err(Error::invalid_data(format!(
                "invalid share type: 0x{:02X}",
                other
            ))),
        }
    }
}

// ── Tree connect request flags ─────────────────────────────────────────

/// Flags for the TREE_CONNECT request (spec section 2.2.9, SMB 3.1.1 only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TreeConnectRequestFlags(pub u16);

impl TreeConnectRequestFlags {
    /// Client has previously connected to the specified cluster share.
    pub const CLUSTER_RECONNECT: u16 = 0x0001;
    /// Client can handle synchronous share redirects.
    pub const REDIRECT_TO_OWNER: u16 = 0x0002;
    /// Tree connect request extension is present.
    pub const EXTENSION_PRESENT: u16 = 0x0004;
}

// ── TreeConnectRequest ─────────────────────────────────────────────────

/// SMB2 TREE_CONNECT request (spec section 2.2.9).
///
/// Sent by the client to request access to a particular share on the
/// server. The path is a Unicode string in the form `\\server\share`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeConnectRequest {
    /// Flags controlling the request (SMB 3.1.1 only, otherwise 0).
    pub flags: TreeConnectRequestFlags,
    /// Full share path name in UTF-8 (encoded as UTF-16LE on the wire).
    pub path: String,
}

impl TreeConnectRequest {
    pub const STRUCTURE_SIZE: u16 = 9;
}

impl Pack for TreeConnectRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Flags/Reserved (2 bytes)
        cursor.write_u16_le(self.flags.0);

        // Compute path length in UTF-16LE bytes
        let path_u16: Vec<u16> = self.path.encode_utf16().collect();
        let path_byte_len = path_u16.len() * 2;

        // PathOffset (2 bytes) -- offset from start of SMB2 header
        let offset = (Header::SIZE + 8) as u16; // 8 = fixed part of this struct
        cursor.write_u16_le(offset);
        // PathLength (2 bytes)
        cursor.write_u16_le(path_byte_len as u16);
        // Buffer: path in UTF-16LE
        cursor.write_utf16_le(&self.path);
    }
}

impl Unpack for TreeConnectRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid TreeConnectRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // Flags/Reserved (2 bytes)
        let flags = TreeConnectRequestFlags(cursor.read_u16_le()?);
        // PathOffset (2 bytes) -- we ignore, read sequentially
        let _offset = cursor.read_u16_le()?;
        // PathLength (2 bytes)
        let path_length = cursor.read_u16_le()? as usize;
        // Buffer: path in UTF-16LE
        if path_length > ReadCursor::MAX_UNPACK_BUFFER {
            return Err(Error::invalid_data(format!(
                "buffer size {} exceeds maximum {} bytes",
                path_length,
                ReadCursor::MAX_UNPACK_BUFFER
            )));
        }
        let path = cursor.read_utf16_le(path_length)?;

        Ok(TreeConnectRequest { flags, path })
    }
}

// ── TreeConnectResponse ────────────────────────────────────────────────

/// SMB2 TREE_CONNECT response (spec section 2.2.10).
///
/// Sent by the server when a TREE_CONNECT request is processed
/// successfully. Contains share metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeConnectResponse {
    /// The type of share being accessed (disk, pipe, or print).
    pub share_type: ShareType,
    /// Properties for this share.
    pub share_flags: ShareFlags,
    /// Capabilities for this share.
    pub capabilities: ShareCapabilities,
    /// Maximum access rights for the connecting user.
    pub maximal_access: u32,
}

impl TreeConnectResponse {
    pub const STRUCTURE_SIZE: u16 = 16;
}

impl Pack for TreeConnectResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // ShareType (1 byte)
        cursor.write_u8(self.share_type as u8);
        // Reserved (1 byte)
        cursor.write_u8(0);
        // ShareFlags (4 bytes)
        cursor.write_u32_le(self.share_flags.bits());
        // Capabilities (4 bytes)
        cursor.write_u32_le(self.capabilities.bits());
        // MaximalAccess (4 bytes)
        cursor.write_u32_le(self.maximal_access);
    }
}

impl Unpack for TreeConnectResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid TreeConnectResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // ShareType (1 byte)
        let share_type = ShareType::try_from_u8(cursor.read_u8()?)?;
        // Reserved (1 byte)
        let _reserved = cursor.read_u8()?;
        // ShareFlags (4 bytes)
        let share_flags = ShareFlags::new(cursor.read_u32_le()?);
        // Capabilities (4 bytes)
        let capabilities = ShareCapabilities::new(cursor.read_u32_le()?);
        // MaximalAccess (4 bytes)
        let maximal_access = cursor.read_u32_le()?;

        Ok(TreeConnectResponse {
            share_type,
            share_flags,
            capabilities,
            maximal_access,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── TreeConnectRequest tests ───────────────────────────────────

    #[test]
    fn tree_connect_request_roundtrip() {
        let original = TreeConnectRequest {
            flags: TreeConnectRequestFlags::default(),
            path: r"\\server\share".to_string(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.path, original.path);
    }

    #[test]
    fn tree_connect_request_with_utf16_path() {
        let path = r"\\myserver.example.com\IPC$";
        let original = TreeConnectRequest {
            flags: TreeConnectRequestFlags::default(),
            path: path.to_string(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.path, path);
    }

    #[test]
    fn tree_connect_request_structure_size_field() {
        let req = TreeConnectRequest {
            flags: TreeConnectRequestFlags::default(),
            path: r"\\s\d".to_string(),
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        // First 2 bytes are structure size = 9
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 9);
    }

    #[test]
    fn tree_connect_request_wrong_structure_size() {
        let mut buf = [0u8; 20];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = TreeConnectRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn tree_connect_request_with_flags() {
        let original = TreeConnectRequest {
            flags: TreeConnectRequestFlags(TreeConnectRequestFlags::CLUSTER_RECONNECT),
            path: r"\\s\d".to_string(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags.0, TreeConnectRequestFlags::CLUSTER_RECONNECT);
    }

    // ── TreeConnectResponse tests ──────────────────────────────────

    #[test]
    fn tree_connect_response_roundtrip_disk() {
        let original = TreeConnectResponse {
            share_type: ShareType::Disk,
            share_flags: ShareFlags::new(ShareFlags::DFS | ShareFlags::ACCESS_BASED_DIRECTORY_ENUM),
            capabilities: ShareCapabilities::new(ShareCapabilities::DFS),
            maximal_access: 0x001F_01FF,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.share_type, ShareType::Disk);
        assert_eq!(decoded.share_flags.bits(), original.share_flags.bits());
        assert_eq!(decoded.capabilities.bits(), original.capabilities.bits());
        assert_eq!(decoded.maximal_access, 0x001F_01FF);
    }

    #[test]
    fn tree_connect_response_roundtrip_pipe() {
        let original = TreeConnectResponse {
            share_type: ShareType::Pipe,
            share_flags: ShareFlags::default(),
            capabilities: ShareCapabilities::default(),
            maximal_access: 0x0012_019F,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.share_type, ShareType::Pipe);
        assert_eq!(decoded.maximal_access, 0x0012_019F);
    }

    #[test]
    fn tree_connect_response_roundtrip_print() {
        let original = TreeConnectResponse {
            share_type: ShareType::Print,
            share_flags: ShareFlags::new(ShareFlags::ENCRYPT_DATA),
            capabilities: ShareCapabilities::new(
                ShareCapabilities::CONTINUOUS_AVAILABILITY | ShareCapabilities::CLUSTER,
            ),
            maximal_access: 0,
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.share_type, ShareType::Print);
        assert!(decoded.share_flags.contains(ShareFlags::ENCRYPT_DATA));
        assert!(decoded
            .capabilities
            .contains(ShareCapabilities::CONTINUOUS_AVAILABILITY));
        assert!(decoded.capabilities.contains(ShareCapabilities::CLUSTER));
    }

    #[test]
    fn tree_connect_response_structure_size_field() {
        let resp = TreeConnectResponse {
            share_type: ShareType::Disk,
            share_flags: ShareFlags::default(),
            capabilities: ShareCapabilities::default(),
            maximal_access: 0,
        };

        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        let bytes = w.into_inner();

        // First 2 bytes are structure size = 16
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 16);
        // Total packed size: 2 + 1 + 1 + 4 + 4 + 4 = 16
        assert_eq!(bytes.len(), 16);
    }

    #[test]
    fn tree_connect_response_wrong_structure_size() {
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        buf[2] = 0x01; // valid share type
        let mut cursor = ReadCursor::new(&buf);
        let result = TreeConnectResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    #[test]
    fn tree_connect_response_invalid_share_type() {
        let mut buf = [0u8; 16];
        buf[0..2].copy_from_slice(&16u16.to_le_bytes());
        buf[2] = 0xFF; // invalid share type
        let mut cursor = ReadCursor::new(&buf);
        let result = TreeConnectResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("share type"), "error was: {err}");
    }

    // Roundtrip property tests live in `roundtrip_props` at file end.

    #[test]
    fn tree_connect_response_known_bytes() {
        // Known bytes from smb-rs test: share_type=Disk, share_flags=0x00000800,
        // capabilities=0, maximal_access=0x001f01ff
        let bytes: Vec<u8> = vec![
            0x10, 0x00, // StructureSize = 16
            0x01, // ShareType = Disk
            0x00, // Reserved
            0x00, 0x08, 0x00, 0x00, // ShareFlags = 0x00000800
            0x00, 0x00, 0x00, 0x00, // Capabilities = 0
            0xFF, 0x01, 0x1F, 0x00, // MaximalAccess = 0x001f01ff
        ];

        let mut r = ReadCursor::new(&bytes);
        let decoded = TreeConnectResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.share_type, ShareType::Disk);
        assert!(decoded
            .share_flags
            .contains(ShareFlags::ACCESS_BASED_DIRECTORY_ENUM));
        assert_eq!(decoded.maximal_access, 0x001F_01FF);
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{
        arb_share_capabilities, arb_share_flags, arb_share_type, arb_utf16_string,
    };
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn tree_connect_request_pack_unpack(
            flags_raw in any::<u16>(),
            // Path is sent as UTF-16LE. Generate strings that survive that
            // encoding cleanly (no unpaired surrogates).
            path in arb_utf16_string(128),
        ) {
            let original = TreeConnectRequest {
                flags: TreeConnectRequestFlags(flags_raw),
                path,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = TreeConnectRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn tree_connect_response_pack_unpack(
            share_type in arb_share_type(),
            share_flags in arb_share_flags(),
            capabilities in arb_share_capabilities(),
            maximal_access in any::<u32>(),
        ) {
            let original = TreeConnectResponse {
                share_type,
                share_flags,
                capabilities,
                maximal_access,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = TreeConnectResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
