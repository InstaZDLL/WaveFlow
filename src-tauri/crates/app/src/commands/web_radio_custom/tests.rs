use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A one-file HTTP server: each request gets the canned answer for
/// its path, and the listener runs until the test ends.
async fn serve(routes: Vec<(&'static str, String)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let answer = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, a)| a.clone())
                    .unwrap_or_else(|| {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                    });
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

fn audio(content_type: &str, name: Option<&str>) -> String {
    let icy = name
        .map(|n| format!("icy-name: {n}\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{icy}Content-Length: 4\r\n\r\nID3\0"
    )
}

fn text(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn an_accented_station_name_is_kept() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "icy-name",
        reqwest::header::HeaderValue::from_bytes("Radio Fréquence".as_bytes()).unwrap(),
    );
    assert_eq!(icy_name(&headers).as_deref(), Some("Radio Fréquence"));
}

#[tokio::test]
async fn a_stream_answers_with_its_own_url_and_name() {
    let base = serve(vec![("/live", audio("audio/mpeg", Some("Test FM")))]).await;
    let station = probe(&format!("{base}/live")).await.unwrap();
    assert_eq!(station.stream_url, format!("{base}/live"));
    assert_eq!(station.name.as_deref(), Some("Test FM"));
}

#[tokio::test]
async fn a_playlist_is_followed_to_the_stream_it_names() {
    let pls = "[playlist]\nFile1=/live\nTitle1=Jazz Club\nNumberOfEntries=1\n";
    let m3u = "#EXTM3U\n#EXTINF:-1,Relative One\nlive\n";
    let base = serve(vec![
        ("/listen.pls", text("audio/x-scpls", pls)),
        ("/dir/list.m3u", text("text/plain", m3u)),
        ("/live", audio("audio/aac", Some("From The Server"))),
        ("/dir/live", audio("audio/ogg", None)),
    ])
    .await;
    let station = probe(&format!("{base}/listen.pls")).await.unwrap();
    assert_eq!(station.stream_url, format!("{base}/live"));
    assert_eq!(station.name.as_deref(), Some("Jazz Club"));

    // A relative entry resolves against the playlist's own URL.
    let station = probe(&format!("{base}/dir/list.m3u")).await.unwrap();
    assert_eq!(station.stream_url, format!("{base}/dir/live"));
    assert_eq!(station.name.as_deref(), Some("Relative One"));
}

#[tokio::test]
async fn an_hls_playlist_is_the_stream_itself() {
    let hls = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg.ts\n";
    let m3u = "#EXTM3U\n#EXTINF:-1,Segmented FM\n/live/index.m3u8\n";
    let base = serve(vec![
        (
            "/live/index.m3u8",
            text("application/vnd.apple.mpegurl", hls),
        ),
        ("/listen.m3u", text("audio/x-mpegurl", m3u)),
    ])
    .await;
    let station = probe(&format!("{base}/live/index.m3u8")).await.unwrap();
    assert_eq!(station.stream_url, format!("{base}/live/index.m3u8"));
    // A station list naming an HLS stream gives that stream, and the
    // list's title for it.
    let station = probe(&format!("{base}/listen.m3u")).await.unwrap();
    assert_eq!(station.stream_url, format!("{base}/live/index.m3u8"));
    assert_eq!(station.name.as_deref(), Some("Segmented FM"));
}

#[tokio::test]
async fn what_is_not_a_station_is_refused_with_its_reason() {
    let encrypted =
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:6,\nseg.ts\n";
    let base = serve(vec![
        ("/page", text("text/html", "<html></html>")),
        (
            "/hls.m3u8",
            text("application/vnd.apple.mpegurl", encrypted),
        ),
        ("/empty.m3u", text("audio/x-mpegurl", "#EXTM3U\n")),
        ("/nested.m3u", text("audio/x-mpegurl", "/empty.m3u\n")),
    ])
    .await;
    let err = |r: AppResult<ProbedStation>| r.unwrap_err().to_string();
    assert!(err(probe(&format!("{base}/page")).await).contains(ERR_NOT_AUDIO));
    // HLS the player cannot play: encrypted segments.
    assert!(err(probe(&format!("{base}/hls.m3u8")).await).contains(ERR_HLS));
    assert!(err(probe(&format!("{base}/empty.m3u")).await).contains(ERR_NO_STREAM));
    assert!(err(probe(&format!("{base}/nested.m3u")).await).contains(ERR_NOT_AUDIO));
    assert!(err(probe(&format!("{base}/missing")).await).contains(ERR_UNREACHABLE));
    assert!(err(probe("ftp://example.com/live").await).contains(ERR_NOT_HTTP));
    assert!(err(probe("file:///etc/passwd").await).contains(ERR_NOT_HTTP));
    assert!(err(probe("not a url").await).contains(ERR_NOT_HTTP));
}

/// The error goes to the dialog and the log: credentials in the URL
/// must reach neither.
#[tokio::test]
async fn an_error_never_carries_the_credentials() {
    // A port nothing listens on: bind, read the port, drop.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = probe(&format!(
        "http://alice:hunter2@127.0.0.1:{port}/live?key=s3cret"
    ))
    .await
    .unwrap_err()
    .to_string();
    assert!(err.starts_with(ERR_UNREACHABLE), "{err}");
    assert!(!err.contains("hunter2") && !err.contains("s3cret"), "{err}");
}

