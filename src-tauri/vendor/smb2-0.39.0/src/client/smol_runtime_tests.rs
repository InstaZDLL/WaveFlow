//! The client on smol, with no tokio runtime anywhere in the process's path.
//!
//! Each test runs inside `smol::block_on` and first proves there is no tokio
//! runtime to fall back on, so a code path that still reaches for tokio's
//! reactor panics here instead of passing quietly. That panic ("there is no
//! reactor running") is exactly what issue #1 reported.
//!
//! The server is a plain `std::net` socket on a thread of its own: it answers
//! NEGOTIATE and ECHO, or stays silent when told to. Real sockets matter
//! here, because the part that differs between runtimes is the socket, the
//! tasks that own it, and the timers around it, and a mock transport has
//! none of those.
//!
//! **Every wait is bounded**, so a regression shows up as a failure, never a
//! hang.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::client::connection::{pack_message, Connection};
use crate::error::Error;
use crate::msg::echo::{EchoRequest, EchoResponse};
use crate::msg::header::Header;
use crate::msg::negotiate::NegotiateResponse;
use crate::pack::{Guid, ReadCursor, Unpack};
use crate::types::flags::{Capabilities, SecurityMode};
use crate::types::{Command, Dialect};

/// How long the client gets to close its end. Closing is immediate once
/// nothing holds the transport, so this is only slack for a loaded machine.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// How the fake server treats ECHO.
#[derive(Clone, Copy)]
enum Echo {
    /// Answer every ECHO.
    Answer,
    /// Read every ECHO and never answer it.
    Ignore,
}

/// The server's side of one connection: its socket, plus a channel that
/// reports each request's command as the server reads it.
struct Server {
    socket: TcpStream,
    seen: mpsc::Receiver<Command>,
}

/// Run `test` on smol, after proving tokio isn't there to catch a mistake.
fn on_smol<F: std::future::Future<Output = ()>>(test: F) {
    smol::block_on(async {
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "these tests must run with no tokio runtime in reach"
        );
        test.await;
    });
}

/// A connected client and the server's end of its socket. The server thread
/// answers NEGOTIATE and treats ECHO as `echo` says; it exits when the
/// client closes the socket.
async fn loopback(echo: Echo) -> (Connection, Server) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (seen_tx, seen) = mpsc::channel();
    thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        accepted_tx.send(socket.try_clone().unwrap()).unwrap();
        serve(socket, echo, seen_tx);
    });

    let conn = Connection::connect(&addr, Duration::from_secs(5))
        .await
        .unwrap();
    let socket = accepted_rx.recv_timeout(CLOSE_BUDGET).unwrap();
    (conn, Server { socket, seen })
}

/// The fake server's loop: one frame in, at most one frame out.
fn serve(mut socket: TcpStream, echo: Echo, seen: mpsc::Sender<Command>) {
    loop {
        let mut prefix = [0u8; 4];
        if socket.read_exact(&mut prefix).is_err() {
            return; // the client closed its end
        }
        let len = u32::from_be_bytes(prefix) as usize;
        let mut frame = vec![0u8; len];
        if socket.read_exact(&mut frame).is_err() {
            return;
        }
        let request = Header::unpack(&mut ReadCursor::new(&frame)).unwrap();
        let _ = seen.send(request.command);

        let response = match (request.command, echo) {
            (Command::Negotiate, _) => negotiate_response(&request),
            (Command::Echo, Echo::Answer) => {
                pack_message(&response_header(&request), &EchoResponse)
            }
            (Command::Echo, Echo::Ignore) => continue,
            (other, _) => panic!("the fake server only speaks NEGOTIATE and ECHO, got {other:?}"),
        };
        let mut out = (response.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&response);
        if socket.write_all(&out).is_err() {
            return;
        }
    }
}

/// A response header for `request`: same command and id, generous credits.
fn response_header(request: &Header) -> Header {
    let mut header = Header::new_request(request.command);
    header.flags.set_response();
    header.message_id = request.message_id;
    header.credits = 32;
    header
}

/// The smallest NEGOTIATE response the client accepts: SMB 2.1, no contexts.
fn negotiate_response(request: &Header) -> Vec<u8> {
    let body = NegotiateResponse {
        security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
        dialect_revision: Dialect::Smb2_1,
        server_guid: Guid::ZERO,
        capabilities: Capabilities::new(0),
        max_transact_size: 65536,
        max_read_size: 65536,
        max_write_size: 65536,
        system_time: 132_000_000_000_000_000,
        server_start_time: 131_000_000_000_000_000,
        security_buffer: vec![0x60, 0x00],
        negotiate_contexts: vec![],
    };
    pack_message(&response_header(request), &body)
}

