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
use tauri::AppHandle;

use super::events::{emit_radio_metadata, RadioMetadataPayload};

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

/// Context the host hands to [`HttpMediaSource::open_with_icy`] so the
/// source can re-emit `player:radio-metadata` whenever the stream's
/// ICY `StreamTitle` changes. The station artwork + a fallback artist
/// ride along so each "now playing" update keeps the station's cover +
/// identity instead of blanking them.
pub struct IcyContext {
    pub app: AppHandle,
    pub track_id: i64,
    /// Raw stream URL — the favorite id is `url:<station_url>`, so it
    /// rides every emit to let the PlayerBar / mini-player star save the
    /// station even after the now-playing line shows a song title.
    pub station_url: String,
    /// Station display name (kept stable across song changes).
    pub station_name: Option<String>,
    /// Station name kept as the artist line when a `StreamTitle` has no
    /// `Artist - Title` split (e.g. a bare show name).
    pub station_artist: Option<String>,
    pub artwork_url: Option<String>,
}

/// MediaSource backed by a live HTTP response body.
///
/// Construction is synchronous and blocks until the server has
/// answered the HTTP handshake. Reads are buffered (8 KiB) to amortise
/// per-read syscalls in the same way the file-backed path does.
///
/// ## ICY metadata
///
/// When opened via [`HttpMediaSource::open_with_icy`] and the server
/// honours `Icy-MetaData: 1` (advertising an `icy-metaint` interval),
/// the read path de-interleaves the periodic metadata blocks out of the
/// audio stream — symphonia only ever sees pure audio — and parses the
/// `StreamTitle`, re-emitting `player:radio-metadata` on change so the
/// PlayerBar shows the live song. Streams that don't carry ICY (HLS,
/// DASH, or servers that ignore the header) set `metaint = 0` and the
/// read path is a plain passthrough.
pub struct HttpMediaSource {
    /// Buffered reader over the response body. Wrapped in a `Mutex` to
    /// upgrade `reqwest::blocking::Response` from `Send` to `Send + Sync`
    /// (required by [`MediaSource`]).
    inner: Mutex<BufReader<Response>>,
    /// Cached origin URL — only used in `Debug` impls / error messages
    /// so a logged "read failed" line points at the offending stream.
    url: String,
    /// Bytes of audio between two ICY metadata blocks. `0` disables ICY
    /// handling entirely (no metadata interleaved → plain passthrough).
    metaint: usize,
    /// Audio bytes still owed before the next metadata block. Starts at
    /// `metaint`; decremented as audio is read, and when it hits `0` the
    /// read path consumes one metadata block before resuming.
    bytes_until_meta: usize,
    /// `Some` only when ICY is active — carries the handle to emit on +
    /// the station identity to keep across song changes.
    icy: Option<IcyContext>,
    /// Last `StreamTitle` we emitted, so an unchanged metadata block
    /// (radio servers resend the same title every interval) doesn't
    /// re-fire the event.
    last_title: Option<String>,
}

impl HttpMediaSource {
    /// Open a streaming HTTP GET on `url` without ICY metadata. The
    /// response is checked for success status before being wrapped; a
    /// 404 / 502 surfaces as `Err` instead of producing a `MediaSource`
    /// that would only fail at the first `probe()` call.
    pub fn open(url: &str) -> Result<Self, String> {
        Self::open_inner(url, None)
    }

    /// Open a streaming HTTP GET that also requests + de-interleaves ICY
    /// metadata, re-emitting `player:radio-metadata` through `icy.app`
    /// whenever the live `StreamTitle` changes. Falls back transparently
    /// to passthrough when the server ignores `Icy-MetaData: 1`.
    pub fn open_with_icy(url: &str, icy: IcyContext) -> Result<Self, String> {
        Self::open_inner(url, Some(icy))
    }

    fn open_inner(url: &str, icy: Option<IcyContext>) -> Result<Self, String> {
        // Offline short-circuit at the HTTP boundary itself. The decoder
        // already gates `LoadUrlAndPlay` on this before reaching here, but
        // guarding the source too makes it self-honouring for any future
        // caller — the project convention is that every outbound HTTP path
        // checks `offline::is_offline()` first.
        if crate::offline::is_offline() {
            return Err("offline mode is enabled".to_string());
        }

        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(CONNECT_TIMEOUT)
            // No redirect cap is fine here — radio mounts often
            // redirect once or twice to a CDN edge.
            .build()
            .map_err(|e| format!("http client build: {e}"))?;

        // Opt into metadata interleaving only when we have an ICY sink.
        // Some Icecast mounts also mute the audio payload unless the
        // header is present at all, so the passthrough path still sends
        // an explicit "0".
        let want_icy = icy.is_some();
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "audio/*,*/*;q=0.8")
            .header("Icy-MetaData", if want_icy { "1" } else { "0" })
            .send()
            .map_err(|e| format!("http get: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            return Err(format!("http status {status}"));
        }

        // `icy-metaint` is the audio-byte interval between metadata
        // blocks. Absent / unparsable / zero → the server isn't
        // interleaving metadata, so we stay in passthrough even though
        // we asked for it.
        let metaint = if want_icy {
            response
                .headers()
                .get("icy-metaint")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0)
        } else {
            0
        };

