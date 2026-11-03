//! When a connection's socket closes: as soon as the connection is finished
//! with, never "whenever the server gets around to it".
//!
//! Each test holds the server end of a real loopback socket and watches for
//! the client's FIN, because that is the only portable way to see whether the
//! client closed its file descriptor. A mock transport can't show it: the bugs
//! these pin are a transport kept alive by the crate's own tasks, and a mock
//! has no socket to leave open.
//!
//! The servers here never speak SMB. None of these tests needs a session, and
//! a connection with nothing outstanding sends nothing (the keepalive only
//! probes a connection that has work in flight), so the wire stays quiet
//! until the client closes it.
//!
//! **Every wait is bounded.** An open socket shows up as a timeout, never as
//! a hang.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::client::connection::Connection;

/// How long the client gets to close its end. Closing is immediate once
/// nothing holds the transport, so this is only slack for a loaded machine.
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

/// A connected client, and the server's end of its socket.
async fn loopback() -> (Connection, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let (conn, accepted) = tokio::join!(
        Connection::connect(&addr, Duration::from_secs(5)),
        listener.accept()
    );
    (conn.unwrap(), accepted.unwrap().0)
}

/// Whether the client closed its end within `budget`, read from the server's
/// side: EOF (or a reset) means the client's fd is gone.
async fn client_closed<R: AsyncReadExt + Unpin>(server: &mut R, budget: Duration) -> bool {
    let mut buf = [0u8; 64];
    match tokio::time::timeout(budget, server.read(&mut buf)).await {
        Ok(Ok(0)) | Ok(Err(_)) => true,
        Ok(Ok(n)) => panic!("the client sent {n} bytes nobody asked for"),
        Err(_) => false,
    }
}

/// Dropping every clone must close the socket, whatever the server does.
///
/// The receiver task used to hold a strong reference to the connection's
/// state, and only that state's `Drop` could stop the receiver task, so the
/// two kept each other alive. The socket stayed open until the server hung
/// up, and a server that never reaps idle sessions (Samba's default) never
/// does. A file manager that mounted and unmounted a share ~60 times held 78
/// such sockets, each with a keepalive timer firing once a second.
#[tokio::test]
async fn dropping_the_last_clone_closes_the_socket() {
    let (conn, mut server) = loopback().await;
    let clone = conn.clone();

    drop(conn);
    assert!(
        !client_closed(&mut server, Duration::from_millis(200)).await,
        "a clone is still alive, so the socket must stay open"
    );

    drop(clone);
    assert!(
        client_closed(&mut server, CLOSE_BUDGET).await,
        "every clone is gone, so the socket must close without waiting for the server"
    );
}

/// A server that hangs up must get the socket closed back, even while the
/// consumer keeps the dead connection around.
///
/// The receiver task noticed the hang-up and exited, but the writer task,
/// which shares the transport, stayed parked waiting for a frame that would
/// never come. The socket sat in `CLOSE_WAIT` for as long as the `Connection`
/// lived, and a consumer that keeps a dead session until its next operation
/// fails can hold it for a long time.
///
/// The server half-closes rather than closing outright, so it can still read
/// and see the client's FIN arrive.
#[tokio::test]
async fn a_server_hang_up_closes_the_socket_on_our_side_too() {
    let (conn, server) = loopback().await;
    let (mut server_read, mut server_write) = server.into_split();
    server_write.shutdown().await.unwrap();

    tokio::time::timeout(CLOSE_BUDGET, async {
        while !conn.is_disconnected() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the connection should notice the hang-up");

    assert!(
        client_closed(&mut server_read, CLOSE_BUDGET).await,
        "the connection is dead, so its socket must close while the consumer still holds it"
    );
    drop(conn);
}

/// `mark_dead` is a consumer saying "this connection is finished", so the
/// socket has to go with it, not linger until the last clone drops.
#[tokio::test]
async fn marking_a_connection_dead_closes_the_socket() {
    let (conn, mut server) = loopback().await;

    conn.mark_dead();

    assert!(conn.is_disconnected());
    assert!(
        client_closed(&mut server, CLOSE_BUDGET).await,
        "a connection marked dead must close its socket while the consumer still holds it"
    );
}
