//! HTTP `MediaSource` for live audio streams (Web Radio).
//!
//! Symphonia probes + decodes through any `Read + Seek + Send + Sync`
//! source. Files are the obvious case (see [`ActiveStream::open`](waveflow_core::playback::stream::ActiveStream::open));
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
use waveflow_core::error::describe_http_error;

/// Strip credentials + query string from a URL before logging it.
/// Radio mount URLs sometimes carry userinfo (`http://user:pwd@host`)
/// or auth tokens in the query (`?key=abcd`); echoing them into the
/// tracing layer would leak the secret into the rolling log file the
/// user could later attach to a bug report.
///
/// A remote server's stream URL keeps its secret in the *path* instead:
/// `/api/v2/stream/<ticket>` is a bearer ticket good for about an hour,
/// so that segment is masked too (see [`mask_stream_ticket`]).
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
            let path = mask_stream_ticket(parsed.path());
            format!("{scheme}://{host}{port}{path}")
        }
        Err(_) => raw.to_string(),
    }
}

/// Where a remote server's stream ticket sits in its URL. Matched anywhere
/// in the path, not only at its start, so a server mounted under a prefix
/// (`https://host/music/api/v2/stream/<ticket>`) is masked as well.
const STREAM_TICKET_PREFIX: &str = "/api/v2/stream/";

/// Replace the segment after [`STREAM_TICKET_PREFIX`] with `<ticket>`.
/// Anyone holding that segment can fetch the track until it expires, so it
/// must never reach the log; the rest of the path stays, since it is what
/// tells a reader which kind of URL failed.
fn mask_stream_ticket(path: &str) -> String {
    let Some(at) = path.find(STREAM_TICKET_PREFIX) else {
        return path.to_string();
    };
    let start = at + STREAM_TICKET_PREFIX.len();
    let end = path[start..].find('/').map_or(path.len(), |i| start + i);
    if start == end {
        return path.to_string();
    }
    format!("{}<ticket>{}", &path[..start], &path[end..])
}

/// Connect timeout for the initial HTTP GET. Radio streams can take a
/// few hundred ms to start sending bytes on a slow source; 10 s is
/// generous enough to cover that without leaving the user staring at
/// a spinner for a misconfigured station.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// No request-level `timeout()` anywhere — reqwest::blocking's is a
// deadline for the *entire* call, which would kill a long-running radio
// stream, and a seekable body is a *long file* whose remainder streams
// through one ranged GET, so a total timeout would cap its playback just
// the same. A per-read (inter-read) timeout would be the right tool for a
// stalled seek-reopen, but reqwest::blocking's `ClientBuilder` doesn't
// expose one (only the async builder does). Rather than pull in a
// watchdog-thread wrapper, we accept the same failure mode as radio: a
// stalled read blocks the decoder thread until the user hits Stop, which
// unwinds it through the next `try_recv`.

/// User-Agent advertised to radio servers. radio-browser entries vary
/// in how strictly they filter — some Icecast configs reject empty UAs
/// outright. Mirror the `Mozilla/5.0` style that desktop browsers use
/// so we look like any other listener.
pub(crate) const USER_AGENT: &str = concat!(
    "WaveFlow/",
    env!("CARGO_PKG_VERSION"),
    " (+https://waveflow.app)"
);

