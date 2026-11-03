//! DFS referral request and response wire format (MS-DFSC sections 2.2.2, 2.2.4).
//!
//! These types are packed into the input/output buffers of an IOCTL request
//! with `ctl_code = FSCTL_DFS_GET_REFERRALS`.

use crate::error::Result;
use crate::pack::{Pack, ReadCursor, Unpack, WriteCursor};
use crate::types::flags::impl_flags;
use crate::Error;
use std::ops::{BitAnd, BitOr, BitOrAssign};

// ── ReqGetDfsReferral ─────────────────────────────────────────────────

/// REQ_GET_DFS_REFERRAL (MS-DFSC 2.2.2).
///
/// Sent as the input buffer of an `FSCTL_DFS_GET_REFERRALS` IOCTL request.
/// Contains the maximum referral version the client understands and the
/// DFS path to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReqGetDfsReferral {
    /// Highest DFS referral version understood by the client (typically 4).
    pub max_referral_level: u16,
    /// The DFS path to resolve (case-insensitive UNC path).
    pub request_file_name: String,
}

impl Pack for ReqGetDfsReferral {
    fn pack(&self, cursor: &mut WriteCursor) {
        // MaxReferralLevel (2 bytes, LE)
        cursor.write_u16_le(self.max_referral_level);
        // RequestFileName (null-terminated UTF-16LE)
        cursor.write_utf16_le(&self.request_file_name);
        // Null terminator (2 bytes)
        cursor.write_u16_le(0);
    }
}

impl Unpack for ReqGetDfsReferral {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let max_referral_level = cursor.read_u16_le()?;
        // Read the rest as null-terminated UTF-16LE.
        let request_file_name = read_null_terminated_utf16(cursor)?;
        Ok(ReqGetDfsReferral {
            max_referral_level,
            request_file_name,
        })
    }
}

// ── RespGetDfsReferral ────────────────────────────────────────────────

/// ReferralHeaderFlags in a RESP_GET_DFS_REFERRAL (MS-DFSC 2.2.4).
///
/// Only the R, S, and T bits are defined; a server sets everything else to 0
/// and a client ignores it.
///
/// ❌ **Don't require any particular combination before accepting a referral.**
/// Windows answers a root referral with R and S both set (MS-DFSC § 4.3,
/// § 4.6), Samba answers the same referral with S alone (verified against
/// Samba 4.20.6 on 2026-09-16). A client that insists on the Windows shape
/// silently refuses every Samba namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReferralHeaderFlags(pub u32);

impl ReferralHeaderFlags {
    /// R: every target in the response is a DFS root target that can itself
    /// answer referral requests.
    pub const REFERRAL_SERVERS: u32 = 0x0000_0001;
    /// S: every target in the response can be reached without a further
    /// referral.
    pub const STORAGE_SERVERS: u32 = 0x0000_0002;
    /// T: target failback is enabled for every target. V4 only; a server sets
    /// it to 0 for every other version.
    pub const TARGET_FAILBACK: u32 = 0x0000_0004;

    /// Whether this response points into another DFS namespace, by the first
    /// of the two tests in MS-DFSC § 3.1.5.4.5: ReferralServers set and
    /// StorageServers clear.
    ///
    /// The section's second test needs a DomainCache, which a client that is
    /// not domain-joined never has, so this is the whole test here.
    #[inline]
    pub const fn is_interlink(&self) -> bool {
        self.contains(Self::REFERRAL_SERVERS) && !self.contains(Self::STORAGE_SERVERS)
    }
}

impl_flags!(ReferralHeaderFlags, u32);

/// ReferralEntryFlags in a referral entry (MS-DFSC 2.2.5.3, 2.2.5.4).
///
/// V1 and V2 entries set this to 0. V3 defines NameListReferral, and V4 adds
/// TargetSetBoundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReferralEntryFlags(pub u16);

impl ReferralEntryFlags {
    /// The entry carries a domain name and a DC list rather than a target
    /// path, so its tail is laid out as in MS-DFSC § 2.2.5.3.2.
    pub const NAME_LIST_REFERRAL: u16 = 0x0002;
    /// V4 only: this entry is the first target of a target set.
    pub const TARGET_SET_BOUNDARY: u16 = 0x0004;
}

impl_flags!(ReferralEntryFlags, u16);

/// RESP_GET_DFS_REFERRAL (MS-DFSC 2.2.4).
///
/// Returned in the output buffer of an IOCTL response for
/// `FSCTL_DFS_GET_REFERRALS`. Contains the number of bytes of the path
/// consumed by the server, header flags, and a list of referral entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RespGetDfsReferral {
    /// Number of **bytes of UTF-16LE**, not characters, of the request path
    /// that the server matched. Divide by two for a count of code units.
    pub path_consumed: u16,
    /// Header flags (ReferralServers | StorageServers | TargetFailback).
    pub header_flags: ReferralHeaderFlags,
    /// The list of referral entries (V1 through V4).
    pub entries: Vec<DfsReferralEntry>,
}

