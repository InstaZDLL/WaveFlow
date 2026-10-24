//! axum router for the DLNA MediaServer.
//!
//! Routes:
//!   GET  /description.xml             — root device descriptor
//!   GET  /service/ContentDirectory.xml — CDS SCPD
//!   GET  /service/ConnectionManager.xml — CM SCPD
//!   POST /control/ContentDirectory    — SOAP control endpoint (étape 4)
//!   GET  /stream/<track_id>           — audio bytes with Range support
//!   GET  /art/<hash>                  — artwork shim (delegates to the
//!                                        per-profile artwork dir or the
//!                                        shared metadata_artwork dir)
//!   GET  /healthz                     — diag (always 200 "ok")

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{connect_info::Connected, ConnectInfo, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    serve::IncomingStream,
    Router,
};
use sqlx::SqlitePool;
use tokio::io::{AsyncSeekExt, SeekFrom};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

use crate::dlna::{cds, description};

/// Shared state handed to every axum handler. `pool_factory` returns
/// the active profile's pool on demand so a profile switch flips the
/// data source without restarting the server.
#[derive(Clone)]
pub struct ServerCtx {
    pub server_name: String,
    /// The preferred address, used only when a connection's own local
    /// address is unknown (see [`ServerCtx::base_url_for`]).
    pub base_url: String,
    pub pool: SqlitePool,
    pub profile_artwork_dir: PathBuf,
    pub metadata_artwork_dir: PathBuf,
}

impl ServerCtx {
    /// The base of every URL handed to the peer behind `conn`: the
    /// address that peer reached us on. On a host with several
    /// interfaces the preferred one may sit on a network the peer
    /// cannot route to, while the one it connected to is reachable by
    /// construction.
    pub fn base_url_for(&self, conn: LocalAddr) -> String {
        base_url(conn.0, &self.base_url)
    }
}

/// The local end of an accepted connection, recorded by the server
/// (`into_make_service_with_connect_info`) for [`ServerCtx::base_url_for`].
#[derive(Debug, Clone, Copy)]
pub struct LocalAddr(pub Option<SocketAddr>);

impl Connected<IncomingStream<'_, TcpListener>> for LocalAddr {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        Self(stream.io().local_addr().ok())
    }
}

fn base_url(local: Option<SocketAddr>, fallback: &str) -> String {
    match local {
        Some(addr) if !addr.ip().is_unspecified() => {
            // A dual-stack socket reports an IPv4 peer as `::ffff:a.b.c.d`.
            let addr = SocketAddr::new(addr.ip().to_canonical(), addr.port());
            format!("http://{addr}")
        }
        _ => fallback.to_string(),
    }
}

pub fn router(ctx: ServerCtx) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/description.xml", get(serve_description))
        .route(
            "/service/ContentDirectory.xml",
            get(|| async {
                xml_response(description::CONTENT_DIRECTORY_SCPD.to_string())
            }),
        )
        .route(
            "/service/ConnectionManager.xml",
            get(|| async {
                xml_response(description::CONNECTION_MANAGER_SCPD.to_string())
            }),
        )
        .route("/stream/{track_id}", get(serve_stream))
        .route("/art/{hash}", get(serve_art))
        .route("/control/ContentDirectory", post(cds::handle_control))
        // Stub the ConnectionManager control endpoint with a 200 OK
        // empty SOAP body — controllers query GetProtocolInfo right
        // after discovery; failing it makes them deprioritise us.
        .route(
            "/control/ConnectionManager",
            post(|| async {
                let body = r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:GetProtocolInfoResponse xmlns:u="urn:schemas-upnp-org:service:ConnectionManager:1">
      <Source>http-get:*:audio/mpeg:*,http-get:*:audio/flac:*,http-get:*:audio/wav:*,http-get:*:audio/x-aiff:*,http-get:*:audio/ogg:*,http-get:*:audio/mp4:*</Source>
      <Sink></Sink>
    </u:GetProtocolInfoResponse>
  </s:Body>
</s:Envelope>"#;
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
                    body,
                )
            }),
        )
        .with_state(Arc::new(ctx))
}

async fn serve_description(
    State(ctx): State<Arc<ServerCtx>>,
    ConnectInfo(conn): ConnectInfo<LocalAddr>,
) -> Response {
    let body = description::device_descriptor(&ctx.server_name, &ctx.base_url_for(conn));
    xml_response(body)
}

fn xml_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=\"utf-8\"")],
        body,
    )
        .into_response()
}

