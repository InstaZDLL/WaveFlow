use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::lan::address::{kind, Kind, PeerFilter};
use crate::lan::backoff::{Backoff, FIRST_WAIT_MS, MAX_WAIT_MS};
use crate::lan::cert::{Fingerprint, Identity};
use crate::lan::code::{Code, ALPHABET, LEN};
use crate::lan::exchange::{ExchangeError, GuestExchange, HostExchange};
use crate::lan::handshake::{message_type, GuestHandshake, HandshakeRefusal, HostHandshake};
use crate::lan::token::{self, TokenHash};

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// One assertion per row of RFC-010's address table, and its edges.
#[test]
fn the_filter_follows_the_address_table() {
    let rows = [
        ("10.0.0.1", Kind::Private),
        ("10.255.255.255", Kind::Private),
        ("172.16.0.1", Kind::Private),
        ("172.31.255.255", Kind::Private),
        ("172.32.0.1", Kind::Public),
        ("192.168.1.20", Kind::Private),
        ("169.254.10.10", Kind::LinkLocal),
        ("fe80::1", Kind::LinkLocal),
        ("127.0.0.1", Kind::Loopback),
        ("127.200.0.9", Kind::Loopback),
        ("::1", Kind::Loopback),
        ("fd12:3456::1", Kind::UniqueLocal),
        ("fc00::1", Kind::UniqueLocal),
        ("fd7a:115c:a1e0::1234", Kind::UniqueLocal),
        ("100.64.0.0", Kind::Shared),
        ("100.100.100.100", Kind::Shared),
        ("100.127.255.255", Kind::Shared),
        ("100.63.255.255", Kind::Public),
        ("100.128.0.0", Kind::Public),
        ("::ffff:192.168.1.20", Kind::Private),
        ("::ffff:100.70.1.1", Kind::Shared),
        ("::ffff:8.8.8.8", Kind::Public),
        ("8.8.8.8", Kind::Public),
        ("0.0.0.0", Kind::Public),
        ("255.255.255.255", Kind::Public),
        ("224.0.0.251", Kind::Public),
        ("::", Kind::Public),
        ("ff02::fb", Kind::Public),
        ("fec0::1", Kind::Public),
        ("2001:db8::1", Kind::Public),
        ("fe00::1", Kind::Public),
    ];
    for (text, expected) in rows {
        assert_eq!(kind(ip(text)), expected, "{text}");
    }

    let plain = PeerFilter::new(false, [ip("192.168.1.2")]);
    for admitted in [
        "10.0.0.1",
        "169.254.1.1",
        "127.0.0.1",
        "::1",
        "fe80::1",
        "fd7a:115c:a1e0::1",
    ] {
        assert!(plain.admits(ip(admitted)), "{admitted}");
    }
    for refused in [
        "8.8.8.8",
        "2001:db8::1",
        "100.80.0.1",
        "::ffff:100.80.0.1",
        "0.0.0.0",
    ] {
        assert!(!plain.admits(ip(refused)), "{refused}");
    }

    // The shared block needs the switch AND an address of the machine's own
    // in it: either alone is a carrier's network, not a VPN.
    let tailnet = [ip("192.168.1.2"), ip("100.101.102.103")];
    assert!(PeerFilter::new(true, tailnet).admits(ip("100.80.0.1")));
    assert!(PeerFilter::new(true, tailnet).admits(ip("::ffff:100.80.0.1")));
    assert!(!PeerFilter::new(false, tailnet).admits(ip("100.80.0.1")));
    assert!(!PeerFilter::new(true, [ip("192.168.1.2")]).admits(ip("100.80.0.1")));
    assert!(!PeerFilter::new(true, tailnet).admits(ip("8.8.8.8")));
}

#[test]
fn wrong_codes_slow_their_sender_and_nobody_else() {
    let mut backoff = Backoff::default();
    let a = ip("192.168.1.10");
    let b = ip("192.168.1.11");
    assert_eq!(backoff.check(a, 0), Ok(()));
    assert_eq!(backoff.failed(a, 0), 1);
    assert_eq!(backoff.check(a, 0), Err(FIRST_WAIT_MS));
    assert_eq!(backoff.check(a, FIRST_WAIT_MS), Ok(()));
    assert_eq!(backoff.check(b, 0), Ok(()), "another machine is not slowed");

    // Each failure doubles the wait, up to the cap.
    let mut now = 10_000;
    let mut last = 0;
    for n in 2..30 {
        assert_eq!(backoff.failed(a, now), n);
        let wait = backoff.check(a, now).unwrap_err();
        assert!(wait >= last && wait <= MAX_WAIT_MS, "{wait}");
        last = wait;
        now += wait;
    }
    assert_eq!(last, MAX_WAIT_MS);

    backoff.succeeded(a);
    assert_eq!(backoff.check(a, now), Ok(()));
    assert_eq!(backoff.total(), 29, "the host's count keeps the history");
}

