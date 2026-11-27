//! A real listener and a real guest on the loopback: TLS, the WebSocket and
//! the exchange as they run between two machines.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::lan::address::PeerFilter;
use crate::lan::cert::Identity;
use crate::lan::client::{self, ClientStream, JoinError};
use crate::lan::code::Code;
use crate::lan::exchange::GuestExchange;
use crate::lan::handshake::{GuestHandshake, HandshakeRefusal, HostHandshake};
use crate::lan::listener::{Admission, Config, Incoming, Listener};
use crate::lan::{HANDSHAKE_TIMEOUT, HANDSHAKE_VERSION};

const CONTEXT: &str = "test";

struct Host {
    identity: Identity,
    listener: Listener,
    incoming: mpsc::Receiver<Incoming>,
    addr: SocketAddr,
}

async fn host_with(code: Option<&Code>, handshake_timeout: Duration) -> Host {
    let identity = Identity::generate().unwrap();
    let (listener, incoming) = Listener::start(
        &identity,
        Config {
            context: CONTEXT.into(),
            allow_private_vpn: false,
            port: 0,
            handshake_timeout,
        },
    )
    .await
    .unwrap();
    listener.set_code(code.cloned());
    let addr = SocketAddr::from(([127, 0, 0, 1], listener.port()));
    Host {
        identity,
        listener,
        incoming,
        addr,
    }
}

async fn host(code: Option<&Code>) -> Host {
    host_with(code, HANDSHAKE_TIMEOUT).await
}

fn filter() -> PeerFilter {
    PeerFilter::new(false, [])
}

fn code(text: &str) -> Code {
    Code::parse(text).unwrap()
}

async fn next(incoming: &mut mpsc::Receiver<Incoming>) -> Incoming {
    tokio::time::timeout(Duration::from_secs(10), incoming.recv())
        .await
        .expect("the listener said nothing")
        .expect("the listener stopped")
}

async fn send_json<T: serde::Serialize>(
    socket: &mut tokio_tungstenite::WebSocketStream<ClientStream>,
    message: &T,
) {
    socket
        .send(Message::text(serde_json::to_string(message).unwrap()))
        .await
        .unwrap();
}

async fn read_host(socket: &mut tokio_tungstenite::WebSocketStream<ClientStream>) -> HostHandshake {
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
        panic!("not a text frame");
    };
    serde_json::from_str(text.as_str()).unwrap()
}

#[tokio::test]
async fn a_guest_with_the_code_gets_in_and_both_can_talk() {
    let right = code("ABCDEFGH");
    let mut host = host(Some(&right)).await;
    let mut joined = client::join(host.addr, None, &right, CONTEXT, filter())
        .await
        .unwrap();
    assert_eq!(joined.fingerprint, host.identity.fingerprint());

    let Incoming::Accepted(mut accepted) = next(&mut host.incoming).await else {
        panic!("not admitted");
    };
    assert_eq!(accepted.admission, Admission::Code);
    assert!(accepted.peer.ip().is_loopback());

    joined
        .socket
        .send(Message::text("from the guest"))
        .await
        .unwrap();
    let got = accepted.socket.next().await.unwrap().unwrap();
    assert_eq!(got, Message::text("from the guest"));
    accepted
        .socket
        .send(Message::binary(vec![1u8, 2, 3]))
        .await
        .unwrap();
    let got = joined.socket.next().await.unwrap().unwrap();
    assert_eq!(got, Message::binary(vec![1u8, 2, 3]));
    assert_eq!(host.listener.wrong_codes(), 0);
}