/// Stream audio with HTTP Range support. Yamaha / Sonos send a
/// `Range: bytes=START-END` header to seek; without `206 Partial
/// Content` they fall back to "play from zero" or refuse to play
/// long files at all.
async fn serve_stream(
    State(ctx): State<Arc<ServerCtx>>,
    Path(track_id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    let row: Result<Option<(String,)>, _> =
        sqlx::query_as("SELECT file_path FROM track WHERE id = ? AND is_available = 1")
            .bind(track_id)
            .fetch_optional(&ctx.pool)
            .await;
    let path = match row {
        Ok(Some((p,))) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, "track not found").into_response(),
        Err(err) => {
            tracing::warn!(?err, track_id, "stream lookup failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    };

    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(err) => {
            tracing::warn!(?err, %path, "stream open failed");
            return (StatusCode::NOT_FOUND, "file missing").into_response();
        }
    };
    let metadata = match file.metadata().await {
        Ok(m) => m,
        Err(err) => {
            tracing::warn!(?err, %path, "stream metadata failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "stat error").into_response();
        }
    };
    let total = metadata.len();
    let mime = mime_for_path(&path);

    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let Some((status, start, length)) = parse_range(range, total).window(total) else {
        return range_not_satisfiable(total);
    };

    let body = match build_range_body(file, start, length).await {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!(?err, %path, start, length, "range read failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "read error").into_response();
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).unwrap());
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        let end = start + length - 1;
        let cr = format!("bytes {start}-{end}/{total}");
        headers.insert(header::CONTENT_RANGE, HeaderValue::from_str(&cr).unwrap());
    }
    // DLNA seek hint — controllers won't expose a scrubber without it.
    headers.insert(
        "transferMode.dlna.org",
        HeaderValue::from_static("Streaming"),
    );
    headers.insert(
        "contentFeatures.dlna.org",
        HeaderValue::from_static("DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000000000000000000000000000"),
    );

    (status, headers, body).into_response()
}

/// Build a streaming body that reads the requested byte range in 64
/// KiB chunks. `take(length)` caps the reader so we never overshoot
/// the Range window even if the controller closes early.
pub(crate) async fn build_range_body(
    mut file: tokio::fs::File,
    start: u64,
    length: u64,
) -> std::io::Result<Body> {
    file.seek(SeekFrom::Start(start)).await?;
    let limited = tokio::io::AsyncReadExt::take(file, length);
    let stream = ReaderStream::with_capacity(limited, 64 * 1024);
    Ok(Body::from_stream(stream))
}

/// How to answer a GET for a `total`-byte file, given its `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RangeReply {
    /// `200` with the whole file: no range, or one this server ignores.
    Full,
    /// `206` with bytes `start..=end`.
    Partial { start: u64, end: u64 },
    /// `416`: the range names no byte of the file.
    Unsatisfiable,
}

impl RangeReply {
    /// The status, first byte and byte count to send, or `None` for a 416.
    pub(crate) fn window(self, total: u64) -> Option<(StatusCode, u64, u64)> {
        match self {
            RangeReply::Full => Some((StatusCode::OK, 0, total)),
            RangeReply::Partial { start, end } => {
                Some((StatusCode::PARTIAL_CONTENT, start, end - start + 1))
            }
            RangeReply::Unsatisfiable => None,
        }
    }
}

/// Read a `Range` header the way RFC 9110 §14 asks.
///
/// - `bytes=S-E`, `bytes=S-` and the suffix form `bytes=-N` (the last `N`
///   bytes) are served as a `206`; an `E` past the end is clamped to the
///   last byte, which the RFC counts as satisfiable.
/// - A first byte at or past the end, or `bytes=-0`, names no byte at all:
///   `416`, which a controller probing past the end of a file expects.
/// - Anything the server may ignore — no header, another unit, a malformed
///   spec, several ranges at once — gets the whole file.
///
/// An empty file is always served whole: no range can name a byte of it,
/// and a `200` with nothing in it is the answer every client takes.
pub(crate) fn parse_range(raw: Option<&str>, total: u64) -> RangeReply {
    let Some(spec) = raw.and_then(|r| r.trim().strip_prefix("bytes=")) else {
        return RangeReply::Full;
    };
    if total == 0 || spec.contains(',') {
        return RangeReply::Full;
    }
    let Some((start_s, end_s)) = spec.trim().split_once('-') else {
        return RangeReply::Full;
    };
    let (start_s, end_s) = (start_s.trim(), end_s.trim());
    let last = total - 1;
    if start_s.is_empty() {
        // Suffix form: the last `N` bytes, all of them when `N` is larger.
        return match end_s.parse::<u64>() {
            Ok(0) => RangeReply::Unsatisfiable,
            Ok(n) => RangeReply::Partial {
                start: total.saturating_sub(n),
                end: last,
            },
            Err(_) => RangeReply::Full,
        };
    }
    let Ok(start) = start_s.parse::<u64>() else {
        return RangeReply::Full;
    };
    let end = if end_s.is_empty() {
        last
    } else {
        match end_s.parse::<u64>() {
            Ok(end) if end >= start => end.min(last),
            // `S-E` with `E < S` is not a valid range-spec: ignore it.
            _ => return RangeReply::Full,
        }
    };
    if start > last {
        return RangeReply::Unsatisfiable;
    }
    RangeReply::Partial { start, end }
}

/// The `416` for a range that names no byte of a `total`-byte file. Its
/// `Content-Range: bytes */<len>` is what tells the client the real length.
pub(crate) fn range_not_satisfiable(total: u64) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{total}")).unwrap(),
    );
    (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response()
}

