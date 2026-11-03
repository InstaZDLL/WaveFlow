//! Bitflag types for SMB2/3 protocol fields.

use std::ops::{BitAnd, BitOr, BitOrAssign};

// ── Macro to reduce boilerplate for flag types ──────────────────────────

macro_rules! impl_flags {
    ($name:ident, $inner:ty) => {
        impl $name {
            /// Create a new flags value from a raw integer.
            #[inline]
            pub const fn new(raw: $inner) -> Self {
                Self(raw)
            }

            /// Return the raw bits.
            #[inline]
            pub const fn bits(&self) -> $inner {
                self.0
            }

            /// Check whether a particular flag bit is set.
            #[inline]
            pub const fn contains(&self, flag: $inner) -> bool {
                self.0 & flag == flag
            }

            /// Set a flag bit.
            #[inline]
            pub fn set(&mut self, flag: $inner) {
                self.0 |= flag;
            }

            /// Clear a flag bit.
            #[inline]
            pub fn clear(&mut self, flag: $inner) {
                self.0 &= !flag;
            }
        }

        impl BitOr for $name {
            type Output = Self;
            #[inline]
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl BitAnd for $name {
            type Output = Self;
            #[inline]
            fn bitand(self, rhs: Self) -> Self {
                Self(self.0 & rhs.0)
            }
        }

        impl BitOrAssign for $name {
            #[inline]
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }
    };
}

// Wire-format flag types that live beside their message module (MS-DFSC's
// referral flags, in `msg::dfs`) get the same accessors from here.
pub(crate) use impl_flags;

// ── HeaderFlags ─────────────────────────────────────────────────────────

/// SMB2 packet header flags (32-bit field from MS-SMB2 2.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeaderFlags(pub u32);

impl HeaderFlags {
    /// The message is a response rather than a request.
    pub const SERVER_TO_REDIR: u32 = 0x0000_0001;
    /// The message is an async SMB2 header.
    pub const ASYNC_COMMAND: u32 = 0x0000_0002;
    /// The message is part of a compounded chain.
    pub const RELATED_OPERATIONS: u32 = 0x0000_0004;
    /// The message is signed.
    pub const SIGNED: u32 = 0x0000_0008;
    /// Priority value mask (SMB 3.1.1).
    pub const PRIORITY_MASK: u32 = 0x0000_0070;
    /// The command is a DFS operation.
    pub const DFS_OPERATIONS: u32 = 0x1000_0000;
    /// The command is a replay operation (SMB 3.x).
    pub const REPLAY_OPERATION: u32 = 0x2000_0000;

    /// Returns `true` if this is a response (server-to-redirector).
    #[inline]
    pub fn is_response(&self) -> bool {
        self.contains(Self::SERVER_TO_REDIR)
    }

    /// Returns `true` if the async flag is set.
    #[inline]
    pub fn is_async(&self) -> bool {
        self.contains(Self::ASYNC_COMMAND)
    }

    /// Returns `true` if the related-operations flag is set.
    #[inline]
    pub fn is_related(&self) -> bool {
        self.contains(Self::RELATED_OPERATIONS)
    }

    /// Returns `true` if the signed flag is set.
    #[inline]
    pub fn is_signed(&self) -> bool {
        self.contains(Self::SIGNED)
    }

    /// Set the response flag.
    #[inline]
    pub fn set_response(&mut self) {
        self.set(Self::SERVER_TO_REDIR);
    }

    /// Set the async flag.
    #[inline]
    pub fn set_async(&mut self) {
        self.set(Self::ASYNC_COMMAND);
    }

    /// Set the related-operations flag.
    #[inline]
    pub fn set_related(&mut self) {
        self.set(Self::RELATED_OPERATIONS);
    }

    /// Set the signed flag.
    #[inline]
    pub fn set_signed(&mut self) {
        self.set(Self::SIGNED);
    }
}

impl_flags!(HeaderFlags, u32);

// ── SecurityMode ────────────────────────────────────────────────────────

/// Security mode flags (16-bit field from MS-SMB2 2.2.3/2.2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SecurityMode(pub u16);

impl SecurityMode {
    /// Signing is supported (enabled).
    pub const SIGNING_ENABLED: u16 = 0x0001;
    /// Signing is required.
    pub const SIGNING_REQUIRED: u16 = 0x0002;

    /// Returns `true` if signing is enabled.
    #[inline]
    pub fn signing_enabled(&self) -> bool {
        self.contains(Self::SIGNING_ENABLED)
    }

    /// Returns `true` if signing is required.
    #[inline]
    pub fn signing_required(&self) -> bool {
        self.contains(Self::SIGNING_REQUIRED)
    }
}

impl_flags!(SecurityMode, u16);

// ── Capabilities ────────────────────────────────────────────────────────

/// Server/client capability flags (32-bit field from MS-SMB2 2.2.3/2.2.4).
///
/// With the `serde` feature on, this serializes as the underlying `u32`
/// bits, **not** a JSON object of named flags. Decode against the
/// associated constants on this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities(pub u32);

#[cfg(feature = "serde")]
impl serde::Serialize for Capabilities {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_u32(self.0)
    }
}

