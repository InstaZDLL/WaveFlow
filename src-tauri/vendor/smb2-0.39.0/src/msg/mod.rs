//! Wire format message structs for SMB2/3.
//!
//! Each sub-module corresponds to one SMB2 command type with its
//! request and response structures.
//!
//! Most users don't need this module directly -- use [`SmbClient`](crate::SmbClient)
//! for high-level file operations.

// Wire format internals, comments would be pretty redundant. Public API docs are enforced at the crate level.
#![allow(missing_docs)]

/// Generates a trivial 4-byte SMB2 stub message (StructureSize + Reserved).
///
/// Many SMB2 commands (echo, cancel, logoff, tree_disconnect) have request
/// and/or response structs that are identical: 2-byte StructureSize (always 4)
/// plus 2-byte Reserved. This macro generates the struct definition and its
/// `Pack`/`Unpack` impls from a single declaration.
///
/// # Usage
///
/// ```ignore
/// trivial_message! {
///     /// Doc comment for the struct.
///     pub struct EchoRequest;
/// }
/// ```
macro_rules! trivial_message {
    (
        $(#[$meta:meta])*
        pub struct $name:ident;
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name;

        impl $name {
            pub const STRUCTURE_SIZE: u16 = 4;
        }

        impl crate::pack::Pack for $name {
            fn pack(&self, cursor: &mut crate::pack::WriteCursor) {
                // StructureSize (2 bytes)
                cursor.write_u16_le(Self::STRUCTURE_SIZE);
                // Reserved (2 bytes)
                cursor.write_u16_le(0);
            }
        }

        impl crate::pack::Unpack for $name {
            fn unpack(cursor: &mut crate::pack::ReadCursor<'_>) -> crate::error::Result<Self> {
                // StructureSize (2 bytes)
                let structure_size = cursor.read_u16_le()?;
                if structure_size != Self::STRUCTURE_SIZE {
                    return Err(crate::Error::invalid_data(format!(
                        "invalid {} structure size: expected {}, got {}",
                        stringify!($name),
                        Self::STRUCTURE_SIZE,
                        structure_size
                    )));
                }

                // Reserved (2 bytes)
                let _reserved = cursor.read_u16_le()?;

                Ok($name)
            }
        }
    };
}

pub(crate) use trivial_message;

/// Generates a minimal test suite for a trivial 4-byte message type.
///
/// Tests: known bytes, pack-unpack roundtrip, wrong structure size, and
/// truncated input. These four tests cover all interesting behavior for
/// types produced by [`trivial_message!`].
#[cfg(test)]
macro_rules! trivial_message_tests {
    ($type:ident, $known:ident, $roundtrip:ident, $wrong_size:ident, $short:ident) => {
        #[test]
        fn $known() {
            let msg = $type;
            let mut cursor = crate::pack::WriteCursor::new();
            crate::pack::Pack::pack(&msg, &mut cursor);
            let bytes = cursor.into_inner();
            // StructureSize=4 (LE), Reserved=0
            assert_eq!(bytes, [0x04, 0x00, 0x00, 0x00]);
        }

        #[test]
        fn $roundtrip() {
            let original = $type;
            let mut w = crate::pack::WriteCursor::new();
            crate::pack::Pack::pack(&original, &mut w);
            let bytes = w.into_inner();

            let mut r = crate::pack::ReadCursor::new(&bytes);
            let decoded = <$type as crate::pack::Unpack>::unpack(&mut r).unwrap();
            assert_eq!(decoded, original);
        }

        #[test]
        fn $wrong_size() {
            let bytes = [0x08, 0x00, 0x00, 0x00];
            let mut cursor = crate::pack::ReadCursor::new(&bytes);
            let result = <$type as crate::pack::Unpack>::unpack(&mut cursor);
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("structure size"), "error was: {err}");
        }

        #[test]
        fn $short() {
            let bytes = [0x04, 0x00];
            let mut cursor = crate::pack::ReadCursor::new(&bytes);
            let result = <$type as crate::pack::Unpack>::unpack(&mut cursor);
            assert!(result.is_err());
        }
    };
}

#[cfg(test)]
pub(crate) use trivial_message_tests;

#[cfg(test)]
pub(crate) mod roundtrip_strategies;

pub mod cancel;
pub mod change_notify;
pub mod close;
pub mod copychunk;
pub mod create;
pub mod create_context;
pub mod dfs;
pub mod directory_info;
pub mod echo;
pub mod flush;
pub mod header;
pub mod ioctl;
pub mod lock;
pub mod logoff;
pub mod negotiate;
pub mod oplock_break;
pub mod query_directory;
pub mod query_info;
pub mod read;
pub mod session_setup;
pub mod set_info;
pub mod transform;
pub mod tree_connect;
pub mod tree_disconnect;
pub mod write;

pub use header::{ErrorResponse, Header, PROTOCOL_ID};
