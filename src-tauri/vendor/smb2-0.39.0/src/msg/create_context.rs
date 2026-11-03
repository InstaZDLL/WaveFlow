//! SMB2 CREATE contexts (MS-SMB2 § 2.2.13.2 / § 2.2.14.2).
//!
//! A CREATE request and response can carry a chain of typed, name-tagged
//! blobs. [`CreateRequest::create_contexts`](crate::msg::create::CreateRequest)
//! and its response twin hold that chain as raw bytes; this module is the
//! codec, plus the three contexts this crate uses.
//!
//! ## The three, and what each one is for
//!
//! - **`DH2Q`** asks for a *durable* handle: an open the server keeps alive
//!   for a while after the connection carrying it dies, so a client that comes
//!   back can pick the file up where it left off instead of starting the
//!   transfer over.
//! - **`DH2C`** claims one back. It carries the old `FileId` **and** the
//!   `CreateGuid` the client chose at open time, which is the whole reason
//!   this crate will only reclaim v2 handles — see below.
//! - **`QFid`** asks the server for the file's on-disk identity: a volume id
//!   and a file id (an inode, in POSIX terms). It rides on a CREATE we were
//!   sending anyway and costs no round trip.
//!
//! ## Why v2 only, never v1
//!
//! SMB 2.1's durable handles (`DHnQ` / `DHnC`) identify the open by nothing
//! but the `FileId` the *server* allocated. The client contributes no secret,
//! so "this id resolved to an open" is the only evidence a reclaim can offer,
//! and a server that recycles persistent ids across a restart could hand back
//! a different file. Writing bytes into the wrong file is far worse than a
//! failed transfer, so this crate does not implement v1 reclaim at all: on
//! SMB 2.1 a dead session means the transfer restarts.
//!
//! SMB 3.x's v2 contexts add a 16-byte `CreateGuid` the client generates and
//! the server stores with the open (MS-SMB2 § 3.3.5.9.12 makes the server
//! match on it). A successful reclaim therefore proves the server matched
//! *our* open, not merely something at that id.

use crate::error::{Error, Result};
use crate::pack::{Guid, Pack, ReadCursor, WriteCursor};
use crate::types::FileId;

/// `SMB2_CREATE_DURABLE_HANDLE_REQUEST_V2` (MS-SMB2 § 2.2.13.2.11).
pub const NAME_DH2Q: &[u8] = b"DH2Q";
/// `SMB2_CREATE_DURABLE_HANDLE_RECONNECT_V2` (MS-SMB2 § 2.2.13.2.12).
pub const NAME_DH2C: &[u8] = b"DH2C";
/// `SMB2_CREATE_QUERY_ON_DISK_ID` (MS-SMB2 § 2.2.13.2.9 / § 2.2.14.2.9).
pub const NAME_QFID: &[u8] = b"QFid";

/// `SMB2_DHANDLE_FLAG_PERSISTENT` (MS-SMB2 § 2.2.13.2.11).
const FLAG_PERSISTENT: u32 = 0x0000_0002;

/// A single name-tagged blob in a CREATE context chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateContext {
    /// The context name. Four ASCII bytes for the standard ones, 16 for the
    /// GUID-named application contexts.
    pub name: Vec<u8>,
    /// The context payload, whose shape depends on the name.
    pub data: Vec<u8>,
}

impl CreateContext {
    /// Build a context from a name constant and a packed body.
    pub fn new(name: &[u8], data: Vec<u8>) -> Self {
        Self {
            name: name.to_vec(),
            data,
        }
    }
}