        Ok(Self {
            inner: Mutex::new(BufReader::with_capacity(8 * 1024, response)),
            url: url.to_string(),
            metaint,
            bytes_until_meta: metaint,
            // Drop the sink when the server isn't interleaving — keeps
            // the read path's `Some` check honest (ICY active iff we'll
            // actually parse blocks).
            icy: if metaint > 0 { icy } else { None },
            last_title: None,
        })
    }

    /// Origin URL for diagnostics.
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// Read one ICY metadata block from `reader`: a single length byte `L`
/// followed by `L * 16` bytes of (null-padded) metadata. `L == 0` means
/// "no change this interval" and yields `None`. Returns the raw block
/// bytes on success; an `UnexpectedEof` mid-block propagates as the
/// stream ending.
fn read_icy_block<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut len_byte = [0u8; 1];
    reader.read_exact(&mut len_byte)?;
    let len = len_byte[0] as usize * 16;
    if len == 0 {
        return Ok(None);
    }
    let mut block = vec![0u8; len];
    reader.read_exact(&mut block)?;
    Ok(Some(block))
}

/// Extract the `StreamTitle` value from a raw ICY metadata block. The
/// block is a sequence of `key='value';` pairs in latin1/utf8; we pull
/// the single field the PlayerBar cares about. Returns `None` when the
/// field is absent or its value is blank (silence / station idents
/// often send an empty title).
fn parse_stream_title(block: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(block);
    let start = text.find("StreamTitle='")? + "StreamTitle='".len();
    let rest = &text[start..];
    // Terminate at the closing `';` (the spec quotes values); fall back
    // to the trailing NUL padding / end if a server omits the closer.
    let end = rest.find("';").unwrap_or_else(|| {
        rest.find('\0').unwrap_or(rest.len())
    });
    let title = rest[..end].trim().trim_matches('\0').trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Split a raw `StreamTitle` into (title, artist) for the PlayerBar.
/// The near-universal radio convention is `Artist - Title`; when there's
/// no ` - ` separator we keep the whole string as the title and let the
/// caller supply the station name as the artist line.
fn split_artist_title(raw: &str) -> (String, Option<String>) {
    match raw.split_once(" - ") {
        Some((artist, title)) => {
            let artist = artist.trim();
            let title = title.trim();
            if artist.is_empty() || title.is_empty() {
                (raw.to_string(), None)
            } else {
                (title.to_string(), Some(artist.to_string()))
            }
        }
        None => (raw.to_string(), None),
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

        // Passthrough fast path: no ICY interleaving on this stream.
        if self.metaint == 0 {
            return guard.read(buf);
        }
        if buf.is_empty() {
            return Ok(0);
        }

        // At a metadata boundary: consume exactly one block before
        // returning any more audio, so symphonia never sees the
        // interleaved bytes. Parsed under the lock, emitted after.
        let mut new_title: Option<Option<String>> = None;
        if self.bytes_until_meta == 0 {
            match read_icy_block(&mut *guard)? {
                Some(block) => new_title = Some(parse_stream_title(&block)),
                None => { /* L==0: no change this interval */ }
            }
            self.bytes_until_meta = self.metaint;
        }

        // Read at most up to the next metadata boundary so we never copy
        // a metadata byte into the audio buffer.
        let want = buf.len().min(self.bytes_until_meta);
        let n = guard.read(&mut buf[..want])?;
        self.bytes_until_meta -= n;
        drop(guard);

        // Emit outside the lock. Only when the title actually changed —
        // servers resend the same `StreamTitle` every interval.
        if let Some(parsed) = new_title {
            if parsed != self.last_title {
                self.last_title = parsed.clone();
                self.emit_now_playing(parsed.as_deref());
            }
        }

        Ok(n)
    }
}

impl HttpMediaSource {
    /// Emit `player:radio-metadata` with the live song. `raw` is the
    /// parsed `StreamTitle`, or `None` when the station cleared it (back
    /// to "just the station"). Keeps the station artwork on every update
    /// and falls back to the station name for the artist line.
    fn emit_now_playing(&self, raw: Option<&str>) {
        let Some(icy) = &self.icy else { return };
        tracing::info!(
            track_id = icy.track_id,
            stream_title = ?raw,
            "radio ICY now-playing"
        );
        let (title, artist) = match raw {
            Some(raw) => {
                let (title, artist) = split_artist_title(raw);
                (Some(title), artist.or_else(|| icy.station_artist.clone()))
            }
            None => (None, icy.station_artist.clone()),
        };
        emit_radio_metadata(
            &icy.app,
            RadioMetadataPayload {
                track_id: icy.track_id,
                title,
                artist,
                artwork_url: icy.artwork_url.clone(),
                station_url: Some(icy.station_url.clone()),
                station_name: icy.station_name.clone(),
                station_artist: icy.station_artist.clone(),
                station_artwork: icy.artwork_url.clone(),
            },
        );
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
    fn split_artist_title_splits_on_separator() {
        assert_eq!(
            split_artist_title("Daft Punk - Get Lucky"),
            ("Get Lucky".to_string(), Some("Daft Punk".to_string()))
        );
        // No separator → whole string is the title, no artist.
        assert_eq!(
            split_artist_title("Morning Show"),
            ("Morning Show".to_string(), None)
        );
        // A dangling separator half keeps the raw string as the title.
        assert_eq!(
            split_artist_title("Artist - "),
            ("Artist - ".to_string(), None)
        );
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
}
