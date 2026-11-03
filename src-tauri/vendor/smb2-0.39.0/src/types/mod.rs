//! Newtypes, enums, and common data structures for SMB2/3 protocol fields.
//!
//! Most users don't need to import from this module directly -- the commonly
//! used types are re-exported at the crate root.

pub mod flags;
pub mod reparse;
pub mod status;

use std::fmt;

use crate::Error;

/// Requested oplock level (MS-SMB2 2.2.13, 2.2.23).
///
/// Used across CREATE requests/responses and oplock break messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OplockLevel {
    /// No oplock is requested.
    None = 0x00,
    /// Level II oplock is requested.
    LevelII = 0x01,
    /// Exclusive oplock is requested.
    Exclusive = 0x08,
    /// Batch oplock is requested.
    Batch = 0x09,
    /// Lease is requested.
    Lease = 0xFF,
}

impl TryFrom<u8> for OplockLevel {
    type Error = Error;

    fn try_from(value: u8) -> crate::error::Result<Self> {
        match value {
            0x00 => Ok(Self::None),
            0x01 => Ok(Self::LevelII),
            0x08 => Ok(Self::Exclusive),
            0x09 => Ok(Self::Batch),
            0xFF => Ok(Self::Lease),
            _ => Err(Error::invalid_data(format!(
                "invalid OplockLevel: 0x{:02X}",
                value
            ))),
        }
    }
}

/// 64-bit session identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
pub struct SessionId(pub u64);

impl SessionId {
    /// Sentinel value indicating no session.
    pub const NONE: Self = Self(0);
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SessionId(0x{:016X})", self.0)
    }
}

/// 64-bit message identifier for request/response correlation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
pub struct MessageId(pub u64);

impl MessageId {
    /// Unsolicited message ID used for oplock/lease break notifications.
    pub const UNSOLICITED: Self = Self(0xFFFF_FFFF_FFFF_FFFF);
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MessageId(0x{:016X})", self.0)
    }
}

/// 32-bit tree connect identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize), serde(transparent))]
pub struct TreeId(pub u32);

impl fmt::Display for TreeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TreeId(0x{:08X})", self.0)
    }
}

/// 16-bit credit charge for multi-credit requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CreditCharge(pub u16);

impl fmt::Display for CreditCharge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CreditCharge({})", self.0)
    }
}

/// 128-bit file identifier consisting of two 64-bit parts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FileId {
    /// Persistent portion of the file handle.
    pub persistent: u64,
    /// Volatile portion of the file handle.
    pub volatile: u64,
}

impl FileId {
    /// Sentinel value used in related compound requests.
    pub const SENTINEL: Self = Self {
        persistent: 0xFFFF_FFFF_FFFF_FFFF,
        volatile: 0xFFFF_FFFF_FFFF_FFFF,
    };
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FileId(0x{:016X}:0x{:016X})",
            self.persistent, self.volatile
        )
    }
}

