use super::*;

#[test]
fn base_url_names_the_address_the_peer_connected_to() {
    let fallback = "http://192.168.1.20:8200";
    let local = |s: &str| Some(s.parse::<SocketAddr>().unwrap());
    assert_eq!(
        base_url(local("172.20.0.1:8200"), fallback),
        "http://172.20.0.1:8200"
    );
    assert_eq!(
        base_url(local("[::ffff:10.0.0.2]:8200"), fallback),
        "http://10.0.0.2:8200"
    );
    assert_eq!(
        base_url(local("[fe80::1]:8200"), fallback),
        "http://[fe80::1]:8200"
    );
    assert_eq!(base_url(local("0.0.0.0:8200"), fallback), fallback);
    assert_eq!(base_url(None, fallback), fallback);
}

fn partial(start: u64, end: u64) -> RangeReply {
    RangeReply::Partial { start, end }
}

#[test]
fn parse_range_full_form() {
    assert_eq!(parse_range(Some("bytes=0-99"), 1000), partial(0, 99));
    assert_eq!(parse_range(Some("bytes=500-999"), 1000), partial(500, 999));
}

#[test]
fn parse_range_open_ended() {
    // `bytes=START-` means "from START to EOF". Common from VLC.
    assert_eq!(parse_range(Some("bytes=500-"), 1000), partial(500, 999));
}

/// `bytes=-N` is the last `N` bytes. It used to fail the start-byte
/// parse and fall back to the whole file.
#[test]
fn parse_range_serves_a_suffix() {
    assert_eq!(parse_range(Some("bytes=-100"), 1000), partial(900, 999));
    // Longer than the file: all of it, still as a 206.
    assert_eq!(parse_range(Some("bytes=-5000"), 1000), partial(0, 999));
    assert_eq!(
        parse_range(Some("bytes=-0"), 1000),
        RangeReply::Unsatisfiable
    );
}

/// An end past the last byte is clamped (RFC 9110 counts it as
/// satisfiable); a start past it names nothing and is a 416.
#[test]
fn parse_range_clamps_the_end_and_refuses_a_start_past_it() {
    assert_eq!(parse_range(Some("bytes=0-9999"), 1000), partial(0, 999));
    assert_eq!(
        parse_range(Some("bytes=1000-"), 1000),
        RangeReply::Unsatisfiable
    );
    assert_eq!(
        parse_range(Some("bytes=5000-6000"), 1000),
        RangeReply::Unsatisfiable
    );
}

/// What the server may ignore is ignored: the whole file, as a 200.
#[test]
fn parse_range_ignores_what_it_does_not_serve() {
    assert_eq!(parse_range(None, 1000), RangeReply::Full);
    assert_eq!(parse_range(Some("xyz=0-10"), 1000), RangeReply::Full);
    assert_eq!(parse_range(Some("bytes=900-100"), 1000), RangeReply::Full);
    assert_eq!(parse_range(Some("bytes=a-b"), 1000), RangeReply::Full);
    assert_eq!(parse_range(Some("bytes=0-1,5-9"), 1000), RangeReply::Full);
}

/// An empty file is a 200 with nothing in it — not `Content-Length: 1`
/// and no body, which is what the old arithmetic sent.
#[test]
fn an_empty_file_is_served_whole_with_no_bytes() {
    for range in [None, Some("bytes=0-"), Some("bytes=-1"), Some("bytes=0-0")] {
        assert_eq!(parse_range(range, 0), RangeReply::Full, "{range:?}");
    }
    assert_eq!(RangeReply::Full.window(0), Some((StatusCode::OK, 0, 0)));
}

#[test]
fn range_window_counts_inclusive_bytes() {
    assert_eq!(
        partial(900, 999).window(1000),
        Some((StatusCode::PARTIAL_CONTENT, 900, 100))
    );
    assert_eq!(
        RangeReply::Full.window(1000),
        Some((StatusCode::OK, 0, 1000))
    );
    assert_eq!(RangeReply::Unsatisfiable.window(1000), None);
}

#[test]
fn a_416_names_the_real_length() {
    let response = range_not_satisfiable(1000);
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        response.headers().get(header::CONTENT_RANGE).unwrap(),
        "bytes */1000"
    );
}

