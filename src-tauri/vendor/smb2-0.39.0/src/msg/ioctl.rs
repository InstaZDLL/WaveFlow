//! SMB2 IOCTL Request and Response (MS-SMB2 sections 2.2.31, 2.2.32).
//!
//! The IOCTL request sends a control code to a server, optionally with input
//! data. The response returns output data from the control operation.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::FileId;
use crate::Error;

// ── IOCTL flags ────────────────────────────────────────────────────────

/// The request is a file system control (FSCTL) request.
pub const SMB2_0_IOCTL_IS_FSCTL: u32 = 0x0000_0001;

// ── Common CtlCode values ──────────────────────────────────────────────

/// Named pipe transceive operation.
pub const FSCTL_PIPE_TRANSCEIVE: u32 = 0x0011_C017;

/// Server-side copy chunk (read handle).
pub const FSCTL_SRV_COPYCHUNK: u32 = 0x0014_40F2;

/// Server-side copy chunk (write handle).
pub const FSCTL_SRV_COPYCHUNK_WRITE: u32 = 0x0014_80F2;

/// Request an opaque resume key for a server-side copy source handle.
pub const FSCTL_SRV_REQUEST_RESUME_KEY: u32 = 0x0014_0078;

/// DFS referral request.
pub const FSCTL_DFS_GET_REFERRALS: u32 = 0x0006_0194;

/// Validate negotiate info (SMB 3.x).
pub const FSCTL_VALIDATE_NEGOTIATE_INFO: u32 = 0x0014_0204;

// ── IoctlRequest ───────────────────────────────────────────────────────

/// SMB2 IOCTL Request (MS-SMB2 section 2.2.31).
///
/// Sent by the client to issue a device or file system control command.
/// The fixed part is 56 bytes (StructureSize = 57 indicates 1 byte of
/// variable data is included in the fixed size, per SMB2 convention).
///
/// Layout:
/// - StructureSize (2 bytes, must be 57)
/// - Reserved (2 bytes)
/// - CtlCode (4 bytes)
/// - FileId (16 bytes)
/// - InputOffset (4 bytes)
/// - InputCount (4 bytes)
/// - MaxInputResponse (4 bytes)
/// - OutputOffset (4 bytes)
/// - OutputCount (4 bytes)
/// - MaxOutputResponse (4 bytes)
/// - Flags (4 bytes)
/// - Reserved2 (4 bytes)
/// - Buffer (variable, InputCount bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoctlRequest {
    /// The control code for the operation.
    pub ctl_code: u32,
    /// The file handle for the operation.
    pub file_id: FileId,
    /// Maximum number of input bytes the server can return.
    pub max_input_response: u32,
    /// Maximum number of output bytes the server can return.
    pub max_output_response: u32,
    /// Flags for the request (for example, `SMB2_0_IOCTL_IS_FSCTL`).
    pub flags: u32,
    /// Input data buffer.
    pub input_data: Vec<u8>,
}

impl IoctlRequest {
    pub const STRUCTURE_SIZE: u16 = 57;

    /// Fixed header size before the variable buffer (56 bytes).
    const FIXED_SIZE: u32 = 56;
}

impl Pack for IoctlRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Reserved (2 bytes)
        cursor.write_u16_le(0);
        // CtlCode (4 bytes)
        cursor.write_u32_le(self.ctl_code);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);

        let input_count = self.input_data.len() as u32;
        // Offset is from the beginning of the SMB2 header per spec.
        // `start` is the cursor position at the beginning of the body;
        // in a standalone request this equals Header::SIZE, in a compound
        // it includes the preceding sub-requests.
        let input_offset = if input_count > 0 {
            (start as u32) + Self::FIXED_SIZE
        } else {
            0
        };

        // InputOffset (4 bytes)
        cursor.write_u32_le(input_offset);
        // InputCount (4 bytes)
        cursor.write_u32_le(input_count);
        // MaxInputResponse (4 bytes)
        cursor.write_u32_le(self.max_input_response);
        // OutputOffset (4 bytes) -- no output data in the request
        cursor.write_u32_le(0);
        // OutputCount (4 bytes) -- no output data in the request
        cursor.write_u32_le(0);
        // MaxOutputResponse (4 bytes)
        cursor.write_u32_le(self.max_output_response);
        // Flags (4 bytes)
        cursor.write_u32_le(self.flags);
        // Reserved2 (4 bytes)
        cursor.write_u32_le(0);
        // Buffer (variable)
        cursor.write_bytes(&self.input_data);
    }
}