/// SMB2 command codes from MS-SMB2 section 2.2.1.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, num_enum::TryFromPrimitive, num_enum::IntoPrimitive,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u16)]
pub enum Command {
    /// Negotiate protocol version and capabilities.
    Negotiate = 0x0000,
    /// Set up an authenticated session.
    SessionSetup = 0x0001,
    /// Log off a session.
    Logoff = 0x0002,
    /// Connect to a share.
    TreeConnect = 0x0003,
    /// Disconnect from a share.
    TreeDisconnect = 0x0004,
    /// Open or create a file.
    Create = 0x0005,
    /// Close a file handle.
    Close = 0x0006,
    /// Flush cached data to stable storage.
    Flush = 0x0007,
    /// Read data from a file.
    Read = 0x0008,
    /// Write data to a file.
    Write = 0x0009,
    /// Lock or unlock byte ranges.
    Lock = 0x000A,
    /// Issue a device control or file system control command.
    Ioctl = 0x000B,
    /// Cancel a previously sent request.
    Cancel = 0x000C,
    /// Check server liveness.
    Echo = 0x000D,
    /// Enumerate directory contents.
    QueryDirectory = 0x000E,
    /// Request change notifications on a directory.
    ChangeNotify = 0x000F,
    /// Query file or filesystem information.
    QueryInfo = 0x0010,
    /// Set file or filesystem information.
    SetInfo = 0x0011,
    /// Oplock or lease break notification/acknowledgment.
    OplockBreak = 0x0012,
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// SMB2 dialect revision identifiers from MS-SMB2 section 2.2.3.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    num_enum::TryFromPrimitive,
    num_enum::IntoPrimitive,
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[repr(u16)]
pub enum Dialect {
    /// SMB 2.0.2 dialect.
    Smb2_0_2 = 0x0202,
    /// SMB 2.1 dialect.
    Smb2_1 = 0x0210,
    /// SMB 3.0 dialect.
    Smb3_0 = 0x0300,
    /// SMB 3.0.2 dialect.
    Smb3_0_2 = 0x0302,
    /// SMB 3.1.1 dialect.
    Smb3_1_1 = 0x0311,
}

impl Dialect {
    /// All supported dialect revisions, in ascending order.
    pub const ALL: &[Dialect] = &[
        Dialect::Smb2_0_2,
        Dialect::Smb2_1,
        Dialect::Smb3_0,
        Dialect::Smb3_0_2,
        Dialect::Smb3_1_1,
    ];
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dialect::Smb2_0_2 => f.write_str("SMB 2.0.2"),
            Dialect::Smb2_1 => f.write_str("SMB 2.1"),
            Dialect::Smb3_0 => f.write_str("SMB 3.0"),
            Dialect::Smb3_0_2 => f.write_str("SMB 3.0.2"),
            Dialect::Smb3_1_1 => f.write_str("SMB 3.1.1"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Newtype tests ───────────────────────────────────────────────

    #[test]
    fn session_id_none_is_zero() {
        assert_eq!(SessionId::NONE, SessionId(0));
        assert_eq!(SessionId::NONE.0, 0);
    }

    #[test]
    fn message_id_unsolicited() {
        assert_eq!(MessageId::UNSOLICITED.0, 0xFFFF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn file_id_sentinel() {
        assert_eq!(FileId::SENTINEL.persistent, 0xFFFF_FFFF_FFFF_FFFF);
        assert_eq!(FileId::SENTINEL.volatile, 0xFFFF_FFFF_FFFF_FFFF);
    }

    #[test]
    fn newtype_display_formatting() {
        assert_eq!(
            SessionId(0x1234).to_string(),
            "SessionId(0x0000000000001234)"
        );
        assert_eq!(
            MessageId(0xABCD).to_string(),
            "MessageId(0x000000000000ABCD)"
        );
        assert_eq!(TreeId(0x42).to_string(), "TreeId(0x00000042)");
        assert_eq!(CreditCharge(5).to_string(), "CreditCharge(5)");
        assert_eq!(
            FileId {
                persistent: 0x11,
                volatile: 0x22
            }
            .to_string(),
            "FileId(0x0000000000000011:0x0000000000000022)"
        );
    }

    // ── Command tests ───────────────────────────────────────────────

    #[test]
    fn command_roundtrip_via_u16() {
        assert_eq!(Command::try_from(0x0005u16), Ok(Command::Create));
        assert_eq!(u16::from(Command::Create), 0x0005);
    }

    #[test]
    fn command_all_variants_correct_values() {
        assert_eq!(u16::from(Command::Negotiate), 0x0000);
        assert_eq!(u16::from(Command::SessionSetup), 0x0001);
        assert_eq!(u16::from(Command::Logoff), 0x0002);
        assert_eq!(u16::from(Command::TreeConnect), 0x0003);
        assert_eq!(u16::from(Command::TreeDisconnect), 0x0004);
        assert_eq!(u16::from(Command::Create), 0x0005);
        assert_eq!(u16::from(Command::Close), 0x0006);
        assert_eq!(u16::from(Command::Flush), 0x0007);
        assert_eq!(u16::from(Command::Read), 0x0008);
        assert_eq!(u16::from(Command::Write), 0x0009);
        assert_eq!(u16::from(Command::Lock), 0x000A);
        assert_eq!(u16::from(Command::Ioctl), 0x000B);
        assert_eq!(u16::from(Command::Cancel), 0x000C);
        assert_eq!(u16::from(Command::Echo), 0x000D);
        assert_eq!(u16::from(Command::QueryDirectory), 0x000E);
        assert_eq!(u16::from(Command::ChangeNotify), 0x000F);
        assert_eq!(u16::from(Command::QueryInfo), 0x0010);
        assert_eq!(u16::from(Command::SetInfo), 0x0011);
        assert_eq!(u16::from(Command::OplockBreak), 0x0012);
    }

    #[test]
    fn command_invalid_u16_is_error() {
        assert!(Command::try_from(0xFFFFu16).is_err());
        assert!(Command::try_from(0x0013u16).is_err());
    }

    #[test]
    fn command_display() {
        assert_eq!(Command::Create.to_string(), "Create");
        assert_eq!(Command::OplockBreak.to_string(), "OplockBreak");
    }

    // ── Dialect tests ───────────────────────────────────────────────

    #[test]
    fn dialect_ordering() {
        assert!(Dialect::Smb2_0_2 < Dialect::Smb2_1);
        assert!(Dialect::Smb2_1 < Dialect::Smb3_0);
        assert!(Dialect::Smb3_0 < Dialect::Smb3_0_2);
        assert!(Dialect::Smb3_0_2 < Dialect::Smb3_1_1);
    }

    #[test]
    fn dialect_roundtrip_via_u16() {
        assert_eq!(Dialect::try_from(0x0311u16), Ok(Dialect::Smb3_1_1));
        assert_eq!(u16::from(Dialect::Smb3_1_1), 0x0311);
    }

    #[test]
    fn dialect_invalid_u16_is_error() {
        assert!(Dialect::try_from(0x0000u16).is_err());
        assert!(Dialect::try_from(0x0201u16).is_err());
    }

    #[test]
    fn dialect_display() {
        assert_eq!(Dialect::Smb2_0_2.to_string(), "SMB 2.0.2");
        assert_eq!(Dialect::Smb2_1.to_string(), "SMB 2.1");
        assert_eq!(Dialect::Smb3_0.to_string(), "SMB 3.0");
        assert_eq!(Dialect::Smb3_0_2.to_string(), "SMB 3.0.2");
        assert_eq!(Dialect::Smb3_1_1.to_string(), "SMB 3.1.1");
    }

    #[test]
    fn dialect_all_has_five_variants() {
        assert_eq!(Dialect::ALL.len(), 5);
        assert_eq!(Dialect::ALL[0], Dialect::Smb2_0_2);
        assert_eq!(Dialect::ALL[4], Dialect::Smb3_1_1);
    }

    #[test]
    fn dialect_all_is_sorted() {
        for w in Dialect::ALL.windows(2) {
            assert!(w[0] < w[1]);
        }
    }
}