/// A public station may not point the probe into the local network;
/// one the user put there themselves may.
#[test]
fn a_private_address_is_opened_only_on_the_user_s_say_so() {
    assert!(may_open(true, "https://cdn.example/live"));
    assert!(!may_open(true, "http://192.168.1.1/admin"));
    assert!(!may_open(true, "http://localhost:8000/live"));
    assert!(!may_open(true, "file:///etc/passwd"));
    // The same LAN address, spelled as IPv4-mapped IPv6, and the
    // carrier-grade NAT range.
    assert!(!may_open(true, "http://[::ffff:192.168.1.1]/admin"));
    assert!(!may_open(true, "http://100.64.0.1/"));
    // Names that only resolve on the local network.
    assert!(!may_open(true, "http://router/cgi-bin/"));
    assert!(!may_open(true, "http://nas.home.arpa:5000/"));
    assert!(!may_open(true, "http://nas.lan:8000/live"));
    assert!(may_open(false, "http://192.168.1.10:8000/live"));
    assert!(!may_open(false, "file:///etc/passwd"));
}

#[test]
fn a_playlist_file_keeps_its_absolute_streams() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.pls");
    std::fs::write(
        &path,
        "[playlist]\nFile1=relative.mp3\nFile2=https://a.example/live\nTitle2=A\n",
    )
    .unwrap();
    let entries = read_playlist_file(&path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].url, "https://a.example/live");
    assert_eq!(entries[0].title.as_deref(), Some("A"));

    std::fs::write(&path, "#EXTM3U\nrelative.mp3\n").unwrap();
    let err = read_playlist_file(&path).unwrap_err().to_string();
    assert!(err.contains(ERR_NO_STREAM), "{err}");
}

#[test]
fn a_file_not_named_like_a_playlist_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".git-credentials");
    std::fs::write(&path, "https://user:secret@a.example\n").unwrap();
    let err = read_playlist_file(&path).unwrap_err().to_string();
    assert!(err.contains(ERR_NO_STREAM), "{err}");
    assert!(!err.contains("secret"), "{err}");

    // A link named like a playlist is judged by what it points at.
    #[cfg(unix)]
    {
        let link = dir.path().join("station.m3u");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let err = read_playlist_file(&link).unwrap_err().to_string();
        assert!(err.contains(ERR_NO_STREAM), "{err}");
        assert!(!err.contains("secret"), "{err}");
    }
}

#[cfg(unix)]
#[test]
fn a_pipe_named_like_a_playlist_is_refused_without_blocking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.m3u");
    let status = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    let err = read_playlist_file(&path).unwrap_err().to_string();
    assert!(err.contains(ERR_NO_STREAM), "{err}");
}

#[test]
fn a_playlist_file_over_the_cap_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.m3u");
    let mut body = String::from("https://a.example/live\n");
    body.push_str(&"#".repeat(MAX_PLAYLIST_FILE_BYTES as usize));
    std::fs::write(&path, body).unwrap();
    let err = read_playlist_file(&path).unwrap_err().to_string();
    assert!(err.contains("too large"), "{err}");
}
