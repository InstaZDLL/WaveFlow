//! SMB2 CREATE request and response (spec sections 2.2.13, 2.2.14).
//!
//! The CREATE request opens or creates a file, named pipe, or printer.
//! The response carries the file handle ([`FileId`]) plus timestamps,
//! attributes, and optional create contexts.

use crate::error::Result;
use crate::msg::header::Header;
use crate::pack::{FileTime, Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::flags::FileAccessMask;
use crate::types::{FileId, OplockLevel};
use crate::Error;

// ── Enums ────────────────────────────────────────────────────────────────

/// Impersonation level (MS-SMB2 2.2.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ImpersonationLevel {
    /// Anonymous impersonation.
    Anonymous = 0,
    /// Identification impersonation.
    Identification = 1,
    /// Impersonation level.
    Impersonation = 2,
    /// Delegate impersonation.
    Delegate = 3,
}

impl TryFrom<u32> for ImpersonationLevel {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::Anonymous),
            1 => Ok(Self::Identification),
            2 => Ok(Self::Impersonation),
            3 => Ok(Self::Delegate),
            _ => Err(Error::invalid_data(format!(
                "invalid ImpersonationLevel: {}",
                value
            ))),
        }
    }
}

/// Share access flags (MS-SMB2 2.2.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShareAccess(pub u32);

impl ShareAccess {
    /// Allow other opens to read the file.
    pub const FILE_SHARE_READ: u32 = 0x0000_0001;
    /// Allow other opens to write the file.
    pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    /// Allow other opens to delete the file.
    pub const FILE_SHARE_DELETE: u32 = 0x0000_0004;
}

/// Create disposition (MS-SMB2 2.2.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CreateDisposition {
    /// If the file exists, supersede it. Otherwise, create.
    FileSupersede = 0,
    /// If the file exists, open it. Otherwise, fail.
    FileOpen = 1,
    /// If the file exists, fail. Otherwise, create.
    FileCreate = 2,
    /// If the file exists, open it. Otherwise, create.
    FileOpenIf = 3,
    /// If the file exists, overwrite it. Otherwise, fail.
    FileOverwrite = 4,
    /// If the file exists, overwrite it. Otherwise, create.
    FileOverwriteIf = 5,
}

impl TryFrom<u32> for CreateDisposition {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::FileSupersede),
            1 => Ok(Self::FileOpen),
            2 => Ok(Self::FileCreate),
            3 => Ok(Self::FileOpenIf),
            4 => Ok(Self::FileOverwrite),
            5 => Ok(Self::FileOverwriteIf),
            _ => Err(Error::invalid_data(format!(
                "invalid CreateDisposition: {}",
                value
            ))),
        }
    }
}

/// Create action returned in the response (MS-SMB2 2.2.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CreateAction {
    /// An existing file was superseded.
    FileSuperseded = 0,
    /// An existing file was opened.
    FileOpened = 1,
    /// A new file was created.
    FileCreated = 2,
    /// An existing file was overwritten.
    FileOverwritten = 3,
}

impl TryFrom<u32> for CreateAction {
    type Error = Error;

    fn try_from(value: u32) -> Result<Self> {
        match value {
            0 => Ok(Self::FileSuperseded),
            1 => Ok(Self::FileOpened),
            2 => Ok(Self::FileCreated),
            3 => Ok(Self::FileOverwritten),
            _ => Err(Error::invalid_data(format!(
                "invalid CreateAction: {}",
                value
            ))),
        }
    }
}

// ── CreateRequest ────────────────────────────────────────────────────────

/// SMB2 CREATE request (spec section 2.2.13).
///
/// Sent by the client to open or create a file on the server.
/// The buffer contains the filename encoded as UTF-16LE, optionally
/// followed by create context data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    /// Requested oplock level.
    pub requested_oplock_level: OplockLevel,
    /// Impersonation level.
    pub impersonation_level: ImpersonationLevel,
    /// Desired access rights.
    pub desired_access: FileAccessMask,
    /// File attributes for create/open.
    pub file_attributes: u32,
    /// Sharing mode.
    pub share_access: ShareAccess,
    /// Disposition: what to do if file exists/does not exist.
    pub create_disposition: CreateDisposition,
    /// Create options flags.
    pub create_options: u32,
    /// The filename to create or open.
    pub name: String,
    /// Raw create context bytes (unparsed).
    pub create_contexts: Vec<u8>,
}

impl CreateRequest {
    pub const STRUCTURE_SIZE: u16 = 57;
}

