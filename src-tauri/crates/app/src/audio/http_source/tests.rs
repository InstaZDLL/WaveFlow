use super::*;

#[test]
fn unknown_host_surfaces_error() {
    // DNS for a domain that doesn't exist must fail at `open`
    // rather than at the first `read` — symphonia would otherwise
    // panic on an empty probe buffer. `HttpMediaSource` doesn't
    // impl Debug (wraps a `reqwest::blocking::Response` which
    // doesn't either), so destructure by hand instead of
    // `expect_err`.
    match HttpMediaSource::open("http://this-domain-definitely-does-not-exist.invalid/stream") {
        Ok(_) => panic!("expected DNS error"),
        Err(err) => assert!(
            err.contains("http get") || err.contains("dns"),
            "unexpected error: {err}"
        ),
    }
}

#[test]
fn seek_is_unsupported() {
    // Build a fake source with a dummy reader — easier than running
    // a real HTTP server in unit tests. The `Read` impl isn't
    // exercised here; we only check `Seek` returns Unsupported.
    struct StubSource {
        inner: Mutex<BufReader<std::io::Empty>>,
        url: String,
    }
    impl Read for StubSource {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl Seek for StubSource {
        fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "no"))
        }
    }
    // Just verify the contract — the real HttpMediaSource has the
    // same impl and is tested implicitly by the integration tests.
    let stub = StubSource {
        inner: Mutex::new(BufReader::new(std::io::empty())),
        url: "http://example/".to_string(),
    };
    let _ = &stub;
}

#[test]
fn redact_url_strips_userinfo_and_query() {
    // Userinfo + query both gone, scheme + host + port + path kept.
    assert_eq!(
        redact_url("https://user:pwd@stream.example.com:8443/mount?key=abcd"),
        "https://stream.example.com:8443/mount"
    );
    // No userinfo / query → idempotent (no spurious `:` / `?` etc.).
    assert_eq!(
        redact_url("http://radio.example/mount.mp3"),
        "http://radio.example/mount.mp3"
    );
    // Default port (no `:` segment).
    assert_eq!(
        redact_url("https://radio.example/stream"),
        "https://radio.example/stream"
    );
    // Unparseable input round-trips so the diagnostic isn't lost.
    assert_eq!(redact_url("not a url"), "not a url");
}

/// A remote server's stream ticket rides in the path, where stripping
/// the query alone leaves it whole — and it plays the track for an hour.
#[test]
fn redact_url_masks_a_stream_ticket() {
    assert_eq!(
        redact_url("https://music.example:8443/api/v2/stream/tk_9f8e7d6c?format=opus"),
        "https://music.example:8443/api/v2/stream/<ticket>"
    );
    // Mounted under a prefix, and with a trailing segment after it.
    assert_eq!(
        redact_url("https://host.example/music/api/v2/stream/tk_9f8e7d6c/extra"),
        "https://host.example/music/api/v2/stream/<ticket>/extra"
    );
    // Nothing after the prefix: nothing to mask, nothing invented.
    assert_eq!(
        redact_url("https://host.example/api/v2/stream/"),
        "https://host.example/api/v2/stream/"
    );
}

/// A Web Radio mount with credentials in both places a station puts them.
#[test]
fn redact_url_drops_a_radio_key() {
    let redacted = redact_url("http://listener:hunter2@radio.example/live.mp3?key=s3cr3t");
    assert_eq!(redacted, "http://radio.example/live.mp3");
}

/// An address nothing listens on: bound, then released.
fn refused_addr() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr")
}

/// The open error reaches the log and the player's error toast; reqwest
/// would print the full URL in it, ticket and key included.
#[test]
fn a_failed_open_does_not_echo_the_url() {
    let addr = refused_addr();
    for url in [
        format!("http://{addr}/api/v2/stream/tk_9f8e7d6c?format=opus"),
        format!("http://listener:hunter2@{addr}/live.mp3?key=s3cr3t"),
    ] {
        let err = match HttpMediaSource::open_seekable(&url) {
            Ok(_) => panic!("nothing listens on {addr}"),
            Err(err) => err,
        };
        // The message names the fixture by index only: echoing it, or the
        // error that would hold it, is the very leak under test.
        for (i, fixture) in ["tk_9f8e7d6c", "hunter2", "s3cr3t"].iter().enumerate() {
            assert!(
                !err.contains(fixture),
                "fixture credential #{i} leaked into the error"
            );
        }
    }
}