/// A single DFS referral entry.
///
/// The three variants are the three tails MS-DFSC defines, kept apart so that
/// a field a given shape does not carry cannot be read off it. A V1 entry has
/// no DFS path and no TTL; a NameList entry has no target address at all and
/// would otherwise be read as three path offsets, yielding strings from
/// wherever in the buffer those bytes happened to point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DfsReferralEntry {
    /// A V1 entry (MS-DFSC § 2.2.5.1): a bare target share name, inline and
    /// null-terminated, with no DFS path and no TTL.
    ///
    /// This crate asks for `MaxReferralLevel = 4` and MS-DFSC § 2.2.2 says a
    /// client MUST support every version up to the one it asks for, so V1 is
    /// parsed even though no server has been seen to send it.
    V1 {
        /// 0 = non-root (link) targets, 1 = root targets. ❌ Never gate on it.
        server_type: u16,
        /// The target share, for example `\\server\share`.
        share_name: String,
    },
    /// A root, link, or sysvol target (V2, V3, or V4).
    Target {
        /// Referral entry version (2, 3, or 4).
        version: u16,
        /// 0 = non-root (link) targets, 1 = root targets.
        ///
        /// ❌ **Never gate on it.** Samba answers a namespace-root referral
        /// with 0 where Windows answers 1 (verified against Samba 4.20.6,
        /// 2026-09-16). Store it, and let the path shape decide.
        server_type: u16,
        /// Entry flags. `TARGET_SET_BOUNDARY` is meaningful on V4 only.
        flags: ReferralEntryFlags,
        /// Time-to-live in seconds for caching this referral.
        ttl: u32,
        /// The DFS path prefix that matched.
        dfs_path: String,
        /// The DFS alternate path (usually identical to `dfs_path`).
        dfs_alternate_path: String,
        /// The target UNC path (for example, `\\server\share`).
        network_address: String,
    },
    /// A domain or DC referral (V3/V4 with `NAME_LIST_REFERRAL` set).
    ///
    /// This crate never requests the referral types that produce one, so
    /// parsing it is about never mistaking it for a target rather than about
    /// using it.
    NameList {
        /// Referral entry version (3 or 4).
        version: u16,
        /// Entry flags, with `NAME_LIST_REFERRAL` set.
        flags: ReferralEntryFlags,
        /// Time-to-live in seconds.
        ttl: u32,
        /// The domain name this entry describes.
        special_name: String,
        /// The DC host names returned for it. Empty for a domain referral.
        expanded_names: Vec<String>,
    },
}

impl DfsReferralEntry {
    /// The referral version this entry was parsed from.
    pub fn version(&self) -> u16 {
        match self {
            Self::V1 { .. } => 1,
            Self::Target { version, .. } | Self::NameList { version, .. } => *version,
        }
    }

    /// The target UNC path, for the two variants that name one.
    pub fn target_address(&self) -> Option<&str> {
        match self {
            Self::V1 { share_name, .. } => Some(share_name),
            Self::Target {
                network_address, ..
            } => Some(network_address),
            Self::NameList { .. } => None,
        }
    }

    /// The DFS path prefix this entry covers, when the entry carries one.
    ///
    /// `None` for V1, which has no DFSPath field: its cache key has to come
    /// from `PathConsumed` against the request path instead.
    pub fn dfs_path(&self) -> Option<&str> {
        match self {
            Self::Target { dfs_path, .. } => Some(dfs_path),
            Self::V1 { .. } | Self::NameList { .. } => None,
        }
    }

    /// `ServerType`, for the two variants that carry one.
    ///
    /// ❌ **Never gate on it.** MS-DFSC § 3.1.1 keeps it as `RootOrLink` for
    /// deciding what to do *next*, and Samba reports 0 for a root referral
    /// where Windows reports 1.
    pub fn server_type(&self) -> Option<u16> {
        match self {
            Self::V1 { server_type, .. } | Self::Target { server_type, .. } => Some(*server_type),
            Self::NameList { .. } => None,
        }
    }

    /// Caching lifetime in seconds. V1 carries none, and reports 0.
    pub fn ttl(&self) -> u32 {
        match self {
            Self::V1 { .. } => 0,
            Self::Target { ttl, .. } | Self::NameList { ttl, .. } => *ttl,
        }
    }

    /// Entry flags. V1 has no meaningful ones and reports empty.
    pub fn flags(&self) -> ReferralEntryFlags {
        match self {
            Self::V1 { .. } => ReferralEntryFlags::default(),
            Self::Target { flags, .. } | Self::NameList { flags, .. } => *flags,
        }
    }
}

impl Unpack for RespGetDfsReferral {
    fn unpack(cursor: &mut ReadCursor<'_>) -> Result<Self> {
        let path_consumed = cursor.read_u16_le()?;
        let number_of_referrals = cursor.read_u16_le()?;
        let header_flags = ReferralHeaderFlags::new(cursor.read_u32_le()?);

        // The remaining data contains all referral entries followed by a
        // string buffer. We need the full remaining slice to resolve
        // offsets that are relative to each entry's start.
        let entry_data = cursor.read_bytes(cursor.remaining())?;

        // `number_of_referrals` is server-controlled, so the vector grows as
        // entries actually parse rather than reserving what a hostile server
        // claims.
        let mut entries = Vec::new();
        let mut offset = 0usize;

        for _ in 0..number_of_referrals {
            if offset + 4 > entry_data.len() {
                return Err(Error::invalid_data(
                    "DFS referral entry truncated (version/size header)",
                ));
            }

            let version = u16::from_le_bytes([entry_data[offset], entry_data[offset + 1]]);
            let entry_size =
                u16::from_le_bytes([entry_data[offset + 2], entry_data[offset + 3]]) as usize;

            if entry_size < 4 {
                return Err(Error::invalid_data(format!(
                    "DFS referral entry size too small: {entry_size}"
                )));
            }

            let entry_start = offset;
            // The entry_size includes the version and size fields themselves.
            let entry_end = entry_start + entry_size;
            if entry_end > entry_data.len() {
                return Err(Error::invalid_data(format!(
                    "DFS referral entry extends past buffer: entry_end={entry_end}, buf={}",
                    entry_data.len()
                )));
            }

            // All strings referenced by offsets live from entry_start onward
            // in the full buffer (not truncated to entry_size, because the
            // strings are in the trailing string buffer).
            let entry = parse_referral_entry(version, entry_data, entry_start)?;
            entries.push(entry);

            offset = entry_end;
        }

        Ok(RespGetDfsReferral {
            path_consumed,
            header_flags,
            entries,
        })
    }
}