impl Pack for CreateRequest {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // SecurityFlags (1 byte) -- must be 0
        cursor.write_u8(0);
        // RequestedOplockLevel (1 byte)
        cursor.write_u8(self.requested_oplock_level as u8);
        // ImpersonationLevel (4 bytes)
        cursor.write_u32_le(self.impersonation_level as u32);
        // SmbCreateFlags (8 bytes) -- must be 0
        cursor.write_u64_le(0);
        // Reserved (8 bytes)
        cursor.write_u64_le(0);
        // DesiredAccess (4 bytes)
        cursor.write_u32_le(self.desired_access.bits());
        // FileAttributes (4 bytes)
        cursor.write_u32_le(self.file_attributes);
        // ShareAccess (4 bytes)
        cursor.write_u32_le(self.share_access.0);
        // CreateDisposition (4 bytes)
        cursor.write_u32_le(self.create_disposition as u32);
        // CreateOptions (4 bytes)
        cursor.write_u32_le(self.create_options);

        // NameOffset (2 bytes) -- placeholder, backpatch later
        let name_offset_pos = cursor.position();
        cursor.write_u16_le(0);
        // NameLength (2 bytes) -- placeholder, backpatch later
        let name_length_pos = cursor.position();
        cursor.write_u16_le(0);
        // CreateContextsOffset (4 bytes) -- placeholder
        let ctx_offset_pos = cursor.position();
        cursor.write_u32_le(0);
        // CreateContextsLength (4 bytes) -- placeholder
        let ctx_length_pos = cursor.position();
        cursor.write_u32_le(0);

        // Buffer: filename in UTF-16LE
        // Offsets are from the beginning of the SMB2 header per spec.
        let name_offset = Header::SIZE + (cursor.position() - start);
        let name_start = cursor.position();
        cursor.write_utf16_le(&self.name);
        let name_byte_len = cursor.position() - name_start;

        // Backpatch name offset and length
        cursor.set_u16_le_at(name_offset_pos, name_offset as u16);
        cursor.set_u16_le_at(name_length_pos, name_byte_len as u16);

        // Create contexts (if any)
        if !self.create_contexts.is_empty() {
            // Align to 8-byte boundary before create contexts
            cursor.align_to(8);
            let ctx_offset = Header::SIZE + (cursor.position() - start);
            cursor.write_bytes(&self.create_contexts);
            let ctx_len = self.create_contexts.len();

            cursor.set_u32_le_at(ctx_offset_pos, ctx_offset as u32);
            cursor.set_u32_le_at(ctx_length_pos, ctx_len as u32);
        } else if name_byte_len == 0 {
            // Per spec, buffer must be at least 1 byte even if name is empty
            cursor.write_u8(0);
        }
    }
}

impl Unpack for CreateRequest {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid CreateRequest structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // SecurityFlags (1 byte)
        let _security_flags = cursor.read_u8()?;
        // RequestedOplockLevel (1 byte)
        let oplock_raw = cursor.read_u8()?;
        let requested_oplock_level = OplockLevel::try_from(oplock_raw)?;
        // ImpersonationLevel (4 bytes)
        let imp_raw = cursor.read_u32_le()?;
        let impersonation_level = ImpersonationLevel::try_from(imp_raw)?;
        // SmbCreateFlags (8 bytes)
        let _smb_create_flags = cursor.read_u64_le()?;
        // Reserved (8 bytes)
        let _reserved = cursor.read_u64_le()?;
        // DesiredAccess (4 bytes)
        let desired_access = FileAccessMask::new(cursor.read_u32_le()?);
        // FileAttributes (4 bytes)
        let file_attributes = cursor.read_u32_le()?;
        // ShareAccess (4 bytes)
        let share_access = ShareAccess(cursor.read_u32_le()?);
        // CreateDisposition (4 bytes)
        let disp_raw = cursor.read_u32_le()?;
        let create_disposition = CreateDisposition::try_from(disp_raw)?;
        // CreateOptions (4 bytes)
        let create_options = cursor.read_u32_le()?;
        // NameOffset (2 bytes)
        let name_offset = cursor.read_u16_le()? as usize;
        // NameLength (2 bytes)
        let name_length = cursor.read_u16_le()? as usize;
        // CreateContextsOffset (4 bytes)
        let ctx_offset = cursor.read_u32_le()? as usize;
        // CreateContextsLength (4 bytes)
        let ctx_length = cursor.read_u32_le()? as usize;