impl Unpack for IoctlRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid IoctlRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let _reserved = cursor.read_u16_le()?;
        let ctl_code = cursor.read_u32_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let _input_offset = cursor.read_u32_le()?;
        let input_count = cursor.read_u32_le()?;
        let max_input_response = cursor.read_u32_le()?;
        let _output_offset = cursor.read_u32_le()?;
        let _output_count = cursor.read_u32_le()?;
        let max_output_response = cursor.read_u32_le()?;
        let flags = cursor.read_u32_le()?;
        let _reserved2 = cursor.read_u32_le()?;

        let input_data = if input_count > 0 {
            cursor.read_bytes_bounded(input_count as usize)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(IoctlRequest {
            ctl_code,
            file_id: FileId {
                persistent,
                volatile,
            },
            max_input_response,
            max_output_response,
            flags,
            input_data,
        })
    }
}

// ── IoctlResponse ──────────────────────────────────────────────────────

/// SMB2 IOCTL Response (MS-SMB2 section 2.2.32).
///
/// Sent by the server to return the results of an IOCTL operation.
///
/// Layout:
/// - StructureSize (2 bytes, must be 49)
/// - Reserved (2 bytes)
/// - CtlCode (4 bytes)
/// - FileId (16 bytes)
/// - InputOffset (4 bytes)
/// - InputCount (4 bytes)
/// - OutputOffset (4 bytes)
/// - OutputCount (4 bytes)
/// - Flags (4 bytes)
/// - Reserved2 (4 bytes)
/// - Buffer (variable -- may contain both input and output data)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoctlResponse {
    /// The control code echoed from the request.
    pub ctl_code: u32,
    /// The file handle echoed from the request.
    pub file_id: FileId,
    /// Flags echoed from the request.
    pub flags: u32,
    /// Output data buffer returned by the server.
    pub output_data: Vec<u8>,
}

impl IoctlResponse {
    pub const STRUCTURE_SIZE: u16 = 49;

    /// Fixed header size before the variable buffer (48 bytes).
    const FIXED_SIZE: u32 = 48;
}

impl Pack for IoctlResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();
        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // Reserved (2 bytes)
        cursor.write_u16_le(0);
        // CtlCode (4 bytes)
        cursor.write_u32_le(self.ctl_code);
        // FileId (16 bytes)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);

        let output_count = self.output_data.len() as u32;
        // Offset is from the beginning of the SMB2 header per spec.
        let output_offset = if output_count > 0 {
            (start as u32) + Self::FIXED_SIZE
        } else {
            0
        };

        // InputOffset (4 bytes) -- no input data in the response
        cursor.write_u32_le(0);
        // InputCount (4 bytes)
        cursor.write_u32_le(0);
        // OutputOffset (4 bytes)
        cursor.write_u32_le(output_offset);
        // OutputCount (4 bytes)
        cursor.write_u32_le(output_count);
        // Flags (4 bytes)
        cursor.write_u32_le(self.flags);
        // Reserved2 (4 bytes)
        cursor.write_u32_le(0);
        // Buffer (variable)
        cursor.write_bytes(&self.output_data);
    }
}

