//! Share enumeration via IPC$ + srvsvc RPC.
//!
//! Lists available shares on an SMB server by connecting to the IPC$ share,
//! opening the srvsvc named pipe, and performing the NetShareEnumAll RPC
//! exchange.

use log::{debug, info, trace};

use crate::client::connection::Connection;
use crate::error::Result;
use crate::msg::close::CloseRequest;
use crate::msg::create::{
    CreateDisposition, CreateRequest, CreateResponse, ImpersonationLevel, ShareAccess,
};
use crate::msg::read::{ReadRequest, ReadResponse, SMB2_CHANNEL_NONE};
use crate::msg::tree_connect::{TreeConnectRequest, TreeConnectRequestFlags, TreeConnectResponse};
use crate::msg::tree_disconnect::TreeDisconnectRequest;
use crate::msg::write::{WriteRequest, WriteResponse};
use crate::pack::{ReadCursor, Unpack};
use crate::rpc;
use crate::rpc::srvsvc::{self, ShareInfo};
use crate::types::flags::FileAccessMask;
use crate::types::status::NtStatus;
use crate::types::{Command, FileId, OplockLevel, TreeId};
use crate::Error;

/// Read buffer size for pipe reads (64 KiB is plenty for share listings).
const PIPE_READ_BUFFER_SIZE: u32 = 65536;

/// List available shares on the server.
///
/// Connects to the IPC$ share, opens the srvsvc named pipe, performs
/// the RPC exchange, and returns filtered disk shares.
///
/// This is a self-contained operation -- it opens and closes its own
/// tree connection to IPC$.
pub async fn list_shares(conn: &mut Connection) -> Result<Vec<ShareInfo>> {
    // The IPC$ tree id and the pipe's file id are used across a dozen round
    // trips, so all of them go out pinned to the session that issued them.
    let conn = &mut conn.pinned_to_session();

    // 1. Tree connect to IPC$
    let tree_id = tree_connect_ipc(conn).await?;

    // Run the pipe operations, then clean up regardless of outcome
    let result = pipe_rpc_exchange(conn, tree_id).await;

    // 8. Tree disconnect (best-effort -- don't mask the real error)
    let _ = tree_disconnect(conn, tree_id).await;

    let all_shares = result?;

    // 9. Filter to disk shares
    let filtered = srvsvc::filter_disk_shares(all_shares);
    info!("shares: found {} disk shares", filtered.len());
    Ok(filtered)
}

/// Connect to the IPC$ share, returning the tree ID.
async fn tree_connect_ipc(conn: &mut Connection) -> Result<TreeId> {
    let server = conn.server_name().to_string();
    let unc_path = format!(r"\\{}\IPC$", server);

    let req = TreeConnectRequest {
        flags: TreeConnectRequestFlags::default(),
        path: unc_path,
    };

    let frame = conn.execute(Command::TreeConnect, &req, None).await?;

    if frame.header.command != Command::TreeConnect {
        return Err(Error::invalid_data(format!(
            "expected TreeConnect response, got {:?}",
            frame.header.command
        )));
    }

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::TreeConnect,
        });
    }

    let mut cursor = ReadCursor::new(&frame.body);
    let _resp = TreeConnectResponse::unpack(&mut cursor)?;

    let tree_id = frame
        .header
        .tree_id
        .ok_or_else(|| Error::invalid_data("TreeConnect response missing tree ID"))?;

    info!("shares: connected to IPC$, tree_id={}", tree_id);
    Ok(tree_id)
}

/// Open the srvsvc pipe, perform the RPC bind and request, then close.
async fn pipe_rpc_exchange(conn: &mut Connection, tree_id: TreeId) -> Result<Vec<ShareInfo>> {
    // 2. Create \pipe\srvsvc
    let file_id = open_srvsvc_pipe(conn, tree_id).await?;

    // Run RPC exchange, then close regardless of outcome
    let result = rpc_bind_and_request(conn, tree_id, file_id).await;

    // 7. Close the pipe handle (best-effort)
    let _ = close_handle(conn, tree_id, file_id).await;

    result
}

