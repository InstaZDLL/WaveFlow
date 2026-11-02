use super::*;

const LISTING: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/Music/Rock/</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/Music/Rock/AC%23DC%20%26%20co.flac</D:href>
    <D:propstat>
      <D:prop>
        <D:resourcetype/>
        <D:getcontentlength>1234</D:getcontentlength>
        <D:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</D:getlastmodified>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>https://nas.local/dav/Music/Rock/Live/</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>
    <D:propstat><D:prop><D:getcontentlength/></D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat>
  </D:response>
</D:multistatus>"#;

fn wanted() -> Vec<String> {
    ["dav", "Music", "Rock"].map(String::from).to_vec()
}

#[test]
fn a_listing_gives_decoded_names_without_the_directory_itself() {
    let raw = parse_multistatus(LISTING.as_bytes()).unwrap();
    assert_eq!(raw.len(), 3);
    let entries = children(raw, &wanted());
    assert_eq!(
        entries,
        vec![
            Entry {
                name: "AC#DC & co.flac".into(),
                is_dir: false,
                size: 1234,
                modified_ms: 784_111_777_000,
            },
            Entry {
                name: "Live".into(),
                is_dir: true,
                size: 0,
                modified_ms: 0,
            },
        ]
    );
}

#[test]
fn a_property_marked_as_missing_does_not_count() {
    let xml = r#"<multistatus xmlns="DAV:"><response><href>/a.flac</href>
        <propstat><prop><getcontentlength>10</getcontentlength></prop><status>HTTP/1.1 200 OK</status></propstat>
        <propstat><prop><getcontentlength>99</getcontentlength></prop><status>HTTP/1.1 404 Not Found</status></propstat>
        </response></multistatus>"#;
    let raw = parse_multistatus(xml.as_bytes()).unwrap();
    assert_eq!(raw[0].size, 10);
}

#[test]
fn any_namespace_prefix_reads_the_same() {
    let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/x/a&amp;b.mp3</d:href>
        <d:propstat><d:prop><d:getcontentlength>7</d:getcontentlength></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>
        </d:response></d:multistatus>"#;
    let raw = parse_multistatus(xml.as_bytes()).unwrap();
    assert_eq!(raw[0].href, "/x/a&b.mp3");
    assert_eq!(raw[0].size, 7);
}

#[test]
fn a_name_that_climbs_or_holds_a_separator_is_dropped() {
    let raw = vec![
        RawEntry {
            href: "/dav/Music/Rock/%2e%2e".into(),
            ..Default::default()
        },
        RawEntry {
            href: "/dav/Music/Rock/a%2Fb.flac".into(),
            ..Default::default()
        },
        RawEntry {
            href: "/dav/Music/Rock/a%5Cb.flac".into(),
            ..Default::default()
        },
        RawEntry {
            href: "/dav/Music/Rock/good.flac".into(),
            ..Default::default()
        },
    ];
    let names: Vec<String> = children(raw, &wanted())
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["good.flac"]);
}

#[test]
fn behind_a_proxy_rewriting_the_prefix_the_directory_is_still_left_out() {
    let raw = vec![
        RawEntry {
            href: "/other/Rock/".into(),
            is_dir: true,
            ..Default::default()
        },
        RawEntry {
            href: "/other/Rock/a.flac".into(),
            size: 3,
            ..Default::default()
        },
    ];
    let names: Vec<String> = children(raw, &wanted())
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["a.flac"]);
}

#[test]
fn an_endpoint_is_refused_for_what_it_would_leak() {
    assert_eq!(
        parse_endpoint("http://nas.local/dav", false).unwrap_err(),
        NetError::Protocol("plain-http-not-accepted".into())
    );
    assert!(parse_endpoint("http://nas.local/dav", true).is_ok());
    assert!(parse_endpoint("https://me:secret@nas.local/dav", false).is_err());
    assert!(parse_endpoint("ftp://nas.local/dav", true).is_err());
    assert!(parse_endpoint("nas.local/dav", true).is_err());
    assert_eq!(
        parse_endpoint("https://nas.local/dav/", false)
            .unwrap()
            .path(),
        "/dav"
    );
}

#[test]
fn a_request_url_encodes_each_segment_of_a_raw_path() {
    let fs = WebDavFs::new("https://nas.local/dav/", None, None, false).unwrap();
    assert_eq!(
        fs.url_for("Rock/AC#DC ? 100%.flac", false)
            .unwrap()
            .as_str(),
        "https://nas.local/dav/Rock/AC%23DC%20%3F%20100%25.flac"
    );
    assert_eq!(
        fs.url_for("", true).unwrap().as_str(),
        "https://nas.local/dav/"
    );
    assert_eq!(
        fs.url_for("Rock", true).unwrap().as_str(),
        "https://nas.local/dav/Rock/"
    );
}

#[test]
fn a_digest_only_server_is_named_rather_than_blamed_on_the_password() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::WWW_AUTHENTICATE,
        "Digest realm=\"x\"".parse().unwrap(),
    );
    let err = classify_status(StatusCode::UNAUTHORIZED, &headers);
    assert!(
        matches!(err, NetError::Auth(ref m) if m.contains("Digest")),
        "{err}"
    );
    assert_eq!(
        classify_status(StatusCode::NOT_FOUND, &headers),
        NetError::NotFound
    );
    assert_eq!(
        classify_status(StatusCode::OK, &headers).code(),
        "protocol",
        "a web page at that address is not a share"
    );
}

/// A sweep reads what is missing from a listing as deleted, so a listing that
/// did not finish must be an error, not a shorter list.
#[test]
fn a_listing_cut_short_is_an_error() {
    let whole = LISTING.as_bytes();
    let cut = whole.len() * 2 / 3;
    assert!(parse_multistatus(&whole[..cut]).is_err());
    assert!(parse_multistatus(b"").is_err());
}

/// An entry the server could not describe could be a directory.
#[test]
fn an_entry_with_nothing_found_makes_the_listing_incomplete() {
    let xml = r#"<multistatus xmlns="DAV:"><response><href>/x/sub/</href>
        <propstat><prop><resourcetype/></prop><status>HTTP/1.1 507 Insufficient Storage</status></propstat>
        </response></multistatus>"#;
    assert!(parse_multistatus(xml.as_bytes()).is_err());
    let xml = r#"<multistatus xmlns="DAV:"><response><href>/x/sub/</href>
        <status>HTTP/1.1 403 Forbidden</status></response></multistatus>"#;
    assert!(parse_multistatus(xml.as_bytes()).is_err());
}
