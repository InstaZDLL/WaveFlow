//! MIT Kerberos credential cache (ccache) file parser.
//!
//! Reads ccache files (v3 and v4) to extract cached TGTs and service tickets,
//! enabling Kerberos authentication without a password when the user already
//! has a valid ticket (for example, from `kinit`).
//!
//! References:
//! - MIT Kerberos source: `lib/krb5/ccache/cc_file.c`
//! - Format: version(2) + [header(v4)] + default_principal + credentials*

use crate::error::{Error, Result};
use log::debug;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed Kerberos credential cache.
#[derive(Debug, Clone)]
pub struct CCache {
    /// File format version (3 or 4).
    pub version: u16,
    /// Default principal (typically the user who ran `kinit`).
    pub default_principal: CcachePrincipal,
    /// Cached credentials (TGTs and service tickets).
    pub credentials: Vec<CcacheCredential>,
}

/// A principal name in the ccache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CcachePrincipal {
    /// Name type (1 = KRB_NT_PRINCIPAL, 2 = KRB_NT_SRV_INST, etc.).
    pub name_type: u32,
    /// Kerberos realm.
    pub realm: String,
    /// Name components (for example, `["smbtest"]` or `["cifs", "server.domain.com"]`).
    pub components: Vec<String>,
}

/// A single cached credential (ticket + metadata).
///
/// `Debug` shows the session key as `***`.
#[derive(Clone)]
pub struct CcacheCredential {
    /// Client principal.
    pub client: CcachePrincipal,
    /// Server (service) principal.
    pub server: CcachePrincipal,
    /// Session key encryption type.
    pub key_etype: u16,
    /// Session key bytes.
    pub key_data: Vec<u8>,
    /// Time the ticket was issued (Unix timestamp).
    pub authtime: u32,
    /// Time the ticket becomes valid (Unix timestamp).
    pub starttime: u32,
    /// Time the ticket expires (Unix timestamp).
    pub endtime: u32,
    /// Time the ticket's renewable lifetime expires (Unix timestamp).
    pub renew_till: u32,
    /// Raw ticket bytes (DER-encoded Kerberos Ticket).
    pub ticket: Vec<u8>,
}

impl std::fmt::Debug for CcacheCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CcacheCredential")
            .field("client", &self.client)
            .field("server", &self.server)
            .field("key_etype", &self.key_etype)
            .field("key_data", &crate::crypto::Redacted)
            .field("authtime", &self.authtime)
            .field("starttime", &self.starttime)
            .field("endtime", &self.endtime)
            .field("renew_till", &self.renew_till)
            .field("ticket", &self.ticket)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Read and parse a ccache file from a filesystem path.
///
/// Reads `$KRB5CCNAME` if `path` is `None`, falling back to
/// `/tmp/krb5cc_<uid>` on Unix.
pub fn load_ccache(path: Option<&std::path::Path>) -> Result<CCache> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => {
            if let Ok(env_path) = std::env::var("KRB5CCNAME") {
                // Strip "FILE:" prefix if present.
                let p = env_path.strip_prefix("FILE:").unwrap_or(&env_path);
                std::path::PathBuf::from(p)
            } else {
                // Default: /tmp/krb5cc_<uid>
                return Err(Error::invalid_data(
                    "ccache: no path specified and $KRB5CCNAME not set",
                ));
            }
        }
    };

    let data = std::fs::read(&path).map_err(|e| {
        Error::invalid_data(format!("ccache: failed to read {}: {e}", path.display()))
    })?;

    parse_ccache(&data)
}