#[tokio::test]
async fn a_wrong_code_is_counted_and_its_sender_waits() {
    let right = code("ABCDEFGH");
    let mut host = host(Some(&right)).await;
    let wrong = client::join(host.addr, None, &code("HGFEDCBA"), CONTEXT, filter()).await;
    assert_eq!(wrong.err(), Some(JoinError::CodeRejected));
    let failed_at = Instant::now();
    assert!(matches!(
        next(&mut host.incoming).await,
        Incoming::WrongCode { total: 1 }
    ));
    assert_eq!(host.listener.wrong_codes(), 1);

    // Even the right code waits now, from the same address; unless the
    // machine was so slow that the second of waiting is already over.
    match client::join(host.addr, None, &right, CONTEXT, filter()).await {
        Err(JoinError::Refused(HandshakeRefusal::RetryLater, Some(wait))) => {
            assert!(wait <= 1_000, "{wait}");
        }
        Ok(_) if failed_at.elapsed() >= Duration::from_millis(900) => {}
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("the right code got in during the wait"),
    }
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    client::join(host.addr, None, &right, CONTEXT, filter())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_pinned_certificate_is_checked_before_anything_is_sent() {
    let right = code("ABCDEFGH");
    let host = host(Some(&right)).await;
    let other = Identity::generate().unwrap().fingerprint();
    let result = client::join(host.addr, Some(other), &right, CONTEXT, filter()).await;
    assert_eq!(result.err(), Some(JoinError::FingerprintMismatch));
    assert_eq!(host.listener.wrong_codes(), 0, "no exchange was attempted");
    let joined = client::join(
        host.addr,
        Some(host.identity.fingerprint()),
        &right,
        CONTEXT,
        filter(),
    )
    .await
    .unwrap();
    assert_eq!(joined.fingerprint, host.identity.fingerprint());
}

/// Somebody else on the network announces a session; the guest types the
/// real host's code into it. The impostor cannot confirm, and the guest stops
/// before sending a confirmation of its own.
#[tokio::test]
async fn an_impostor_without_the_code_is_found_out() {
    let real = code("ABCDEFGH");
    let mut impostor = host(Some(&code("ZZZZZZZZ"))).await;
    let result = client::join(impostor.addr, None, &real, CONTEXT, filter()).await;
    assert_eq!(result.err(), Some(JoinError::CodeRejected));
    // The impostor only sees a guest that stopped.
    assert!(matches!(
        next(&mut impostor.incoming).await,
        Incoming::WrongCode { .. }
    ));
}

#[tokio::test]
async fn a_listener_says_why_it_refuses() {
    let right = code("ABCDEFGH");
    let closed = host(None).await;
    assert_eq!(
        client::join(closed.addr, None, &right, CONTEXT, filter())
            .await
            .err(),
        Some(JoinError::Refused(HandshakeRefusal::Closed, None))
    );
    let open = host(Some(&right)).await;
    assert_eq!(
        client::join(open.addr, None, &right, "phone-link", filter())
            .await
            .err(),
        Some(JoinError::Refused(HandshakeRefusal::Context, None))
    );
    // A new code replaces the old one at once.
    open.listener.set_code(Some(code("JKLMNPQR")));
    assert_eq!(
        client::join(open.addr, None, &right, CONTEXT, filter())
            .await
            .err(),
        Some(JoinError::CodeRejected)
    );
}

/// The host draws a new code while a guest is answering with the old one:
/// a correct answer to the old code opens nothing, and costs the guest no
/// wait.
#[tokio::test]
async fn a_code_withdrawn_while_the_guest_answers_opens_nothing() {
    let right = code("ABCDEFGH");
    let host = host(Some(&right)).await;
    let (mut joined, exporter) =
        client::connect(host.addr, Some(host.identity.fingerprint()), filter())
            .await
            .unwrap();
    let (exchange, message) = GuestExchange::start(&right, CONTEXT);
    send_json(
        &mut joined.socket,
        &GuestHandshake::ExchangeStart {
            version: HANDSHAKE_VERSION,
            context: CONTEXT.into(),
            guest: hex::encode(message),
        },
    )
    .await;
    let HostHandshake::ExchangeReply {
        host: host_message,
        confirm,
    } = read_host(&mut joined.socket).await
    else {
        panic!("not a reply");
    };
    host.listener.set_code(Some(code("JKLMNPQR")));
    let confirm = exchange
        .finish(
            &hex::decode(host_message).unwrap(),
            &hex::decode(confirm).unwrap(),
            &exporter,
        )
        .unwrap();
    send_json(
        &mut joined.socket,
        &GuestHandshake::ExchangeConfirm {
            confirm: hex::encode(confirm),
        },
    )
    .await;
    assert_eq!(
        read_host(&mut joined.socket).await,
        HostHandshake::ExchangeRefused {
            reason: HandshakeRefusal::Expired,
            retry_after_ms: None
        }
    );
    assert_eq!(host.listener.wrong_codes(), 0);
    // And the new code works straight away from the same address.
    client::join(host.addr, None, &code("JKLMNPQR"), CONTEXT, filter())
        .await
        .unwrap();
}

/// Closing admission and opening it again with the same code still ends the
/// exchanges begun before.
#[tokio::test]
async fn closing_admission_ends_the_exchanges_under_way() {
    let right = code("ABCDEFGH");
    let host = host(Some(&right)).await;
    let (mut joined, exporter) =
        client::connect(host.addr, Some(host.identity.fingerprint()), filter())
            .await
            .unwrap();
    let (exchange, message) = GuestExchange::start(&right, CONTEXT);
    send_json(
        &mut joined.socket,
        &GuestHandshake::ExchangeStart {
            version: HANDSHAKE_VERSION,
            context: CONTEXT.into(),
            guest: hex::encode(message),
        },
    )
    .await;
    let HostHandshake::ExchangeReply {
        host: host_message,
        confirm,
    } = read_host(&mut joined.socket).await
    else {
        panic!("not a reply");
    };
    host.listener.set_code(None);
    host.listener.set_code(Some(right.clone()));
    let confirm = exchange
        .finish(
            &hex::decode(host_message).unwrap(),
            &hex::decode(confirm).unwrap(),
            &exporter,
        )
        .unwrap();
    send_json(
        &mut joined.socket,
        &GuestHandshake::ExchangeConfirm {
            confirm: hex::encode(confirm),
        },
    )
    .await;
    assert_eq!(
        read_host(&mut joined.socket).await,
        HostHandshake::ExchangeRefused {
            reason: HandshakeRefusal::Expired,
            retry_after_ms: None
        }
    );
}

/// A guest that goes silent after the host's confirmation is counted when its
/// handshake runs out, and a second attempt from the same address meanwhile
/// waits for the first.
#[tokio::test]
async fn a_guest_that_never_confirms_is_counted() {
    let right = code("ABCDEFGH");
    let mut host = host_with(Some(&right), Duration::from_secs(1)).await;
    let (mut joined, _) = client::connect(host.addr, Some(host.identity.fingerprint()), filter())
        .await
        .unwrap();
    let (_exchange, message) = GuestExchange::start(&right, CONTEXT);
    send_json(
        &mut joined.socket,
        &GuestHandshake::ExchangeStart {
            version: HANDSHAKE_VERSION,
            context: CONTEXT.into(),
            guest: hex::encode(message),
        },
    )
    .await;
    assert!(matches!(
        read_host(&mut joined.socket).await,
        HostHandshake::ExchangeReply { .. }
    ));
    assert_eq!(
        client::join(host.addr, None, &right, CONTEXT, filter())
            .await
            .err(),
        Some(JoinError::Refused(
            HandshakeRefusal::RetryLater,
            Some(1_000)
        ))
    );
    assert!(matches!(
        next(&mut host.incoming).await,
        Incoming::WrongCode { total: 1 }
    ));
    drop(joined);
}

#[tokio::test]
async fn a_returning_guest_is_handed_over_unverified() {
    let mut host = host(None).await;
    let mut joined = client::reconnect(host.addr, host.identity.fingerprint(), filter())
        .await
        .unwrap();
    let first = r#"{"type":"resume","participant":2}"#;
    joined.socket.send(Message::text(first)).await.unwrap();
    let Incoming::Accepted(accepted) = next(&mut host.incoming).await else {
        panic!("not handed over");
    };
    assert_eq!(
        accepted.admission,
        Admission::Unverified {
            first: first.to_string()
        }
    );

    let wrong_pin = Identity::generate().unwrap().fingerprint();
    assert_eq!(
        client::reconnect(host.addr, wrong_pin, filter())
            .await
            .err(),
        Some(JoinError::FingerprintMismatch)
    );
}

#[tokio::test]
async fn a_guest_never_connects_to_a_public_address() {
    let result = client::join(
        "8.8.8.8:9".parse().unwrap(),
        None,
        &code("ABCDEFGH"),
        CONTEXT,
        filter(),
    )
    .await;
    assert_eq!(result.err(), Some(JoinError::NotPrivate));
}

#[tokio::test]
async fn the_listener_answers_on_ipv6_too() {
    // Whether this machine has IPv6 on the loopback at all, asked of the
    // system rather than of the listener: if it has, the listener must answer
    // there.
    if std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_err() {
        return;
    }
    let right = code("ABCDEFGH");
    let host = host(Some(&right)).await;
    let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, host.listener.port()));
    client::join(v6, None, &right, CONTEXT, filter())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_dropped_listener_lets_nobody_in() {
    let right = code("ABCDEFGH");
    let host = host(Some(&right)).await;
    let addr = host.addr;
    drop(host);
    let mut refused = false;
    for _ in 0..20 {
        if client::join(addr, None, &right, CONTEXT, filter())
            .await
            .is_err()
        {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(refused);
}