impl Capabilities {
    /// Distributed File System (DFS) support.
    pub const DFS: u32 = 0x0000_0001;
    /// Leasing support.
    pub const LEASING: u32 = 0x0000_0002;
    /// Multi-credit (large MTU) support.
    pub const LARGE_MTU: u32 = 0x0000_0004;
    /// Multi-channel support.
    pub const MULTI_CHANNEL: u32 = 0x0000_0008;
    /// Persistent handle support.
    pub const PERSISTENT_HANDLES: u32 = 0x0000_0010;
    /// Directory leasing support.
    pub const DIRECTORY_LEASING: u32 = 0x0000_0020;
    /// Encryption support.
    pub const ENCRYPTION: u32 = 0x0000_0040;
}

impl_flags!(Capabilities, u32);

// ── ShareFlags ──────────────────────────────────────────────────────────

/// Share property flags (32-bit field from MS-SMB2 2.2.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShareFlags(pub u32);

impl ShareFlags {
    /// The share is in a DFS tree structure.
    pub const DFS: u32 = 0x0000_0001;
    /// The share is a DFS root.
    pub const DFS_ROOT: u32 = 0x0000_0002;

    // Offline caching policies (mutually exclusive, stored in bits 4-5).

    /// The client can cache files explicitly selected by the user.
    pub const MANUAL_CACHING: u32 = 0x0000_0000;
    /// The client can automatically cache files used by the user.
    pub const AUTO_CACHING: u32 = 0x0000_0010;
    /// Auto-cache with offline mode even when the share is available.
    pub const VDO_CACHING: u32 = 0x0000_0020;
    /// Offline caching must not occur.
    pub const NO_CACHING: u32 = 0x0000_0030;

    /// Disallows exclusive file opens that deny reads.
    pub const RESTRICT_EXCLUSIVE_OPENS: u32 = 0x0000_0100;
    /// Disallows exclusive opens that prevent deletion.
    pub const FORCE_SHARED_DELETE: u32 = 0x0000_0200;
    /// Allow namespace caching (client must ignore).
    pub const ALLOW_NAMESPACE_CACHING: u32 = 0x0000_0400;
    /// Server filters directory entries based on access permissions.
    pub const ACCESS_BASED_DIRECTORY_ENUM: u32 = 0x0000_0800;
    /// Server will not issue exclusive caching rights.
    pub const FORCE_LEVELII_OPLOCK: u32 = 0x0000_1000;
    /// Hash generation v1 for branch cache (not valid for SMB 2.0.2).
    pub const ENABLE_HASH_V1: u32 = 0x0000_2000;
    /// Hash generation v2 for branch cache.
    pub const ENABLE_HASH_V2: u32 = 0x0000_4000;
    /// Encryption of remote file access messages required (SMB 3.x).
    pub const ENCRYPT_DATA: u32 = 0x0000_8000;
    /// The share supports identity remoting.
    pub const IDENTITY_REMOTING: u32 = 0x0004_0000;
    /// The server supports compression on this share (SMB 3.1.1).
    pub const COMPRESS_DATA: u32 = 0x0010_0000;
    /// Prefer isolated transport for this share (advisory).
    pub const ISOLATED_TRANSPORT: u32 = 0x0020_0000;
}