/// Parse a single referral entry starting at `entry_start` within `buf`.
///
/// String offsets in V2/V3/V4 are relative to the start of the entry
/// (which includes the 4-byte version+size prefix). V1's `ShareName` carries
/// no offset at all: it sits inline right after the 8-byte fixed part.
fn parse_referral_entry(version: u16, buf: &[u8], entry_start: usize) -> Result<DfsReferralEntry> {
    // Skip version (2) + size (2) -- already read by caller.
    let mut pos = entry_start + 4;

    match version {
        1 => {
            // V1: server_type(2) + referral_entry_flags(2), then an inline
            // null-terminated ShareName. No offsets, no DFSPath, no TTL.
            ensure_remaining(buf, pos, 4)?;
            let server_type = read_u16(buf, pos);
            pos += 2;
            // ReferralEntryFlags MUST be 0 and ignored on receipt for V1.
            let _flags = read_u16(buf, pos);
            pos += 2;

            let share_name = read_null_terminated_utf16_at(buf, pos)?;

            Ok(DfsReferralEntry::V1 {
                server_type,
                share_name,
            })
        }
        2 => {
            // V2: server_type(2) + flags(2) + proximity(4) + ttl(4) +
            //     dfs_path_offset(2) + dfs_alternate_path_offset(2) + network_address_offset(2)
            //     = 18 bytes of fixed entry body after the 4-byte version/size prefix.
            ensure_remaining(buf, pos, 18)?;
            let server_type = read_u16(buf, pos);
            pos += 2;
            let flags = ReferralEntryFlags::new(read_u16(buf, pos));
            pos += 2;
            let _proximity = read_u32(buf, pos);
            pos += 4;
            let ttl = read_u32(buf, pos);
            pos += 4;
            let dfs_path_offset = read_u16(buf, pos) as usize;
            pos += 2;
            let dfs_alternate_path_offset = read_u16(buf, pos) as usize;
            pos += 2;
            let network_address_offset = read_u16(buf, pos) as usize;

            let dfs_path = read_offset_string(buf, entry_start, dfs_path_offset)?;
            let dfs_alternate_path =
                read_offset_string(buf, entry_start, dfs_alternate_path_offset)?;
            let network_address = read_offset_string(buf, entry_start, network_address_offset)?;

            Ok(DfsReferralEntry::Target {
                version,
                server_type,
                flags,
                ttl,
                dfs_path,
                dfs_alternate_path,
                network_address,
            })
        }
        3 | 4 => {
            // V3/V4 share a common head: server_type(2) + flags(2) + ttl(4).
            // The tail depends on the NameListReferral flag (§ 2.2.5.3.1 vs
            // § 2.2.5.3.2), and reading the wrong one yields strings from
            // arbitrary buffer positions rather than an error.
            ensure_remaining(buf, pos, 8)?;
            let server_type = read_u16(buf, pos);
            pos += 2;
            let flags = ReferralEntryFlags::new(read_u16(buf, pos));
            pos += 2;
            let ttl = read_u32(buf, pos);
            pos += 4;

            if flags.contains(ReferralEntryFlags::NAME_LIST_REFERRAL) {
                // § 2.2.5.3.2: special_name_offset(2) +
                // number_of_expanded_names(2) + expanded_name_offset(2).
                ensure_remaining(buf, pos, 6)?;
                let special_name_offset = read_u16(buf, pos) as usize;
                pos += 2;
                let number_of_expanded_names = read_u16(buf, pos);
                pos += 2;
                let expanded_name_offset = read_u16(buf, pos) as usize;

                let special_name = read_offset_string(buf, entry_start, special_name_offset)?;

                // The names are packed back to back, each null-terminated, and
                // the count is server-supplied: read them one at a time so a
                // bogus count runs out of buffer instead of allocating for it.
                let mut expanded_names = Vec::new();
                let mut name_pos = entry_start
                    .checked_add(expanded_name_offset)
                    .ok_or_else(|| Error::invalid_data("DFS NameList offset overflows"))?;
                for _ in 0..number_of_expanded_names {
                    let (name, next) = read_null_terminated_utf16_span(buf, name_pos)?;
                    expanded_names.push(name);
                    name_pos = next;
                }

                Ok(DfsReferralEntry::NameList {
                    version,
                    flags,
                    ttl,
                    special_name,
                    expanded_names,
                })
            } else {
                // § 2.2.5.3.1: three path offsets, then a 16-byte
                // ServiceSiteGuid that is always zero and always ignored.
                ensure_remaining(buf, pos, 6)?;
                let dfs_path_offset = read_u16(buf, pos) as usize;
                pos += 2;
                let dfs_alternate_path_offset = read_u16(buf, pos) as usize;
                pos += 2;
                let network_address_offset = read_u16(buf, pos) as usize;

                let dfs_path = read_offset_string(buf, entry_start, dfs_path_offset)?;
                let dfs_alternate_path =
                    read_offset_string(buf, entry_start, dfs_alternate_path_offset)?;
                let network_address = read_offset_string(buf, entry_start, network_address_offset)?;

                Ok(DfsReferralEntry::Target {
                    version,
                    server_type,
                    flags,
                    ttl,
                    dfs_path,
                    dfs_alternate_path,
                    network_address,
                })
            }
        }
        _ => Err(Error::invalid_data(format!(
            "unsupported DFS referral version: {version} (only V1-V4 are supported)"
        ))),
    }
}