/// What a stream's ICY `StreamTitle` changes are handed to: the time-shift
/// buffer of [`super::live`], which records each at the offset it arrived
/// at so the title shows when the audio around it is heard. `None` is a
/// title the station cleared.
pub type TitleSink = Box<dyn FnMut(Option<String>) + Send + Sync>;

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
/// `StreamTitle`, handing each change to the caller's [`TitleSink`].
/// Streams that don't carry ICY (servers that ignore the header) set
/// `metaint = 0` and the read path is a plain passthrough.
pub struct HttpMediaSource {
    /// Buffered reader over the response body. Wrapped in a `Mutex` to
    /// upgrade `reqwest::blocking::Response` from `Send` to `Send + Sync`
    /// (required by [`MediaSource`]). Replaced wholesale on a seek, when a
    /// fresh ranged GET reopens the body at the target byte.
    inner: Mutex<BufReader<Response>>,
    /// Cached origin URL — reused to reissue ranged GETs on a seek, and in
    /// error messages so a logged "read failed" line points at the stream.
    url: String,
    /// The client kept alive so a seek can reissue a ranged GET on the same
    /// connection pool / config as the initial open.
    client: Client,
    /// Current absolute byte offset into the body. Tracked on every read so
    /// a `SeekFrom::Current` resolves, and reset to the target on a seek.
    pos: u64,
    /// Total body length from `Content-Length`, when the server sent one.
    /// Enables `SeekFrom::End` and lets symphonia estimate a VBR duration.
    len: Option<u64>,
    /// Whether this stream can be seeked. True only for a finite,
    /// range-capable file (a remote-queue track) — never for live radio,
    /// whose bytes are produced on the wire and have no rewind.
    seekable: bool,
    /// Bytes of audio between two ICY metadata blocks. `0` disables ICY
    /// handling entirely (no metadata interleaved → plain passthrough).
    metaint: usize,
    /// Audio bytes still owed before the next metadata block. Starts at
    /// `metaint`; decremented as audio is read, and when it hits `0` the
    /// read path consumes one metadata block before resuming.
    bytes_until_meta: usize,
    /// `Some` only when ICY is active: where title changes go.
    on_title: Option<TitleSink>,
    /// Last `StreamTitle` handed on, so an unchanged metadata block
    /// (radio servers resend the same title every interval) is not handed
    /// on again.
    last_title: Option<String>,
    /// `icy-br`, the bitrate the station says it sends, in kbit/s.
    icy_bitrate_kbps: Option<u32>,
    /// The answer's `Content-Type`, for a caller to tell a playlist (HLS)
    /// behind a URL that does not say so.
    content_type: Option<String>,
    /// Writes what this source reads into the on-disk stream cache, when the
    /// caller asked for it and the body declared a length. `None` for radio
    /// (endless, so never complete) and whenever the cache is unusable — it
    /// is an optimisation, and its absence is never a playback failure.
    cache: Option<crate::audio::stream_cache::CacheWriter>,
    /// Parked at or past the end of the body, with no open response.
    ///
    /// Seeking to the length of a file is a legal position — it is where a
    /// reader lands to discover there is nothing left — but it cannot be
    /// asked for over HTTP: `Range: bytes={len}-` has no satisfiable byte
    /// and RFC 9110 obliges the server to answer 416. So the position is
    /// held here instead of on the wire, and `read` reports the EOF that a
    /// local file would have reported. Cleared by the next seek that lands
    /// inside the body.
    ///
    /// Not a corner case: symphonia seeks to the end of an MP4 to find a
    /// `moov` atom the file kept there, so without this every AAC track
    /// streamed from a server fails to open while an MP3 plays fine.
    at_eof: bool,
}

impl HttpMediaSource {
    /// Open a streaming HTTP GET on `url` without ICY metadata. The
    /// response is checked for success status before being wrapped; a
    /// 404 / 502 surfaces as `Err` instead of producing a `MediaSource`
    /// that would only fail at the first `probe()` call.
    pub fn open(url: &str) -> Result<Self, String> {
        Self::open_inner(url, None, false, None)
    }

    /// Open a streaming HTTP GET that also requests + de-interleaves ICY
    /// metadata, handing every change of the live `StreamTitle` to
    /// `on_title`. Falls back transparently to passthrough when the server
    /// ignores `Icy-MetaData: 1`.
    pub fn open_with_icy(url: &str, on_title: TitleSink) -> Result<Self, String> {
        Self::open_inner(url, Some(on_title), false, None)
    }

    /// Open a finite, range-capable file for **seekable** playback — a
    /// remote-queue track streamed from the server. No ICY (a file carries
    /// none); the source advertises `is_seekable()` when the server sent
    /// `Accept-Ranges: bytes` and a length, so the PlayerBar scrubber can
    /// drive a real `format.seek`. Degrades to forward-only if the server
    /// doesn't answer ranges — playback still works, only scrubbing won't.
    pub fn open_seekable(url: &str) -> Result<Self, String> {
        Self::open_inner(url, None, true, None)
    }

    /// [`Self::open_seekable`], additionally filling the on-disk stream cache
    /// from the bytes playback reads.
    ///
    /// Nothing extra is fetched and nothing is delayed: the cache is written
    /// from the blocks the decoder was going to read anyway, at their absolute
    /// offsets, so a seek leaves a hole rather than corrupting the file and
    /// the entry is simply never published.
    pub fn open_seekable_caching(
        url: &str,
        target: crate::audio::stream_cache::CacheTarget,
    ) -> Result<Self, String> {
        Self::open_inner(url, None, true, Some(target))
    }

