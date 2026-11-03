//! Transport abstraction for sending and receiving SMB2 messages.
//!
//! The transport layer handles framing (TCP's 4-byte length-prefix header)
//! and provides split send/receive traits to avoid deadlocks in the
//! pipeline's `tokio::select!` loop.
//!
//! Two implementations are provided:
//! - [`TcpTransport`] -- direct TCP connection to an SMB server (port 445)
//! - [`MockTransport`] -- canned responses for testing
//!
//! Most users don't need this module directly -- use [`SmbClient`](crate::SmbClient)
//! which handles transport setup internally.

pub mod mock;
pub mod progress;
pub mod tcp;

pub use mock::MockTransport;
pub use progress::{FrameProgress, ReceiveProgress, ReceiveSnapshot};
pub use tcp::{ConnectAttempt, ConnectOptions, TcpTransport};

use std::sync::Arc;

use crate::error::Result;
use async_trait::async_trait;

/// Send half of a transport connection.
#[async_trait]
pub trait TransportSend: Send + Sync {
    /// Send a complete SMB2 message (the implementation adds framing).
    async fn send(&self, data: &[u8]) -> Result<()>;
}

/// Receive half of a transport connection.
#[async_trait]
pub trait TransportReceive: Send + Sync {
    /// Receive one complete SMB2 transport frame.
    ///
    /// The implementation handles the TCP framing (4-byte header:
    /// 1 zero byte + 3-byte big-endian length). The returned buffer
    /// contains the SMB2 message(s) without the framing header.
    ///
    /// The buffer may contain multiple compounded responses linked
    /// by NextCommand in the SMB2 headers -- the caller must split them.
    async fn receive(&self) -> Result<Vec<u8>>;

    /// Live counts for what this transport is receiving, published while a
    /// frame is still arriving, or `None` if it can't see inside a frame.
    ///
    /// A connection asks once, when it takes the transport over, and reads the
    /// counts from then on. They feed its liveness clock, which is why this
    /// matters beyond a progress display: a large response trickling in over
    /// a slow link is the server talking, and a transport that returns `None`
    /// makes it look like silence until the last byte lands. The connection
    /// then counts whole frames itself, so the totals stay right.
    ///
    /// Return the same `Arc` every time. See [`ReceiveProgress`] for what to
    /// call from the receive path.
    fn receive_progress(&self) -> Option<Arc<ReceiveProgress>> {
        None
    }
}

/// A combined transport that can both send and receive.
pub trait Transport: TransportSend + TransportReceive {}