/// Perform the RPC bind + NetShareEnumAll request over the pipe.
async fn rpc_bind_and_request(
    conn: &mut Connection,
    tree_id: TreeId,
    file_id: FileId,
) -> Result<Vec<ShareInfo>> {
    // 3. Write RPC BIND
    let bind_data = rpc::build_srvsvc_bind(1);
    write_pipe(conn, tree_id, file_id, &bind_data).await?;
    trace!("shares: sent RPC BIND ({} bytes)", bind_data.len());

    // 4. Read RPC BIND_ACK
    let bind_ack_data = read_pipe_message(conn, tree_id, file_id).await?;
    rpc::parse_bind_ack(&bind_ack_data)?;
    trace!("shares: received BIND_ACK, context accepted");

    // 5. Write RPC REQUEST (NetShareEnumAll)
    let server_name = format!(r"\\{}", conn.server_name());
    let request_data = srvsvc::build_net_share_enum_all(2, &server_name);
    write_pipe(conn, tree_id, file_id, &request_data).await?;
    trace!(
        "shares: sent NetShareEnumAll request ({} bytes)",
        request_data.len()
    );

    // 6. Read RPC RESPONSE, reassembling DCE/RPC fragments (MS-RPCE 2.2.2.6).
    // A large NetShareEnum reply may arrive as several fragment PDUs, each its
    // own pipe message, with PFC_LAST_FRAG set only on the last.
    let mut stub = Vec::new();
    let mut fragments = 0;
    loop {
        let pdu = read_pipe_message(conn, tree_id, file_id).await?;
        let (frag_stub, is_last) = rpc::parse_response_fragment(&pdu)?;
        stub.extend_from_slice(frag_stub);
        fragments += 1;
        if is_last {
            break;
        }
    }
    let shares = srvsvc::parse_net_share_enum_all_stub(&stub)?;
    debug!(
        "shares: received {} shares in response ({} RPC fragment(s))",
        shares.len(),
        fragments
    );

    Ok(shares)
}

/// Open the `\pipe\srvsvc` named pipe via CREATE.
async fn open_srvsvc_pipe(conn: &mut Connection, tree_id: TreeId) -> Result<FileId> {
    let req = CreateRequest {
        requested_oplock_level: OplockLevel::None,
        impersonation_level: ImpersonationLevel::Impersonation,
        desired_access: FileAccessMask::new(
            FileAccessMask::FILE_READ_DATA | FileAccessMask::FILE_WRITE_DATA,
        ),
        file_attributes: 0,
        share_access: ShareAccess(ShareAccess::FILE_SHARE_READ | ShareAccess::FILE_SHARE_WRITE),
        create_disposition: CreateDisposition::FileOpen,
        create_options: 0,
        name: r"srvsvc".to_string(),
        create_contexts: vec![],
    };

    let frame = conn.execute(Command::Create, &req, Some(tree_id)).await?;

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::Create,
        });
    }

    let mut cursor = ReadCursor::new(&frame.body);
    let resp = CreateResponse::unpack(&mut cursor)?;
    trace!("shares: opened srvsvc pipe, file_id={:?}", resp.file_id);
    Ok(resp.file_id)
}

/// Write data to the pipe.
async fn write_pipe(
    conn: &mut Connection,
    tree_id: TreeId,
    file_id: FileId,
    data: &[u8],
) -> Result<()> {
    // DataOffset: header (64) + fixed write body (48) = 112 = 0x70
    let req = WriteRequest {
        data_offset: 0x70,
        offset: 0,
        file_id,
        channel: 0,
        remaining_bytes: 0,
        write_channel_info_offset: 0,
        write_channel_info_length: 0,
        flags: 0,
        data: data.to_vec(),
    };

    let frame = conn.execute(Command::Write, &req, Some(tree_id)).await?;

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::Write,
        });
    }

    let mut cursor = ReadCursor::new(&frame.body);
    let resp = WriteResponse::unpack(&mut cursor)?;
    trace!("shares: wrote {} bytes to pipe", resp.count);
    Ok(())
}