    fn open_inner(
        url: &str,
        on_title: Option<TitleSink>,
        want_seek: bool,
        cache_target: Option<crate::audio::stream_cache::CacheTarget>,
    ) -> Result<Self, String> {
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
        let want_icy = on_title.is_some();
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "audio/*,*/*;q=0.8")
            .header("Icy-MetaData", if want_icy { "1" } else { "0" })
            .send()
            // `describe_http_error`: reqwest prints the request URL in its
            // error, which would hand the caller's log the ticket, the
            // userinfo and the `?key=` that `redact_url` exists to keep out
            // of it; the cause (refused, DNS, timeout) is kept.
            .map_err(|e| format!("http get: {}", describe_http_error(&e)))?;

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

        // Seekability is a property of a finite, range-capable body. Only
        // consider it when the caller asked (a remote file, never radio),
        // the server advertised byte ranges, gave a length, and isn't
        // interleaving metadata (ICY and seeking are mutually exclusive).
        let len = response.content_length();
        let accepts_ranges = response
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase().contains("bytes"))
            .unwrap_or(false);
        let seekable = want_seek && metaint == 0 && accepts_ranges && len.is_some();
        // Keep a length only for a seekable file. Radio must keep answering
        // `byte_len() == None` exactly as before, even on the rare stream
        // that sends a Content-Length, so symphonia never treats it as
        // finite.
        // Two different questions were sharing one variable. Seeking needs a
        // length AND ranges; caching needs only a length, because it is filled
        // by reading forward. Collapsing them meant a server that sends
        // `Content-Length` without `Accept-Ranges` — perfectly cacheable —
        // was never cached at all.
        let declared_len = len;
        let len = if seekable { len } else { None };
        // `icy-br` is sometimes a list ("128,128" on a dual-rate mount);
        // the first is the stream's. Anything outside what a station can
        // send is ignored rather than trusted.
        let icy_bitrate_kbps = response
            .headers()
            .get("icy-br")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|kbps| (8..=1_536).contains(kbps));
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        Ok(Self {
            inner: Mutex::new(BufReader::with_capacity(8 * 1024, response)),
            url: url.to_string(),
            client,
            pos: 0,
            len,
            seekable,
            metaint,
            bytes_until_meta: metaint,
            // Drop the sink when the server isn't interleaving — keeps
            // the read path's `Some` check honest (ICY active iff we'll
            // actually parse blocks).
            on_title: if metaint > 0 { on_title } else { None },
            last_title: None,
            icy_bitrate_kbps,
            content_type,
            // Only a body that declared its length can be cached: without one
            // there is no way to decide that every byte has been covered, and
            // a file we cannot call complete must never be offered as one.
            cache: cache_target.and_then(|target| {
                declared_len.and_then(|len| {
                    crate::audio::stream_cache::CacheWriter::create(&target.dir, &target.name, len)
                })
            }),
            at_eof: false,
        })
    }

    /// Origin URL for diagnostics.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The bitrate the station declares (`icy-br`), in kbit/s.
    pub fn icy_bitrate_kbps(&self) -> Option<u32> {
        self.icy_bitrate_kbps
    }

    /// The answer's `Content-Type`, as sent.
    pub fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
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
    let end = rest
        .find("';")
        .unwrap_or_else(|| rest.find('\0').unwrap_or(rest.len()));
    let title = rest[..end].trim().trim_matches('\0').trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}

/// Parse the start byte out of a `Content-Range: bytes <start>-<end>/<total>`
/// header value. Returns `None` for any shape we don't recognize — an
/// unsatisfied-range `bytes */<total>`, a unit other than `bytes`, or
/// garbage — so the caller treats an unverifiable range as a failure
/// rather than trusting it.
fn parse_content_range_start(value: &str) -> Option<u64> {
    // The range unit is case-insensitive per HTTP — accept "bytes",
    // "Bytes", "BYTES" — then a single space before the range.
    let (unit, rest) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    // Exactly "<range>/<total>": one slash, total required (a 206 always
    // sends it). A missing or extra slash is malformed.
    let (range, total) = rest.split_once('/')?;
    if total.contains('/') {
        return None;
    }
    // Total is a byte count, or "*" when the full length is unknown.
    let total = match total.trim() {
        "*" => None,
        other => Some(other.parse::<u64>().ok()?),
    };
    // A 206 carries BOTH bounds — an open-ended "<start>-" is only valid in
    // a *request* Range. Require both, reject a start past the end, and
    // reject a range that reaches or passes the total (end is inclusive, so
    // the last valid byte is total-1).
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if start > end {
        return None;
    }
    if total.is_some_and(|total| end >= total) {
        return None;
    }
    Some(start)
}

