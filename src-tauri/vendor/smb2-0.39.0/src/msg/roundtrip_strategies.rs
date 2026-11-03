//! Shared proptest strategies for wire-format roundtrip tests.
//!
//! Each strategy generates a value that a real encoder could emit. The goal
//! is not to stress-test the decoder against malformed input (that's fuzzing)
//! but to exercise encode/decode symmetry on well-formed inputs.
//!
//! Rules followed here:
//! - Typed enums always yield valid variants (no invalid discriminants).
//! - `Vec<u8>` lengths stay moderate (at most a few KB) to keep tests fast.
//! - Internally-dependent sizes (for example, a length field that must match a
//!   sibling `Vec`) are produced via `prop_map` so generated instances are
//!   always consistent.

// Note: `#[cfg(test)]` is applied at the module declaration in `src/msg/mod.rs`
// (`#[cfg(test)] pub(crate) mod roundtrip_strategies;`). We don't repeat it
// here; clippy's `duplicated_attributes` lint rejects that.
#![allow(dead_code)] // Helpers might be unused while tests are being added.

use proptest::prelude::*;

use crate::pack::{FileTime, Guid};
use crate::types::flags::{
    Capabilities, FileAccessMask, HeaderFlags, SecurityMode, ShareCapabilities, ShareFlags,
};
use crate::types::status::NtStatus;
use crate::types::{
    Command, CreditCharge, Dialect, FileId, MessageId, OplockLevel, SessionId, TreeId,
};

/// Max size (in bytes) used for generated `Vec<u8>` buffers across tests.
/// Kept small so a 256-case proptest run stays well under a second.
pub const MAX_BUFFER_BYTES: usize = 1024;

/// Moderate buffer for structs that usually carry small bodies.
pub const MAX_SMALL_BUFFER_BYTES: usize = 256;

/// Generate a `Vec<u8>` up to `max` bytes long (including zero).
pub fn bytes_up_to(max: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max)
}

/// A standard moderate-length byte buffer.
pub fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    bytes_up_to(MAX_BUFFER_BYTES)
}

/// A smaller byte buffer, for sub-fields or tightly-nested structures.
pub fn arb_small_bytes() -> impl Strategy<Value = Vec<u8>> {
    bytes_up_to(MAX_SMALL_BUFFER_BYTES)
}

/// Generate a valid UTF-16-encodable String, up to `max_chars` chars.
///
/// Excludes unpaired surrogates (U+D800..=U+DFFF) because UTF-16 decoding
/// would reject any surrogate that isn't part of a valid pair. We use the
/// BMP-minus-surrogates range plus occasional supplementary characters, so
/// both one-code-unit and two-code-unit forms are covered.
pub fn arb_utf16_string(max_chars: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::char::range('\u{0000}', '\u{D7FF}')
            .prop_union(prop::char::range('\u{E000}', '\u{FFFF}'))
            .or(prop::char::range('\u{1_0000}', '\u{10_FFFF}')),
        0..=max_chars,
    )
    .prop_map(|chars| chars.into_iter().collect())
}

// ── Primitive newtype strategies ────────────────────────────────────

pub fn arb_session_id() -> impl Strategy<Value = SessionId> {
    any::<u64>().prop_map(SessionId)
}

pub fn arb_message_id() -> impl Strategy<Value = MessageId> {
    any::<u64>().prop_map(MessageId)
}

pub fn arb_tree_id() -> impl Strategy<Value = TreeId> {
    any::<u32>().prop_map(TreeId)
}

pub fn arb_credit_charge() -> impl Strategy<Value = CreditCharge> {
    any::<u16>().prop_map(CreditCharge)
}

pub fn arb_file_id() -> impl Strategy<Value = FileId> {
    (any::<u64>(), any::<u64>()).prop_map(|(persistent, volatile)| FileId {
        persistent,
        volatile,
    })
}

pub fn arb_file_time() -> impl Strategy<Value = FileTime> {
    any::<u64>().prop_map(FileTime)
}

pub fn arb_guid() -> impl Strategy<Value = Guid> {
    (any::<u32>(), any::<u16>(), any::<u16>(), any::<[u8; 8]>()).prop_map(
        |(data1, data2, data3, data4)| Guid {
            data1,
            data2,
            data3,
            data4,
        },
    )
}

pub fn arb_nt_status() -> impl Strategy<Value = NtStatus> {
    any::<u32>().prop_map(NtStatus)
}

// ── Flags ────────────────────────────────────────────────────────────

pub fn arb_header_flags() -> impl Strategy<Value = HeaderFlags> {
    any::<u32>().prop_map(HeaderFlags::new)
}

pub fn arb_security_mode() -> impl Strategy<Value = SecurityMode> {
    any::<u16>().prop_map(SecurityMode::new)
}

