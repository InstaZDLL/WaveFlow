use super::guest::{invitation, parse_target};
use waveflow_core::lan::cert::Identity;
use waveflow_core::lan::code::Code;

#[test]
fn a_target_is_an_address_with_its_port_or_an_invitation() {
    let plain = parse_target(" 192.168.1.20:43399 ").unwrap();
    assert_eq!(plain.addr, "192.168.1.20:43399".parse().unwrap());
    assert_eq!((plain.fingerprint, plain.code), (None, None));
    assert_eq!(
        parse_target("[fd00::2]:9").unwrap().addr,
        "[fd00::2]:9".parse().unwrap()
    );

    let identity = Identity::generate().unwrap();
    let code = Code::parse("ABCD-EFGH").unwrap();
    let link = invitation(
        "192.168.1.20:43399".parse().unwrap(),
        &identity.fingerprint(),
        &code,
    );
    let read = parse_target(&link).unwrap();
    assert_eq!(read.addr, "192.168.1.20:43399".parse().unwrap());
    assert_eq!(read.fingerprint, Some(identity.fingerprint()));
    assert_eq!(read.code.as_deref(), Some("ABCDEFGH"));

    for bad in [
        "",
        "192.168.1.20",
        "host.local:9",
        "wavesync://host.local:9/",
        "wavesync://192.168.1.20/",
        "wavesync://192.168.1.20:9/?fp=xyz",
        "http://192.168.1.20:9/",
    ] {
        assert!(parse_target(bad).is_err(), "{bad:?}");
    }
}
