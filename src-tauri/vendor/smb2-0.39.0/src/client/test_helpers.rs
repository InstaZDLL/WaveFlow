//! Shared test helper functions for `client` module tests.
//!
//! These build mock SMB2 responses used across pipeline, shares, and tree tests.

use std::sync::Arc;

use crate::client::connection::{pack_message, Connection, NegotiatedParams};
use crate::msg::close::CloseResponse;
use crate::msg::create::{CreateAction, CreateResponse};
use crate::msg::header::Header;
use crate::msg::tree_connect::{ShareType, TreeConnectResponse};
use crate::pack::{FileTime, Guid};
use crate::transport::MockTransport;
use crate::types::flags::{Capabilities, ShareCapabilities, ShareFlags};
use crate::types::{Command, Dialect, FileId, OplockLevel, SessionId, TreeId};

/// Build a successful SET_INFO response.
pub(crate) fn build_set_info_response() -> Vec<u8> {
    use crate::msg::set_info::SetInfoResponse;
    let mut h = Header::new_request(Command::SetInfo);
    h.flags.set_response();
    h.credits = 32;
    pack_message(&h, &SetInfoResponse)
}

/// Create a mock-backed connection with standard negotiated params.
///
/// Enables the mock's auto-msg_id-rewrite so canned `build_*_response`
/// helpers (which hardcode `MessageId(0)` and don't know the caller's
/// allocated msg_ids) still route through the Phase 3 receiver task: on
/// each `receive()` the mock patches sub-frame msg_ids to match the next
/// pending sent msg_id in FIFO order. Replaces the pre-Phase-3
/// `set_orphan_filter_enabled(false)` path.
pub(crate) fn setup_connection(mock: &Arc<MockTransport>) -> Connection {
    let conn = setup_connection_without_credits(mock);
    // Stage a credit window the way a real connection would have one by this
    // point: NEGOTIATE, SESSION_SETUP, and TREE_CONNECT have all come back
    // with grants before any of these tests' operations run.
    conn.set_credits(512);
    conn
}

/// [`setup_connection`] with the credit window left exactly as a connection
/// that has negotiated nothing yet holds it: empty.
///
/// The server funds every credit, so before its first grant a client can send
/// NEGOTIATE and nothing else (MS-SMB2 § 3.2.5.1.1).
pub(crate) fn setup_connection_without_credits(mock: &Arc<MockTransport>) -> Connection {
    mock.enable_auto_rewrite_msg_id();
    let mut conn = Connection::from_transport(
        Box::new(mock.clone()),
        Box::new(mock.clone()),
        "test-server",
    );
    conn.set_test_params(NegotiatedParams {
        dialect: Dialect::Smb2_0_2,
        max_read_size: 65536,
        max_write_size: 65536,
        max_transact_size: 65536,
        server_guid: Guid::ZERO,
        signing_required: false,
        capabilities: Capabilities::default(),
        gmac_negotiated: false,
        cipher: None,
        compression_supported: false,
    });
    conn.set_session_id(SessionId(0x1234));
    conn
}

/// [`setup_connection`] with a server that negotiated `max_read` bytes per
/// READ, for tests that care what a read puts on the wire.
///
/// The 64 KiB default is below every size a compound read has to distinguish
/// between; real servers commonly grant 8 MiB.
pub(crate) fn setup_connection_with_max_read(
    mock: &Arc<MockTransport>,
    max_read: u32,
) -> Connection {
    let mut conn = setup_connection(mock);
    let mut params = conn.params().expect("setup_connection negotiates params");
    params.max_read_size = max_read;
    conn.set_test_params(params);
    conn
}

/// Build a CREATE response with the given file ID and end-of-file size.
pub(crate) fn build_create_response(file_id: FileId, end_of_file: u64) -> Vec<u8> {
    let mut h = Header::new_request(Command::Create);
    h.flags.set_response();
    h.credits = 32;

    let body = CreateResponse {
        oplock_level: OplockLevel::None,
        flags: 0,
        create_action: CreateAction::FileOpened,
        creation_time: FileTime::ZERO,
        last_access_time: FileTime::ZERO,
        last_write_time: FileTime::ZERO,
        change_time: FileTime::ZERO,
        allocation_size: 0,
        end_of_file,
        file_attributes: 0,
        file_id,
        create_contexts: vec![],
    };

    pack_message(&h, &body)
}

/// The four times [`build_create_response_dated`] reports, all distinct so a
/// test can tell which field landed where: creation, last access, last write,
/// change.
pub(crate) const CREATE_TIMES: [FileTime; 4] = [
    FileTime(132_000_000_000_000_000),
    FileTime(133_100_000_000_000_000),
    FileTime(133_000_000_000_000_000),
    FileTime(133_200_000_000_000_000),
];