/// Whether the client closed its end within `budget`, read from the server's
/// side: EOF (or a reset) means the client's fd is gone. Blocking, which is
/// fine: the client's tasks run on smol's executor threads, not this one.
fn client_closed(server: &mut TcpStream, budget: Duration) -> bool {
    // macOS refuses the option (EINVAL) on a socket whose connection is
    // already closed both ways, which after our own half-close means the
    // client closed its end.
    if server.set_read_timeout(Some(budget)).is_err() {
        return true;
    }
    let mut buf = [0u8; 64];
    match server.read(&mut buf) {
        Ok(0) => true,
        Ok(n) => panic!("the client sent {n} bytes nobody asked for"),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => false,
        Err(_) => true,
    }
}

/// The whole round trip: a NEGOTIATE and an ECHO over a real socket, which
/// needs the socket, the writer and receiver tasks, and the timers around
/// every wait to all work on smol.
#[test]
fn a_connection_negotiates_and_answers_without_a_tokio_runtime() {
    on_smol(async {
        let (mut conn, server) = loopback(Echo::Answer).await;

        conn.negotiate().await.unwrap();
        assert_eq!(conn.params().unwrap().dialect, Dialect::Smb2_1);

        let frame = conn
            .execute(Command::Echo, &EchoRequest, None)
            .await
            .unwrap();
        assert_eq!(frame.header.command, Command::Echo);

        assert_eq!(
            server.seen.recv_timeout(CLOSE_BUDGET).unwrap(),
            Command::Negotiate
        );
        assert_eq!(
            server.seen.recv_timeout(CLOSE_BUDGET).unwrap(),
            Command::Echo
        );
    });
}

/// A server that stops answering still ends the wait: the response deadline
/// is a timer, and on smol it has to be smol's.
#[test]
fn the_response_deadline_fires_without_a_tokio_runtime() {
    on_smol(async {
        let (mut conn, server) = loopback(Echo::Ignore).await;
        conn.negotiate().await.unwrap();
        conn.set_keepalive(None);
        conn.set_response_timeout(Some(Duration::from_millis(300)));

        let started = std::time::Instant::now();
        let result = conn.execute(Command::Echo, &EchoRequest, None).await;

        assert!(matches!(result, Err(Error::Timeout)), "got {result:?}");
        assert!(
            started.elapsed() < CLOSE_BUDGET,
            "the deadline must fire, not the test's patience"
        );
        assert_eq!(
            server.seen.recv_timeout(CLOSE_BUDGET).unwrap(),
            Command::Negotiate
        );
        assert_eq!(
            server.seen.recv_timeout(CLOSE_BUDGET).unwrap(),
            Command::Echo
        );
    });
}

/// Dropping every clone closes the socket, exactly as on tokio (see
/// `socket_lifecycle_tests`). On smol this also depends on aborting a task
/// really stopping it, and on the write half letting go of the socket.
#[test]
fn dropping_the_last_clone_closes_the_socket_without_a_tokio_runtime() {
    on_smol(async {
        let (conn, mut server) = loopback(Echo::Answer).await;
        let clone = conn.clone();

        drop(conn);
        assert!(
            !client_closed(&mut server.socket, Duration::from_millis(200)),
            "a clone is still alive, so the socket must stay open"
        );

        drop(clone);
        assert!(
            client_closed(&mut server.socket, CLOSE_BUDGET),
            "every clone is gone, so the socket must close without waiting for the server"
        );
    });
}

/// A server hang-up gets the socket closed back while the consumer still
/// holds the dead connection.
#[test]
fn a_server_hang_up_closes_the_socket_on_our_side_too_without_a_tokio_runtime() {
    on_smol(async {
        let (conn, mut server) = loopback(Echo::Answer).await;
        server.socket.shutdown(std::net::Shutdown::Write).unwrap();

        // Polled from this thread, which the client's tasks don't run on.
        let started = std::time::Instant::now();
        while !conn.is_disconnected() {
            assert!(
                started.elapsed() < CLOSE_BUDGET,
                "the connection should notice the hang-up"
            );
            thread::sleep(Duration::from_millis(5));
        }

        assert!(
            client_closed(&mut server.socket, CLOSE_BUDGET),
            "the connection is dead, so its socket must close while the consumer still holds it"
        );
        drop(conn);
    });
}