#[test]
fn one_exchange_at_a_time_per_sender() {
    let mut backoff = Backoff::default();
    let a = ip("192.168.1.10");
    assert_eq!(backoff.begin(a, 0), Ok(()));
    assert_eq!(backoff.begin(a, 0), Err(FIRST_WAIT_MS), "one under way");
    assert_eq!(backoff.begin(ip("192.168.1.11"), 0), Ok(()), "others go on");
    backoff.failed(a, 0);
    assert_eq!(backoff.begin(a, 0), Err(FIRST_WAIT_MS), "now it waits");
    assert_eq!(backoff.begin(a, FIRST_WAIT_MS), Ok(()));
    backoff.succeeded(a);
    assert_eq!(backoff.begin(a, FIRST_WAIT_MS), Ok(()));
}

#[test]
fn an_ipv6_sender_is_its_prefix() {
    let mut backoff = Backoff::default();
    backoff.failed(ip("fd00:1:2:3::a"), 0);
    assert!(backoff.check(ip("fd00:1:2:3::b"), 0).is_err(), "same /64");
    assert!(backoff.check(ip("fd00:1:2:4::a"), 0).is_ok(), "another /64");
    backoff.failed(ip("::ffff:10.0.0.5"), 0);
    assert!(
        backoff.check(ip("10.0.0.5"), 0).is_err(),
        "mapped is the IPv4"
    );
}

#[test]
fn the_backoff_forgets_the_oldest_past_its_capacity() {
    let mut backoff = Backoff::default();
    let first = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0));
    backoff.failed(first, 0);
    for n in 1..=1_024u32 {
        let [_, a, b, c] = n.to_be_bytes();
        backoff.failed(IpAddr::V4(Ipv4Addr::new(10, a, b, c)), 100);
    }
    assert_eq!(backoff.check(first, 0), Ok(()), "evicted first");
    assert!(backoff
        .check(IpAddr::V4(Ipv4Addr::new(10, 0, 4, 0)), 100)
        .is_err());
}

#[test]
fn a_code_is_drawn_without_bias_and_typed_loosely() {
    let code = Code::from_random([0, 1, 31, 32, 255, 64, 100, 7]);
    assert_eq!(code.as_str(), "AB9A9AEH");
    assert_eq!(code.grouped(), "AB9A-9AEH");
    assert_eq!(format!("{code:?}"), "Code(***)");
    // 256 is a multiple of the alphabet's size: every character as likely.
    assert_eq!(256 % ALPHABET.len(), 0);
    for c in *b"IO01" {
        assert!(!ALPHABET.contains(&c), "{}", c as char);
    }

    for typed in ["ab9a9aeh", "AB9A-9AEH", " ab9a 9aeh ", "a-b-9-a-9-a-e-h"] {
        assert_eq!(Code::parse(typed), Some(code.clone()), "{typed:?}");
    }
    for typed in [
        "",
        "AB9A9AE",
        "AB9A9AEHX",
        "AB9A9AEI",
        "AB9A9AEO",
        "AB9A9AE0",
        "AB9A9AEé",
        "AB9A_9AE",
    ] {
        assert_eq!(Code::parse(typed), None, "{typed:?}");
    }
    assert_eq!(code.as_str().len(), LEN);
    assert_ne!(Code::generate(), Code::generate());
}

#[test]
fn a_token_is_kept_as_its_hash() {
    let token = token::from_random("wst_", [0xab; 32]);
    assert_eq!(token, format!("wst_{}", "ab".repeat(32)));
    let hash = TokenHash::of(&token);
    assert!(hash.matches(&token));
    assert!(!hash.matches(&token.replace('a', "b")));
    assert!(!hash.matches(""));
    assert_eq!(format!("{hash:?}"), "TokenHash(***)");
    assert_ne!(token::generate("wst_"), token::generate("wst_"));
}

fn exchange(
    guest_code: &str,
    host_code: &str,
    guest_context: &str,
    host_context: &str,
    guest_exporter: &[u8],
    host_exporter: &[u8],
) -> Result<(), ExchangeError> {
    let (guest, a) = GuestExchange::start(&Code::parse(guest_code).unwrap(), guest_context);
    let (host, b, host_confirm) = HostExchange::respond(
        &Code::parse(host_code).unwrap(),
        host_context,
        &a,
        host_exporter,
    )?;
    let guest_confirm = guest.finish(&b, &host_confirm, guest_exporter)?;
    host.verify(&guest_confirm)
}

