use super::*;

#[test]
fn device_uuid_is_stable_per_name() {
    let a = device_uuid("WaveFlow");
    let b = device_uuid("WaveFlow");
    assert_eq!(a, b);
}

#[test]
fn device_uuid_changes_with_name() {
    let a = device_uuid("WaveFlow");
    let b = device_uuid("WaveFlow-Lab");
    assert_ne!(a, b);
}

#[test]
fn descriptor_embeds_uuid_and_name() {
    let xml = device_descriptor("My Server", "http://10.0.0.5:1234");
    assert!(xml.contains("<friendlyName>My Server</friendlyName>"));
    assert!(xml.contains("<URLBase>http://10.0.0.5:1234</URLBase>"));
    assert!(xml.contains("uuid:"));
}

#[test]
fn descriptor_escapes_xml_metachars() {
    let xml = device_descriptor("A & B <test>", "http://x");
    assert!(xml.contains("A &amp; B &lt;test&gt;"));
}