        // Read filename
        // Offsets on the wire are from the beginning of the SMB2 header,
        // so subtract Header::SIZE to get position within the body.
        let name = if name_length > 0 {
            let current = cursor.position();
            let body_offset = name_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_utf16_le(name_length)?
        } else {
            String::new()
        };

        // Read create contexts
        let create_contexts = if ctx_length > 0 {
            let current = cursor.position();
            let body_offset = ctx_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_bytes_bounded(ctx_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(CreateRequest {
            requested_oplock_level,
            impersonation_level,
            desired_access,
            file_attributes,
            share_access,
            create_disposition,
            create_options,
            name,
            create_contexts,
        })
    }
}

// ── CreateResponse ───────────────────────────────────────────────────────

/// SMB2 CREATE response (spec section 2.2.14).
///
/// Returned by the server with the file handle and metadata about
/// the created or opened file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateResponse {
    /// Oplock level granted by the server.
    pub oplock_level: OplockLevel,
    /// Flags (SMB 3.x only).
    pub flags: u8,
    /// Action taken by the server (opened, created, etc.).
    pub create_action: CreateAction,
    /// Time the file was created.
    pub creation_time: FileTime,
    /// Time the file was last accessed.
    pub last_access_time: FileTime,
    /// Time the file was last written.
    pub last_write_time: FileTime,
    /// Time the file metadata was last changed.
    pub change_time: FileTime,
    /// Allocation size of the file in bytes.
    pub allocation_size: u64,
    /// End-of-file position (actual file size in bytes).
    pub end_of_file: u64,
    /// File attributes.
    pub file_attributes: u32,
    /// The file handle.
    pub file_id: FileId,
    /// Raw create context bytes from the response.
    pub create_contexts: Vec<u8>,
}

impl CreateResponse {
    pub const STRUCTURE_SIZE: u16 = 89;
}

impl Pack for CreateResponse {
    fn pack(&self, cursor: &mut WriteCursor) {
        let start = cursor.position();

        // StructureSize (2 bytes)
        cursor.write_u16_le(Self::STRUCTURE_SIZE);
        // OplockLevel (1 byte)
        cursor.write_u8(self.oplock_level as u8);
        // Flags (1 byte)
        cursor.write_u8(self.flags);
        // CreateAction (4 bytes)
        cursor.write_u32_le(self.create_action as u32);
        // CreationTime (8 bytes)
        self.creation_time.pack(cursor);
        // LastAccessTime (8 bytes)
        self.last_access_time.pack(cursor);
        // LastWriteTime (8 bytes)
        self.last_write_time.pack(cursor);
        // ChangeTime (8 bytes)
        self.change_time.pack(cursor);
        // AllocationSize (8 bytes)
        cursor.write_u64_le(self.allocation_size);
        // EndOfFile (8 bytes)
        cursor.write_u64_le(self.end_of_file);
        // FileAttributes (4 bytes)
        cursor.write_u32_le(self.file_attributes);
        // Reserved2 (4 bytes)
        cursor.write_u32_le(0);
        // FileId (16 bytes = persistent u64 + volatile u64)
        cursor.write_u64_le(self.file_id.persistent);
        cursor.write_u64_le(self.file_id.volatile);
        // CreateContextsOffset (4 bytes) -- placeholder
        let ctx_offset_pos = cursor.position();
        cursor.write_u32_le(0);
        // CreateContextsLength (4 bytes) -- placeholder
        let ctx_length_pos = cursor.position();
        cursor.write_u32_le(0);

        // Create contexts (if any)
        if !self.create_contexts.is_empty() {
            cursor.align_to(8);
            let ctx_offset = Header::SIZE + (cursor.position() - start);
            cursor.write_bytes(&self.create_contexts);
            let ctx_len = self.create_contexts.len();

            cursor.set_u32_le_at(ctx_offset_pos, ctx_offset as u32);
            cursor.set_u32_le_at(ctx_length_pos, ctx_len as u32);
        }
    }
}

impl Unpack for CreateResponse {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let start = cursor.position();

        // StructureSize (2 bytes)
        let structure_size = cursor.read_u16_le()?;
        if structure_size != Self::STRUCTURE_SIZE {
            return Err(Error::invalid_data(format!(
                "invalid CreateResponse structure size: expected {}, got {}",
                Self::STRUCTURE_SIZE,
                structure_size
            )));
        }

