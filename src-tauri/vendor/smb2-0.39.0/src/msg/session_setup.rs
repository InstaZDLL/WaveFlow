//! SMB2 SESSION_SETUP request and response (spec sections 2.2.5, 2.2.6).
//!
//! Session setup messages are used to establish an authenticated session
//! between the client and the server. The request carries a security token
//! (for example, SPNEGO/NTLM) and the response carries the server's reply token
//! along with session flags.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::flags::{Capabilities, SecurityMode};
use crate::Error;

// ── Session setup request flags ────────────────────────────────────────

/// Flags for the SESSION_SETUP request (1 byte, spec section 2.2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionSetupRequestFlags(pub u8);

impl SessionSetupRequestFlags {
    /// Bind an existing session to a new connection (SMB 3.x only).
    pub const BINDING: u8 = 0x01;

    /// Returns `true` if the binding flag is set.
    #[inline]
    pub fn is_binding(&self) -> bool {
        self.0 & Self::BINDING != 0
    }
}

// ── Session flags (response) ───────────────────────────────────────────

/// Session flags returned in the SESSION_SETUP response (spec section 2.2.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionFlags(pub u16);

impl SessionFlags {
    /// The client has been authenticated as a guest user.
    pub const IS_GUEST: u16 = 0x0001;
    /// The client has been authenticated as an anonymous user.
    pub const IS_NULL: u16 = 0x0002;
    /// The server requires encryption of messages on this session (SMB 3.x only).
    pub const ENCRYPT_DATA: u16 = 0x0004;

    /// Returns `true` if the guest flag is set.
    #[inline]
    pub fn is_guest(&self) -> bool {
        self.0 & Self::IS_GUEST != 0
    }

    /// Returns `true` if the null session flag is set.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.0 & Self::IS_NULL != 0
    }

    /// Returns `true` if the encrypt-data flag is set.
    #[inline]
    pub fn encrypt_data(&self) -> bool {
        self.0 & Self::ENCRYPT_DATA != 0
    }
}

// ── SessionSetupRequest ────────────────────────────────────────────────

/// SMB2 SESSION_SETUP request (spec section 2.2.5).
///
/// Sent by the client to establish an authenticated session. The security
/// buffer carries a GSS/SPNEGO token (or other auth protocol token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSetupRequest {
    /// Flags controlling the request (for example, session binding).
    pub flags: SessionSetupRequestFlags,
    /// Security mode indicating signing requirements.
    pub security_mode: SecurityMode,
    /// Client capabilities.
    pub capabilities: Capabilities,
    /// Channel field (reserved, must be 0).
    pub channel: u32,
    /// Previously established session identifier for reconnection.
    pub previous_session_id: u64,
    /// Security buffer containing the authentication token.
    pub security_buffer: Vec<u8>,
}

impl SessionSetupRequest {
    pub const STRUCTURE_SIZE: u16 = 25;
}

impl Pack for SessionSetupRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Flags (1 byte)
        cursor.write_u8(self.flags.0);
        // SecurityMode (1 byte)
        cursor.write_u8(self.security_mode.bits() as u8);
        // Capabilities (4 bytes)
        cursor.write_u32_le(self.capabilities.bits());
        // Channel (4 bytes)
        cursor.write_u32_le(self.channel);

        // SecurityBufferOffset (2 bytes) -- offset from start of SMB2 header
        let offset = (Header::SIZE + 24) as u16; // 24 = bytes before the buffer in this struct
        cursor.write_u16_le(offset);
        // SecurityBufferLength (2 bytes)
        cursor.write_u16_le(self.security_buffer.len() as u16);
        // PreviousSessionId (8 bytes)
        cursor.write_u64_le(self.previous_session_id);
        // Buffer (variable)
        cursor.write_bytes(&self.security_buffer);
    }
}