#[test]
fn the_exchange_holds_only_for_one_code_one_context_one_session() {
    let x = [7u8; 32];
    assert_eq!(
        exchange("ABCDEFGH", "abcd-efgh", "wavesync", "wavesync", &x, &x),
        Ok(())
    );
    assert_eq!(
        exchange("ABCDEFGH", "ABCDEFGJ", "wavesync", "wavesync", &x, &x),
        Err(ExchangeError::Mismatch),
        "wrong code"
    );
    assert_eq!(
        exchange("ABCDEFGH", "ABCDEFGH", "wavesync", "phone-link", &x, &x),
        Err(ExchangeError::Mismatch),
        "a code for another feature"
    );
    assert_eq!(
        exchange("ABCDEFGH", "ABCDEFGH", "wavesync", "wavesync", &x, &[8u8; 32]),
        Err(ExchangeError::Mismatch),
        "two TLS sessions: somebody in between"
    );
}

#[test]
fn a_host_confirmation_is_checked_before_the_guest_answers() {
    let code = Code::parse("ABCDEFGH").unwrap();
    let x = [1u8; 32];
    let (guest, a) = GuestExchange::start(&code, "wavesync");
    let (host, b, mut confirm) = HostExchange::respond(&code, "wavesync", &a, &x).unwrap();
    confirm[0] ^= 1;
    assert_eq!(guest.finish(&b, &confirm, &x), Err(ExchangeError::Mismatch));

    let (guest, _) = GuestExchange::start(&code, "wavesync");
    assert_eq!(
        guest.finish(&[1, 2, 3], &confirm, &x),
        Err(ExchangeError::Malformed)
    );
    assert!(HostExchange::respond(&code, "wavesync", b"short", &x).is_err());
    assert_eq!(host.verify(&[0u8; 32]), Err(ExchangeError::Mismatch));
}

#[test]
fn a_certificate_has_one_fingerprint_and_hides_its_key() {
    let identity = Identity::generate().unwrap();
    let again = Identity::from_der(identity.cert_der().to_vec(), identity.key_der().to_vec());
    assert_eq!(again.fingerprint(), identity.fingerprint());
    assert_ne!(
        Identity::generate().unwrap().fingerprint(),
        identity.fingerprint()
    );

    let fingerprint = identity.fingerprint();
    let written = fingerprint.to_string();
    assert_eq!(written.len(), 64);
    assert_eq!(written.parse::<Fingerprint>(), Ok(fingerprint));
    assert_eq!(
        written.to_uppercase().parse::<Fingerprint>(),
        Ok(fingerprint)
    );
    assert!("abc".parse::<Fingerprint>().is_err());

    let short = fingerprint.short();
    assert_eq!(short.len(), 19);
    assert_eq!(short.replace(' ', ""), written[..16].to_uppercase());

    let debug = format!("{identity:?}");
    assert!(!debug.contains(&hex::encode(identity.key_der())));
}

#[test]
fn the_handshake_json_is_pinned() {
    let start = GuestHandshake::ExchangeStart {
        version: 1,
        context: "wavesync".into(),
        guest: "ab".into(),
    };
    let text = serde_json::to_string(&start).unwrap();
    assert_eq!(
        text,
        r#"{"type":"exchange_start","version":1,"context":"wavesync","guest":"ab"}"#
    );
    assert_eq!(message_type(&text).as_deref(), Some("exchange_start"));
    assert_eq!(
        message_type(r#"{"type":"resume","token":"x"}"#).as_deref(),
        Some("resume")
    );
    assert_eq!(message_type("not json"), None);
    assert_eq!(
        serde_json::to_string(&HostHandshake::ExchangeRefused {
            reason: HandshakeRefusal::RetryLater,
            retry_after_ms: Some(2_000),
        })
        .unwrap(),
        r#"{"type":"exchange_refused","reason":"retry_later","retry_after_ms":2000}"#
    );
    assert_eq!(
        serde_json::to_string(&HostHandshake::ExchangeRefused {
            reason: HandshakeRefusal::Closed,
            retry_after_ms: None,
        })
        .unwrap(),
        r#"{"type":"exchange_refused","reason":"closed"}"#
    );
    let _ = Ipv6Addr::LOCALHOST;
}
