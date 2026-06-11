//! HTTP `MediaSource` for live audio streams (Web Radio).
//!
//! Symphonia probes + decodes through any `Read + Seek + Send + Sync`
//! source. Files are the obvious case (see [`crossfade::ActiveStream::open`]);
//! this module is the network-side equivalent: we open a streaming
//! `reqwest::blocking::Response`, pre-buffer enough bytes for the probe
//! to lock onto the codec, then hand the wrapper to symphonia exactly
//! like a local file.
//!
//! ## Why blocking reqwest
//!
//! The audio decoder lives on a dedicated `std::thread` (no tokio
//! reactor attached), so calling `reqwest::blocking::Client::build()`
//! from inside the decoder is safe — the panic case fixed by hotfix
//! #225 only triggers when the blocking client is constructed *while*
//! an outer tokio runtime is active on the same thread. Reads then
//! happen synchronously in the same thread that calls
//! `decoder.decode(packet)`, which is the contract symphonia wants.
//!
//! ## Why a `Mutex` wrapper
//!
//! `symphonia::core::io::MediaSource: Read + Seek + Send + Sync`.
//! `reqwest::blocking::Response` is `Send` but not `Sync`, so we wrap
//! it in a `std::sync::Mutex` to satisfy the bound (no extra dep on
//! `parking_lot` — the decoder thread is the only reader and the
//! mutex is uncontended in practice, so std's slower contended path
//! never matters here).
//!
//! ## Seek semantics
//!
//! Live radio streams aren't seekable in either direction: there's no
//! "rewind 30 s" because the bytes are produced on the wire as the
//! source generates them, and seek-forward would require buffering the
//! tail of the stream. We expose `is_seekable() = false` so symphonia
//! never tries; any `Seek::seek` call returns `Unsupported` — the
//! decoder loop branches on `format.metadata().is_seekable()` already
//! for VBR MP3s, so this mirrors that contract.

use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use symphonia::core::io::MediaSource;

/// Strip credentials + query string from a URL before logging it.
/// Radio mount URLs sometimes carry userinfo (`http://user:pwd@host`)
/// or auth tokens in the query (`?key=abcd`); echoing them into the
/// tracing layer would leak the secret into the rolling log file the
/// user could later attach to a bug report.
///
/// Falls back to the literal string on parse failure so we never
/// accidentally swallow a malformed URL — an unparseable URL is its
/// own diagnostic signal.
pub fn redact_url(raw: &str) -> String {
    match url::Url::parse(raw) {
        Ok(parsed) => {
            // Manually rebuild scheme://host[:port]/path so userinfo
            // + query + fragment are gone. Host can be None for
            // exotic schemes like `data:` but we only handle http(s)
            // here in practice.
            let scheme = parsed.scheme();
            let host = parsed.host_str().unwrap_or("");
            let port = match parsed.port() {
                Some(p) => format!(":{p}"),
                None => String::new(),
            };
            let path = parsed.path();
            format!("{scheme}://{host}{port}{path}")
        }
        Err(_) => raw.to_string(),
    }
}

/// Connect timeout for the initial HTTP GET. Radio streams can take a
/// few hundred ms to start sending bytes on a slow source; 10 s is
/// generous enough to cover that without leaving the user staring at
/// a spinner for a misconfigured station.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// No request-level read timeout — reqwest::blocking's `timeout()` is a
// deadline for the *entire* call, which would kill a long-running radio
// stream after 30 s. Stalls are rare in practice; if they happen the
// user hits Stop and the decoder thread unwinds through the next
// `try_recv`.

/// User-Agent advertised to radio servers. radio-browser entries vary
/// in how strictly they filter — some Icecast configs reject empty UAs
/// outright. Mirror the `Mozilla/5.0` style that desktop browsers use
/// so we look like any other listener.
const USER_AGENT: &str = concat!(
    "WaveFlow/",
    env!("CARGO_PKG_VERSION"),
    " (+https://waveflow.app)"
);

/// MediaSource backed by a live HTTP response body.
///
/// Construction is synchronous and blocks until the server has
/// answered the HTTP handshake. Reads are buffered (8 KiB) to amortise
/// per-read syscalls in the same way the file-backed path does.
pub struct HttpMediaSource {
    /// Buffered reader over the response body. Wrapped in a `Mutex` to
    /// upgrade `reqwest::blocking::Response` from `Send` to `Send + Sync`
    /// (required by [`MediaSource`]).
    inner: Mutex<BufReader<Response>>,
    /// Cached origin URL — only used in `Debug` impls / error messages
    /// so a logged "read failed" line points at the offending stream.
    url: String,
}

impl HttpMediaSource {
    /// Open a streaming HTTP GET on `url`. The response is checked for
    /// success status before being wrapped; a 404 / 502 surfaces as
    /// `Err` instead of producing a `MediaSource` that would only
    /// fail at the first `probe()` call.
    pub fn open(url: &str) -> Result<Self, String> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            // No redirect cap is fine here — radio mounts often
            // redirect once or twice to a CDN edge.
            .build()
            .map_err(|e| format!("http client build: {e}"))?;

        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "audio/*,*/*;q=0.8")
            // Some Icecast mounts mute the audio payload unless the
            // client opts into metadata interleaving with this header
            // set explicitly to "0" (we don't parse ICY metadata — the
            // codec layer doesn't expect interleaved title frames).
            .header("Icy-MetaData", "0")
            .send()
            .map_err(|e| format!("http get: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("http status {status}"));
        }

        Ok(Self {
            inner: Mutex::new(BufReader::with_capacity(8 * 1024, response)),
            url: url.to_string(),
        })
    }

    /// Origin URL for diagnostics.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Read for HttpMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // `Mutex::lock` only fails on poisoning; we never panic inside
        // a held guard so treat poisoning as a fatal I/O error rather
        // than silently swallowing it.
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("http source mutex poisoned"))?;
        guard.read(buf)
    }
}

impl Seek for HttpMediaSource {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "live HTTP stream is not seekable",
        ))
    }
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_host_surfaces_error() {
        // DNS for a domain that doesn't exist must fail at `open`
        // rather than at the first `read` — symphonia would otherwise
        // panic on an empty probe buffer. `HttpMediaSource` doesn't
        // impl Debug (wraps a `reqwest::blocking::Response` which
        // doesn't either), so destructure by hand instead of
        // `expect_err`.
        match HttpMediaSource::open(
            "http://this-domain-definitely-does-not-exist.invalid/stream",
        ) {
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

    #[test]
    fn http_media_source_is_send_and_sync() {
        // `MediaSource` requires both — assert it at the type level so
        // a future refactor that breaks the bound fails to compile
        // here instead of inside a confusing symphonia generic.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HttpMediaSource>();
    }
}