impl_flags!(ShareFlags, u32);

// ── ShareCapabilities ───────────────────────────────────────────────────

/// Share capability flags (32-bit field from MS-SMB2 2.2.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShareCapabilities(pub u32);

impl ShareCapabilities {
    /// The share is part of a DFS tree.
    pub const DFS: u32 = 0x0000_0008;
    /// The share has continuously available file handles.
    pub const CONTINUOUS_AVAILABILITY: u32 = 0x0000_0010;
    /// The share is a scale-out share.
    pub const SCALEOUT: u32 = 0x0000_0020;
    /// The share is a cluster share.
    pub const CLUSTER: u32 = 0x0000_0040;
    /// The share is an asymmetric share.
    pub const ASYMMETRIC: u32 = 0x0000_0080;
    /// The share supports redirect to owner.
    pub const REDIRECT_TO_OWNER: u32 = 0x0000_0100;
}

impl_flags!(ShareCapabilities, u32);

// ── FileAccessMask ──────────────────────────────────────────────────────

/// File access rights mask (32-bit, from MS-SMB2 2.2.13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FileAccessMask(pub u32);

impl FileAccessMask {
    /// Read data from the file.
    pub const FILE_READ_DATA: u32 = 0x0000_0001;
    /// Write data to the file.
    pub const FILE_WRITE_DATA: u32 = 0x0000_0002;
    /// Append data to the file.
    pub const FILE_APPEND_DATA: u32 = 0x0000_0004;
    /// Read extended attributes.
    pub const FILE_READ_EA: u32 = 0x0000_0008;
    /// Write extended attributes.
    pub const FILE_WRITE_EA: u32 = 0x0000_0010;
    /// Execute the file.
    pub const FILE_EXECUTE: u32 = 0x0000_0020;
    /// Read file attributes.
    pub const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
    /// Write file attributes.
    pub const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
    /// Delete the object.
    pub const DELETE: u32 = 0x0001_0000;
    /// Read the security descriptor.
    pub const READ_CONTROL: u32 = 0x0002_0000;
    /// Modify the DACL.
    pub const WRITE_DAC: u32 = 0x0004_0000;
    /// Change the owner.
    pub const WRITE_OWNER: u32 = 0x0008_0000;
    /// Synchronize access.
    pub const SYNCHRONIZE: u32 = 0x0010_0000;
    /// Request maximum allowed access.
    pub const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    /// All possible access rights.
    pub const GENERIC_ALL: u32 = 0x1000_0000;
    /// Execute access.
    pub const GENERIC_EXECUTE: u32 = 0x2000_0000;
    /// Write access.
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    /// Read access.
    pub const GENERIC_READ: u32 = 0x8000_0000;
}

impl_flags!(FileAccessMask, u32);

// ── FileAttributes ──────────────────────────────────────────────────────

/// A file's or directory's attributes (32-bit, from MS-FSCC 2.6): Hidden,
/// Read-only, and the rest.
///
/// What [`FileInfo`](crate::FileInfo) and
/// [`DirectoryEntry`](crate::DirectoryEntry) report, and what
/// [`FileTimes::set_attributes`](crate::FileTimes::set_attributes) sends.
///
/// ```
/// use smb2::FileAttributes;
///
/// let attrs = FileAttributes::new(FileAttributes::HIDDEN | FileAttributes::READONLY);
/// assert!(attrs.is_hidden() && attrs.is_readonly());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FileAttributes(pub u32);

impl FileAttributes {
    /// Read-only: a file can be read but not written or deleted; a directory
    /// can't be deleted. macOS shows it as "Locked".
    pub const READONLY: u32 = 0x0000_0001;
    /// Hidden from an ordinary directory listing.
    pub const HIDDEN: u32 = 0x0000_0002;
    /// Used by the operating system.
    pub const SYSTEM: u32 = 0x0000_0004;
    /// A directory. Reported, never set.
    pub const DIRECTORY: u32 = 0x0000_0010;
    /// Marked for backup.
    pub const ARCHIVE: u32 = 0x0000_0020;
    /// No other attribute set. Only valid alone; a server ignores it beside
    /// any other.
    pub const NORMAL: u32 = 0x0000_0080;
    /// Temporary storage. Settable on files only.
    pub const TEMPORARY: u32 = 0x0000_0100;
    /// A sparse file. Reported, never set this way.
    pub const SPARSE_FILE: u32 = 0x0000_0200;
    /// Has a reparse point (a symlink, for example). Reported, never set.
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    /// Compressed. Reported, never set this way.
    pub const COMPRESSED: u32 = 0x0000_0800;
    /// Its data lives in offline storage.
    pub const OFFLINE: u32 = 0x0000_1000;
    /// Left out of content indexing.
    pub const NOT_CONTENT_INDEXED: u32 = 0x0000_2000;
    /// Encrypted. Reported, never set this way.
    pub const ENCRYPTED: u32 = 0x0000_4000;