pub fn arb_capabilities() -> impl Strategy<Value = Capabilities> {
    any::<u32>().prop_map(Capabilities::new)
}

pub fn arb_share_flags() -> impl Strategy<Value = ShareFlags> {
    any::<u32>().prop_map(ShareFlags::new)
}

pub fn arb_share_capabilities() -> impl Strategy<Value = ShareCapabilities> {
    any::<u32>().prop_map(ShareCapabilities::new)
}

pub fn arb_file_access_mask() -> impl Strategy<Value = FileAccessMask> {
    any::<u32>().prop_map(FileAccessMask::new)
}

// ── Typed enums: only valid variants ────────────────────────────────

pub fn arb_oplock_level() -> impl Strategy<Value = OplockLevel> {
    prop_oneof![
        Just(OplockLevel::None),
        Just(OplockLevel::LevelII),
        Just(OplockLevel::Exclusive),
        Just(OplockLevel::Batch),
        Just(OplockLevel::Lease),
    ]
}

pub fn arb_dialect() -> impl Strategy<Value = Dialect> {
    prop_oneof![
        Just(Dialect::Smb2_0_2),
        Just(Dialect::Smb2_1),
        Just(Dialect::Smb3_0),
        Just(Dialect::Smb3_0_2),
        Just(Dialect::Smb3_1_1),
    ]
}

pub fn arb_share_type() -> impl Strategy<Value = crate::msg::tree_connect::ShareType> {
    use crate::msg::tree_connect::ShareType;
    prop_oneof![
        Just(ShareType::Disk),
        Just(ShareType::Pipe),
        Just(ShareType::Print),
    ]
}

pub fn arb_impersonation_level() -> impl Strategy<Value = crate::msg::create::ImpersonationLevel> {
    use crate::msg::create::ImpersonationLevel;
    prop_oneof![
        Just(ImpersonationLevel::Anonymous),
        Just(ImpersonationLevel::Identification),
        Just(ImpersonationLevel::Impersonation),
        Just(ImpersonationLevel::Delegate),
    ]
}

pub fn arb_create_disposition() -> impl Strategy<Value = crate::msg::create::CreateDisposition> {
    use crate::msg::create::CreateDisposition;
    prop_oneof![
        Just(CreateDisposition::FileSupersede),
        Just(CreateDisposition::FileOpen),
        Just(CreateDisposition::FileCreate),
        Just(CreateDisposition::FileOpenIf),
        Just(CreateDisposition::FileOverwrite),
        Just(CreateDisposition::FileOverwriteIf),
    ]
}

pub fn arb_create_action() -> impl Strategy<Value = crate::msg::create::CreateAction> {
    use crate::msg::create::CreateAction;
    prop_oneof![
        Just(CreateAction::FileSuperseded),
        Just(CreateAction::FileOpened),
        Just(CreateAction::FileCreated),
        Just(CreateAction::FileOverwritten),
    ]
}

pub fn arb_share_access() -> impl Strategy<Value = crate::msg::create::ShareAccess> {
    any::<u32>().prop_map(crate::msg::create::ShareAccess)
}

pub fn arb_info_type() -> impl Strategy<Value = crate::msg::query_info::InfoType> {
    use crate::msg::query_info::InfoType;
    prop_oneof![
        Just(InfoType::File),
        Just(InfoType::Filesystem),
        Just(InfoType::Security),
        Just(InfoType::Quota),
    ]
}

pub fn arb_file_information_class(
) -> impl Strategy<Value = crate::msg::query_directory::FileInformationClass> {
    use crate::msg::query_directory::FileInformationClass;
    prop_oneof![
        Just(FileInformationClass::FileDirectoryInformation),
        Just(FileInformationClass::FileFullDirectoryInformation),
        Just(FileInformationClass::FileBothDirectoryInformation),
        Just(FileInformationClass::FileNamesInformation),
        Just(FileInformationClass::FileIdBothDirectoryInformation),
        Just(FileInformationClass::FileIdFullDirectoryInformation),
    ]
}

pub fn arb_command() -> impl Strategy<Value = Command> {
    prop_oneof![
        Just(Command::Negotiate),
        Just(Command::SessionSetup),
        Just(Command::Logoff),
        Just(Command::TreeConnect),
        Just(Command::TreeDisconnect),
        Just(Command::Create),
        Just(Command::Close),
        Just(Command::Flush),
        Just(Command::Read),
        Just(Command::Write),
        Just(Command::Lock),
        Just(Command::Ioctl),
        Just(Command::Cancel),
        Just(Command::Echo),
        Just(Command::QueryDirectory),
        Just(Command::ChangeNotify),
        Just(Command::QueryInfo),
        Just(Command::SetInfo),
        Just(Command::OplockBreak),
    ]
}
