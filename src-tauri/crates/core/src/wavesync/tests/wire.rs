use super::*;
use crate::wavesync::wire::{
    check_protocol, decode_guest, decode_host, encode, GuestMessage, HostMessage, Outcome,
    WireError, MAX_TEXT_FRAME,
};

#[test]
fn every_message_reads_back_as_itself() {
    let (mut host, me, guest) = session();
    add(&mut host, guest, &[1]);
    let ops = host.handle(me, Action::Play { entry: None }, 7).unwrap();

    let guests = [
        GuestMessage::Join {
            protocol: PROTOCOL_VERSION,
            name: "Sam".into(),
        },
        GuestMessage::Resume {
            protocol: PROTOCOL_VERSION,
            participant: guest,
            token: Secret::new("wst_abc"),
            after: 3,
        },
        GuestMessage::Request {
            req: 9,
            action: Action::Add {
                entries: vec![entry(2)],
                position: Position::After(id(1)),
            },
        },
        GuestMessage::Request {
            req: 10,
            action: Action::Seek { position_ms: 1 },
        },
        GuestMessage::Ping { sent_ms: 12 },
    ];
    for message in guests {
        assert_eq!(decode_guest(&encode(&message)), Ok(message));
    }

    let hosts = [
        HostMessage::Welcome {
            protocol: PROTOCOL_VERSION,
            you: guest,
            token: Some(Secret::new("wst_abc")),
            snapshot: host.snapshot(),
        },
        HostMessage::Refused {
            reason: Refusal::SessionFull,
        },
        HostMessage::Ops { ops },
        HostMessage::Snapshot {
            snapshot: host.snapshot(),
        },
        HostMessage::Answer {
            req: 9,
            outcome: Outcome::Applied { seq: 4 },
        },
        HostMessage::Answer {
            req: 10,
            outcome: Outcome::Refused {
                reason: Refusal::NotAllowed,
            },
        },
        HostMessage::Pong {
            sent_ms: 12,
            host_ms: 40,
        },
        HostMessage::Unsupported {
            kind: "teleport".into(),
        },
    ];
    for message in hosts {
        assert_eq!(decode_host(&encode(&message)), Ok(message));
    }
}

/// The shape a second implementation (the Android app, one day) has to
/// match, pinned in full for one op and one request.
#[test]
fn the_json_shape_is_pinned() {
    let op = Sequenced {
        seq: 5,
        op: Op::Transport {
            cursor: 2,
            anchor: Anchor {
                entry: Some(EntryId::from_bytes([0x0f; 16])),
                position_ms: 1_500,
                at_ms: 9_000,
                playing: true,
            },
        },
    };
    assert_eq!(
        encode(&op),
        r#"{"seq":5,"op":"transport","cursor":2,"anchor":{"entry":"wse_0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f","position_ms":1500,"at_ms":9000,"playing":true}}"#
    );
    let request = GuestMessage::Request {
        req: 1,
        action: Action::Move {
            entry: EntryId::from_bytes([1; 16]),
            position: Position::After(EntryId::from_bytes([2; 16])),
        },
    };
    assert_eq!(
        encode(&request),
        r#"{"type":"request","req":1,"action":{"action":"move","entry":"wse_01010101010101010101010101010101","position":{"at":"after","entry":"wse_02020202020202020202020202020202"}}}"#
    );
    assert_eq!(
        encode(&HostMessage::Answer {
            req: 1,
            outcome: Outcome::Refused {
                reason: Refusal::QueueFull
            }
        }),
        r#"{"type":"answer","req":1,"outcome":"refused","reason":"queue_full"}"#
    );
}

#[test]
fn an_unknown_message_is_told_apart_from_a_broken_one() {
    assert_eq!(
        decode_guest(r#"{"type":"teleport","to":"mars"}"#),
        Err(WireError::Unsupported("teleport".into()))
    );
    let long = format!(r#"{{"type":"{}"}}"#, "x".repeat(500));
    assert_eq!(
        decode_guest(&long),
        Err(WireError::Unsupported("x".repeat(32)))
    );
    for broken in [
        "",
        "null",
        "[]",
        r#"{"type":"ping"}"#,
        r#"{"type":"ping","sent_ms":"soon"}"#,
        r#"{"type":"request","req":1,"action":{"action":"fly"}}"#,
        r#"{"no_type":1}"#,
    ] {
        assert_eq!(decode_guest(broken), Err(WireError::Malformed), "{broken}");
    }
    // A message the host sends is not one a guest may send.
    assert!(matches!(
        decode_guest(r#"{"type":"pong","sent_ms":1,"host_ms":2}"#),
        Err(WireError::Unsupported(_))
    ));
    let huge = format!(
        r#"{{"type":"join","protocol":1,"name":"{}"}}"#,
        "a".repeat(MAX_TEXT_FRAME)
    );
    assert_eq!(decode_guest(&huge), Err(WireError::TooLarge));
}

#[test]
fn a_token_never_reaches_a_debug_line() {
    let message = GuestMessage::Resume {
        protocol: PROTOCOL_VERSION,
        participant: ParticipantId(2),
        token: Secret::new("wst_do_not_print"),
        after: 0,
    };
    assert!(!format!("{message:?}").contains("wst_do_not_print"));
    // It does cross the wire, inside TLS: that is its only way.
    assert!(encode(&message).contains("wst_do_not_print"));
}

#[test]
fn another_protocol_version_is_refused() {
    assert_eq!(check_protocol(PROTOCOL_VERSION), Ok(()));
    assert_eq!(
        check_protocol(PROTOCOL_VERSION + 1),
        Err(Refusal::ProtocolMismatch)
    );
    assert_eq!(check_protocol(0), Err(Refusal::ProtocolMismatch));
}
