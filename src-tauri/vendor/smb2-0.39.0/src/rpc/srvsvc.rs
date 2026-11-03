//! NetShareEnumAll NDR encoding/decoding for the srvsvc interface.
//!
//! Encodes the NetrShareEnum request (opnum 15) and decodes the response,
//! extracting share names, types, and comments.

use crate::error::Result;
use crate::pack::{ReadCursor, WriteCursor};
use crate::Error;

/// Share type: disk share.
pub const STYPE_DISKTREE: u32 = 0x0000_0000;
/// Share type: printer queue.
pub const STYPE_PRINTQ: u32 = 0x0000_0001;
/// Share type: device.
pub const STYPE_DEVICE: u32 = 0x0000_0002;
/// Share type: IPC (inter-process communication).
pub const STYPE_IPC: u32 = 0x0000_0003;
/// Share type modifier: special/admin share (combined with above via OR).
pub const STYPE_SPECIAL: u32 = 0x8000_0000;

/// Mask for the base share type (low bits).
const STYPE_BASE_MASK: u32 = 0x0000_FFFF;

/// Information about a single network share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareInfo {
    /// The share name (for example, "Documents" or "IPC$").
    pub name: String,
    /// The share type as a raw u32 (see `STYPE_*` constants).
    pub share_type: u32,
    /// An optional comment/description for the share.
    pub comment: String,
}

/// Build the NDR-encoded stub data for a NetShareEnumAll request.
///
/// The stub is meant to be wrapped in an RPC REQUEST PDU with opnum 15.
pub fn build_net_share_enum_all_stub(server_name: &str) -> Vec<u8> {
    let mut w = WriteCursor::with_capacity(128);

    // ServerName: NDR unique pointer to conformant+varying string (UTF-16LE, null-terminated)
    // Referent ID (non-null pointer)
    w.write_u32_le(0x0002_0000); // referent ID

    // Encode the server name as a conformant+varying NDR string
    let name_utf16: Vec<u16> = server_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let char_count = name_utf16.len() as u32;

    // MaxCount
    w.write_u32_le(char_count);
    // Offset
    w.write_u32_le(0);
    // ActualCount
    w.write_u32_le(char_count);
    // String data (UTF-16LE)
    for &code_unit in &name_utf16 {
        w.write_u16_le(code_unit);
    }
    // Align to 4 bytes after string data
    w.align_to(4);

    // InfoStruct: SHARE_ENUM_STRUCT
    // Level = 1 (we want SHARE_INFO_1)
    w.write_u32_le(1);

    // ShareInfo union discriminant = 1 (matches level)
    w.write_u32_le(1);

    // Pointer to SHARE_INFO_1_CONTAINER (unique pointer)
    w.write_u32_le(0x0002_0004); // referent ID

    // SHARE_INFO_1_CONTAINER (deferred pointer data)
    // EntriesRead = 0 (server fills this)
    w.write_u32_le(0);
    // Buffer pointer = NULL (let server allocate)
    w.write_u32_le(0);

    // PreferedMaximumLength = 0xFFFFFFFF (no limit)
    w.write_u32_le(0xFFFF_FFFF);

    // ResumeHandle: unique pointer to u32
    // NULL pointer (no resume)
    w.write_u32_le(0);

    w.into_inner()
}

/// Build a complete RPC REQUEST PDU for NetShareEnumAll.
///
/// Combines the RPC REQUEST header (opnum 15) with the NDR stub data.
pub fn build_net_share_enum_all(call_id: u32, server_name: &str) -> Vec<u8> {
    let stub = build_net_share_enum_all_stub(server_name);
    super::build_request(call_id, 15, &stub)
}

/// Parse the NDR stub data from a NetShareEnumAll RPC RESPONSE.
///
/// Extracts all share entries from the response. The caller should use
/// [`filter_disk_shares`] to get only disk shares.
pub fn parse_net_share_enum_all_response(data: &[u8]) -> Result<Vec<ShareInfo>> {
    // First, parse the RPC RESPONSE envelope to get the stub data
    let stub = super::parse_response(data)?;
    parse_net_share_enum_all_stub(stub)
}