impl Unpack for IoctlResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid IoctlResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        let _reserved = cursor.read_u16_le()?;
        let ctl_code = cursor.read_u32_le()?;
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let _input_offset = cursor.read_u32_le()?;
        let _input_count = cursor.read_u32_le()?;
        let _output_offset = cursor.read_u32_le()?;
        let output_count = cursor.read_u32_le()?;
        let flags = cursor.read_u32_le()?;
        let _reserved2 = cursor.read_u32_le()?;

        let output_data = if output_count > 0 {
            cursor.read_bytes_bounded(output_count as usize)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(IoctlResponse {
            ctl_code,
            file_id: FileId {
                persistent,
                volatile,
            },
            flags,
            output_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── IoctlRequest tests ────────────────────────────────────────────

    #[test]
    fn ioctl_request_roundtrip_with_input_data() {
        let original = IoctlRequest {
            ctl_code: FSCTL_PIPE_TRANSCEIVE,
            file_id: FileId {
                persistent: 0x1122_3344_5566_7788,
                volatile: 0xAABB_CCDD_EEFF_0011,
            },
            max_input_response: 0,
            max_output_response: 4096,
            flags: SMB2_0_IOCTL_IS_FSCTL,
            input_data: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed 56 bytes + 5 bytes input data
        assert_eq!(bytes.len(), 61);

        let mut r = ReadCursor::new(&bytes);
        let decoded = IoctlRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.ctl_code, FSCTL_PIPE_TRANSCEIVE);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.max_input_response, 0);
        assert_eq!(decoded.max_output_response, 4096);
        assert_eq!(decoded.flags, SMB2_0_IOCTL_IS_FSCTL);
        assert_eq!(decoded.input_data, vec![0x01, 0x02, 0x03, 0x04, 0x05]);
    }

    #[test]
    fn ioctl_request_roundtrip_no_input_data() {
        let original = IoctlRequest {
            ctl_code: FSCTL_VALIDATE_NEGOTIATE_INFO,
            file_id: FileId::SENTINEL,
            max_input_response: 0,
            max_output_response: 256,
            flags: SMB2_0_IOCTL_IS_FSCTL,
            input_data: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), 56);

        let mut r = ReadCursor::new(&bytes);
        let decoded = IoctlRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.ctl_code, FSCTL_VALIDATE_NEGOTIATE_INFO);
        assert_eq!(decoded.file_id, FileId::SENTINEL);
        assert!(decoded.input_data.is_empty());
    }

    #[test]
    fn ioctl_request_wrong_structure_size() {
        let mut buf = [0u8; 56];
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = IoctlRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── IoctlResponse tests ───────────────────────────────────────────

    #[test]
    fn ioctl_response_roundtrip_with_output_data() {
        let original = IoctlResponse {
            ctl_code: FSCTL_PIPE_TRANSCEIVE,
            file_id: FileId {
                persistent: 0x42,
                volatile: 0x99,
            },
            flags: SMB2_0_IOCTL_IS_FSCTL,
            output_data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        // Fixed 48 bytes + 4 bytes output data
        assert_eq!(bytes.len(), 52);

        let mut r = ReadCursor::new(&bytes);
        let decoded = IoctlResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.ctl_code, FSCTL_PIPE_TRANSCEIVE);
        assert_eq!(decoded.file_id, original.file_id);
        assert_eq!(decoded.flags, SMB2_0_IOCTL_IS_FSCTL);
        assert_eq!(decoded.output_data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn ioctl_response_roundtrip_no_output_data() {
        let original = IoctlResponse {
            ctl_code: FSCTL_SRV_COPYCHUNK,
            file_id: FileId::default(),
            flags: 0,
            output_data: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        assert_eq!(bytes.len(), 48);

        let mut r = ReadCursor::new(&bytes);
        let decoded = IoctlResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.ctl_code, FSCTL_SRV_COPYCHUNK);
        assert!(decoded.output_data.is_empty());
    }

    #[test]
    fn ioctl_response_wrong_structure_size() {
        let mut buf = [0u8; 48];
        buf[0..2].copy_from_slice(&42u16.to_le_bytes());

        let mut cursor = ReadCursor::new(&buf);
        let result = IoctlResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{arb_bytes, arb_file_id};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn ioctl_request_pack_unpack(
            ctl_code in any::<u32>(),
            file_id in arb_file_id(),
            max_input_response in any::<u32>(),
            max_output_response in any::<u32>(),
            flags in any::<u32>(),
            input_data in arb_bytes(),
        ) {
            let original = IoctlRequest {
                ctl_code,
                file_id,
                max_input_response,
                max_output_response,
                flags,
                input_data,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = IoctlRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }

        #[test]
        fn ioctl_response_pack_unpack(
            ctl_code in any::<u32>(),
            file_id in arb_file_id(),
            flags in any::<u32>(),
            output_data in arb_bytes(),
        ) {
            let original = IoctlResponse {
                ctl_code,
                file_id,
                flags,
                output_data,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = IoctlResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
