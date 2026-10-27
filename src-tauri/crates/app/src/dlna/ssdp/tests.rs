use super::*;

#[test]
fn parse_header_is_case_insensitive() {
    let msg = "M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\nHOST: x\r\n\r\n";
    assert_eq!(parse_header(msg, "st"), Some("ssdp:all".into()));
    assert_eq!(parse_header(msg, "ST"), Some("ssdp:all".into()));
    assert!(parse_header(msg, "missing").is_none());
}

#[test]
fn matching_targets_ssdp_all_expands_to_full_list() {
    let out = matching_targets("ssdp:all", "abc");
    assert_eq!(out.len(), NOTIFICATION_TARGETS.len());
    assert!(out.iter().any(|t| t == "uuid:abc"));
}

#[test]
fn matching_targets_specific_st_returns_single_match() {
    let out = matching_targets("urn:schemas-upnp-org:service:ContentDirectory:1", "abc");
    assert_eq!(
        out,
        vec!["urn:schemas-upnp-org:service:ContentDirectory:1".to_string()]
    );
}

#[test]
fn matching_targets_unknown_st_returns_empty() {
    assert!(matching_targets("urn:foo:bar", "abc").is_empty());
}

#[test]
fn location_names_the_interface_address() {
    assert_eq!(
        location_url(Ipv4Addr::new(192, 168, 1, 20), 8200),
        "http://192.168.1.20:8200/description.xml"
    );
}

#[test]
fn spawn_refuses_an_empty_interface_list() {
    let err = spawn("WaveFlow".into(), Vec::new(), 8200).err();
    assert_eq!(err.map(|e| e.kind()), Some(std::io::ErrorKind::NotFound));
}

#[test]
fn build_usn_handles_uuid_only_target() {
    assert_eq!(build_usn("uuid:abc", "abc"), "uuid:abc");
    assert_eq!(
        build_usn("upnp:rootdevice", "abc"),
        "uuid:abc::upnp:rootdevice"
    );
}