/// Serialize a context chain into the bytes a CREATE carries.
///
/// Layout per entry (MS-SMB2 § 2.2.13.2): `Next` (u32, bytes to the next
/// entry, 0 on the last), `NameOffset` (u16), `NameLength` (u16), `Reserved`
/// (u16), `DataOffset` (u16), `DataLength` (u32), then the name and the data,
/// each padded so the next field is 8-byte aligned.
pub fn pack_contexts(contexts: &[CreateContext]) -> Vec<u8> {
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(contexts.len());
    for ctx in contexts {
        let mut cursor = WriteCursor::new();
        let name_offset: u16 = 16;
        let name_end = usize::from(name_offset) + ctx.name.len();
        let data_offset = if ctx.data.is_empty() {
            // A context with no data reports offset 0, not a position past
            // the end: servers read `DataOffset` even when `DataLength` is 0
            // and some reject a dangling one.
            0u16
        } else {
            align8(name_end) as u16
        };

        cursor.write_u32_le(0); // Next, filled in below
        cursor.write_u16_le(name_offset);
        cursor.write_u16_le(ctx.name.len() as u16);
        cursor.write_u16_le(0); // Reserved
        cursor.write_u16_le(data_offset);
        cursor.write_u32_le(ctx.data.len() as u32);
        cursor.write_bytes(&ctx.name);
        let mut bytes = cursor.into_inner();
        if !ctx.data.is_empty() {
            bytes.resize(usize::from(data_offset), 0);
            bytes.extend_from_slice(&ctx.data);
        }
        out.push(bytes);
    }

    let mut chain = Vec::new();
    for (i, mut bytes) in out.into_iter().enumerate() {
        // Every entry is padded to 8, the last one included. The spec only
        // requires it of entries something follows, but Windows pads them all
        // and an aligned buffer is what servers are tested against.
        bytes.resize(align8(bytes.len()), 0);
        if i + 1 != contexts.len() {
            let next = bytes.len() as u32;
            bytes[0..4].copy_from_slice(&next.to_le_bytes());
        }
        chain.extend_from_slice(&bytes);
    }
    chain
}

/// Parse a context chain out of a CREATE request or response.
///
/// Tolerant of contexts this crate does not know: they come back as raw
/// name/data pairs. Intolerant of a chain that points outside itself, which is
/// how a malformed or hostile response would try to walk us off the buffer.
pub fn parse_contexts(mut buf: &[u8]) -> Result<Vec<CreateContext>> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 16 {
            if buf.is_empty() {
                return Ok(out);
            }
            return Err(Error::invalid_data(
                "create context chain ends inside an entry header",
            ));
        }
        let mut cursor = ReadCursor::new(buf);
        let next = cursor.read_u32_le()? as usize;
        let name_offset = cursor.read_u16_le()? as usize;
        let name_length = cursor.read_u16_le()? as usize;
        let _reserved = cursor.read_u16_le()?;
        let data_offset = cursor.read_u16_le()? as usize;
        let data_length = cursor.read_u32_le()? as usize;

        let entry_len = if next == 0 { buf.len() } else { next };
        if next != 0 && next > buf.len() {
            return Err(Error::invalid_data(
                "create context Next points past the end of the chain",
            ));
        }
        let name_end = name_offset
            .checked_add(name_length)
            .ok_or_else(|| Error::invalid_data("create context name overflows"))?;
        if name_end > entry_len {
            return Err(Error::invalid_data(
                "create context name runs past the entry",
            ));
        }
        let data = if data_length == 0 {
            Vec::new()
        } else {
            let data_end = data_offset
                .checked_add(data_length)
                .ok_or_else(|| Error::invalid_data("create context data overflows"))?;
            if data_end > entry_len {
                return Err(Error::invalid_data(
                    "create context data runs past the entry",
                ));
            }
            buf[data_offset..data_end].to_vec()
        };
        out.push(CreateContext {
            name: buf[name_offset..name_end].to_vec(),
            data,
        });

        if next == 0 {
            return Ok(out);
        }
        buf = &buf[next..];
    }
}

/// Find a context by name.
pub fn find<'a>(contexts: &'a [CreateContext], name: &[u8]) -> Option<&'a CreateContext> {
    contexts.iter().find(|c| c.name == name)
}

fn align8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

// ── DH2Q: ask for a durable handle ─────────────────────────────────────────

/// Ask the server to keep this open alive across a disconnect.
///
/// ⚠️ **The server only grants this when the CREATE also requests a batch
/// oplock or a handle-caching lease** (MS-SMB2 § 3.3.5.9.10). Without one the
/// context is silently ignored and the response carries no `DH2Q` reply, which
/// is why [`DurableGrant`] is an `Option` everywhere it appears rather than
/// something a caller may assume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableRequestV2 {
    /// How long, in milliseconds, the client would like the server to hold the
    /// open after the connection drops. `0` means "server's choice", which is
    /// what this crate sends: the server knows its own resource limits and
    /// every implementation clamps the request anyway.
    pub timeout_ms: u32,
    /// Ask for a *persistent* handle, which survives a server restart rather
    /// than only a connection drop. Only meaningful on a share advertising
    /// `SMB2_SHARE_CAP_CONTINUOUS_AVAILABILITY`.
    pub persistent: bool,
    /// The client's proof of ownership, echoed back in a later `DH2C`. Must be
    /// unpredictable and unique per open.
    pub create_guid: Guid,
}