/// Parse the NDR stub data directly (without the RPC envelope).
///
/// Used by the share-enumeration reassembly path, which concatenates the stub
/// of each RPC fragment before decoding.
pub(crate) fn parse_net_share_enum_all_stub(stub: &[u8]) -> Result<Vec<ShareInfo>> {
    let mut r = ReadCursor::new(stub);

    // Level (u32) -- should be 1
    let level = r.read_u32_le()?;
    if level != 1 {
        return Err(Error::invalid_data(format!(
            "expected share info level 1, got {level}"
        )));
    }

    // Union discriminant (u32) -- should be 1
    let discriminant = r.read_u32_le()?;
    if discriminant != 1 {
        return Err(Error::invalid_data(format!(
            "expected union discriminant 1, got {discriminant}"
        )));
    }

    // Pointer to SHARE_INFO_1_CONTAINER
    let container_ptr = r.read_u32_le()?;
    if container_ptr == 0 {
        return Ok(Vec::new());
    }

    // SHARE_INFO_1_CONTAINER
    let count = r.read_u32_le()?;

    // Pointer to array of SHARE_INFO_1
    let array_ptr = r.read_u32_le()?;
    if array_ptr == 0 || count == 0 {
        return Ok(Vec::new());
    }

    // Array: MaxCount header
    let max_count = r.read_u32_le()?;
    if max_count < count {
        return Err(Error::invalid_data(format!(
            "array max_count ({max_count}) < entries ({count})"
        )));
    }

    // Read the fixed-size parts of each SHARE_INFO_1 entry:
    // Each entry has: name_ptr (u32), type (u32), comment_ptr (u32)
    struct RawEntry {
        name_ptr: u32,
        share_type: u32,
        comment_ptr: u32,
    }

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_ptr = r.read_u32_le()?;
        let share_type = r.read_u32_le()?;
        let comment_ptr = r.read_u32_le()?;
        entries.push(RawEntry {
            name_ptr,
            share_type,
            comment_ptr,
        });
    }

    // Now read the deferred pointer data (conformant+varying strings)
    let mut shares = Vec::with_capacity(count as usize);
    for entry in &entries {
        let name = if entry.name_ptr != 0 {
            read_ndr_string(&mut r)?
        } else {
            String::new()
        };

        let comment = if entry.comment_ptr != 0 {
            read_ndr_string(&mut r)?
        } else {
            String::new()
        };

        shares.push(ShareInfo {
            name,
            share_type: entry.share_type,
            comment,
        });
    }

    Ok(shares)
}

/// Read an NDR conformant+varying UTF-16LE string from the cursor.
///
/// Format: MaxCount(u32) + Offset(u32) + ActualCount(u32) + UTF-16LE data.
/// The string is null-terminated on the wire; we strip the null.
fn read_ndr_string(r: &mut ReadCursor<'_>) -> Result<String> {
    let _max_count = r.read_u32_le()?;
    let _offset = r.read_u32_le()?;
    let actual_count = r.read_u32_le()?;

    if actual_count == 0 {
        return Ok(String::new());
    }

    let byte_len = actual_count as usize * 2;
    let s = r.read_utf16_le(byte_len)?;

    // Align to 4 bytes after reading string data
    let pos = r.position();
    let padding = (4 - (pos % 4)) % 4;
    if padding > 0 && r.remaining() >= padding {
        r.skip(padding)?;
    }

    // Strip trailing null
    Ok(s.trim_end_matches('\0').to_string())
}