/// [`build_create_response`] for a file the server reports [`CREATE_TIMES`]
/// for.
pub(crate) fn build_create_response_dated(file_id: FileId, end_of_file: u64) -> Vec<u8> {
    let mut h = Header::new_request(Command::Create);
    h.flags.set_response();
    h.credits = 32;

    let [creation_time, last_access_time, last_write_time, change_time] = CREATE_TIMES;
    let body = CreateResponse {
        oplock_level: OplockLevel::None,
        flags: 0,
        create_action: CreateAction::FileOpened,
        creation_time,
        last_access_time,
        last_write_time,
        change_time,
        allocation_size: 0,
        end_of_file,
        file_attributes: 0x20, // FILE_ATTRIBUTE_ARCHIVE
        file_id,
        create_contexts: vec![],
    };

    pack_message(&h, &body)
}

/// Build a CREATE response carrying a chain of create contexts.
pub(crate) fn build_create_response_with_contexts(
    file_id: FileId,
    end_of_file: u64,
    contexts: &[crate::msg::create_context::CreateContext],
) -> Vec<u8> {
    let mut h = Header::new_request(Command::Create);
    h.flags.set_response();
    h.credits = 32;

    let body = CreateResponse {
        oplock_level: OplockLevel::Batch,
        flags: 0,
        create_action: CreateAction::FileOpened,
        creation_time: FileTime::ZERO,
        last_access_time: FileTime::ZERO,
        last_write_time: FileTime::ZERO,
        change_time: FileTime::ZERO,
        allocation_size: 0,
        end_of_file,
        file_attributes: 0,
        file_id,
        create_contexts: crate::msg::create_context::pack_contexts(contexts),
    };

    pack_message(&h, &body)
}

/// Build a CREATE response with a non-success status (for error tests).
pub(crate) fn build_create_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    let mut h = Header::new_request(Command::Create);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = ErrorResponse {
        error_context_count: 0,
        error_data: vec![],
    };

    pack_message(&h, &body)
}

/// Build a CLOSE response with zeroed fields.
pub(crate) fn build_close_response() -> Vec<u8> {
    let mut h = Header::new_request(Command::Close);
    h.flags.set_response();
    h.credits = 32;

    let body = CloseResponse {
        flags: 0,
        creation_time: FileTime::ZERO,
        last_access_time: FileTime::ZERO,
        last_write_time: FileTime::ZERO,
        change_time: FileTime::ZERO,
        allocation_size: 0,
        end_of_file: 0,
        file_attributes: 0,
    };

    pack_message(&h, &body)
}

/// Build a WRITE response with the given byte count.
pub(crate) fn build_write_response(count: u32) -> Vec<u8> {
    use crate::msg::write::WriteResponse;
    let mut h = Header::new_request(Command::Write);
    h.flags.set_response();
    h.credits = 32;

    let body = WriteResponse {
        count,
        remaining: 0,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
    };

    pack_message(&h, &body)
}

/// [`build_write_response`] from a server that grants back only `credits`,
/// the way one that stopped growing its window answers.
pub(crate) fn build_write_response_granting(count: u32, credits: u16) -> Vec<u8> {
    use crate::msg::write::WriteResponse;
    let mut h = Header::new_request(Command::Write);
    h.flags.set_response();
    h.credits = credits;

    let body = WriteResponse {
        count,
        remaining: 0,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
    };

    pack_message(&h, &body)
}

/// Build a WRITE response with a non-success status (for error tests).
pub(crate) fn build_write_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    let mut h = Header::new_request(Command::Write);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = ErrorResponse {
        error_context_count: 0,
        error_data: vec![],
    };

    pack_message(&h, &body)
}

/// Build a CLOSE response with a non-success status (for error tests).
pub(crate) fn build_close_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    let mut h = Header::new_request(Command::Close);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = ErrorResponse {
        error_context_count: 0,
        error_data: vec![],
    };

    pack_message(&h, &body)
}

/// Build a FLUSH response.
pub(crate) fn build_flush_response() -> Vec<u8> {
    let mut h = Header::new_request(Command::Flush);
    h.flags.set_response();
    h.credits = 32;

    let body = crate::msg::flush::FlushResponse;
    pack_message(&h, &body)
}

/// Build a FLUSH response with a non-success status (for error tests).
pub(crate) fn build_flush_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    let mut h = Header::new_request(Command::Flush);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = ErrorResponse {
        error_context_count: 0,
        error_data: vec![],
    };

    pack_message(&h, &body)
}