impl Read for HttpMediaSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Parked past the end by a seek that was never sent (see `at_eof`):
        // the body still open is the one from before that seek, so reading it
        // would hand back bytes from the wrong offset. EOF is the honest
        // answer, and the one a local file gives.
        if self.at_eof {
            return Ok(0);
        }
        // `Mutex::lock` only fails on poisoning; we never panic inside
        // a held guard so treat poisoning as a fatal I/O error rather
        // than silently swallowing it.
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("http source mutex poisoned"))?;

        // Passthrough fast path: no ICY interleaving on this stream. This
        // is also the only path a seekable source takes, so advance the
        // byte cursor here for `SeekFrom::Current`.
        if self.metaint == 0 {
            let n = guard.read(buf)?;
            drop(guard);
            let start = self.pos;
            self.pos += n as u64;
            // Record where these bytes live, not merely that they arrived: a
            // seek reopens the body at another offset, so an append would
            // interleave two regions into one corrupt file.
            if n > 0 {
                if let Some(cache) = self.cache.as_mut() {
                    cache.write_at(start, &buf[..n]);
                }
            }
            return Ok(n);
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

        // Handed on outside the lock. Only when the title actually changed —
        // servers resend the same `StreamTitle` every interval.
        if let Some(parsed) = new_title {
            if parsed != self.last_title {
                self.last_title = parsed.clone();
                if let Some(on_title) = self.on_title.as_mut() {
                    on_title(parsed);
                }
            }
        }

        Ok(n)
    }
}

impl Seek for HttpMediaSource {
    /// Seek a finite, range-capable body by reopening it at the target byte
    /// with a `Range` GET and swapping the reader. Live radio isn't
    /// seekable, so it keeps returning `Unsupported` — symphonia checks
    /// `is_seekable()` first, so this only fires for a remote file.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if !self.seekable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "live HTTP stream is not seekable",
            ));
        }
        // Honour the process-wide offline switch: a seek reopens a socket.
        if crate::offline::is_offline() {
            return Err(io::Error::other("offline mode is enabled"));
        }

        let target = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(delta) => (self.pos as i64 + delta).max(0) as u64,
            SeekFrom::End(delta) => {
                let len = self
                    .len
                    .ok_or_else(|| io::Error::other("seek from end: unknown length"))?;
                (len as i64 + delta).max(0) as u64
            }
        };

        // Landing at or past the end is legal, and it is exactly where
        // symphonia goes to read an MP4's trailing `moov` atom. There is no
        // request to make for it — `bytes={len}-` is unsatisfiable by
        // definition — so record the position and let `read` answer EOF.
        if let Some(len) = self.len {
            if target >= len {
                self.pos = target;
                self.at_eof = true;
                return Ok(target);
            }
        }

        let response = self
            .client
            .get(&self.url)
            .header(reqwest::header::ACCEPT, "audio/*,*/*;q=0.8")
            .header(reqwest::header::RANGE, format!("bytes={target}-"))
            .send()
            // Without the URL, for the same reason as the first GET: this
            // error travels up through symphonia into a logged seek failure.
            .map_err(|e| io::Error::other(format!("range get: {}", describe_http_error(&e))))?;
        // 206 = ranged answer; a 200 means the server ignored the range and
        // is streaming from 0, which would silently corrupt the position —
        // refuse it rather than play the wrong bytes.
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(io::Error::other(format!(
                "range not honoured: HTTP {}",
                response.status().as_u16()
            )));
        }
        // A 206 must actually start at the byte we asked for. A server that
        // answers 206 from a different offset — or with a malformed or
        // absent Content-Range — would desync `pos` and corrupt the decode
        // just as a bare 200 would, so verify the range's start byte.
        match response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_start)
        {
            Some(start) if start == target => {}
            Some(start) => {
                return Err(io::Error::other(format!(
                    "range start mismatch: asked for byte {target}, got {start}"
                )));
            }
            None => {
                return Err(io::Error::other(
                    "206 response without a usable Content-Range header",
                ));
            }
        }

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("http source mutex poisoned"))?;
        *guard = BufReader::with_capacity(8 * 1024, response);
        drop(guard);
        self.pos = target;
        // Back inside the body, with a response open at `target`.
        self.at_eof = false;
        Ok(target)
    }
}

impl MediaSource for HttpMediaSource {
    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

#[cfg(test)]
mod tests;