impl Unpack for SessionSetupRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid SessionSetupRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // Flags (1 byte)
        let flags = SessionSetupRequestFlags(cursor.read_u8()?);
        // SecurityMode (1 byte)
        let security_mode = SecurityMode::new(cursor.read_u8()? as u16);
        // Capabilities (4 bytes)
        let capabilities = Capabilities::new(cursor.read_u32_le()?);
        // Channel (4 bytes)
        let channel = cursor.read_u32_le()?;
        // SecurityBufferOffset (2 bytes) -- we ignore, read sequentially
        let _offset = cursor.read_u16_le()?;
        // SecurityBufferLength (2 bytes)
        let buffer_length = cursor.read_u16_le()? as usize;
        // PreviousSessionId (8 bytes)
        let previous_session_id = cursor.read_u64_le()?;
        // Buffer (variable)
        let security_buffer = if buffer_length > 0 {
            cursor.read_bytes_bounded(buffer_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(SessionSetupRequest {
            flags,
            security_mode,
            capabilities,
            channel,
            previous_session_id,
            security_buffer,
        })
    }
}

// ── SessionSetupResponse ───────────────────────────────────────────────

/// SMB2 SESSION_SETUP response (spec section 2.2.6).
///
/// Sent by the server in response to a SESSION_SETUP request. Contains
/// session flags and a security buffer with the server's auth token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSetupResponse {
    /// Flags indicating additional information about the session.
    pub session_flags: SessionFlags,
    /// Security buffer containing the server's authentication token.
    pub security_buffer: Vec<u8>,
}

impl SessionSetupResponse {
    pub const STRUCTURE_SIZE: u16 = 9;
}

impl Pack for SessionSetupResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // SessionFlags (2 bytes)
        cursor.write_u16_le(self.session_flags.0);
        // SecurityBufferOffset (2 bytes) -- offset from start of SMB2 header
        let offset = (Header::SIZE + 8) as u16; // 8 = fixed part of response struct
        cursor.write_u16_le(offset);
        // SecurityBufferLength (2 bytes)
        cursor.write_u16_le(self.security_buffer.len() as u16);
        // Buffer (variable)
        cursor.write_bytes(&self.security_buffer);
    }
}