/// Read one complete pipe message, following `STATUS_BUFFER_OVERFLOW`.
///
/// A pipe message larger than our read buffer comes back as one or more
/// `STATUS_BUFFER_OVERFLOW` reads carrying partial data, terminated by a
/// `STATUS_SUCCESS` read with the remainder (MS-SMB2 3.3.5.10). We append each
/// chunk until a `SUCCESS` read completes the message.
async fn read_pipe_message(
    conn: &mut Connection,
    tree_id: TreeId,
    file_id: FileId,
) -> Result<Vec<u8>> {
    let mut message = Vec::new();

    loop {
        let req = ReadRequest {
            padding: 0x50,
            flags: 0,
            length: PIPE_READ_BUFFER_SIZE,
            offset: 0,
            file_id,
            minimum_count: 0,
            channel: SMB2_CHANNEL_NONE,
            remaining_bytes: 0,
            read_channel_info: vec![],
        };

        let frame = conn.execute(Command::Read, &req, Some(tree_id)).await?;

        let status = frame.header.status;
        // BUFFER_OVERFLOW is a warning meaning "partial data, read again", not a
        // failure -- accept it alongside SUCCESS.
        if !status.is_success_or_partial() {
            return Err(Error::Protocol {
                status,
                command: Command::Read,
            });
        }

        let mut cursor = ReadCursor::new(&frame.body);
        let resp = ReadResponse::unpack(&mut cursor)?;
        let chunk_len = resp.data.len();
        message.extend_from_slice(&resp.data);

        // SUCCESS completes the message; BUFFER_OVERFLOW means read more.
        if status != NtStatus::BUFFER_OVERFLOW {
            break;
        }
        // Guard against a server that signals overflow but sends no data, which
        // would otherwise spin forever.
        if chunk_len == 0 {
            return Err(Error::invalid_data(
                "pipe read returned BUFFER_OVERFLOW with no data",
            ));
        }
    }

    trace!("shares: read {} bytes from pipe", message.len());
    Ok(message)
}

/// Close a file handle.
async fn close_handle(conn: &mut Connection, tree_id: TreeId, file_id: FileId) -> Result<()> {
    let req = CloseRequest { flags: 0, file_id };

    let frame = conn.execute(Command::Close, &req, Some(tree_id)).await?;

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::Close,
        });
    }

    Ok(())
}