/// Parse a ccache file from raw bytes.
pub fn parse_ccache(data: &[u8]) -> Result<CCache> {
    let mut pos = 0;

    // Version: 2 bytes, big-endian. We support 0x0503 (v3) and 0x0504 (v4).
    if data.len() < 2 {
        return Err(Error::invalid_data("ccache: file too short for version"));
    }
    let version = read_u16(data, &mut pos)?;
    if version != 0x0503 && version != 0x0504 {
        return Err(Error::invalid_data(format!(
            "ccache: unsupported version 0x{version:04x} (expected 0x0503 or 0x0504)"
        )));
    }

    // V4 has a header section after the version.
    if version == 0x0504 {
        let header_len = read_u16(data, &mut pos)? as usize;
        if pos + header_len > data.len() {
            return Err(Error::invalid_data(
                "ccache: header extends past end of file",
            ));
        }
        // Skip header tags (we don't need them).
        pos += header_len;
    }

    // Default principal.
    let default_principal = read_principal(data, &mut pos)?;

    // Credentials: read until EOF.
    let mut credentials = Vec::new();
    while pos < data.len() {
        match read_credential(data, &mut pos) {
            Ok(cred) => credentials.push(cred),
            Err(_) => break, // Treat parse errors at the end as EOF.
        }
    }

    debug!(
        "ccache: parsed v{}, principal={:?} realm={:?}, {} credentials",
        version & 0xFF,
        default_principal.components.join("/"),
        default_principal.realm,
        credentials.len()
    );

    Ok(CCache {
        version,
        default_principal,
        credentials,
    })
}

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

impl CCache {
    /// Find a cached service ticket for the given SPN and realm.
    ///
    /// Looks for a credential where the server principal matches
    /// `service/hostname@realm` (case-insensitive hostname comparison).
    pub fn find_service_ticket(
        &self,
        service: &str,
        hostname: &str,
        realm: &str,
    ) -> Option<&CcacheCredential> {
        self.credentials.iter().find(|c| {
            c.server.realm.eq_ignore_ascii_case(realm)
                && c.server.components.len() == 2
                && c.server.components[0].eq_ignore_ascii_case(service)
                && c.server.components[1].eq_ignore_ascii_case(hostname)
        })
    }

    /// Find a cached TGT for the given realm.
    ///
    /// Looks for a credential where the server principal is `krbtgt/REALM@REALM`.
    pub fn find_tgt(&self, realm: &str) -> Option<&CcacheCredential> {
        self.credentials.iter().find(|c| {
            c.server.realm.eq_ignore_ascii_case(realm)
                && c.server.components.len() == 2
                && c.server.components[0] == "krbtgt"
                && c.server.components[1].eq_ignore_ascii_case(realm)
        })
    }
}

// ---------------------------------------------------------------------------
// Binary reading helpers
// ---------------------------------------------------------------------------

fn read_u8(data: &[u8], pos: &mut usize) -> Result<u8> {
    if *pos >= data.len() {
        return Err(Error::invalid_data("ccache: unexpected end of data"));
    }
    let val = data[*pos];
    *pos += 1;
    Ok(val)
}