/// Filter shares, keeping only disk shares and excluding admin shares (ending with `$`).
pub fn filter_disk_shares(shares: Vec<ShareInfo>) -> Vec<ShareInfo> {
    shares
        .into_iter()
        .filter(|s| {
            let base_type = s.share_type & STYPE_BASE_MASK;
            let is_disk = base_type == STYPE_DISKTREE;
            let is_special = (s.share_type & STYPE_SPECIAL) != 0;
            let ends_with_dollar = s.name.ends_with('$');
            is_disk && !is_special && !ends_with_dollar
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_has_opnum_15() {
        let pdu = build_net_share_enum_all(1, r"\\server");
        // OpNum is at offset 22 in the RPC REQUEST PDU
        let opnum = u16::from_le_bytes([pdu[22], pdu[23]]);
        assert_eq!(opnum, 15);
    }

    #[test]
    fn build_request_stub_contains_server_name() {
        let stub = build_net_share_enum_all_stub(r"\\server");
        // The server name should appear as UTF-16LE somewhere in the stub
        let expected_utf16: Vec<u8> = r"\\server"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();

        let found = stub
            .windows(expected_utf16.len())
            .any(|window| window == expected_utf16.as_slice());
        assert!(found, "stub should contain the server name in UTF-16LE");
    }

    #[test]
    fn parse_response_with_three_shares() {
        let response_pdu = build_test_enum_response(&[
            ("Documents", STYPE_DISKTREE, "Shared docs"),
            ("IPC$", STYPE_IPC | STYPE_SPECIAL, "Remote IPC"),
            ("C$", STYPE_DISKTREE | STYPE_SPECIAL, "Default share"),
        ]);

        let shares = parse_net_share_enum_all_response(&response_pdu).unwrap();
        assert_eq!(shares.len(), 3);
        assert_eq!(shares[0].name, "Documents");
        assert_eq!(shares[0].share_type, STYPE_DISKTREE);
        assert_eq!(shares[0].comment, "Shared docs");
        assert_eq!(shares[1].name, "IPC$");
        assert_eq!(shares[2].name, "C$");
    }

    #[test]
    fn filter_keeps_disk_shares() {
        let shares = vec![
            ShareInfo {
                name: "Documents".to_string(),
                share_type: STYPE_DISKTREE,
                comment: "Shared docs".to_string(),
            },
            ShareInfo {
                name: "Photos".to_string(),
                share_type: STYPE_DISKTREE,
                comment: String::new(),
            },
        ];

        let filtered = filter_disk_shares(shares);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_removes_ipc() {
        let shares = vec![ShareInfo {
            name: "IPC$".to_string(),
            share_type: STYPE_IPC | STYPE_SPECIAL,
            comment: "Remote IPC".to_string(),
        }];

        let filtered = filter_disk_shares(shares);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_removes_admin_shares() {
        let shares = vec![
            ShareInfo {
                name: "C$".to_string(),
                share_type: STYPE_DISKTREE | STYPE_SPECIAL,
                comment: "Default share".to_string(),
            },
            ShareInfo {
                name: "ADMIN$".to_string(),
                share_type: STYPE_DISKTREE | STYPE_SPECIAL,
                comment: "Remote Admin".to_string(),
            },
        ];

        let filtered = filter_disk_shares(shares);
        assert!(filtered.is_empty());
    }

    #[test]
    fn filter_mixed_shares() {
        let shares = vec![
            ShareInfo {
                name: "Documents".to_string(),
                share_type: STYPE_DISKTREE,
                comment: "Shared docs".to_string(),
            },
            ShareInfo {
                name: "IPC$".to_string(),
                share_type: STYPE_IPC | STYPE_SPECIAL,
                comment: "Remote IPC".to_string(),
            },
            ShareInfo {
                name: "C$".to_string(),
                share_type: STYPE_DISKTREE | STYPE_SPECIAL,
                comment: "Default share".to_string(),
            },
            ShareInfo {
                name: "Photos".to_string(),
                share_type: STYPE_DISKTREE,
                comment: String::new(),
            },
            ShareInfo {
                name: "Printer".to_string(),
                share_type: STYPE_PRINTQ,
                comment: "Office printer".to_string(),
            },
        ];

        let filtered = filter_disk_shares(shares);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].name, "Documents");
        assert_eq!(filtered[1].name, "Photos");
    }

    #[test]
    fn parse_empty_share_list() {
        let response_pdu = build_test_enum_response(&[]);
        let shares = parse_net_share_enum_all_response(&response_pdu).unwrap();
        assert!(shares.is_empty());
    }

    #[test]
    fn parse_share_with_unicode_name() {
        let response_pdu = build_test_enum_response(&[(
            "\u{00C4}rchive",
            STYPE_DISKTREE,
            "Archiv f\u{00FC}r Dateien",
        )]);

        let shares = parse_net_share_enum_all_response(&response_pdu).unwrap();
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0].name, "\u{00C4}rchive");
        assert_eq!(shares[0].comment, "Archiv f\u{00FC}r Dateien");
    }

    #[test]
    fn parse_share_with_cjk_characters() {
        let response_pdu = build_test_enum_response(&[(
            "\u{5171}\u{6709}",
            STYPE_DISKTREE,
            "\u{5171}\u{6709}\u{30D5}\u{30A9}\u{30EB}\u{30C0}",
        )]);

        let shares = parse_net_share_enum_all_response(&response_pdu).unwrap();
        assert_eq!(shares.len(), 1);
        assert_eq!(shares[0].name, "\u{5171}\u{6709}");
        assert_eq!(
            shares[0].comment,
            "\u{5171}\u{6709}\u{30D5}\u{30A9}\u{30EB}\u{30C0}"
        );
    }

    #[test]
    fn roundtrip_build_and_parse() {
        // Build a request, then manually construct a response and parse it
        let _request = build_net_share_enum_all(1, r"\\testserver");

        let response_pdu = build_test_enum_response(&[
            ("Share1", STYPE_DISKTREE, "First share"),
            ("Share2", STYPE_DISKTREE, "Second share"),
        ]);

        let shares = parse_net_share_enum_all_response(&response_pdu).unwrap();
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].name, "Share1");
        assert_eq!(shares[0].comment, "First share");
        assert_eq!(shares[1].name, "Share2");
        assert_eq!(shares[1].comment, "Second share");
    }

    #[test]
    fn filter_preserves_non_dollar_disk_shares_only() {
        // A share named "My$hare" (dollar in middle) should be kept
        let shares = vec![ShareInfo {
            name: "My$hare".to_string(),
            share_type: STYPE_DISKTREE,
            comment: String::new(),
        }];

        let filtered = filter_disk_shares(shares);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "My$hare");
    }

    // -- Test helpers --

    /// Write an NDR conformant+varying UTF-16LE string into the cursor.
    fn write_ndr_string(w: &mut WriteCursor, s: &str) {
        let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let char_count = utf16.len() as u32;

        w.write_u32_le(char_count); // MaxCount
        w.write_u32_le(0); // Offset
        w.write_u32_le(char_count); // ActualCount
        for &code_unit in &utf16 {
            w.write_u16_le(code_unit);
        }
        w.align_to(4);
    }

    /// Build a complete RPC RESPONSE PDU containing the given shares.
    ///
    /// This constructs valid NDR stub data wrapped in an RPC RESPONSE envelope.
    fn build_test_enum_response(shares: &[(&str, u32, &str)]) -> Vec<u8> {
        let stub = build_test_enum_stub(shares);
        build_test_response_pdu(1, &stub)
    }

    /// Build NDR stub data for a NetShareEnumAll response.
    fn build_test_enum_stub(shares: &[(&str, u32, &str)]) -> Vec<u8> {
        let mut w = WriteCursor::with_capacity(512);
        let count = shares.len() as u32;

        // Level = 1
        w.write_u32_le(1);
        // Union discriminant = 1
        w.write_u32_le(1);

        if count == 0 {
            // Null container pointer
            w.write_u32_le(0);
            // TotalEntries
            w.write_u32_le(0);
            // ResumeHandle pointer (null)
            w.write_u32_le(0);
            // Return value (Windows error code, 0 = success)
            w.write_u32_le(0);
            return w.into_inner();
        }

        // Container pointer (non-null)
        w.write_u32_le(0x0002_0000);

        // SHARE_INFO_1_CONTAINER
        w.write_u32_le(count); // EntriesRead
        w.write_u32_le(0x0002_0004); // Array pointer (non-null)

        // Array: MaxCount
        w.write_u32_le(count);

        // Fixed-size entries: name_ptr, type, comment_ptr
        for (i, &(_, share_type, _)) in shares.iter().enumerate() {
            w.write_u32_le(0x0002_0008 + (i as u32) * 2); // name referent ID
            w.write_u32_le(share_type);
            w.write_u32_le(0x0002_0108 + (i as u32) * 2); // comment referent ID
        }

        // Deferred string data (name then comment for each entry)
        for &(name, _, comment) in shares {
            write_ndr_string(&mut w, name);
            write_ndr_string(&mut w, comment);
        }

        // TotalEntries
        w.write_u32_le(count);
        // ResumeHandle pointer (null)
        w.write_u32_le(0);
        // Return value (0 = success)
        w.write_u32_le(0);

        w.into_inner()
    }

    /// Build a minimal RPC RESPONSE PDU wrapping stub data.
    fn build_test_response_pdu(call_id: u32, stub: &[u8]) -> Vec<u8> {
        use crate::pack::WriteCursor;

        let mut w = WriteCursor::with_capacity(24 + stub.len());

        // Common header
        w.write_u8(5); // Version
        w.write_u8(0); // VersionMinor
        w.write_u8(2); // PacketType = RESPONSE
        w.write_u8(0x03); // Flags (first + last)
        w.write_bytes(&[0x10, 0x00, 0x00, 0x00]); // DataRep
        let frag_len_pos = w.position();
        w.write_u16_le(0); // FragLength placeholder
        w.write_u16_le(0); // AuthLength
        w.write_u32_le(call_id);

        // RESPONSE specific
        w.write_u32_le(stub.len() as u32); // AllocHint
        w.write_u16_le(0); // ContextId
        w.write_u8(0); // CancelCount
        w.write_u8(0); // Reserved

        w.write_bytes(stub);

        let total_len = w.position();
        w.set_u16_le_at(frag_len_pos, total_len as u16);

        w.into_inner()
    }
}