impl Unpack for SessionSetupResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid SessionSetupResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // SessionFlags (2 bytes)
        let session_flags = SessionFlags(cursor.read_u16_le()?);
        // SecurityBufferOffset (2 bytes)
        let _offset = cursor.read_u16_le()?;
        // SecurityBufferLength (2 bytes)
        let buffer_length = cursor.read_u16_le()? as usize;
        // Buffer (variable)
        let security_buffer = if buffer_length > 0 {
            cursor.read_bytes_bounded(buffer_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(SessionSetupResponse {
            session_flags,
            security_buffer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SessionSetupRequest tests ──────────────────────────────────

    #[test]
    fn session_setup_request_roundtrip() {
        let token = vec![0x60, 0x28, 0x06, 0x06, 0x2b, 0x06, 0x01, 0x05];
        let original = SessionSetupRequest {
            flags: SessionSetupRequestFlags(0),
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::new(Capabilities::DFS),
            channel: 0,
            previous_session_id: 0,
            security_buffer: token.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SessionSetupRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.security_mode.bits(), original.security_mode.bits());
        assert_eq!(decoded.capabilities.bits(), original.capabilities.bits());
        assert_eq!(decoded.channel, 0);
        assert_eq!(decoded.previous_session_id, 0);
        assert_eq!(decoded.security_buffer, token);
    }

    #[test]
    fn session_setup_request_with_binding_flag() {
        let original = SessionSetupRequest {
            flags: SessionSetupRequestFlags(SessionSetupRequestFlags::BINDING),
            security_mode: SecurityMode::new(
                SecurityMode::SIGNING_ENABLED | SecurityMode::SIGNING_REQUIRED,
            ),
            capabilities: Capabilities::default(),
            channel: 0,
            previous_session_id: 0xDEAD_BEEF_CAFE_BABE,
            security_buffer: vec![0xAA, 0xBB],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SessionSetupRequest::unpack(&mut r).unwrap();

        assert!(decoded.flags.is_binding());
        assert!(decoded.security_mode.signing_enabled());
        assert!(decoded.security_mode.signing_required());
        assert_eq!(decoded.previous_session_id, 0xDEAD_BEEF_CAFE_BABE);
        assert_eq!(decoded.security_buffer, vec![0xAA, 0xBB]);
    }

    #[test]
    fn session_setup_request_empty_buffer() {
        let original = SessionSetupRequest {
            flags: SessionSetupRequestFlags(0),
            security_mode: SecurityMode::default(),
            capabilities: Capabilities::default(),
            channel: 0,
            previous_session_id: 0,
            security_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SessionSetupRequest::unpack(&mut r).unwrap();

        assert!(decoded.security_buffer.is_empty());
    }

    #[test]
    fn session_setup_request_structure_size_field() {
        let req = SessionSetupRequest {
            flags: SessionSetupRequestFlags(0),
            security_mode: SecurityMode::default(),
            capabilities: Capabilities::default(),
            channel: 0,
            previous_session_id: 0,
            security_buffer: vec![0x01],
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        // First 2 bytes are structure size = 25
        assert_eq!(bytes[0], 25);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn session_setup_request_wrong_structure_size() {
        let mut buf = [0u8; 26];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = SessionSetupRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── SessionSetupResponse tests ─────────────────────────────────

    #[test]
    fn session_setup_response_roundtrip() {
        let token = vec![0xA1, 0x81, 0xB0, 0x30, 0x81, 0xAD];
        let original = SessionSetupResponse {
            session_flags: SessionFlags(0),
            security_buffer: token.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SessionSetupResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.session_flags, original.session_flags);
        assert_eq!(decoded.security_buffer, token);
    }

    #[test]
    fn session_setup_response_with_flags() {
        let original = SessionSetupResponse {
            session_flags: SessionFlags(SessionFlags::IS_GUEST | SessionFlags::ENCRYPT_DATA),
            security_buffer: vec![0x01, 0x02, 0x03],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SessionSetupResponse::unpack(&mut r).unwrap();

        assert!(decoded.session_flags.is_guest());
        assert!(!decoded.session_flags.is_null());
        assert!(decoded.session_flags.encrypt_data());
        assert_eq!(decoded.security_buffer, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn session_setup_response_null_session() {
        let original = SessionSetupResponse {
            session_flags: SessionFlags(SessionFlags::IS_NULL),
            security_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = SessionSetupResponse::unpack(&mut r).unwrap();

        assert!(decoded.session_flags.is_null());
        assert!(!decoded.session_flags.is_guest());
        assert!(decoded.security_buffer.is_empty());
    }

    #[test]
    fn session_setup_response_structure_size_field() {
        let resp = SessionSetupResponse {
            session_flags: SessionFlags(0),
            security_buffer: Vec::new(),
        };

        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        let bytes = w.into_inner();

        // First 2 bytes are structure size = 9
        assert_eq!(bytes[0], 9);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn session_setup_response_wrong_structure_size() {
        let mut buf = [0u8; 10];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = SessionSetupResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_capabilities, arb_small_bytes};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn session_setup_request_pack_unpack(
            flags_raw in any::<u8>(),
            // SESSION_SETUP packs SecurityMode as a single byte, so only the
            // low 8 bits survive the roundtrip. Generate u8 values to avoid
            // producing inputs the encoder would never emit from a real caller.
            security_mode_raw in any::<u8>(),
            capabilities in arb_capabilities(),
            channel in any::<u32>(),
            previous_session_id in any::<u64>(),
            security_buffer in arb_small_bytes(),
        ) {
            let original = SessionSetupRequest {
                flags: SessionSetupRequestFlags(flags_raw),
                security_mode: SecurityMode::new(security_mode_raw as u16),
                capabilities,
                channel,
                previous_session_id,
                security_buffer,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = SessionSetupRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn session_setup_response_pack_unpack(
            session_flags_raw in any::<u16>(),
            security_buffer in arb_small_bytes(),
        ) {
            let original = SessionSetupResponse {
                session_flags: SessionFlags(session_flags_raw),
                security_buffer,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = SessionSetupResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