// ── Helper functions ──────────────────────────────────────────────────

/// Read a null-terminated UTF-16LE string from a `ReadCursor`.
fn read_null_terminated_utf16(cursor: &mut ReadCursor<'_>) -> Result<String> {
    let mut code_units: Vec<u16> = Vec::new();
    loop {
        let cu = cursor.read_u16_le()?;
        if cu == 0 {
            break;
        }
        code_units.push(cu);
    }
    String::from_utf16(&code_units)
        .map_err(|_| Error::invalid_data("invalid UTF-16LE in DFS request file name"))
}

/// Read a null-terminated UTF-16LE string from a raw byte buffer at a given absolute offset.
fn read_null_terminated_utf16_at(buf: &[u8], offset: usize) -> Result<String> {
    read_null_terminated_utf16_span(buf, offset).map(|(s, _)| s)
}

/// Read a null-terminated UTF-16LE string, returning it and the offset just
/// past its terminator (where the next string in a packed list begins).
fn read_null_terminated_utf16_span(buf: &[u8], offset: usize) -> Result<(String, usize)> {
    let mut code_units: Vec<u16> = Vec::new();
    let mut pos = offset;
    loop {
        if pos.saturating_add(2) > buf.len() {
            return Err(Error::invalid_data(
                "DFS referral string extends past buffer",
            ));
        }
        let cu = u16::from_le_bytes([buf[pos], buf[pos + 1]]);
        pos += 2;
        if cu == 0 {
            break;
        }
        code_units.push(cu);
    }
    let s = String::from_utf16(&code_units)
        .map_err(|_| Error::invalid_data("invalid UTF-16LE in DFS referral string"))?;
    Ok((s, pos))
}

/// Read a null-terminated UTF-16LE string at an offset relative to an entry start.
fn read_offset_string(buf: &[u8], entry_start: usize, offset: usize) -> Result<String> {
    let abs = entry_start
        .checked_add(offset)
        .ok_or_else(|| Error::invalid_data("DFS referral string offset overflows"))?;
    read_null_terminated_utf16_at(buf, abs)
}

/// Inline LE u16 read from a byte buffer.
fn read_u16(buf: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes([buf[pos], buf[pos + 1]])
}

/// Inline LE u32 read from a byte buffer.
fn read_u32(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
}

