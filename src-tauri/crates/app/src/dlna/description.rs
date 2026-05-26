//! Generates the device + service XML descriptors served at
//! `/description.xml` and `/service/<name>.xml`.
//!
//! The descriptors are mostly static text but carry runtime values
//! (LAN IP, port, server friendly name, stable UUID) so they're
//! built per request rather than cached.

use uuid::Uuid;

/// Stable namespace UUID we hash the device's friendly name against
/// so controllers see the same `uuid:` URN across launches even when
/// the LAN IP changes. v5 (SHA-1) keeps it deterministic without any
/// persistence.
const NAMESPACE: Uuid = Uuid::from_u128(0x6ba7_b811_9dad_11d1_80b4_00c0_4fd4_30c8);

/// Compute the stable device UUID for a given friendly name.
pub fn device_uuid(server_name: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, server_name.as_bytes())
}

/// Render the `MediaServer:1` device descriptor.
///
/// Conforms to UPnP Device Architecture 1.0; `serviceList` includes
/// only ContentDirectory + ConnectionManager (we don't expose the
/// AVTransport service since WaveFlow is a server, not a renderer).
pub fn device_descriptor(server_name: &str, base_url: &str) -> String {
    let uuid = device_uuid(server_name);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion>
    <major>1</major>
    <minor>0</minor>
  </specVersion>
  <URLBase>{base_url}</URLBase>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
    <friendlyName>{name}</friendlyName>
    <manufacturer>WaveFlow</manufacturer>
    <manufacturerURL>https://wave.flow</manufacturerURL>
    <modelDescription>WaveFlow local music library</modelDescription>
    <modelName>WaveFlow</modelName>
    <modelNumber>1</modelNumber>
    <UDN>uuid:{uuid}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>
        <SCPDURL>/service/ContentDirectory.xml</SCPDURL>
        <controlURL>/control/ContentDirectory</controlURL>
        <eventSubURL>/event/ContentDirectory</eventSubURL>
      </service>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ConnectionManager:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ConnectionManager</serviceId>
        <SCPDURL>/service/ConnectionManager.xml</SCPDURL>
        <controlURL>/control/ConnectionManager</controlURL>
        <eventSubURL>/event/ConnectionManager</eventSubURL>
      </service>
    </serviceList>
  </device>
</root>
"#,
        name = xml_escape(server_name),
    )
}

/// ContentDirectory service descriptor. Lists only the actions and
/// state variables WaveFlow actually implements — Browse, Search,
/// GetSearchCapabilities, GetSortCapabilities, GetSystemUpdateID.
/// Strict subset of the full SCPD spec; controllers gracefully
/// degrade for missing actions.
pub const CONTENT_DIRECTORY_SCPD: &str = include_str!("scpd_content_directory.xml");

/// ConnectionManager service descriptor. Only the mandatory
/// `GetProtocolInfo` action; SourceProtocolInfo lists every audio
/// MIME type WaveFlow can serve so controllers know what they can
/// expect to play back.
pub const CONNECTION_MANAGER_SCPD: &str = include_str!("scpd_connection_manager.xml");

/// Minimal XML escaper for the four characters allowed inside element
/// content. `quick-xml` would do this for us if we built the document
/// with its writer API, but `format!` keeps the device descriptor
/// readable.
pub fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
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
}