#[test]
fn art_names_are_a_hex_hash_and_an_image_extension() {
    let hash = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
    for ext in [
        "jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "bin", "JPG",
    ] {
        assert!(is_art_file_name(&format!("{hash}.{ext}")), "{ext}");
    }
    for name in [
        // A drive prefix with no root: `join` would replace the base.
        "D:secret.txt",
        "C:secret.jpg",
        "D:af13.jpg",
        // What the old deny-list already caught.
        "../secret.jpg",
        "..\\secret.jpg",
        "a/b.jpg",
        // Not a hash, or not an image.
        "secret.jpg",
        ".jpg",
        "af13",
        "af13.txt",
        "af13.jpg.exe",
        "af13.",
    ] {
        assert!(!is_art_file_name(name), "{name} must be refused");
    }
}

/// A `.bin` cover — what the scanner writes for an embedded picture whose
/// MIME type it could not map, typically a WebP — is served through the
/// real handler, typed by its bytes rather than refused.
#[tokio::test]
async fn a_bin_cover_is_served_with_the_type_of_its_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let profile_artwork_dir = dir.path().join("artwork");
    std::fs::create_dir_all(&profile_artwork_dir).unwrap();
    let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
    std::fs::write(profile_artwork_dir.join("af13.bin"), &webp).unwrap();
    std::fs::write(profile_artwork_dir.join("be57.bin"), b"not an image").unwrap();

    let ctx = Arc::new(ServerCtx {
        server_name: "WaveFlow".into(),
        base_url: "http://127.0.0.1:0".into(),
        pool: SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
        profile_artwork_dir,
        metadata_artwork_dir: dir.path().join("metadata_artwork"),
        labels: Default::default(),
    });

    let response = serve_art(State(ctx.clone()), Path("af13.bin".into())).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/webp"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], &webp[..]);

    // Bytes nothing recognises: still served, but not passed off as JPEG.
    let response = serve_art(State(ctx), Path("be57.bin".into())).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/octet-stream"
    );
}

#[test]
fn mime_for_art_trusts_the_extension_except_for_bin() {
    assert_eq!(mime_for_art("a.png", b""), "image/png");
    assert_eq!(mime_for_art("a.jpg", b""), "image/jpeg");
    assert_eq!(mime_for_art("a.bin", b"\xFF\xD8\xFF\xE0"), "image/jpeg");
    assert_eq!(mime_for_art("a.bin", b"\x89PNG\r\n\x1a\n"), "image/png");
}

/// Whatever the platform's path rules, an accepted name stays a child
/// of the artwork dir once joined — the property the route relies on.
#[test]
fn an_accepted_art_name_never_leaves_the_dir() {
    let dir = std::env::temp_dir().join("artwork");
    for name in ["af13.jpg", "AF13.PNG", "0.webp"] {
        assert!(is_art_file_name(name));
        let joined = dir.join(name);
        assert_eq!(joined.parent(), Some(dir.as_path()), "{name}");
    }
}

#[test]
fn mime_for_path_recognises_lossless_and_lossy() {
    assert_eq!(mime_for_path("a.flac"), "audio/flac");
    assert_eq!(mime_for_path("a.MP3"), "audio/mpeg");
    assert_eq!(mime_for_path("a.wv"), "audio/x-wavpack");
    assert_eq!(mime_for_path("a.ape"), "audio/x-ape");
    assert_eq!(mime_for_path("a.unknown"), "application/octet-stream");
}

/// Range headers a hostile or broken client can send. None of them may make
/// the server name a byte outside the file, or fail in any other way than
/// by ignoring the header or answering `416`: the DLNA stream and the media
/// loopback both read their `Range` through `parse_range`.
const HOSTILE_RANGES: &[&str] = &[
    "bytes=",
    "bytes=-",
    "bytes=--",
    "bytes=0--1",
    "bytes=-1-",
    "bytes=1-0",
    "bytes=1-2-3",
    "bytes=5",
    "bytes=a-",
    "bytes=0x10-20",
    "bytes=+5-10",
    "bytes= 5 - 10 ",
    "bytes=\t1-2",
    "bytes=5-10 , 20-30",
    "BYTES=0-1",
    "bytes=0-0",
    "bytes=-1",
    "bytes=999-",
    "bytes=1000-",
    "bytes=18446744073709551615-",
    "bytes=18446744073709551616-",
    "bytes=0-18446744073709551615",
    "bytes=0-18446744073709551616",
    "bytes=-18446744073709551615",
    "bytes=-18446744073709551616",
    "bytes=18446744073709551615-18446744073709551615",
    "bytes=999999999999999999999999999-",
    "bytes=\u{ff19}-10",
];