    /// The attributes a SET_INFO `FileBasicInformation` can change (MS-FSA
    /// 2.1.5.15.2's `ValidSetAttributes`). A server keeps every other bit as
    /// it is, and refuses DIRECTORY on a file outright.
    pub const SETTABLE: u32 = Self::READONLY
        | Self::HIDDEN
        | Self::SYSTEM
        | Self::ARCHIVE
        | Self::TEMPORARY
        | Self::OFFLINE
        | Self::NOT_CONTENT_INDEXED;

    /// Hidden from an ordinary directory listing.
    #[inline]
    pub const fn is_hidden(&self) -> bool {
        self.contains(Self::HIDDEN)
    }

    /// Read-only (macOS's "Locked").
    #[inline]
    pub const fn is_readonly(&self) -> bool {
        self.contains(Self::READONLY)
    }

    /// A directory.
    #[inline]
    pub const fn is_directory(&self) -> bool {
        self.contains(Self::DIRECTORY)
    }

    /// A reparse point: a junction, a symlink, or a file a storage filter
    /// manages. Its [`ReparseTag`](crate::ReparseTag) says which.
    #[inline]
    pub const fn is_reparse_point(&self) -> bool {
        self.contains(Self::REPARSE_POINT)
    }
}

impl_flags!(FileAttributes, u32);

#[cfg(test)]
mod tests {
    use super::*;

    // ── HeaderFlags ─────────────────────────────────────────────────

    #[test]
    fn header_flags_default_is_zero() {
        let f = HeaderFlags::default();
        assert_eq!(f.bits(), 0);
        assert!(!f.is_response());
        assert!(!f.is_async());
        assert!(!f.is_related());
        assert!(!f.is_signed());
    }

    #[test]
    fn header_flags_set_and_check() {
        let mut f = HeaderFlags::default();
        f.set_response();
        assert!(f.is_response());
        assert!(!f.is_async());

        f.set_signed();
        assert!(f.is_signed());
        assert!(f.is_response());
    }

    #[test]
    fn header_flags_clear() {
        let mut f = HeaderFlags::new(0xFFFF_FFFF);
        assert!(f.is_response());
        f.clear(HeaderFlags::SERVER_TO_REDIR);
        assert!(!f.is_response());
        assert!(f.is_async()); // other flags untouched
    }

    #[test]
    fn header_flags_contains() {
        let f = HeaderFlags::new(HeaderFlags::SIGNED | HeaderFlags::ASYNC_COMMAND);
        assert!(f.contains(HeaderFlags::SIGNED));
        assert!(f.contains(HeaderFlags::ASYNC_COMMAND));
        assert!(!f.contains(HeaderFlags::SERVER_TO_REDIR));
    }

    #[test]
    fn header_flags_bitor() {
        let a = HeaderFlags::new(HeaderFlags::SERVER_TO_REDIR);
        let b = HeaderFlags::new(HeaderFlags::SIGNED);
        let c = a | b;
        assert!(c.is_response());
        assert!(c.is_signed());
    }

    #[test]
    fn header_flags_bitand() {
        let a = HeaderFlags::new(HeaderFlags::SERVER_TO_REDIR | HeaderFlags::SIGNED);
        let b = HeaderFlags::new(HeaderFlags::SIGNED);
        let c = a & b;
        assert!(!c.is_response());
        assert!(c.is_signed());
    }