impl DurableRequestV2 {
    /// Pack into the 32-byte context body.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut c = WriteCursor::new();
        c.write_u32_le(self.timeout_ms);
        c.write_u32_le(if self.persistent { FLAG_PERSISTENT } else { 0 });
        c.write_u64_le(0); // Reserved
        self.create_guid.pack(&mut c);
        c.into_inner()
    }

    /// The whole context, ready for [`pack_contexts`].
    pub fn context(self) -> CreateContext {
        CreateContext::new(NAME_DH2Q, self.to_bytes())
    }
}

/// What the server granted, parsed from the `DH2Q` context on a CREATE
/// response (MS-SMB2 § 2.2.14.2.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableGrant {
    /// How long the server will hold the open after a disconnect, in
    /// milliseconds. `0` means the server did not say.
    pub timeout_ms: u32,
    /// Whether the grant is persistent (survives a server restart) rather than
    /// merely durable (survives a connection drop).
    pub persistent: bool,
}

impl DurableGrant {
    /// Parse the 8-byte response body.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut c = ReadCursor::new(data);
        let timeout_ms = c.read_u32_le()?;
        let flags = c.read_u32_le()?;
        Ok(Self {
            timeout_ms,
            persistent: flags & FLAG_PERSISTENT != 0,
        })
    }
}

// ── DH2C: claim one back ───────────────────────────────────────────────────

/// Claim a durable open back on a new connection (MS-SMB2 § 2.2.13.2.12).
///
/// The `create_guid` is the load-bearing field. The server matches it against
/// the `CreateGuid` it stored when the open was made, so a reclaim that
/// succeeds proves the server found *this client's* open — not merely
/// something living at the same `FileId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableReconnectV2 {
    /// The `FileId` the dead session was using.
    pub file_id: FileId,
    /// The GUID sent in the original `DH2Q`.
    pub create_guid: Guid,
    /// Whether the handle being reclaimed was persistent.
    pub persistent: bool,
}

impl DurableReconnectV2 {
    /// Pack into the 36-byte context body.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut c = WriteCursor::new();
        c.write_u64_le(self.file_id.persistent);
        c.write_u64_le(self.file_id.volatile);
        self.create_guid.pack(&mut c);
        c.write_u32_le(if self.persistent { FLAG_PERSISTENT } else { 0 });
        c.into_inner()
    }

    /// The whole context, ready for [`pack_contexts`].
    pub fn context(self) -> CreateContext {
        CreateContext::new(NAME_DH2C, self.to_bytes())
    }
}

// ── QFid: which file is this, really ───────────────────────────────────────

/// The file's identity on the server's disk (MS-SMB2 § 2.2.14.2.9).
///
/// The pair is what a filesystem calls a volume id plus an inode number, and
/// it is stable across opens of the same file and different for any other
/// file on the server. That makes it the client-side half of proving a
/// reclaimed handle is the same file: the `CreateGuid` says the *server*
/// matched our open, and this says the open is still pointing at the bytes we
/// were writing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnDiskId {
    /// The file's id on its volume (an inode number, on a POSIX server).
    pub disk_file_id: u64,
    /// The volume the file lives on.
    pub volume_id: u64,
}

impl OnDiskId {
    /// The request context. It has no body.
    pub fn request() -> CreateContext {
        CreateContext::new(NAME_QFID, Vec::new())
    }