/// Disconnect from a tree.
async fn tree_disconnect(conn: &mut Connection, tree_id: TreeId) -> Result<()> {
    let body = TreeDisconnectRequest;
    let frame = conn
        .execute(Command::TreeDisconnect, &body, Some(tree_id))
        .await?;

    if frame.header.status != NtStatus::SUCCESS {
        return Err(Error::Protocol {
            status: frame.header.status,
            command: Command::TreeDisconnect,
        });
    }

    info!("shares: disconnected from IPC$");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::client::connection::{pack_message, NegotiatedParams};
    use crate::client::test_helpers::{
        build_close_response, build_create_response, build_tree_connect_response, setup_connection,
    };
    use crate::msg::header::Header;
    use crate::msg::read::ReadResponse as ReadResp;
    use crate::msg::tree_connect::ShareType;
    use crate::msg::tree_disconnect::TreeDisconnectResponse;
    use crate::msg::write::WriteResponse as WriteResp;
    use crate::pack::Guid;
    use crate::rpc::srvsvc::{STYPE_DISKTREE, STYPE_IPC, STYPE_SPECIAL};
    use crate::transport::MockTransport;
    use crate::types::flags::Capabilities;
    use crate::types::{Dialect, SessionId, TreeId};
    use std::sync::Arc;

    fn build_write_response(count: u32) -> Vec<u8> {
        let mut h = Header::new_request(Command::Write);
        h.flags.set_response();
        h.credits = 32;

        let body = WriteResp {
            count,
            remaining: 0,
            write_channel_info_offset: 0,
            write_channel_info_length: 0,
        };

        pack_message(&h, &body)
    }

    fn build_read_response(data: Vec<u8>) -> Vec<u8> {
        build_read_response_with_status(data, NtStatus::SUCCESS)
    }

    /// Build a READ response with an explicit NTSTATUS.
    ///
    /// Pipe reads use `STATUS_BUFFER_OVERFLOW` to mean "this read returned a
    /// partial message; read again for the rest."
    fn build_read_response_with_status(data: Vec<u8>, status: NtStatus) -> Vec<u8> {
        let mut h = Header::new_request(Command::Read);
        h.flags.set_response();
        h.credits = 32;
        h.status = status;

        let body = ReadResp {
            data_offset: 0x50,
            data_remaining: 0,
            flags: 0,
            data,
        };

        pack_message(&h, &body)
    }

    fn build_tree_disconnect_response() -> Vec<u8> {
        let mut h = Header::new_request(Command::TreeDisconnect);
        h.flags.set_response();
        h.credits = 32;
        pack_message(&h, &TreeDisconnectResponse)
    }

    /// Build a canned RPC BIND_ACK response.
    fn build_bind_ack() -> Vec<u8> {
        use crate::pack::WriteCursor;

        let mut w = WriteCursor::with_capacity(64);
        // Common header
        w.write_u8(5); // version
        w.write_u8(0); // version minor
        w.write_u8(12); // BIND_ACK type
        w.write_u8(0x03); // flags (first + last)
        w.write_bytes(&[0x10, 0x00, 0x00, 0x00]); // data rep
        let frag_len_pos = w.position();
        w.write_u16_le(0); // frag length placeholder
        w.write_u16_le(0); // auth length
        w.write_u32_le(1); // call id

        // BIND_ACK specific
        w.write_u16_le(4280); // max xmit frag
        w.write_u16_le(4280); // max recv frag
        w.write_u32_le(0x12345); // assoc group

        // Secondary address (empty)
        w.write_u16_le(0);
        w.write_bytes(&[0, 0]); // padding

        // Result list
        w.write_u8(1); // num results
        w.write_bytes(&[0, 0, 0]); // reserved
        w.write_u16_le(0); // result = accepted
        w.write_u16_le(0); // reason

        // Transfer syntax UUID + version (20 bytes)
        use crate::pack::Pack;
        let ndr_uuid = Guid {
            data1: 0x8A885D04,
            data2: 0x1CEB,
            data3: 0x11C9,
            data4: [0x9F, 0xE8, 0x08, 0x00, 0x2B, 0x10, 0x48, 0x60],
        };
        ndr_uuid.pack(&mut w);
        w.write_u32_le(2);

        let total_len = w.position();
        w.set_u16_le_at(frag_len_pos, total_len as u16);
        w.into_inner()
    }

    /// Build the NDR stub for a NetShareEnumAll RESPONSE (no RPC envelope).
    fn build_share_enum_stub(shares: &[(&str, u32, &str)]) -> Vec<u8> {
        use crate::pack::WriteCursor;

        // Build NDR stub
        let mut w = WriteCursor::with_capacity(512);
        let count = shares.len() as u32;

        // Level = 1
        w.write_u32_le(1);
        // Union discriminant = 1
        w.write_u32_le(1);

        if count == 0 {
            w.write_u32_le(0); // null container
            w.write_u32_le(0); // total entries
            w.write_u32_le(0); // resume handle
            w.write_u32_le(0); // return value
        } else {
            // Container pointer
            w.write_u32_le(0x0002_0000);
            // EntriesRead
            w.write_u32_le(count);
            // Array pointer
            w.write_u32_le(0x0002_0004);
            // MaxCount
            w.write_u32_le(count);

            // Fixed entries
            for (i, &(_, share_type, _)) in shares.iter().enumerate() {
                w.write_u32_le(0x0002_0008 + (i as u32) * 2); // name ref
                w.write_u32_le(share_type);
                w.write_u32_le(0x0002_0108 + (i as u32) * 2); // comment ref
            }

            // Deferred strings
            for &(name, _, comment) in shares {
                write_ndr_string(&mut w, name);
                write_ndr_string(&mut w, comment);
            }

            w.write_u32_le(count); // total entries
            w.write_u32_le(0); // resume handle
            w.write_u32_le(0); // return value
        }

        w.into_inner()
    }

    /// Wrap NDR stub bytes in an RPC RESPONSE PDU with the given PFC flags.
    ///
    /// `pfc_flags` lets a caller emit a fragment (for example, `PFC_FIRST_FRAG`
    /// alone for a non-final fragment) instead of the usual `FIRST | LAST`.
    fn wrap_rpc_response_pdu(stub_chunk: &[u8], pfc_flags: u8) -> Vec<u8> {
        use crate::pack::WriteCursor;

        let mut w = WriteCursor::with_capacity(24 + stub_chunk.len());
        w.write_u8(5);
        w.write_u8(0);
        w.write_u8(2); // RESPONSE
        w.write_u8(pfc_flags);
        w.write_bytes(&[0x10, 0x00, 0x00, 0x00]);
        let frag_len_pos = w.position();
        w.write_u16_le(0);
        w.write_u16_le(0);
        w.write_u32_le(2); // call id

        w.write_u32_le(stub_chunk.len() as u32); // alloc hint
        w.write_u16_le(0); // context id
        w.write_u8(0); // cancel count
        w.write_u8(0); // reserved

        w.write_bytes(stub_chunk);

        let total_len = w.position();
        w.set_u16_le_at(frag_len_pos, total_len as u16);
        w.into_inner()
    }

    /// Build a canned single-fragment RPC RESPONSE with NetShareEnumAll data.
    fn build_share_enum_response(shares: &[(&str, u32, &str)]) -> Vec<u8> {
        // 0x03 = PFC_FIRST_FRAG | PFC_LAST_FRAG (a complete, single-fragment PDU).
        wrap_rpc_response_pdu(&build_share_enum_stub(shares), 0x03)
    }

    fn write_ndr_string(w: &mut crate::pack::WriteCursor, s: &str) {
        let utf16: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        let char_count = utf16.len() as u32;
        w.write_u32_le(char_count);
        w.write_u32_le(0);
        w.write_u32_le(char_count);
        for &code_unit in &utf16 {
            w.write_u16_le(code_unit);
        }
        w.align_to(4);
    }

    /// Queue all the responses needed for a full list_shares flow.
    pub(crate) fn queue_share_listing_responses(
        mock: &MockTransport,
        shares: &[(&str, u32, &str)],
    ) {
        let tree_id = TreeId(42);
        let file_id = FileId {
            persistent: 0xAAAA,
            volatile: 0xBBBB,
        };

        // 1. TREE_CONNECT response
        mock.queue_response(build_tree_connect_response(tree_id, ShareType::Pipe));
        // 2. CREATE response (open srvsvc pipe)
        mock.queue_response(build_create_response(file_id, 0));
        // 3. WRITE response (RPC BIND)
        mock.queue_response(build_write_response(72));
        // 4. READ response (BIND_ACK)
        mock.queue_response(build_read_response(build_bind_ack()));
        // 5. WRITE response (NetShareEnumAll request)
        mock.queue_response(build_write_response(100));
        // 6. READ response (NetShareEnumAll response)
        mock.queue_response(build_read_response(build_share_enum_response(shares)));
        // 7. CLOSE response
        mock.queue_response(build_close_response());
        // 8. TREE_DISCONNECT response
        mock.queue_response(build_tree_disconnect_response());
    }

    /// Like `queue_share_listing_responses`, but the server splits a single
    /// RPC RESPONSE PDU across two pipe reads: the first read returns
    /// `STATUS_BUFFER_OVERFLOW` with the leading bytes, the second returns
    /// `SUCCESS` with the rest. The client must stitch them before parsing.
    fn queue_overflow_share_listing_responses(mock: &MockTransport, shares: &[(&str, u32, &str)]) {
        let tree_id = TreeId(42);
        let file_id = FileId {
            persistent: 0xAAAA,
            volatile: 0xBBBB,
        };

        let pdu = build_share_enum_response(shares);
        let split = pdu.len() / 2;
        let (first, rest) = pdu.split_at(split);

        mock.queue_response(build_tree_connect_response(tree_id, ShareType::Pipe));
        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response(72));
        mock.queue_response(build_read_response(build_bind_ack()));
        mock.queue_response(build_write_response(100));
        // The response PDU arrives in two chunks: overflow then success.
        mock.queue_response(build_read_response_with_status(
            first.to_vec(),
            NtStatus::BUFFER_OVERFLOW,
        ));
        mock.queue_response(build_read_response_with_status(
            rest.to_vec(),
            NtStatus::SUCCESS,
        ));
        mock.queue_response(build_close_response());
        mock.queue_response(build_tree_disconnect_response());
    }

    /// Like `queue_share_listing_responses`, but the RPC RESPONSE is split into
    /// two DCE/RPC fragments (each its own pipe message): the first carries
    /// `PFC_FIRST_FRAG`, the second `PFC_LAST_FRAG`. The client must reassemble
    /// the stub across fragments before parsing.
    fn queue_fragmented_share_listing_responses(
        mock: &MockTransport,
        shares: &[(&str, u32, &str)],
    ) {
        let tree_id = TreeId(42);
        let file_id = FileId {
            persistent: 0xAAAA,
            volatile: 0xBBBB,
        };

        let stub = build_share_enum_stub(shares);
        let split = stub.len() / 2;
        let (first, rest) = stub.split_at(split);
        let frag1 = wrap_rpc_response_pdu(first, 0x01); // PFC_FIRST_FRAG only
        let frag2 = wrap_rpc_response_pdu(rest, 0x02); // PFC_LAST_FRAG only

        mock.queue_response(build_tree_connect_response(tree_id, ShareType::Pipe));
        mock.queue_response(build_create_response(file_id, 0));
        mock.queue_response(build_write_response(72));
        mock.queue_response(build_read_response(build_bind_ack()));
        mock.queue_response(build_write_response(100));
        mock.queue_response(build_read_response(frag1));
        mock.queue_response(build_read_response(frag2));
        mock.queue_response(build_close_response());
        mock.queue_response(build_tree_disconnect_response());
    }

    #[tokio::test]
    async fn list_shares_reassembles_buffer_overflow_reads() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        queue_overflow_share_listing_responses(
            &mock,
            &[
                ("Documents", STYPE_DISKTREE, "Shared docs"),
                ("Photos", STYPE_DISKTREE, "Family photos"),
            ],
        );

        let shares = list_shares(&mut conn).await.unwrap();

        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].name, "Documents");
        assert_eq!(shares[1].name, "Photos");
    }

    #[tokio::test]
    async fn list_shares_reassembles_rpc_fragments() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        queue_fragmented_share_listing_responses(
            &mock,
            &[
                ("Documents", STYPE_DISKTREE, "Shared docs"),
                ("Photos", STYPE_DISKTREE, "Family photos"),
            ],
        );

        let shares = list_shares(&mut conn).await.unwrap();

        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].name, "Documents");
        assert_eq!(shares[1].name, "Photos");
    }

    #[tokio::test]
    async fn list_shares_returns_disk_shares() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        queue_share_listing_responses(
            &mock,
            &[
                ("Documents", STYPE_DISKTREE, "Shared docs"),
                ("IPC$", STYPE_IPC | STYPE_SPECIAL, "Remote IPC"),
                ("C$", STYPE_DISKTREE | STYPE_SPECIAL, "Default share"),
                ("Photos", STYPE_DISKTREE, "Family photos"),
            ],
        );

        let shares = list_shares(&mut conn).await.unwrap();

        // Only disk shares without $ suffix and without STYPE_SPECIAL
        assert_eq!(shares.len(), 2);
        assert_eq!(shares[0].name, "Documents");
        assert_eq!(shares[0].comment, "Shared docs");
        assert_eq!(shares[1].name, "Photos");
        assert_eq!(shares[1].comment, "Family photos");
    }

    #[tokio::test]
    async fn list_shares_sends_correct_number_of_messages() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        queue_share_listing_responses(&mock, &[("TestShare", STYPE_DISKTREE, "A test share")]);

        let _shares = list_shares(&mut conn).await.unwrap();

        // Should have sent 8 messages:
        // TREE_CONNECT, CREATE, WRITE(bind), READ(bind_ack),
        // WRITE(request), READ(response), CLOSE, TREE_DISCONNECT
        assert_eq!(mock.sent_count(), 8);
    }

    #[tokio::test]
    async fn list_shares_empty_server() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        queue_share_listing_responses(&mock, &[]);

        let shares = list_shares(&mut conn).await.unwrap();
        assert!(shares.is_empty());
    }

    #[tokio::test]
    async fn list_shares_filters_non_disk_shares() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = setup_connection(&mock);

        // All non-disk or special shares
        queue_share_listing_responses(
            &mock,
            &[
                ("IPC$", STYPE_IPC | STYPE_SPECIAL, "Remote IPC"),
                ("ADMIN$", STYPE_DISKTREE | STYPE_SPECIAL, "Remote Admin"),
            ],
        );

        let shares = list_shares(&mut conn).await.unwrap();
        assert!(shares.is_empty());
    }

    #[tokio::test]
    async fn list_shares_uses_correct_server_name() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn =
            Connection::from_transport(Box::new(mock.clone()), Box::new(mock.clone()), "my-nas");
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        conn.set_credits(512);
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

        queue_share_listing_responses(&mock, &[("share1", STYPE_DISKTREE, "")]);

        let shares = list_shares(&mut conn).await.unwrap();
        assert_eq!(shares.len(), 1);

        // Verify the TREE_CONNECT request contains \\my-nas\IPC$
        let sent = mock.sent_messages();
        let tree_connect_bytes = &sent[0];
        // The UNC path is UTF-16LE in the request body
        let unc_utf8 = String::from_utf8_lossy(tree_connect_bytes);
        // Verify the server name appears somewhere in the raw bytes
        assert!(
            tree_connect_bytes.windows(2).any(|w| w == b"m\0"), // 'm' in UTF-16LE from "my-nas"
            "TREE_CONNECT should reference the server name"
        );
        drop(unc_utf8);
    }
}