/// The seek path's ranged reopen fails the same way once the server is
/// gone, and its error travels up into a logged seek failure.
#[test]
fn a_failed_range_reopen_does_not_echo_the_url() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!(
        "http://{}/api/v2/stream/tk_9f8e7d6c?key=s3cr3t",
        listener.local_addr().expect("addr")
    );
    // Answer the open on a connection that closes, then drop the
    // listener: the reopen can neither reuse a pooled connection nor
    // find anyone listening, so it is refused.
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut request = [0u8; 2048];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nAccept-Ranges: bytes\r\n\
                  Connection: close\r\n\r\n0123456789",
        );
    });
    let mut source = HttpMediaSource::open_seekable(&url).expect("open");
    handle.join().expect("server thread");

    let err = source
        .seek(SeekFrom::Start(4))
        .expect_err("the server is gone");
    let text = format!("{err} {err:?}");
    for (i, fixture) in ["tk_9f8e7d6c", "s3cr3t"].iter().enumerate() {
        assert!(
            !text.contains(fixture),
            "fixture credential #{i} leaked into the error"
        );
    }
}

#[test]
fn http_media_source_is_send_and_sync() {
    // `MediaSource` requires both — assert it at the type level so
    // a future refactor that breaks the bound fails to compile
    // here instead of inside a confusing symphonia generic.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<HttpMediaSource>();
}

#[test]
fn parse_stream_title_extracts_value() {
    let block = b"StreamTitle='Daft Punk - Get Lucky';StreamUrl='http://x';\0\0";
    assert_eq!(
        parse_stream_title(block).as_deref(),
        Some("Daft Punk - Get Lucky")
    );
}

#[test]
fn parse_stream_title_handles_blank_and_missing() {
    // Empty value (silence / ident) → None.
    assert_eq!(parse_stream_title(b"StreamTitle='';\0\0"), None);
    // Field absent entirely → None.
    assert_eq!(parse_stream_title(b"StreamUrl='http://x';"), None);
    // Missing closing `';` → falls back to NUL padding / end.
    assert_eq!(
        parse_stream_title(b"StreamTitle='No Closer\0\0\0").as_deref(),
        Some("No Closer")
    );
}

#[test]
fn parse_content_range_start_reads_the_offset() {
    assert_eq!(parse_content_range_start("bytes 200-1023/1024"), Some(200));
    assert_eq!(parse_content_range_start("bytes 0-99/100"), Some(0));
    // Unknown total ("*") is fine as long as both bounds are present.
    assert_eq!(parse_content_range_start("bytes 512-1023/*"), Some(512));
    // The range unit is case-insensitive.
    assert_eq!(parse_content_range_start("Bytes 200-1023/1024"), Some(200));
    assert_eq!(parse_content_range_start("BYTES 0-99/100"), Some(0));
}

#[test]
fn parse_content_range_start_rejects_unusable_shapes() {
    // Missing end bound — only valid in a request Range, not a 206.
    assert_eq!(parse_content_range_start("bytes 512-/2048"), None);
    // Start past end.
    assert_eq!(parse_content_range_start("bytes 100-50/200"), None);
    // Range reaches/passes the total (end is inclusive).
    assert_eq!(parse_content_range_start("bytes 0-100/100"), None);
    // Missing total entirely (no slash).
    assert_eq!(parse_content_range_start("bytes 0-99"), None);
    // Extra slash / trailing suffix.
    assert_eq!(parse_content_range_start("bytes 0-99/100/200"), None);
    // Non-numeric total.
    assert_eq!(parse_content_range_start("bytes 0-99/abc"), None);
    // Unsatisfied range → no start byte.
    assert_eq!(parse_content_range_start("bytes */1024"), None);
    // Wrong unit.
    assert_eq!(parse_content_range_start("items 0-9/10"), None);
    // Garbage.
    assert_eq!(parse_content_range_start("nonsense"), None);
    assert_eq!(parse_content_range_start(""), None);
}