/// Check that at least `need` bytes are available at `pos` in `buf`.
fn ensure_remaining(buf: &[u8], pos: usize, need: usize) -> Result<()> {
    if pos.saturating_add(need) > buf.len() {
        Err(Error::invalid_data(format!(
            "DFS referral entry truncated: need {need} bytes at offset {pos}, buf len {}",
            buf.len()
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Request tests ─────────────────────────────────────────────────

    #[test]
    fn req_pack_known_bytes() {
        // Test vector from smb-rs: ReqGetDfsReferral { max_referral_level: 4,
        // request_file_name: r"\ADC.aviv.local\dfs\Docs" }
        let expected = hex_to_bytes(
            "04005c004100440043002e0061007600690076002e006c006f00630061006c005c006400660073005c0044006f00630073000000",
        );
        let req = ReqGetDfsReferral {
            max_referral_level: 4,
            request_file_name: r"\ADC.aviv.local\dfs\Docs".to_string(),
        };
        let mut cursor = WriteCursor::new();
        req.pack(&mut cursor);
        assert_eq!(cursor.into_inner(), expected);
    }

    #[test]
    fn req_pack_roundtrip() {
        let original = ReqGetDfsReferral {
            max_referral_level: 4,
            request_file_name: r"\server\share\path".to_string(),
        };
        let mut w = WriteCursor::new();
        original.pack(&mut w);
        let bytes = w.into_inner();

        let mut r = ReadCursor::new(&bytes);
        let decoded = ReqGetDfsReferral::unpack(&mut r).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn req_pack_empty_path() {
        let req = ReqGetDfsReferral {
            max_referral_level: 3,
            request_file_name: String::new(),
        };
        let mut w = WriteCursor::new();
        req.pack(&mut w);
        let bytes = w.into_inner();
        // max_referral_level (2) + null terminator (2) = 4 bytes
        assert_eq!(bytes.len(), 4);
        assert_eq!(bytes, [0x03, 0x00, 0x00, 0x00]);

        let mut r = ReadCursor::new(&bytes);
        let decoded = ReqGetDfsReferral::unpack(&mut r).unwrap();
        assert_eq!(decoded, req);
    }

    #[test]
    fn req_unpack_truncated() {
        // Only 1 byte -- not enough for max_referral_level.
        let bytes = [0x04];
        let mut r = ReadCursor::new(&bytes);
        assert!(ReqGetDfsReferral::unpack(&mut r).is_err());
    }

    // ── Response tests ────────────────────────────────────────────────

    #[test]
    fn resp_parse_v4_referral() {
        // Test vector from smb-rs: two V4 entries.
        let hex = "300002000200000004002200000004000807000044007600\
            a800000000000000000000000000000000000400220000000000\
            0807000022005400a8000000000000000000000000000000\
            00005c004100440043002e0061007600690076002e006c00\
            6f00630061006c005c006400660073005c0044006f006300\
            730000005c004100440043002e0061007600690076002e00\
            6c006f00630061006c005c006400660073005c0044006f00\
            6300730000005c004100440043005c005300680061007200\
            650073005c0044006f006300730000005c00460053005200\
            56005c005300680061007200650073005c004d0079005300\
            6800610072006500000000";
        let data = hex_to_bytes(hex);
        let mut cursor = ReadCursor::new(&data);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();

        assert_eq!(resp.path_consumed, 48);
        // header_flags = 0x00000002 (StorageServers)
        assert_eq!(resp.header_flags.bits(), 0x0000_0002);
        assert!(!resp.header_flags.is_interlink());
        assert_eq!(resp.entries.len(), 2);

        let DfsReferralEntry::Target {
            version,
            server_type,
            flags,
            ttl,
            dfs_path,
            dfs_alternate_path,
            network_address,
        } = &resp.entries[0]
        else {
            panic!("expected a Target entry, got {:?}", resp.entries[0]);
        };
        assert_eq!(*version, 4);
        assert_eq!(*server_type, 0); // non-root
        assert_eq!(*ttl, 1800);
        // The first entry of a V4 response MUST carry TargetSetBoundary.
        assert!(flags.contains(ReferralEntryFlags::TARGET_SET_BOUNDARY));
        assert_eq!(dfs_path, r"\ADC.aviv.local\dfs\Docs");
        assert_eq!(dfs_alternate_path, r"\ADC.aviv.local\dfs\Docs");
        assert_eq!(network_address, r"\ADC\Shares\Docs");

        let e1 = &resp.entries[1];
        assert_eq!(e1.version(), 4);
        assert_eq!(e1.ttl(), 1800);
        assert!(!e1.flags().contains(ReferralEntryFlags::TARGET_SET_BOUNDARY));
        assert_eq!(e1.dfs_path(), Some(r"\ADC.aviv.local\dfs\Docs"));
        assert_eq!(e1.target_address(), Some(r"\FSRV\Shares\MyShare"));
    }

    /// V1 entries carry their share name inline, with no offsets, no DFS
    /// path, and no TTL. MS-DFSC § 2.2.2 makes supporting them a MUST for a
    /// client that asks for `MaxReferralLevel = 4`, as this one does.
    #[test]
    fn resp_parse_v1_referral() {
        let share_name = encode_null_utf16(r"\srv\legacy");
        // 4 (version+size) + 2 (server_type) + 2 (flags) + the inline string.
        let entry_size = 8u16 + share_name.len() as u16;

        let mut buf = Vec::new();
        buf.extend_from_slice(&22u16.to_le_bytes()); // path_consumed
        buf.extend_from_slice(&1u16.to_le_bytes()); // number_of_referrals
        buf.extend_from_slice(&1u32.to_le_bytes()); // header_flags

        buf.extend_from_slice(&1u16.to_le_bytes()); // version = 1
        buf.extend_from_slice(&entry_size.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // server_type = root
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags (MUST be 0)
        buf.extend_from_slice(&share_name);

        let mut cursor = ReadCursor::new(&buf);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();

        assert_eq!(resp.entries.len(), 1);
        assert_eq!(
            resp.entries[0],
            DfsReferralEntry::V1 {
                server_type: 1,
                share_name: r"\srv\legacy".to_string(),
            }
        );
        // No DFS path means the cache key has to come from `path_consumed`.
        assert_eq!(resp.entries[0].dfs_path(), None);
        assert_eq!(resp.entries[0].ttl(), 0);
        assert_eq!(resp.entries[0].target_address(), Some(r"\srv\legacy"));
    }

    /// A NameListReferral entry's tail is `SpecialNameOffset` +
    /// `NumberOfExpandedNames` + `ExpandedNameOffset` (§ 2.2.5.3.2), not the
    /// three path offsets. Read as a target it would yield strings from
    /// wherever those two bytes happened to point.
    #[test]
    fn resp_parse_name_list_referral() {
        let domain = encode_null_utf16("CONTOSO");
        let dc1 = encode_null_utf16(r"\dc1.contoso.com");
        let dc2 = encode_null_utf16(r"\dc2.contoso.com");

        // 4 + 2+2+4 (head) + 2+2+2 (tail) = 18, then 16 optional padding bytes
        // the server MAY insert; use them here so the parser proves it follows
        // `Size` rather than assuming a layout.
        let entry_size: u16 = 34;
        let special_name_offset = entry_size;
        let expanded_name_offset = special_name_offset + domain.len() as u16;

        let mut buf = Vec::new();
        buf.extend_from_slice(&14u16.to_le_bytes()); // path_consumed
        buf.extend_from_slice(&1u16.to_le_bytes()); // number_of_referrals
        buf.extend_from_slice(&0u32.to_le_bytes()); // header_flags

        buf.extend_from_slice(&3u16.to_le_bytes()); // version = 3
        buf.extend_from_slice(&entry_size.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // server_type
        buf.extend_from_slice(&ReferralEntryFlags::NAME_LIST_REFERRAL.to_le_bytes());
        buf.extend_from_slice(&600u32.to_le_bytes()); // ttl
        buf.extend_from_slice(&special_name_offset.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes()); // number_of_expanded_names
        buf.extend_from_slice(&expanded_name_offset.to_le_bytes());
        buf.extend_from_slice(&[0u8; 16]); // padding the server MAY insert

        buf.extend_from_slice(&domain);
        buf.extend_from_slice(&dc1);
        buf.extend_from_slice(&dc2);

        let mut cursor = ReadCursor::new(&buf);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();

        assert_eq!(resp.entries.len(), 1);
        let DfsReferralEntry::NameList {
            version,
            ttl,
            special_name,
            expanded_names,
            ..
        } = &resp.entries[0]
        else {
            panic!("expected a NameList entry, got {:?}", resp.entries[0]);
        };
        assert_eq!(*version, 3);
        assert_eq!(*ttl, 600);
        assert_eq!(special_name, "CONTOSO");
        assert_eq!(expanded_names, &[r"\dc1.contoso.com", r"\dc2.contoso.com"]);
        // Nothing here can be mistaken for somewhere to connect.
        assert_eq!(resp.entries[0].target_address(), None);
    }

    /// A NameList entry claiming more DC names than the buffer holds is a
    /// clean error, never an allocation sized by the server.
    #[test]
    fn resp_parse_name_list_bogus_count_errors() {
        let domain = encode_null_utf16("CONTOSO");
        let entry_size: u16 = 18;
        let special_name_offset = entry_size;

        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());

        buf.extend_from_slice(&3u16.to_le_bytes());
        buf.extend_from_slice(&entry_size.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&ReferralEntryFlags::NAME_LIST_REFERRAL.to_le_bytes());
        buf.extend_from_slice(&600u32.to_le_bytes());
        buf.extend_from_slice(&special_name_offset.to_le_bytes());
        buf.extend_from_slice(&u16::MAX.to_le_bytes()); // absurd name count
        buf.extend_from_slice(&(special_name_offset + domain.len() as u16).to_le_bytes());
        buf.extend_from_slice(&domain);

        let mut cursor = ReadCursor::new(&buf);
        assert!(RespGetDfsReferral::unpack(&mut cursor).is_err());
    }

    /// The Interlink test of § 3.1.5.4.5: R set and S clear.
    #[test]
    fn header_flags_interlink_test() {
        let interlink = ReferralHeaderFlags::new(ReferralHeaderFlags::REFERRAL_SERVERS);
        assert!(interlink.is_interlink());

        // What Samba answers for a namespace root: S alone.
        let samba = ReferralHeaderFlags::new(ReferralHeaderFlags::STORAGE_SERVERS);
        assert!(!samba.is_interlink());

        // What Windows answers for the same: R and S together.
        let windows = ReferralHeaderFlags::new(
            ReferralHeaderFlags::REFERRAL_SERVERS | ReferralHeaderFlags::STORAGE_SERVERS,
        );
        assert!(!windows.is_interlink());

        assert!(!ReferralHeaderFlags::default().is_interlink());
    }

    #[test]
    fn resp_parse_v3_referral() {
        // Manually constructed V3 response: one entry.
        // Header: path_consumed=20, num_referrals=1, flags=0x03
        // Entry: version=3, size=34 (fixed part), server_type=1, flags=0,
        //   ttl=600, offsets point to strings after the entry.
        let dfs_path = encode_null_utf16(r"\dom\share");
        let alt_path = encode_null_utf16(r"\dom\share");
        let net_addr = encode_null_utf16(r"\srv\share");

        let entry_fixed_size: u16 = 34; // 4 + 2+2+4 + 2+2+2 + 16 = 34
        let dfs_path_offset = entry_fixed_size;
        let alt_path_offset = dfs_path_offset + dfs_path.len() as u16;
        let net_addr_offset = alt_path_offset + alt_path.len() as u16;

        let mut buf = Vec::new();
        // Response header
        buf.extend_from_slice(&20u16.to_le_bytes()); // path_consumed
        buf.extend_from_slice(&1u16.to_le_bytes()); // number_of_referrals
        buf.extend_from_slice(&3u32.to_le_bytes()); // header_flags

        // Entry header
        buf.extend_from_slice(&3u16.to_le_bytes()); // version
        buf.extend_from_slice(&entry_fixed_size.to_le_bytes()); // size (fixed part)
        buf.extend_from_slice(&1u16.to_le_bytes()); // server_type (root)
        buf.extend_from_slice(&0u16.to_le_bytes()); // referral_entry_flags
        buf.extend_from_slice(&600u32.to_le_bytes()); // ttl
        buf.extend_from_slice(&dfs_path_offset.to_le_bytes());
        buf.extend_from_slice(&alt_path_offset.to_le_bytes());
        buf.extend_from_slice(&net_addr_offset.to_le_bytes());
        buf.extend_from_slice(&[0u8; 16]); // service_site_guid

        // String buffer
        buf.extend_from_slice(&dfs_path);
        buf.extend_from_slice(&alt_path);
        buf.extend_from_slice(&net_addr);

        let mut cursor = ReadCursor::new(&buf);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();

        assert_eq!(resp.path_consumed, 20);
        assert_eq!(resp.header_flags.bits(), 3);
        assert_eq!(resp.entries.len(), 1);

        assert_eq!(
            resp.entries[0],
            DfsReferralEntry::Target {
                version: 3,
                server_type: 1,
                flags: ReferralEntryFlags::default(),
                ttl: 600,
                dfs_path: r"\dom\share".to_string(),
                dfs_alternate_path: r"\dom\share".to_string(),
                network_address: r"\srv\share".to_string(),
            }
        );
    }

    #[test]
    fn resp_parse_v2_referral() {
        // Manually constructed V2 response: one entry.
        let dfs_path = encode_null_utf16(r"\domain\dfs");
        let alt_path = encode_null_utf16(r"\domain\dfs");
        let net_addr = encode_null_utf16(r"\server\data");

        let entry_fixed_size: u16 = 22; // 4 + 2+2+4+4 + 2+2+2 = 22
        let dfs_path_offset = entry_fixed_size;
        let alt_path_offset = dfs_path_offset + dfs_path.len() as u16;
        let net_addr_offset = alt_path_offset + alt_path.len() as u16;

        let mut buf = Vec::new();
        // Response header
        buf.extend_from_slice(&24u16.to_le_bytes()); // path_consumed
        buf.extend_from_slice(&1u16.to_le_bytes()); // number_of_referrals
        buf.extend_from_slice(&1u32.to_le_bytes()); // header_flags (ReferralServers)

        // Entry
        buf.extend_from_slice(&2u16.to_le_bytes()); // version
        buf.extend_from_slice(&entry_fixed_size.to_le_bytes()); // size
        buf.extend_from_slice(&0u16.to_le_bytes()); // server_type
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&0u32.to_le_bytes()); // proximity
        buf.extend_from_slice(&300u32.to_le_bytes()); // ttl
        buf.extend_from_slice(&dfs_path_offset.to_le_bytes());
        buf.extend_from_slice(&alt_path_offset.to_le_bytes());
        buf.extend_from_slice(&net_addr_offset.to_le_bytes());

        // String buffer
        buf.extend_from_slice(&dfs_path);
        buf.extend_from_slice(&alt_path);
        buf.extend_from_slice(&net_addr);

        let mut cursor = ReadCursor::new(&buf);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();

        assert_eq!(resp.path_consumed, 24);
        assert_eq!(resp.header_flags.bits(), 1);
        assert_eq!(resp.entries.len(), 1);

        assert_eq!(
            resp.entries[0],
            DfsReferralEntry::Target {
                version: 2,
                server_type: 0,
                flags: ReferralEntryFlags::default(),
                ttl: 300,
                dfs_path: r"\domain\dfs".to_string(),
                dfs_alternate_path: r"\domain\dfs".to_string(),
                network_address: r"\server\data".to_string(),
            }
        );
    }

    #[test]
    fn resp_parse_empty() {
        // Zero referral entries.
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u16.to_le_bytes()); // path_consumed
        buf.extend_from_slice(&0u16.to_le_bytes()); // number_of_referrals
        buf.extend_from_slice(&0u32.to_le_bytes()); // header_flags

        let mut cursor = ReadCursor::new(&buf);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();
        assert_eq!(resp.path_consumed, 0);
        assert_eq!(resp.header_flags, ReferralHeaderFlags::default());
        assert!(resp.entries.is_empty());
    }

    #[test]
    fn resp_parse_multiple_entries() {
        // Two V2 entries with different targets.
        // Layout: [entry1 fixed][entry2 fixed][strings for entry1][strings for entry2]
        // Offsets are relative to each entry's start.
        let dfs_path = encode_null_utf16(r"\ns\link");
        let alt_path = encode_null_utf16(r"\ns\link");
        let net_addr_1 = encode_null_utf16(r"\srv1\data");
        let net_addr_2 = encode_null_utf16(r"\srv2\data");

        let entry_fixed_size: u16 = 22;
        let total_fixed: u16 = entry_fixed_size * 2; // both entries' fixed parts

        // Entry 1 string offsets (relative to entry 1 start = 0 in entry_data).
        // Strings start after both entries' fixed parts.
        let e1_dfs_offset = total_fixed; // 44
        let e1_alt_offset = e1_dfs_offset + dfs_path.len() as u16;
        let e1_net_offset = e1_alt_offset + alt_path.len() as u16;
        let e1_strings_end = e1_net_offset + net_addr_1.len() as u16;

        // Entry 2 string offsets (relative to entry 2 start = 22 in entry_data).
        let e2_dfs_offset = e1_strings_end - entry_fixed_size; // offset from entry 2 start
        let e2_alt_offset = e2_dfs_offset + dfs_path.len() as u16;
        let e2_net_offset = e2_alt_offset + alt_path.len() as u16;

        let mut buf = Vec::new();
        // Response header
        buf.extend_from_slice(&16u16.to_le_bytes()); // path_consumed
        buf.extend_from_slice(&2u16.to_le_bytes()); // number_of_referrals
        buf.extend_from_slice(&0u32.to_le_bytes()); // header_flags

        // Entry 1 fixed part
        buf.extend_from_slice(&2u16.to_le_bytes()); // version
        buf.extend_from_slice(&entry_fixed_size.to_le_bytes()); // size
        buf.extend_from_slice(&0u16.to_le_bytes()); // server_type
        buf.extend_from_slice(&0u16.to_le_bytes()); // flags
        buf.extend_from_slice(&0u32.to_le_bytes()); // proximity
        buf.extend_from_slice(&120u32.to_le_bytes()); // ttl
        buf.extend_from_slice(&e1_dfs_offset.to_le_bytes());
        buf.extend_from_slice(&e1_alt_offset.to_le_bytes());
        buf.extend_from_slice(&e1_net_offset.to_le_bytes());

        // Entry 2 fixed part
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&entry_fixed_size.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // server_type = root
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&240u32.to_le_bytes());
        buf.extend_from_slice(&e2_dfs_offset.to_le_bytes());
        buf.extend_from_slice(&e2_alt_offset.to_le_bytes());
        buf.extend_from_slice(&e2_net_offset.to_le_bytes());

        // String buffer for entry 1
        buf.extend_from_slice(&dfs_path);
        buf.extend_from_slice(&alt_path);
        buf.extend_from_slice(&net_addr_1);

        // String buffer for entry 2
        buf.extend_from_slice(&dfs_path);
        buf.extend_from_slice(&alt_path);
        buf.extend_from_slice(&net_addr_2);

        let mut cursor = ReadCursor::new(&buf);
        let resp = RespGetDfsReferral::unpack(&mut cursor).unwrap();

        assert_eq!(resp.entries.len(), 2);
        assert_eq!(resp.entries[0].ttl(), 120);
        assert_eq!(resp.entries[0].target_address(), Some(r"\srv1\data"));
        assert_eq!(resp.entries[1].ttl(), 240);
        assert_eq!(resp.entries[1].target_address(), Some(r"\srv2\data"));
    }

    #[test]
    fn resp_parse_unsupported_version() {
        let mut buf = Vec::new();
        // Response header
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        buf.extend_from_slice(&0u32.to_le_bytes());
        // Entry with version 5 (past everything MS-DFSC defines)
        buf.extend_from_slice(&5u16.to_le_bytes()); // version
        buf.extend_from_slice(&8u16.to_le_bytes()); // size
        buf.extend_from_slice(&[0u8; 4]); // padding to reach size

        let mut cursor = ReadCursor::new(&buf);
        let result = RespGetDfsReferral::unpack(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unsupported DFS referral version"),
            "error was: {err}"
        );
    }

    #[test]
    fn resp_parse_truncated_header() {
        // Only 4 bytes -- missing header_flags.
        let buf = [0x00, 0x00, 0x01, 0x00];
        let mut cursor = ReadCursor::new(&buf);
        assert!(RespGetDfsReferral::unpack(&mut cursor).is_err());
    }

    /// Regression: fuzz-found crash. A V2 entry that claims `entry_size = 16`
    /// used to panic inside the entry-body read. The V2 body needs 18 bytes
    /// (server_type+flags+proximity+ttl + three u16 offsets), but the guard
    /// only ensured 16 bytes were available, so the final offset read would
    /// slip past the buffer. See fuzz target
    /// `fuzz_dfs_referral_response_parse` crash
    /// `a6933afd5a1ccec7166d914caed66154416a2fcb`.
    #[test]
    fn resp_parse_v2_short_entry_returns_clean_error() {
        let crash_input: [u8; 28] = [
            0x10, 0x00, 0x01, 0x00, 0x22, 0x23, 0x00, 0x03, // header
            0x02, 0x00, 0x10, 0x00, 0x01, 0x00, 0x22, 0x23, // v2 entry start (size=16)
            0x00, 0x03, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, // body bytes
            0x00, 0x00, 0x00, 0x00, // tail
        ];
        let mut cursor = ReadCursor::new(&crash_input);
        let result = RespGetDfsReferral::unpack(&mut cursor);
        assert!(result.is_err(), "expected clean error, got {result:?}");
    }

    // ── Test helpers ──────────────────────────────────────────────────

    /// Decode a hex string (no spaces, no 0x prefix) into bytes.
    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Encode a string as null-terminated UTF-16LE bytes.
    fn encode_null_utf16(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for cu in s.encode_utf16() {
            out.extend_from_slice(&cu.to_le_bytes());
        }
        out.extend_from_slice(&[0x00, 0x00]); // null terminator
        out
    }
}

#[cfg(test)]
mod roundtrip_props {
    use super::*;
    use crate::msg::roundtrip_strategies::arb_utf16_string;
    use proptest::prelude::*;

    /// Generate a UTF-16 string without interior null (U+0000). The encoder
    /// terminates with a 0x0000 code unit, so an interior null would end
    /// the string early on decode.
    fn arb_utf16_no_nul(max: usize) -> impl Strategy<Value = String> {
        arb_utf16_string(max).prop_filter("string must not contain interior U+0000", |s| {
            !s.contains('\0')
        })
    }

    proptest! {
        #[test]
        fn req_get_dfs_referral_pack_unpack(
            max_referral_level in any::<u16>(),
            request_file_name in arb_utf16_no_nul(128),
        ) {
            let original = ReqGetDfsReferral {
                max_referral_level,
                request_file_name,
            };
            let mut w = WriteCursor::new();
            original.pack(&mut w);
            let bytes = w.into_inner();

            let mut r = ReadCursor::new(&bytes);
            let decoded = ReqGetDfsReferral::unpack(&mut r).unwrap();
            prop_assert_eq!(decoded, original);
            prop_assert!(r.is_empty());
        }
    }
}