fn mime_for_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        // `x-aiff` rather than `aiff`: it is what Apple emits and what
        // renderers actually match on.
        Some("aiff" | "aif") => "audio/x-aiff",
        Some("ogg" | "oga") => "audio/ogg",
        Some("m4a" | "mp4" | "aac") => "audio/mp4",
        _ => "application/octet-stream",
    }
}

/// Serve cached artwork by blake3 hash. Probes both the per-profile
/// `artwork/` dir (embedded + folder-cover bytes from the scanner)
/// and the shared `metadata_artwork/` dir (Deezer downloads). Returns
/// 404 when neither match — the controller will fall back to its
/// generic music-note placeholder.
async fn serve_art(
    State(ctx): State<Arc<ServerCtx>>,
    Path(hash_with_ext): Path<String>,
) -> Response {
    // Only the one shape this route serves goes near the filesystem. A
    // deny-list of `/`, `\` and `..` is not enough: on Windows a name with a
    // drive prefix and no root (`D:secret.txt`) *replaces* the base in
    // `PathBuf::join`, so the lookup below would read any file on that drive.
    if !is_art_file_name(&hash_with_ext) {
        return (StatusCode::BAD_REQUEST, "bad hash").into_response();
    }
    for dir in [&ctx.profile_artwork_dir, &ctx.metadata_artwork_dir] {
        let candidate = dir.join(&hash_with_ext);
        if candidate.is_file() {
            return match tokio::fs::read(&candidate).await {
                Ok(bytes) => (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, mime_for_art(&hash_with_ext, &bytes))],
                    bytes,
                )
                    .into_response(),
                Err(err) => {
                    tracing::warn!(?err, ?candidate, "art read failed");
                    (StatusCode::INTERNAL_SERVER_ERROR, "read error").into_response()
                }
            };
        }
    }
    (StatusCode::NOT_FOUND, "art missing").into_response()
}

/// The extensions a stored cover can carry — what the scanner, the tag
/// editor and the metadata downloads write — and so the only ones `/art`
/// will serve. `bin` is the scanner's name for an embedded picture whose
/// MIME type lofty could not map, most often a WebP tagged with an empty or
/// non-standard type; Browse links to it like any other cover.
const ART_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "bin"];

/// Whether `name` is `<hex hash>.<image extension>`, the name of every cover
/// Browse links to. Hex digits and a known extension leave no character a
/// path could be built from: no separator, no `..`, no drive prefix.
fn is_art_file_name(name: &str) -> bool {
    let Some((hash, ext)) = name.split_once('.') else {
        return false;
    };
    (1..=128).contains(&hash.len())
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && ART_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
}

/// The `Content-Type` for a cover named `name`. A `.bin` says nothing about
/// what it holds, so its bytes decide; one the sniffer does not recognise
/// goes out as `application/octet-stream` rather than a guessed image type.
fn mime_for_art(name: &str, bytes: &[u8]) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("bin") => match waveflow_core::artwork::image_format::detect_image_format(bytes) {
            Some("jpg") => "image/jpeg",
            Some("png") => "image/png",
            Some("webp") => "image/webp",
            _ => "application/octet-stream",
        },
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("tiff") => "image/tiff",
        _ => "image/jpeg",
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(mime_for_path("a.unknown"), "application/octet-stream");
    }
}