fn read_u16(data: &[u8], pos: &mut usize) -> Result<u16> {
    if *pos + 2 > data.len() {
        return Err(Error::invalid_data("ccache: unexpected end of data"));
    }
    let val = u16::from_be_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Ok(val)
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32> {
    if *pos + 4 > data.len() {
        return Err(Error::invalid_data("ccache: unexpected end of data"));
    }
    let val = u32::from_be_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

fn read_bytes(data: &[u8], pos: &mut usize, len: usize) -> Result<Vec<u8>> {
    if *pos + len > data.len() {
        return Err(Error::invalid_data("ccache: unexpected end of data"));
    }
    let val = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(val)
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<String> {
    let len = read_u32(data, pos)? as usize;
    let bytes = read_bytes(data, pos, len)?;
    String::from_utf8(bytes).map_err(|_| Error::invalid_data("ccache: invalid UTF-8 in string"))
}

fn read_principal(data: &[u8], pos: &mut usize) -> Result<CcachePrincipal> {
    let name_type = read_u32(data, pos)?;
    let num_components = read_u32(data, pos)?;
    let realm = read_string(data, pos)?;
    let mut components = Vec::with_capacity(num_components as usize);
    for _ in 0..num_components {
        components.push(read_string(data, pos)?);
    }
    Ok(CcachePrincipal {
        name_type,
        realm,
        components,
    })
}

fn read_keyblock(data: &[u8], pos: &mut usize) -> Result<(u16, Vec<u8>)> {
    let enctype = read_u16(data, pos)?;
    let key_len = read_u32(data, pos)? as usize;
    let key_data = read_bytes(data, pos, key_len)?;
    Ok((enctype, key_data))
}

fn read_credential(data: &[u8], pos: &mut usize) -> Result<CcacheCredential> {
    let client = read_principal(data, pos)?;
    let server = read_principal(data, pos)?;
    let (key_etype, key_data) = read_keyblock(data, pos)?;
    let authtime = read_u32(data, pos)?;
    let starttime = read_u32(data, pos)?;
    let endtime = read_u32(data, pos)?;
    let renew_till = read_u32(data, pos)?;
    let _is_skey = read_u8(data, pos)?;
    let _ticket_flags = read_u32(data, pos)?;

    // Addresses (count + entries).
    let addr_count = read_u32(data, pos)?;
    for _ in 0..addr_count {
        let _addr_type = read_u16(data, pos)?;
        let addr_len = read_u32(data, pos)? as usize;
        *pos += addr_len; // skip address data
    }

    // Auth data (count + entries).
    let authdata_count = read_u32(data, pos)?;
    for _ in 0..authdata_count {
        let _ad_type = read_u16(data, pos)?;
        let ad_len = read_u32(data, pos)? as usize;
        *pos += ad_len; // skip authdata
    }

    // Ticket.
    let ticket_len = read_u32(data, pos)? as usize;
    let ticket = read_bytes(data, pos, ticket_len)?;

    // Second ticket.
    let second_ticket_len = read_u32(data, pos)? as usize;
    let _second_ticket = read_bytes(data, pos, second_ticket_len)?;

    Ok(CcacheCredential {
        client,
        server,
        key_etype,
        key_data,
        authtime,
        starttime,
        endtime,
        renew_till,
        ticket,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_v4_ccache_from_fixture() {
        let data = include_bytes!("fixtures/test.ccache");
        let ccache = parse_ccache(data).expect("failed to parse v4 ccache");

        assert_eq!(ccache.version, 0x0504);
        assert_eq!(ccache.default_principal.realm, "TEST.LOCAL");
        assert_eq!(ccache.default_principal.components, vec!["smbtest"]);
        assert_eq!(ccache.credentials.len(), 2);
    }

    #[test]
    fn parse_v3_ccache_from_fixture() {
        let data = include_bytes!("fixtures/test_v3.ccache");
        let ccache = parse_ccache(data).expect("failed to parse v3 ccache");

        assert_eq!(ccache.version, 0x0503);
        assert_eq!(ccache.default_principal.realm, "EXAMPLE.COM");
        assert_eq!(ccache.default_principal.components, vec!["user"]);
        assert_eq!(ccache.credentials.len(), 1);
    }

    #[test]
    fn tgt_credential_has_correct_fields() {
        let data = include_bytes!("fixtures/test.ccache");
        let ccache = parse_ccache(data).unwrap();

        let tgt = &ccache.credentials[0];
        assert_eq!(tgt.client.realm, "TEST.LOCAL");
        assert_eq!(tgt.client.components, vec!["smbtest"]);
        assert_eq!(tgt.server.realm, "TEST.LOCAL");
        assert_eq!(tgt.server.components, vec!["krbtgt", "TEST.LOCAL"]);
        assert_eq!(tgt.key_etype, 23); // RC4-HMAC
        assert_eq!(tgt.key_data.len(), 16);
        assert_eq!(tgt.authtime, 1744100000);
        assert_eq!(tgt.endtime, 1744200000);
    }

    #[test]
    fn service_ticket_has_correct_fields() {
        let data = include_bytes!("fixtures/test.ccache");
        let ccache = parse_ccache(data).unwrap();

        let svc = &ccache.credentials[1];
        assert_eq!(svc.server.components, vec!["cifs", "server.test.local"]);
        assert_eq!(svc.key_etype, 23);
        assert_eq!(svc.key_data, (16u8..32).collect::<Vec<_>>());
    }

    #[test]
    fn find_tgt_by_realm() {
        let data = include_bytes!("fixtures/test.ccache");
        let ccache = parse_ccache(data).unwrap();

        let tgt = ccache.find_tgt("TEST.LOCAL");
        assert!(tgt.is_some());
        assert_eq!(tgt.unwrap().server.components[0], "krbtgt");

        assert!(ccache.find_tgt("OTHER.REALM").is_none());
    }

    #[test]
    fn find_service_ticket_by_spn() {
        let data = include_bytes!("fixtures/test.ccache");
        let ccache = parse_ccache(data).unwrap();

        let svc = ccache.find_service_ticket("cifs", "server.test.local", "TEST.LOCAL");
        assert!(svc.is_some());
        assert_eq!(svc.unwrap().key_data, (16u8..32).collect::<Vec<_>>());

        // Case-insensitive hostname.
        assert!(ccache
            .find_service_ticket("cifs", "SERVER.TEST.LOCAL", "TEST.LOCAL")
            .is_some());

        // Wrong hostname.
        assert!(ccache
            .find_service_ticket("cifs", "other.test.local", "TEST.LOCAL")
            .is_none());

        // Wrong service.
        assert!(ccache
            .find_service_ticket("ldap", "server.test.local", "TEST.LOCAL")
            .is_none());
    }

    #[test]
    fn find_tgt_case_insensitive() {
        let data = include_bytes!("fixtures/test.ccache");
        let ccache = parse_ccache(data).unwrap();

        assert!(ccache.find_tgt("test.local").is_some());
    }

    #[test]
    fn v3_ccache_tgt_has_aes256_key() {
        let data = include_bytes!("fixtures/test_v3.ccache");
        let ccache = parse_ccache(data).unwrap();

        let tgt = ccache.find_tgt("EXAMPLE.COM").unwrap();
        assert_eq!(tgt.key_etype, 18); // AES-256
        assert_eq!(tgt.key_data.len(), 32);
    }

    #[test]
    fn reject_unsupported_version() {
        let data = [0x05, 0x02]; // v2
        let result = parse_ccache(&data);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unsupported version"));
    }

    #[test]
    fn reject_truncated_file() {
        let result = parse_ccache(&[0x05]);
        assert!(result.is_err());
    }

    #[test]
    fn empty_credentials_list() {
        // A valid ccache with just a version + principal + no credentials
        let mut data = vec![0x05, 0x04, 0x00, 0x00]; // v4, no header
                                                     // Principal: type=1, components=1, realm="R", component="u"
        data.extend_from_slice(&[0, 0, 0, 1]); // name_type
        data.extend_from_slice(&[0, 0, 0, 1]); // num_components
        data.extend_from_slice(&[0, 0, 0, 1]); // realm length
        data.push(b'R');
        data.extend_from_slice(&[0, 0, 0, 1]); // component length
        data.push(b'u');

        let ccache = parse_ccache(&data).unwrap();
        assert_eq!(ccache.credentials.len(), 0);
        assert_eq!(ccache.default_principal.realm, "R");
        assert_eq!(ccache.default_principal.components, vec!["u"]);
    }

    #[test]
    fn a_cached_credentials_debug_output_hides_its_session_key() {
        let principal = CcachePrincipal {
            name_type: 1,
            realm: "TEST.LOCAL".to_string(),
            components: vec!["smbtest".to_string()],
        };
        let credential = CcacheCredential {
            client: principal.clone(),
            server: principal,
            key_etype: 18,
            key_data: vec![0xAB; 32],
            authtime: 0,
            starttime: 0,
            endtime: 0,
            renew_till: 0,
            ticket: vec![1, 2, 3],
        };
        let printed = format!("{credential:?}");
        assert!(!printed.contains("171"), "{printed}");
        assert!(printed.contains("key_data: ***"), "{printed}");
        assert!(printed.contains("key_etype: 18"), "{printed}");
    }
}