    /// Parse the 32-byte response body (16 bytes of ids, 16 reserved).
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut c = ReadCursor::new(data);
        Ok(Self {
            disk_file_id: c.read_u64_le()?,
            volume_id: c.read_u64_le()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guid(s: &str) -> Guid {
        // "5a08e844-45c3-234d-87c6-596d2bc8bca5"
        let hex: Vec<u8> = s
            .chars()
            .filter(|c| *c != '-')
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|p| u8::from_str_radix(&p.iter().collect::<String>(), 16).unwrap())
            .collect();
        Guid {
            data1: u32::from_be_bytes([hex[0], hex[1], hex[2], hex[3]]),
            data2: u16::from_be_bytes([hex[4], hex[5]]),
            data3: u16::from_be_bytes([hex[6], hex[7]]),
            data4: [
                hex[8], hex[9], hex[10], hex[11], hex[12], hex[13], hex[14], hex[15],
            ],
        }
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    /// Pinned against the wire bytes in the `smb-rs` reference implementation's
    /// own test vector, so a packing mistake shows up here rather than as a
    /// server silently declining to grant a durable handle.
    #[test]
    fn a_durable_request_matches_the_reference_wire_bytes() {
        let req = DurableRequestV2 {
            timeout_ms: 0,
            persistent: false,
            create_guid: guid("5a08e844-45c3-234d-87c6-596d2bc8bca5"),
        };
        assert_eq!(
            hex(&req.to_bytes()),
            "0000000000000000000000000000000044e8085ac3454d2387c6596d2bc8bca5"
        );
        assert_eq!(req.to_bytes().len(), 32);
    }

    #[test]
    fn a_durable_reconnect_matches_the_reference_wire_bytes() {
        let req = DurableReconnectV2 {
            file_id: FileId {
                persistent: 0x0000_0008_0000_00b3,
                volatile: 0x0000_0008_0000_00dd,
            },
            create_guid: guid("a23e428c-1bac-7e43-8451-91f9f2277a95"),
            persistent: false,
        };
        assert_eq!(
            hex(&req.to_bytes()),
            "b300000008000000dd000000080000008c423ea2ac1b437e845191f9f2277a9500000000"
        );
        assert_eq!(req.to_bytes().len(), 36);
    }

    #[test]
    fn a_durable_grant_reads_its_timeout_and_persistence() {
        // 180000 ms, not persistent -- the reference vector for DH2QResp.
        let grant = DurableGrant::from_bytes(&[0x20, 0xbf, 0x02, 0x00, 0, 0, 0, 0]).unwrap();
        assert_eq!(grant.timeout_ms, 180_000);
        assert!(!grant.persistent);

        let persistent =
            DurableGrant::from_bytes(&[0, 0, 0, 0, FLAG_PERSISTENT as u8, 0, 0, 0]).unwrap();
        assert!(persistent.persistent);
    }

    #[test]
    fn an_on_disk_id_reads_the_reference_wire_bytes() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x2ae7_0100_0000_0400u64.to_le_bytes());
        body.extend_from_slice(&0xd9cf_17b0_0000_0000u64.to_le_bytes());
        body.extend_from_slice(&[0u8; 16]);
        let id = OnDiskId::from_bytes(&body).unwrap();
        assert_eq!(id.disk_file_id, 0x2ae7_0100_0000_0400);
        assert_eq!(id.volume_id, 0xd9cf_17b0_0000_0000);
    }

    #[test]
    fn a_context_chain_round_trips() {
        let chain = vec![
            DurableRequestV2 {
                timeout_ms: 0,
                persistent: false,
                create_guid: guid("5a08e844-45c3-234d-87c6-596d2bc8bca5"),
            }
            .context(),
            OnDiskId::request(),
        ];
        let bytes = pack_contexts(&chain);
        assert_eq!(bytes.len() % 8, 0, "the chain stays 8-byte aligned");
        let parsed = parse_contexts(&bytes).unwrap();
        assert_eq!(parsed, chain);
        assert!(find(&parsed, NAME_DH2Q).is_some());
        assert!(find(&parsed, NAME_QFID).is_some());
        assert!(find(&parsed, NAME_DH2C).is_none());
    }

    #[test]
    fn a_context_with_no_data_reports_a_zero_data_offset() {
        let bytes = pack_contexts(&[OnDiskId::request()]);
        // DataOffset lives at bytes 10..12.
        assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 0);
        assert_eq!(
            u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            0
        );
        assert_eq!(parse_contexts(&bytes).unwrap(), vec![OnDiskId::request()]);
    }

    #[test]
    fn an_empty_chain_parses_to_nothing() {
        assert_eq!(parse_contexts(&[]).unwrap(), vec![]);
    }

    /// A malformed chain must be rejected, not walked. These are the shapes a
    /// hostile or buggy server could use to make the parser read past its
    /// buffer or loop forever.
    #[test]
    fn a_chain_that_points_outside_itself_is_rejected() {
        let mut bytes = pack_contexts(&[OnDiskId::request()]);
        bytes[0..4].copy_from_slice(&9999u32.to_le_bytes()); // Next past the end
        assert!(parse_contexts(&bytes).is_err());

        let mut bytes = pack_contexts(&[OnDiskId::request()]);
        bytes[6..8].copy_from_slice(&9999u16.to_le_bytes()); // NameLength past the end
        assert!(parse_contexts(&bytes).is_err());

        let mut bytes = pack_contexts(&[DurableRequestV2 {
            timeout_ms: 0,
            persistent: false,
            create_guid: Guid::ZERO,
        }
        .context()]);
        bytes[12..16].copy_from_slice(&9999u32.to_le_bytes()); // DataLength past the end
        assert!(parse_contexts(&bytes).is_err());

        assert!(parse_contexts(&[0u8; 8]).is_err(), "truncated entry header");
    }
}
