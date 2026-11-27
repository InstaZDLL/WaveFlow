//! Ranged reads, as binary frames.
//!
//! The same three frames run both ways on a connection: a guest asks the
//! host for an entry it does not own, the host asks an entry's owner for the
//! bytes it relays. Big-endian, fixed headers, nothing optional:
//!
//! ```text
//! read  01 | req u32 | entry 16 bytes | offset u64 | len u32     (33 bytes)
//! data  02 | req u32 | the bytes, at most MAX_READ_LEN
//! fail  03 | req u32 | code u8                                   (6 bytes)
//! ```
//!
//! A frame of any other shape is refused whole ([`FrameError`]); nothing is
//! read past what its header promises.

use super::{Entry, EntryId};

/// Most bytes one read asks for, and one data frame carries.
pub const MAX_READ_LEN: u32 = 1024 * 1024;

const READ: u8 = 1;
const DATA: u8 = 2;
const FAIL: u8 = 3;
const READ_LEN: usize = 1 + 4 + 16 + 8 + 4;
const HEADER_LEN: usize = 1 + 4;
const FAIL_LEN: usize = HEADER_LEN + 1;

/// A request for bytes of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadRequest {
    pub req: u32,
    pub entry: EntryId,
    pub offset: u64,
    pub len: u32,
}

/// Why a read returns no bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadFailure {
    /// Not readable: removed, played, or never in the queue.
    #[error("the entry is not readable")]
    Gone,
    /// The owner's file is no longer the one announced (another size, or
    /// rewritten since it was hashed).
    #[error("the file changed")]
    Changed,
    /// The owner cannot answer: away, or its disk failed.
    #[error("the bytes are unavailable")]
    Unavailable,
    /// The offset is past the end of the file.
    #[error("the offset is past the end")]
    OutOfRange,
}

impl ReadFailure {
    fn code(self) -> u8 {
        match self {
            Self::Gone => 1,
            Self::Changed => 2,
            Self::Unavailable => 3,
            Self::OutOfRange => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::Gone,
            2 => Self::Changed,
            3 => Self::Unavailable,
            4 => Self::OutOfRange,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Read(ReadRequest),
    Data { req: u32, bytes: Vec<u8> },
    Fail { req: u32, failure: ReadFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("empty frame")]
    Empty,
    #[error("unknown frame kind")]
    UnknownKind,
    #[error("the frame is not the length its kind has")]
    BadLength,
    #[error("a read of 0 bytes, or of more than the maximum")]
    BadReadLength,
    #[error("unknown failure code")]
    UnknownFailure,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Read(r) => {
                let mut out = Vec::with_capacity(READ_LEN);
                out.push(READ);
                out.extend_from_slice(&r.req.to_be_bytes());
                out.extend_from_slice(r.entry.as_bytes());
                out.extend_from_slice(&r.offset.to_be_bytes());
                out.extend_from_slice(&r.len.to_be_bytes());
                out
            }
            Frame::Data { req, bytes } => {
                let mut out = Vec::with_capacity(HEADER_LEN + bytes.len());
                out.push(DATA);
                out.extend_from_slice(&req.to_be_bytes());
                out.extend_from_slice(bytes);
                out
            }
            Frame::Fail { req, failure } => {
                let mut out = Vec::with_capacity(FAIL_LEN);
                out.push(FAIL);
                out.extend_from_slice(&req.to_be_bytes());
                out.push(failure.code());
                out
            }
        }
    }

    pub fn decode(frame: &[u8]) -> Result<Self, FrameError> {
        let (&kind, rest) = frame.split_first().ok_or(FrameError::Empty)?;
        let req = |rest: &[u8]| -> Result<u32, FrameError> {
            let bytes: [u8; 4] = rest
                .get(..4)
                .and_then(|b| b.try_into().ok())
                .ok_or(FrameError::BadLength)?;
            Ok(u32::from_be_bytes(bytes))
        };
        match kind {
            READ => {
                if frame.len() != READ_LEN {
                    return Err(FrameError::BadLength);
                }
                let req = req(rest)?;
                let mut entry = [0u8; 16];
                entry.copy_from_slice(&rest[4..20]);
                let offset =
                    u64::from_be_bytes(rest[20..28].try_into().map_err(|_| FrameError::BadLength)?);
                let len =
                    u32::from_be_bytes(rest[28..32].try_into().map_err(|_| FrameError::BadLength)?);
                if len == 0 || len > MAX_READ_LEN {
                    return Err(FrameError::BadReadLength);
                }
                Ok(Frame::Read(ReadRequest {
                    req,
                    entry: EntryId::from_bytes(entry),
                    offset,
                    len,
                }))
            }
            DATA => {
                if frame.len() < HEADER_LEN || frame.len() - HEADER_LEN > MAX_READ_LEN as usize {
                    return Err(FrameError::BadLength);
                }
                Ok(Frame::Data {
                    req: req(rest)?,
                    bytes: rest[4..].to_vec(),
                })
            }
            FAIL => {
                if frame.len() != FAIL_LEN {
                    return Err(FrameError::BadLength);
                }
                Ok(Frame::Fail {
                    req: req(rest)?,
                    failure: ReadFailure::from_code(rest[4]).ok_or(FrameError::UnknownFailure)?,
                })
            }
            _ => Err(FrameError::UnknownKind),
        }
    }
}

impl ReadRequest {
    /// How many bytes to read for this request from `entry`'s file, which the
    /// caller found readable: `len`, cut at the end of the file. A read that
    /// starts exactly at the end returns no bytes, as a file read does; one
    /// past it is refused.
    pub fn span(&self, entry: &Entry) -> Result<u64, ReadFailure> {
        if self.offset > entry.size {
            return Err(ReadFailure::OutOfRange);
        }
        Ok(u64::from(self.len).min(entry.size - self.offset))
    }
}