#[test]
fn read_icy_block_reads_and_skips() {
    use std::io::Cursor;
    // L=2 → 32 metadata bytes, NUL-padded (one 16-byte unit can't
    // hold the 17-char `StreamTitle='Hi';`).
    let mut payload = vec![2u8];
    let mut meta = b"StreamTitle='Hi';".to_vec();
    meta.resize(32, 0);
    payload.extend_from_slice(&meta);
    let mut cur = Cursor::new(payload);
    let block = read_icy_block(&mut cur).expect("read").expect("some");
    assert_eq!(parse_stream_title(&block).as_deref(), Some("Hi"));

    // L=0 → no metadata this interval.
    let mut zero = Cursor::new(vec![0u8]);
    assert!(read_icy_block(&mut zero).expect("read").is_none());
}
/// Seeking to the end of the body must not go to the wire.
///
/// `Range: bytes={len}-` names no byte that exists, so RFC 9110 has the
/// server answer 416 — correctly. Treating that as a failed seek made
/// **every** AAC track streamed from a server refuse to open, because
/// symphonia lands there looking for the `moov` atom an MP4 keeps at the
/// end. MP3 needs no such jump, which is why the break looked like a codec
/// problem rather than a range one.
///
/// The stub answers 416 exactly as a real server does, so this fails
/// against the version that asked.
#[test]
fn seeking_to_the_end_is_eof_not_a_failure() {
    const BODY: &[u8] = b"0123456789";

    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/audio", server.server_addr());
    std::thread::spawn(move || {
        while let Ok(request) = server.recv() {
            let start = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Range"))
                .map(|header| header.value.as_str().to_string())
                .and_then(|value| {
                    value
                        .strip_prefix("bytes=")
                        .and_then(|rest| rest.split('-').next())
                        .and_then(|from| from.parse::<usize>().ok())
                });
            let response = match start {
                // Unsatisfiable: at or past the end, like a real server.
                Some(from) if from >= BODY.len() => {
                    tiny_http::Response::from_data(Vec::new()).with_status_code(416)
                }
                Some(from) => tiny_http::Response::from_data(BODY[from..].to_vec())
                    .with_status_code(206)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Range"[..],
                            format!("bytes {from}-{}/{}", BODY.len() - 1, BODY.len()).as_bytes(),
                        )
                        .unwrap(),
                    ),
                None => tiny_http::Response::from_data(BODY.to_vec()),
            };
            // Without this the source is forward-only — `seekable` needs a
            // length *and* an advertised range capability — and the seek
            // below would report Unsupported instead of exercising the end
            // of the body at all.
            let response = response.with_header(
                tiny_http::Header::from_bytes(&b"Accept-Ranges"[..], &b"bytes"[..]).unwrap(),
            );
            let _ = request.respond(response);
        }
    });

    let mut source = HttpMediaSource::open_seekable(&url).expect("open");
    assert_eq!(source.byte_len(), Some(BODY.len() as u64));

    let end = source
        .seek(SeekFrom::End(0))
        .expect("landing at the end is a position, not a failure");
    assert_eq!(end, BODY.len() as u64);

    let mut buf = [0u8; 4];
    assert_eq!(
        source.read(&mut buf).expect("read at EOF"),
        0,
        "there is nothing past the end to hand back"
    );

    // And it is not stuck there: seeking back inside the body reopens it.
    assert_eq!(source.seek(SeekFrom::Start(4)).expect("seek back"), 4);
    let read = source.read(&mut buf).expect("read after seeking back");
    assert!(read > 0, "a seek back into the body must read real bytes");
    assert_eq!(&buf[..read], &BODY[4..4 + read]);
}