        // OplockLevel (1 byte)
        let oplock_level = OplockLevel::try_from(cursor.read_u8()?)?;
        // Flags (1 byte)
        let flags = cursor.read_u8()?;
        // CreateAction (4 bytes)
        let create_action = CreateAction::try_from(cursor.read_u32_le()?)?;
        // CreationTime (8 bytes)
        let creation_time = FileTime::unpack(cursor)?;
        // LastAccessTime (8 bytes)
        let last_access_time = FileTime::unpack(cursor)?;
        // LastWriteTime (8 bytes)
        let last_write_time = FileTime::unpack(cursor)?;
        // ChangeTime (8 bytes)
        let change_time = FileTime::unpack(cursor)?;
        // AllocationSize (8 bytes)
        let allocation_size = cursor.read_u64_le()?;
        // EndOfFile (8 bytes)
        let end_of_file = cursor.read_u64_le()?;
        // FileAttributes (4 bytes)
        let file_attributes = cursor.read_u32_le()?;
        // Reserved2 (4 bytes)
        let _reserved2 = cursor.read_u32_le()?;
        // FileId (16 bytes)
        let persistent = cursor.read_u64_le()?;
        let volatile = cursor.read_u64_le()?;
        let file_id = FileId {
            persistent,
            volatile,
        };
        // CreateContextsOffset (4 bytes)
        let ctx_offset = cursor.read_u32_le()? as usize;
        // CreateContextsLength (4 bytes)
        let ctx_length = cursor.read_u32_le()? as usize;

        // Read create contexts
        // Offset on the wire is from beginning of SMB2 header.
        let create_contexts = if ctx_length > 0 {
            let current = cursor.position();
            let body_offset = ctx_offset.saturating_sub(Header::SIZE);
            let target = start + body_offset;
            if target > current {
                cursor.skip(target - current)?;
            }
            cursor.read_bytes_bounded(ctx_length)?.to_vec()
        } else {
            Vec::new()
        };