    #[test]
    fn header_flags_bitor_assign() {
        let mut a = HeaderFlags::new(HeaderFlags::SERVER_TO_REDIR);
        a |= HeaderFlags::new(HeaderFlags::ASYNC_COMMAND);
        assert!(a.is_response());
        assert!(a.is_async());
    }

    // ── SecurityMode ────────────────────────────────────────────────

    #[test]
    fn security_mode_signing_enabled() {
        let m = SecurityMode::new(SecurityMode::SIGNING_ENABLED);
        assert!(m.signing_enabled());
        assert!(!m.signing_required());
    }

    #[test]
    fn security_mode_signing_required() {
        let m = SecurityMode::new(SecurityMode::SIGNING_ENABLED | SecurityMode::SIGNING_REQUIRED);
        assert!(m.signing_enabled());
        assert!(m.signing_required());
    }

    // ── Capabilities ────────────────────────────────────────────────

    #[test]
    fn capabilities_combine_with_bitor() {
        let a = Capabilities::new(Capabilities::DFS);
        let b = Capabilities::new(Capabilities::ENCRYPTION);
        let c = a | b;
        assert!(c.contains(Capabilities::DFS));
        assert!(c.contains(Capabilities::ENCRYPTION));
        assert!(!c.contains(Capabilities::LEASING));
    }

    #[test]
    fn capabilities_set_and_clear() {
        let mut c = Capabilities::default();
        c.set(Capabilities::LARGE_MTU);
        assert!(c.contains(Capabilities::LARGE_MTU));
        c.clear(Capabilities::LARGE_MTU);
        assert!(!c.contains(Capabilities::LARGE_MTU));
    }

    // ── ShareFlags ──────────────────────────────────────────────────

    #[test]
    fn share_flags_encrypt_data() {
        let f = ShareFlags::new(ShareFlags::ENCRYPT_DATA | ShareFlags::DFS);
        assert!(f.contains(ShareFlags::ENCRYPT_DATA));
        assert!(f.contains(ShareFlags::DFS));
        assert!(!f.contains(ShareFlags::COMPRESS_DATA));
    }

    // ── ShareCapabilities ───────────────────────────────────────────

    #[test]
    fn share_capabilities_dfs() {
        let c = ShareCapabilities::new(ShareCapabilities::DFS);
        assert!(c.contains(ShareCapabilities::DFS));
        assert!(!c.contains(ShareCapabilities::CLUSTER));
    }

    // ── FileAccessMask ──────────────────────────────────────────────

    #[test]
    fn file_access_mask_generic_read() {
        let m = FileAccessMask::new(FileAccessMask::GENERIC_READ);
        assert!(m.contains(FileAccessMask::GENERIC_READ));
        assert!(!m.contains(FileAccessMask::GENERIC_WRITE));
    }

    #[test]
    fn file_access_mask_combine() {
        let m =
            FileAccessMask::new(FileAccessMask::FILE_READ_DATA | FileAccessMask::FILE_WRITE_DATA);
        assert!(m.contains(FileAccessMask::FILE_READ_DATA));
        assert!(m.contains(FileAccessMask::FILE_WRITE_DATA));
        assert!(!m.contains(FileAccessMask::DELETE));
    }

    // ── FileAttributes ──────────────────────────────────────────────

    #[test]
    fn file_attributes_read_their_own_bits() {
        let a = FileAttributes::new(FileAttributes::HIDDEN | FileAttributes::ARCHIVE);
        assert!(a.is_hidden());
        assert!(!a.is_readonly());
        assert!(!a.is_directory());
        assert!(FileAttributes::new(FileAttributes::READONLY).is_readonly());
        assert!(FileAttributes::new(FileAttributes::DIRECTORY).is_directory());
    }

    #[test]
    fn file_attributes_settable_is_ms_fsa_valid_set_attributes() {
        // MS-FSA 2.1.5.15.2: READONLY | HIDDEN | SYSTEM | ARCHIVE | TEMPORARY
        // | OFFLINE | NOT_CONTENT_INDEXED.
        assert_eq!(FileAttributes::SETTABLE, 0x0000_3127);
        assert_eq!(FileAttributes::SETTABLE & FileAttributes::DIRECTORY, 0);
        assert_eq!(FileAttributes::SETTABLE & FileAttributes::NORMAL, 0);
    }
}
