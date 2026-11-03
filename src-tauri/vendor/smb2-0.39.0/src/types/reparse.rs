//! Reparse tags: what kind of reparse point a file or directory is.

use std::fmt;

/// Generates the named tags and the `Debug` names from one table.
macro_rules! reparse_tags {
    ($($(#[$meta:meta])* $name:ident = $value:expr;)*) => {
        impl ReparseTag {
            $(
                $(#[$meta])*
                pub const $name: Self = Self($value);
            )*

            fn name(self) -> Option<&'static str> {
                match self.0 {
                    $( $value => Some(stringify!($name)), )*
                    _ => None,
                }
            }
        }
    };
}

/// The tag of a reparse point (MS-FSCC § 2.1.2.1), which says what kind of
/// reparse point it is: a junction, a symlink, a deduplicated file, a
/// cloud-files placeholder, and so on.
///
/// [`DirectoryEntry::reparse_tag`](crate::DirectoryEntry::reparse_tag) and
/// [`FileInfo::reparse_tag`](crate::FileInfo::reparse_tag) carry one for
/// anything whose attributes have
/// [`REPARSE_POINT`](crate::FileAttributes::REPARSE_POINT). The bit alone
/// doesn't tell a link from a file a storage filter manages, and the two need
/// opposite handling: a junction or symlink names another place, so a walk
/// that descends into one acts on its target, while a deduplicated or cloud
/// file is an ordinary file with its data kept elsewhere.
/// [`is_name_surrogate`](Self::is_name_surrogate) is the test that tells them
/// apart, and it covers tags this type has no name for.
///
/// Any `u32` is a valid tag, so unknown ones round-trip; the constants name
/// the ones a client is likely to meet on a share. Compare against them, or
/// `match`:
///
/// ```
/// use smb2::ReparseTag;
///
/// let tag = ReparseTag(0xA000_0003);
/// assert_eq!(tag, ReparseTag::MOUNT_POINT);
/// assert!(tag.is_name_surrogate());
/// assert!(!ReparseTag(0x9000_101A).is_name_surrogate()); // OneDrive
/// match tag {
///     ReparseTag::MOUNT_POINT | ReparseTag::SYMLINK => {}
///     _ => unreachable!(),
/// }
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReparseTag(pub u32);

reparse_tags! {
    /// A junction (`mklink /J`) or a mounted volume
    /// (`IO_REPARSE_TAG_MOUNT_POINT`). Always a directory, and the server
    /// follows it on an open that doesn't ask for the reparse point itself.
    MOUNT_POINT = 0xA000_0003;
    /// A Windows symbolic link (`mklink`, `mklink /D`;
    /// `IO_REPARSE_TAG_SYMLINK`). The server never follows one: an open
    /// through it fails with
    /// [`NtStatus::STOPPED_ON_SYMLINK`](crate::types::status::NtStatus::STOPPED_ON_SYMLINK)
    /// (MS-SMB2 § 3.3.5.9).
    SYMLINK = 0xA000_000C;
    /// A symbolic link made by the Windows Subsystem for Linux
    /// (`IO_REPARSE_TAG_LX_SYMLINK`, MS-FSCC § 2.1.2.7).
    LX_SYMLINK = 0xA000_001D;
    /// A file the Windows NFS server made: a symlink or a special file
    /// (`IO_REPARSE_TAG_NFS`).
    NFS = 0x8000_0014;
    /// A file the Data Deduplication filter manages
    /// (`IO_REPARSE_TAG_DEDUP`). Reads and writes like any other file.
    DEDUP = 0x8000_0013;
    /// A file the Windows Overlay filter compresses or backs with a WIM
    /// (`IO_REPARSE_TAG_WOF`). Reads like any other file.
    WOF = 0x8000_0017;
    /// A cloud-files placeholder, kept by a sync engine such as OneDrive
    /// (`IO_REPARSE_TAG_CLOUD`). Its 15 siblings differ in bits 12–15;
    /// [`is_cloud`](ReparseTag::is_cloud) matches all 16.
    CLOUD = 0x9000_001A;
    /// A file Azure File Sync has tiered to the cloud
    /// (`IO_REPARSE_TAG_STORAGE_SYNC`).
    STORAGE_SYNC = 0x8000_001E;
    /// A Store app's execution alias (`IO_REPARSE_TAG_APPEXECLINK`).
    APPEXECLINK = 0x8000_001B;
    /// A Unix domain socket (`IO_REPARSE_TAG_AF_UNIX`).
    AF_UNIX = 0x8000_0023;
    /// A FIFO made by the Windows Subsystem for Linux
    /// (`IO_REPARSE_TAG_LX_FIFO`).
    LX_FIFO = 0x8000_0024;
    /// A character device made by the Windows Subsystem for Linux
    /// (`IO_REPARSE_TAG_LX_CHR`).
    LX_CHR = 0x8000_0025;
    /// A block device made by the Windows Subsystem for Linux
    /// (`IO_REPARSE_TAG_LX_BLK`).
    LX_BLK = 0x8000_0026;
}

impl ReparseTag {
    /// The Microsoft bit (M): the tag is Microsoft's.
    const MICROSOFT_BIT: u32 = 0x8000_0000;
    /// The Name Surrogate bit (N).
    const NAME_SURROGATE_BIT: u32 = 0x2000_0000;
    /// The bits that tell the 16 cloud-files tags apart.
    const CLOUD_VARIANT_BITS: u32 = 0x0000_F000;

    /// The file or directory stands for another named entity: a junction, a
    /// symlink, or any other link (the tag's Name Surrogate bit, MS-FSCC
    /// § 2.1.2.1).
    ///
    /// This is the test for "don't descend into it": anything recursing
    /// through a share (a delete, a copy, a size count) acts on the link's
    /// target otherwise, and can loop. Storage filters' tags (dedup, cloud
    /// files, tiering) never set it, so their files are left to ordinary
    /// handling.
    #[inline]
    pub const fn is_name_surrogate(self) -> bool {
        self.0 & Self::NAME_SURROGATE_BIT != 0
    }

    /// One of the 16 cloud-files placeholder tags (`IO_REPARSE_TAG_CLOUD`
    /// through `IO_REPARSE_TAG_CLOUD_F`).
    #[inline]
    pub const fn is_cloud(self) -> bool {
        self.0 & !Self::CLOUD_VARIANT_BITS == Self::CLOUD.0
    }

    /// The tag is Microsoft's (its Microsoft bit). Every named constant is.
    #[inline]
    pub const fn is_microsoft(self) -> bool {
        self.0 & Self::MICROSOFT_BIT != 0
    }
}

impl fmt::Debug for ReparseTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "ReparseTag::{name}"),
            None => write!(f, "ReparseTag({:#010X})", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_name_surrogates() {
        for tag in [
            ReparseTag::MOUNT_POINT,
            ReparseTag::SYMLINK,
            ReparseTag::LX_SYMLINK,
            // WCI_LINK: a link this type has no name for still counts.
            ReparseTag(0xA000_0027),
        ] {
            assert!(tag.is_name_surrogate(), "{tag:?}");
        }
    }

    #[test]
    fn storage_filter_tags_are_not_name_surrogates() {
        for tag in [
            ReparseTag::DEDUP,
            ReparseTag::CLOUD,
            ReparseTag(0x9000_101A),
            ReparseTag::WOF,
            ReparseTag::STORAGE_SYNC,
        ] {
            assert!(!tag.is_name_surrogate(), "{tag:?}");
        }
    }

    #[test]
    fn every_cloud_variant_is_cloud_and_nothing_else_is() {
        for variant in 0..16u32 {
            assert!(ReparseTag(0x9000_001A | (variant << 12)).is_cloud());
        }
        assert!(!ReparseTag::DEDUP.is_cloud());
        // PROJFS shares CLOUD's high bits but not its value.
        assert!(!ReparseTag(0x9000_001C).is_cloud());
    }

    #[test]
    fn microsoft_bit() {
        assert!(ReparseTag::MOUNT_POINT.is_microsoft());
        assert!(!ReparseTag(0x0000_1234).is_microsoft());
    }

    #[test]
    fn debug_names_known_tags_and_shows_the_rest_in_hex() {
        assert_eq!(
            format!("{:?}", ReparseTag::MOUNT_POINT),
            "ReparseTag::MOUNT_POINT"
        );
        assert_eq!(
            format!("{:?}", ReparseTag(0x9000_101A)),
            "ReparseTag(0x9000101A)"
        );
    }
}