        Ok(CreateResponse {
            oplock_level,
            flags,
            create_action,
            creation_time,
            last_access_time,
            last_write_time,
            change_time,
            allocation_size,
            end_of_file,
            file_attributes,
            file_id,
            create_contexts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CreateRequest tests ──────────────────────────────────────────

    #[test]
    fn create_request_roundtrip_no_contexts() {
        let original = CreateRequest {
            requested_oplock_level: OplockLevel::Exclusive,
            impersonation_level: ImpersonationLevel::Impersonation,
            desired_access: FileAccessMask::new(
                FileAccessMask::GENERIC_READ | FileAccessMask::FILE_READ_ATTRIBUTES,
            ),
            file_attributes: 0x80, // FILE_ATTRIBUTE_NORMAL
            share_access: ShareAccess(ShareAccess::FILE_SHARE_READ | ShareAccess::FILE_SHARE_WRITE),
            create_disposition: CreateDisposition::FileOpenIf,
            create_options: 0,
            name: "test\\file.txt".to_string(),
            create_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = CreateRequest::unpack(&mut r).unwrap();

        assert_eq!(
            decoded.requested_oplock_level,
            original.requested_oplock_level
        );
        assert_eq!(decoded.impersonation_level, original.impersonation_level);
        assert_eq!(decoded.desired_access, original.desired_access);
        assert_eq!(decoded.file_attributes, original.file_attributes);
        assert_eq!(decoded.share_access, original.share_access);
        assert_eq!(decoded.create_disposition, original.create_disposition);
        assert_eq!(decoded.create_options, original.create_options);
        assert_eq!(decoded.name, original.name);
        assert!(decoded.create_contexts.is_empty());
    }

    #[test]
    fn create_request_roundtrip_with_create_contexts() {
        // Simulate a raw create context blob (for example, a
        // SMB2_CREATE_QUERY_MAXIMAL_ACCESS_REQUEST context).
        let fake_ctx = vec![
            0x00, 0x00, 0x00, 0x00, // NextEntryOffset = 0 (last entry)
            0x10, 0x00, // NameOffset = 16
            0x04, 0x00, // NameLength = 4
            0x00, 0x00, // Reserved
            0x18, 0x00, // DataOffset = 24
            0x04, 0x00, 0x00, 0x00, // DataLength = 4
            b'M', b'x', b'A', b'c', // Name = "MxAc"
            0x00, 0x00, 0x00, 0x00, // padding
            0x01, 0x02, 0x03, 0x04, // Data (4 bytes)
        ];

        let original = CreateRequest {
            requested_oplock_level: OplockLevel::Batch,
            impersonation_level: ImpersonationLevel::Delegate,
            desired_access: FileAccessMask::new(FileAccessMask::GENERIC_ALL),
            file_attributes: 0x20, // FILE_ATTRIBUTE_ARCHIVE
            share_access: ShareAccess(ShareAccess::FILE_SHARE_DELETE),
            create_disposition: CreateDisposition::FileCreate,
            create_options: 0x0000_0040, // FILE_NON_DIRECTORY_FILE
            name: "share\\docs\\report.docx".to_string(),
            create_contexts: fake_ctx.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = CreateRequest::unpack(&mut r).unwrap();

        assert_eq!(decoded.requested_oplock_level, OplockLevel::Batch);
        assert_eq!(decoded.impersonation_level, ImpersonationLevel::Delegate);
        assert_eq!(decoded.name, "share\\docs\\report.docx");
        assert_eq!(decoded.create_contexts, fake_ctx);
    }

    #[test]
    fn create_request_structure_size_field() {
        let req = CreateRequest {
            requested_oplock_level: OplockLevel::None,
            impersonation_level: ImpersonationLevel::Anonymous,
            desired_access: FileAccessMask::default(),
            file_attributes: 0,
            share_access: ShareAccess::default(),
            create_disposition: CreateDisposition::FileOpen,
            create_options: 0,
            name: "x".to_string(),
            create_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();

        // First two bytes are StructureSize = 57
        assert_eq!(bytes[0], 57);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn create_request_wrong_structure_size() {
        let mut buf = vec![0u8; 64];
        // Set wrong structure size
        buf[0..2].copy_from_slice(&99u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = CreateRequest::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── CreateResponse tests ─────────────────────────────────────────

    #[test]
    fn create_response_roundtrip() {
        let original = CreateResponse {
            oplock_level: OplockLevel::LevelII,
            flags: 0,
            create_action: CreateAction::FileOpened,
            creation_time: FileTime(133_485_408_000_000_000),
            last_access_time: FileTime(133_485_408_100_000_000),
            last_write_time: FileTime(133_485_408_200_000_000),
            change_time: FileTime(133_485_408_300_000_000),
            allocation_size: 4096,
            end_of_file: 1234,
            file_attributes: 0x20, // FILE_ATTRIBUTE_ARCHIVE
            file_id: FileId {
                persistent: 0x1111_2222_3333_4444,
                volatile: 0x5555_6666_7777_8888,
            },
            create_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = CreateResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.oplock_level, original.oplock_level);
        assert_eq!(decoded.flags, original.flags);
        assert_eq!(decoded.create_action, original.create_action);
        assert_eq!(decoded.creation_time, original.creation_time);
        assert_eq!(decoded.last_access_time, original.last_access_time);
        assert_eq!(decoded.last_write_time, original.last_write_time);
        assert_eq!(decoded.change_time, original.change_time);
        assert_eq!(decoded.allocation_size, original.allocation_size);
        assert_eq!(decoded.end_of_file, original.end_of_file);
        assert_eq!(decoded.file_attributes, original.file_attributes);
        assert_eq!(decoded.file_id, original.file_id);
        assert!(decoded.create_contexts.is_empty());
    }

    #[test]
    fn create_response_with_contexts() {
        let ctx_data = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let original = CreateResponse {
            oplock_level: OplockLevel::None,
            flags: 0x01,
            create_action: CreateAction::FileCreated,
            creation_time: FileTime(100),
            last_access_time: FileTime(200),
            last_write_time: FileTime(300),
            change_time: FileTime(400),
            allocation_size: 0,
            end_of_file: 0,
            file_attributes: 0,
            file_id: FileId {
                persistent: 1,
                volatile: 2,
            },
            create_contexts: ctx_data.clone(),
        };

        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = CreateResponse::unpack(&mut r).unwrap();

        assert_eq!(decoded.create_action, CreateAction::FileCreated);
        assert_eq!(decoded.file_id.persistent, 1);
        assert_eq!(decoded.file_id.volatile, 2);
        assert_eq!(decoded.create_contexts, ctx_data);
    }

    #[test]
    fn create_response_structure_size_field() {
        let resp = CreateResponse {
            oplock_level: OplockLevel::None,
            flags: 0,
            create_action: CreateAction::FileOpened,
            creation_time: FileTime::ZERO,
            last_access_time: FileTime::ZERO,
            last_write_time: FileTime::ZERO,
            change_time: FileTime::ZERO,
            allocation_size: 0,
            end_of_file: 0,
            file_attributes: 0,
            file_id: FileId::default(),
            create_contexts: Vec::new(),
        };

        let mut w = WriteCursor::new();
        resp.pack(&mut w);
        let bytes = w.into_inner();

        // First two bytes are StructureSize = 89
        assert_eq!(bytes[0], 89);
        assert_eq!(bytes[1], 0);
    }

    #[test]
    fn create_response_wrong_structure_size() {
        let mut buf = vec![0u8; 96];
        buf[0..2].copy_from_slice(&42u16.to_le_bytes());
        let mut cursor = ReadCursor::new(&buf);
        let result = CreateResponse::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("structure size"), "error was: {err}");
    }

    // ── Enum conversion tests ────────────────────────────────────────

    #[test]
    fn oplock_level_roundtrip() {
        for &level in &[
            OplockLevel::None,
            OplockLevel::LevelII,
            OplockLevel::Exclusive,
            OplockLevel::Batch,
            OplockLevel::Lease,
        ] {
            let raw = level as u8;
            let decoded = OplockLevel::try_from(raw).unwrap();
            assert_eq!(decoded, level);
        }
    }

    #[test]
    fn oplock_level_invalid() {
        assert!(OplockLevel::try_from(0x42).is_err());
    }

    #[test]
    fn impersonation_level_roundtrip() {
        for &level in &[
            ImpersonationLevel::Anonymous,
            ImpersonationLevel::Identification,
            ImpersonationLevel::Impersonation,
            ImpersonationLevel::Delegate,
        ] {
            let raw = level as u32;
            let decoded = ImpersonationLevel::try_from(raw).unwrap();
            assert_eq!(decoded, level);
        }
    }

    #[test]
    fn create_disposition_roundtrip() {
        for &disp in &[
            CreateDisposition::FileSupersede,
            CreateDisposition::FileOpen,
            CreateDisposition::FileCreate,
            CreateDisposition::FileOpenIf,
            CreateDisposition::FileOverwrite,
            CreateDisposition::FileOverwriteIf,
        ] {
            let raw = disp as u32;
            let decoded = CreateDisposition::try_from(raw).unwrap();
            assert_eq!(decoded, disp);
        }
    }

    #[test]
    fn create_action_roundtrip() {
        for &action in &[
            CreateAction::FileSuperseded,
            CreateAction::FileOpened,
            CreateAction::FileCreated,
            CreateAction::FileOverwritten,
        ] {
            let raw = action as u32;
            let decoded = CreateAction::try_from(raw).unwrap();
            assert_eq!(decoded, action);
        }
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::{
        arb_create_action, arb_create_disposition, arb_file_access_mask, arb_file_id,
        arb_file_time, arb_impersonation_level, arb_oplock_level, arb_share_access,
        arb_small_bytes, arb_utf16_string,
    };
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn create_request_pack_unpack(
            requested_oplock_level in arb_oplock_level(),
            impersonation_level in arb_impersonation_level(),
            desired_access in arb_file_access_mask(),
            file_attributes in any::<u32>(),
            share_access in arb_share_access(),
            create_disposition in arb_create_disposition(),
            create_options in any::<u32>(),
            name in arb_utf16_string(128),
            create_contexts in arb_small_bytes(),
        ) {
            let original = CreateRequest {
                requested_oplock_level,
                impersonation_level,
                desired_access,
                file_attributes,
                share_access,
                create_disposition,
                create_options,
                name,
                create_contexts,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = CreateRequest::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            // Note: pack may write a trailing 1-byte pad when name is empty
            // and there are no create contexts. Unpack only advances through
            // fields it reads, so the cursor may have 1 trailing byte in
            // that corner case. That's fine for symmetry on struct contents.
        }

        #[test]
        fn create_response_pack_unpack(
            oplock_level in arb_oplock_level(),
            flags in any::<u8>(),
            create_action in arb_create_action(),
            creation_time in arb_file_time(),
            last_access_time in arb_file_time(),
            last_write_time in arb_file_time(),
            change_time in arb_file_time(),
            allocation_size in any::<u64>(),
            end_of_file in any::<u64>(),
            file_attributes in any::<u32>(),
            file_id in arb_file_id(),
            create_contexts in arb_small_bytes(),
        ) {
            let original = CreateResponse {
                oplock_level,
                flags,
                create_action,
                creation_time,
                last_access_time,
                last_write_time,
                change_time,
                allocation_size,
                end_of_file,
                file_attributes,
                file_id,
                create_contexts,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = CreateResponse::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
        }
    }
}