#[test]
fn parse_range_never_names_a_byte_outside_the_file() {
    for &total in &[0, 1, 2, 1000, u64::MAX] {
        for &raw in HOSTILE_RANGES {
            let reply = parse_range(Some(raw), total);
            if let RangeReply::Partial { start, end } = reply {
                assert!(
                    start <= end && end < total,
                    "{raw:?} of {total} bytes gave {start}-{end}"
                );
            }
            match reply.window(total) {
                Some((status, start, length)) => {
                    assert!(
                        status == StatusCode::OK || status == StatusCode::PARTIAL_CONTENT,
                        "{raw:?} of {total} bytes: {status}"
                    );
                    assert!(
                        start.checked_add(length).is_some_and(|end| end <= total),
                        "{raw:?} of {total} bytes: {length} bytes from {start}"
                    );
                }
                None => assert!(total > 0, "{raw:?}: a 416 for an empty file"),
            }
        }
    }
}

/// The same headers through the real stream handler, on a real file: each
/// is answered `200`, `206` or `416`, never an error, and a body is exactly
/// the bytes its headers announce. A header that is not text (obs-text) is
/// ignored like any malformed one.
#[tokio::test]
async fn a_hostile_range_is_answered_and_never_breaks_the_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("track.flac");
    let bytes: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();

    // One connection: every connection to `sqlite::memory:` is a database
    // of its own.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE track (id INTEGER PRIMARY KEY, file_path TEXT NOT NULL,
                             is_available INTEGER NOT NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO track (id, file_path, is_available) VALUES (1, ?, 1)")
        .bind(path.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .unwrap();
    let ctx = Arc::new(ServerCtx {
        server_name: "WaveFlow".into(),
        base_url: "http://127.0.0.1:0".into(),
        pool,
        profile_artwork_dir: dir.path().join("artwork"),
        metadata_artwork_dir: dir.path().join("metadata_artwork"),
        labels: Default::default(),
    });

    let mut values: Vec<HeaderValue> = HOSTILE_RANGES
        .iter()
        .map(|raw| HeaderValue::from_str(raw).unwrap())
        .collect();
    values.push(HeaderValue::from_bytes(b"bytes=\xff0-1").unwrap());

    for value in values {
        let mut request = HeaderMap::new();
        request.insert(header::RANGE, value.clone());
        let response = serve_stream(State(ctx.clone()), Path(1), request).await;
        let status = response.status();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        match status {
            StatusCode::RANGE_NOT_SATISFIABLE => {
                assert_eq!(headers[header::CONTENT_RANGE], "bytes */1000", "{value:?}");
                assert!(body.is_empty(), "{value:?}");
            }
            StatusCode::OK => assert_eq!(&body[..], &bytes[..], "{value:?}"),
            StatusCode::PARTIAL_CONTENT => {
                let range = headers[header::CONTENT_RANGE].to_str().unwrap();
                let (span, total) = range
                    .strip_prefix("bytes ")
                    .and_then(|r| r.split_once('/'))
                    .unwrap();
                assert_eq!(total, "1000", "{value:?}");
                let (start, end) = span.split_once('-').unwrap();
                let (start, end): (usize, usize) = (start.parse().unwrap(), end.parse().unwrap());
                assert_eq!(&body[..], &bytes[start..=end], "{value:?}");
            }
            other => panic!("{value:?} was answered {other}"),
        }
        if status != StatusCode::RANGE_NOT_SATISFIABLE {
            assert_eq!(
                headers[header::CONTENT_LENGTH],
                body.len().to_string().as_str(),
                "{value:?}"
            );
        }
    }
}