/// Build a READ response carrying the given data bytes.
pub(crate) fn build_read_response(data: Vec<u8>) -> Vec<u8> {
    use crate::msg::read::ReadResponse;
    let mut h = Header::new_request(Command::Read);
    h.flags.set_response();
    h.credits = 32;

    let body = ReadResponse {
        data_offset: 0x50,
        data_remaining: 0,
        flags: 0,
        data,
    };

    pack_message(&h, &body)
}

/// Build a QUERY_DIRECTORY response: `entries_data` as the output buffer on
/// success, an error body (as `STATUS_NO_MORE_FILES` comes back) otherwise.
pub(crate) fn build_query_directory_response(
    status: crate::types::status::NtStatus,
    entries_data: Vec<u8>,
) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    use crate::msg::query_directory::QueryDirectoryResponse;
    let mut h = Header::new_request(Command::QueryDirectory);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    if status != crate::types::status::NtStatus::SUCCESS {
        let body = ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };
        return pack_message(&h, &body);
    }

    let body = QueryDirectoryResponse {
        output_buffer: entries_data,
    };
    pack_message(&h, &body)
}

/// Build a single `FileBothDirectoryInformation` entry (MS-FSCC § 2.4.8).
pub(crate) fn build_file_both_dir_info(
    name: &str,
    size: u64,
    is_directory: bool,
    next_offset: u32,
) -> Vec<u8> {
    let name_u16: Vec<u16> = name.encode_utf16().collect();
    let name_bytes_len = name_u16.len() * 2;

    let mut buf = Vec::new();
    // NextEntryOffset (4)
    buf.extend_from_slice(&next_offset.to_le_bytes());
    // FileIndex (4)
    buf.extend_from_slice(&0u32.to_le_bytes());
    // CreationTime (8)
    buf.extend_from_slice(&132_000_000_000_000_000u64.to_le_bytes());
    // LastAccessTime (8)
    buf.extend_from_slice(&132_000_000_000_000_000u64.to_le_bytes());
    // LastWriteTime (8)
    buf.extend_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
    // ChangeTime (8)
    buf.extend_from_slice(&133_100_000_000_000_000u64.to_le_bytes());
    // EndOfFile (8)
    buf.extend_from_slice(&size.to_le_bytes());
    // AllocationSize (8)
    buf.extend_from_slice(&((size + 4095) & !4095).to_le_bytes());
    // FileAttributes (4)
    let attrs = if is_directory {
        crate::types::flags::FileAttributes::DIRECTORY
    } else {
        0x00000020 // ARCHIVE
    };
    buf.extend_from_slice(&attrs.to_le_bytes());
    // FileNameLength (4)
    buf.extend_from_slice(&(name_bytes_len as u32).to_le_bytes());
    // EaSize (4)
    buf.extend_from_slice(&0u32.to_le_bytes());
    // ShortNameLength (1)
    buf.push(0);
    // Reserved (1)
    buf.push(0);
    // ShortName (24 bytes, zero-padded)
    buf.extend_from_slice(&[0u8; 24]);
    // FileName (variable)
    for &u in &name_u16 {
        buf.extend_from_slice(&u.to_le_bytes());
    }

    buf
}

/// Build a READ response with a non-success status (for error tests).
pub(crate) fn build_read_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    let mut h = Header::new_request(Command::Read);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = ErrorResponse {
        error_context_count: 0,
        error_data: vec![],
    };

    pack_message(&h, &body)
}

/// Build a successful IOCTL response carrying the given output buffer.
pub(crate) fn build_ioctl_response(ctl_code: u32, output_data: Vec<u8>) -> Vec<u8> {
    build_ioctl_response_status(
        ctl_code,
        crate::types::status::NtStatus::SUCCESS,
        output_data,
    )
}

/// Build an IOCTL response with an explicit status and output buffer. Used for
/// the server-side-copy limits negotiation, where the server returns
/// `STATUS_INVALID_PARAMETER` *with* a 12-byte copychunk response payload.
pub(crate) fn build_ioctl_response_status(
    ctl_code: u32,
    status: crate::types::status::NtStatus,
    output_data: Vec<u8>,
) -> Vec<u8> {
    use crate::msg::ioctl::IoctlResponse;
    let mut h = Header::new_request(Command::Ioctl);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = IoctlResponse {
        ctl_code,
        file_id: FileId::SENTINEL,
        flags: crate::msg::ioctl::SMB2_0_IOCTL_IS_FSCTL,
        output_data,
    };
    pack_message(&h, &body)
}