// Blanket implementation: anything that implements both halves is a Transport.
impl<T: TransportSend + TransportReceive> Transport for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::header::{Header, PROTOCOL_ID};
    use crate::msg::negotiate::{
        NegotiateContext, NegotiateRequest, NegotiateResponse, HASH_ALGORITHM_SHA512,
    };
    use crate::pack::{Guid, Pack, ReadCursor, Unpack, WriteCursor};
    use crate::types::flags::{Capabilities, SecurityMode};
    use crate::types::{Command, Dialect};

    /// Pack a header + body into raw SMB2 message bytes (no transport framing).
    fn pack_message(header: &Header, body: &dyn Pack) -> Vec<u8> {
        let mut cursor = WriteCursor::new();
        header.pack(&mut cursor);
        body.pack(&mut cursor);
        cursor.into_inner()
    }

    #[tokio::test]
    async fn cross_module_negotiate_via_mock_transport() {
        // Build a NegotiateRequest, send it through MockTransport,
        // receive a canned NegotiateResponse, and verify unpacking.

        let mock = MockTransport::new();

        // Build a negotiate request.
        let req_header = Header::new_request(Command::Negotiate);
        let req_body = NegotiateRequest {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::default(),
            client_guid: Guid {
                data1: 0xDEAD_BEEF,
                data2: 0xCAFE,
                data3: 0xF00D,
                data4: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            },
            dialects: vec![Dialect::Smb2_0_2, Dialect::Smb2_1, Dialect::Smb3_1_1],
            negotiate_contexts: vec![NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0xAA; 32],
            }],
        };
        let req_msg = pack_message(&req_header, &req_body);

        // Build a canned NegotiateResponse.
        let resp_header = {
            let mut h = Header::new_request(Command::Negotiate);
            h.flags.set_response();
            h.credits = 1;
            h
        };
        let resp_body = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: Dialect::Smb3_1_1,
            server_guid: Guid {
                data1: 0x1111_2222,
                data2: 0x3333,
                data3: 0x4444,
                data4: [0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC],
            },
            capabilities: Capabilities::new(Capabilities::DFS | Capabilities::LEASING),
            max_transact_size: 65536,
            max_read_size: 65536,
            max_write_size: 65536,
            system_time: 132_000_000_000_000_000,
            server_start_time: 131_000_000_000_000_000,
            security_buffer: vec![0x60, 0x00], // minimal placeholder
            negotiate_contexts: vec![NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0xBB; 32],
            }],
        };
        let resp_msg = pack_message(&resp_header, &resp_body);

        // Queue the canned response.
        mock.queue_response(resp_msg);

        // Send the request through the mock.
        mock.send(&req_msg).await.unwrap();

        // Receive the canned response.
        let received = mock.receive().await.unwrap();

        // Unpack and verify.
        let mut cursor = ReadCursor::new(&received);
        let hdr = Header::unpack(&mut cursor).unwrap();
        assert!(hdr.is_response());
        assert_eq!(hdr.command, Command::Negotiate);

        let body = NegotiateResponse::unpack(&mut cursor).unwrap();
        assert_eq!(body.dialect_revision, Dialect::Smb3_1_1);
        assert_eq!(body.max_read_size, 65536);
        assert!(body.security_mode.signing_enabled());

        // Verify the request was recorded.
        assert_eq!(mock.sent_count(), 1);
        let sent = mock.sent_message(0).unwrap();

        // Verify we can unpack what was sent.
        let mut cursor = ReadCursor::new(&sent);
        let sent_hdr = Header::unpack(&mut cursor).unwrap();
        assert_eq!(sent_hdr.command, Command::Negotiate);
        assert!(!sent_hdr.is_response());

        let sent_body = NegotiateRequest::unpack(&mut cursor).unwrap();
        assert_eq!(sent_body.dialects.len(), 3);
        assert!(sent_body.dialects.contains(&Dialect::Smb3_1_1));
    }

    #[tokio::test]
    #[ignore] // Requires NAS at 192.168.1.111
    async fn negotiate_via_tcp_transport() {
        use std::time::Duration;

        let transport = TcpTransport::connect("192.168.1.111:445", Duration::from_secs(5))
            .await
            .expect("failed to connect to NAS");

        // Build a negotiate request.
        let header = Header::new_request(Command::Negotiate);
        let request = NegotiateRequest {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::new(
                Capabilities::DFS | Capabilities::LEASING | Capabilities::LARGE_MTU,
            ),
            client_guid: Guid {
                data1: 0xDEAD_BEEF,
                data2: 0xCAFE,
                data3: 0xF00D,
                data4: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            },
            dialects: vec![
                Dialect::Smb2_0_2,
                Dialect::Smb2_1,
                Dialect::Smb3_0,
                Dialect::Smb3_0_2,
                Dialect::Smb3_1_1,
            ],
            negotiate_contexts: vec![NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: vec![0xAA; 32],
            }],
        };

        let msg = pack_message(&header, &request);

        // Send through transport (framing added automatically).
        transport.send(&msg).await.unwrap();

        // Receive response (framing stripped automatically).
        let resp_bytes = transport.receive().await.unwrap();

        // Verify we got a valid response.
        assert!(resp_bytes[0..4] == PROTOCOL_ID);

        let mut cursor = ReadCursor::new(&resp_bytes);
        let resp_header = Header::unpack(&mut cursor).unwrap();
        assert!(resp_header.is_response());
        assert_eq!(resp_header.command, Command::Negotiate);

        let resp_body = NegotiateResponse::unpack(&mut cursor).unwrap();
        assert!(Dialect::ALL.contains(&resp_body.dialect_revision));
        assert!(resp_body.max_read_size >= 65536);
    }
}