/// Build an IOCTL error response (an `ErrorResponse` body, no output buffer) --
/// for example a server that lacks server-side copy replying `STATUS_NOT_SUPPORTED`.
pub(crate) fn build_ioctl_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    use crate::msg::header::ErrorResponse;
    let mut h = Header::new_request(Command::Ioctl);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;

    let body = ErrorResponse {
        error_context_count: 0,
        error_data: vec![],
    };
    pack_message(&h, &body)
}

/// Build a TREE_CONNECT response with the given tree ID and share type.
pub(crate) fn build_tree_connect_response(tree_id: TreeId, share_type: ShareType) -> Vec<u8> {
    let mut h = Header::new_request(Command::TreeConnect);
    h.flags.set_response();
    h.credits = 32;
    h.tree_id = Some(tree_id);

    let body = TreeConnectResponse {
        share_type,
        share_flags: ShareFlags::default(),
        capabilities: ShareCapabilities::default(),
        maximal_access: 0x001F_01FF,
    };

    pack_message(&h, &body)
}

/// Concatenate sub-responses into one compound transport frame, chaining them
/// with `NextCommand` offsets the way a server does.
pub(crate) fn build_compound_response_frame(responses: &[Vec<u8>]) -> Vec<u8> {
    let mut frame = Vec::new();
    for (i, resp) in responses.iter().enumerate() {
        let mut r = resp.clone();
        if i != responses.len() - 1 {
            // Every sub-response but the last is 8-byte aligned and points at
            // the next one.
            let remainder = r.len() % 8;
            if remainder != 0 {
                r.resize(r.len() + (8 - remainder), 0);
            }
            let next_cmd = r.len() as u32;
            r[20..24].copy_from_slice(&next_cmd.to_le_bytes());
        }
        frame.extend_from_slice(&r);
    }
    frame
}

/// Build a successful QUERY_INFO response wrapping `output_buffer`.
pub(crate) fn build_query_info_response(output_buffer: Vec<u8>) -> Vec<u8> {
    build_query_info_response_with_status(crate::types::status::NtStatus::SUCCESS, output_buffer)
}

/// Build a QUERY_INFO response the server refused, with no payload.
pub(crate) fn build_query_info_error_response(status: crate::types::status::NtStatus) -> Vec<u8> {
    build_query_info_response_with_status(status, Vec::new())
}

/// `FILE_NAME_INFORMATION` (MS-FSCC § 2.1.7): a `u32` byte length, then the
/// name in UTF-16LE. What `FileNormalizedNameInformation` returns on its own.
pub(crate) fn file_name_information(name: &str) -> Vec<u8> {
    let utf16: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = (utf16.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&utf16);
    out
}

/// What every `FileWriter` open gets back: the CREATE, then the two identity
/// queries (answered when `identity` is `Some`, refused otherwise), then class
/// 48 (answered with `name` when given, refused otherwise).
pub(crate) fn build_writer_open_response(
    file_id: FileId,
    end_of_file: u64,
    identity: Option<crate::client::durable::FileIdentity>,
    name: Option<&str>,
) -> Vec<u8> {
    use crate::types::status::NtStatus;
    let refused = || build_query_info_error_response(NtStatus::NOT_SUPPORTED);
    let (index, volume) = match identity {
        Some(id) => {
            let mut volume = vec![0u8; 8];
            volume.extend_from_slice(&id.volume_serial.to_le_bytes());
            volume.extend_from_slice(&[0u8; 6]);
            (
                build_query_info_response(id.index_number.to_le_bytes().to_vec()),
                build_query_info_response(volume),
            )
        }
        None => (refused(), refused()),
    };
    build_compound_response_frame(&[
        build_create_response(file_id, end_of_file),
        index,
        volume,
        name.map_or_else(refused, |n| {
            build_query_info_response(file_name_information(n))
        }),
    ])
}

/// `FILE_ALL_INFORMATION` (MS-FSCC § 2.4.2) with every fixed field zeroed and
/// `name` in its trailing `FILE_NAME_INFORMATION`, 96 bytes in.
pub(crate) fn file_all_information(name: &str) -> Vec<u8> {
    let mut out = vec![0u8; 96];
    out.extend_from_slice(&file_name_information(name));
    out
}

/// Build a QUERY_INFO response with an explicit status.
pub(crate) fn build_query_info_response_with_status(
    status: crate::types::status::NtStatus,
    output_buffer: Vec<u8>,
) -> Vec<u8> {
    use crate::msg::query_info::QueryInfoResponse;
    let mut h = Header::new_request(Command::QueryInfo);
    h.flags.set_response();
    h.credits = 32;
    h.status = status;
    pack_message(&h, &QueryInfoResponse { output_buffer })
}
