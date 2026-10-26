//! Live radio, read ahead of the decoder into a time-shift buffer.
//!
//! A station is opened here, on the decoder thread, then read by a thread
//! of its own ("radio-ingest") into a [`Buffer`]; the decoder reads the
//! buffer through a [`LiveSource`]. So a pause stops the decoder and not
//! the stream: when it ends, playback goes on from where it stopped, behind
//! the live edge, for up to [`waveflow_core::playback::timeshift::SPAN`].
//! Going back to live is playing the station again — a new load, which
//! drops this buffer and its thread with the stream it replaces.
//!
//! Two kinds of station:
//!
//! - **A continuous stream** (Icecast, Shoutcast): one long GET with ICY
//!   metadata, read by [`HttpMediaSource`]. When it drops — the server
//!   closes it, the network blips — the ingest opens it again after a
//!   growing pause, a handful of times, and the new bytes follow the old
//!   ones: MP3 and ADTS resynchronise on the next frame.
//! - **HTTP Live Streaming**: a playlist of short segments, polled. Each
//!   segment's audio is taken out of its container (MPEG-TS, or ADTS / MP3
//!   behind an ID3 tag) and appended, so the decoder sees one ADTS or MP3
//!   stream. A multivariant playlist plays its best audio-only variant.
//!   Encrypted segments and fragmented MP4 are refused.
//!
//! Nothing here runs on the audio callback; the ingest thread and the
//! decoder thread may block, the callback never waits on either.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use symphonia::core::io::MediaSource;
use tauri::{AppHandle, Emitter};
use waveflow_core::playback::hls::playlist::{self as hls, HlsError, Playlist};
use waveflow_core::playback::hls::ts::{self, AudioKind, TsDemuxer};
use waveflow_core::playback::timeshift::{Buffer, End};

use super::events::{emit_radio_metadata, RadioMetadataPayload};
use super::http_source::{redact_url, HttpMediaSource};

/// The event the time-shift state travels on.
pub const EVENT_RADIO_TIMESHIFT: &str = "player:radio-timeshift";

/// How long an empty buffer is waited on at a time before the reader looks
/// again at whether it was told to stop.
const READ_WAIT: Duration = Duration::from_millis(500);

/// The pauses between attempts to reopen a dropped stream. Five tries over
/// half a minute: a blip is ridden out, a station that went away is let go.
const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
];

/// One network read on a continuous stream.
const READ_BLOCK: usize = 16 * 1024;

/// The most one HLS segment may weigh. A 10 s segment at 320 kbit/s is
/// 400 KB; the cap only keeps a server sending something else from
/// filling memory.
const MAX_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

/// The most an HLS playlist may weigh.
const MAX_PLAYLIST_BYTES: u64 = 1024 * 1024;

/// What a station is to the player: what to emit its titles with.
#[derive(Clone)]
pub struct Station {
    pub app: AppHandle,
    pub track_id: i64,
    /// The URL the station was played from — the favorite id is
    /// `url:<station_url>`, so it rides every emit to let the star save
    /// the station even after the now-playing line shows a song title.
    pub url: String,
    pub name: Option<String>,
    /// Kept as the artist line when a `StreamTitle` has no
    /// `Artist - Title` split (a bare show name).
    pub artist: Option<String>,
    pub artwork_url: Option<String>,
}

/// The time-shift state, as the player bar reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeshiftPayload {
    pub track_id: i64,
    /// How far behind live the listener is, in milliseconds.
    pub behind_ms: u64,
    /// How far behind live they can fall before the oldest audio goes.
    pub capacity_ms: Option<u64>,
}

/// The buffer the decoder is waiting on, if any — so a new load or a stop
/// can release a read that waits for a station still reconnecting, rather
/// than queue behind it for half a minute.
static WAITING: Mutex<Option<Weak<Buffer>>> = Mutex::new(None);

/// Release a read blocked on a starved live buffer. Called before a
/// command that replaces or stops what plays is sent to the decoder.
pub fn release_starved_reader() {
    let current = WAITING.lock().ok().and_then(|slot| slot.clone());
    if let Some(buffer) = current.and_then(|weak| weak.upgrade()) {
        buffer.stop_if_starved();
    }
}

/// The decoder's side of a live station.
pub struct LiveSource {
    buffer: Arc<Buffer>,
    station: Station,
    last_title: Option<String>,
}

impl LiveSource {
    fn new(buffer: Arc<Buffer>, station: Station) -> Self {
        if let Ok(mut slot) = WAITING.lock() {
            *slot = Some(Arc::downgrade(&buffer));
        }
        Self {
            buffer,
            station,
            last_title: None,
        }
    }
}

impl Drop for LiveSource {
    fn drop(&mut self) {
        // Whatever replaced this station, the ingest thread is told to let
        // the stream go; it notices on its next read.
        self.buffer.finish(End::Stopped);
    }
}

impl Read for LiveSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.buffer.read(buf, READ_WAIT) {
                Ok(n) => {
                    if let Some(title) = self.buffer.take_title() {
                        if title != self.last_title {
                            self.last_title = title.clone();
                            emit_now_playing(&self.station, title.as_deref());
                        }
                    }
                    return Ok(n);
                }
                // Nothing yet: a station reconnecting. Wait again — the
                // ingest ends the buffer when it gives up, and a stop
                // releases it through `release_starved_reader`.
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

impl Seek for LiveSource {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a live stream is not seekable",
        ))
    }
}

impl MediaSource for LiveSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// Split a raw `StreamTitle` into (title, artist) for the PlayerBar. The
/// near-universal radio convention is `Artist - Title`; without the
/// separator the whole string is the title and the station stands in for
/// the artist.
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

/// Emit `player:radio-metadata` with the song being heard. `raw` is the
/// `StreamTitle`, or `None` when the station cleared it (back to "just the
/// station"). Keeps the station artwork on every update and falls back to
/// the station name for the artist line.
fn emit_now_playing(station: &Station, raw: Option<&str>) {
    tracing::info!(
        track_id = station.track_id,
        stream_title = ?raw,
        "radio ICY now-playing"
    );
    let (title, artist) = match raw {
        Some(raw) => {
            let (title, artist) = split_artist_title(raw);
            (Some(title), artist.or_else(|| station.artist.clone()))
        }
        None => (None, station.artist.clone()),
    };
    emit_radio_metadata(
        &station.app,
        RadioMetadataPayload {
            track_id: station.track_id,
            title,
            artist,
            artwork_url: station.artwork_url.clone(),
            station_url: Some(station.url.clone()),
            station_name: station.name.clone(),
            station_artist: station.artist.clone(),
            station_artwork: station.artwork_url.clone(),
            // A live station: an open-ended timeline, no server track.
            is_remote: false,
            duration_ms: None,
            artwork_hash: None,
            remote_track_id: None,
            is_file: false,
            file_path: None,
        },
    );
}

/// Says how far behind live the listener is, at most once a second and
/// only when it changed by a second or more.
struct LagReporter {
    station: Station,
    last_sent: Option<(Instant, u64)>,
}

impl LagReporter {
    fn new(station: Station) -> Self {
        Self {
            station,
            last_sent: None,
        }
    }

    fn tick(&mut self, buffer: &Buffer) {
        let now = Instant::now();
        if let Some((at, _)) = self.last_sent {
            if now.duration_since(at) < Duration::from_secs(1) {
                return;
            }
        }
        let lag = buffer.lag();
        let behind_ms = lag.behind.as_millis() as u64;
        if let Some((_, previous)) = self.last_sent {
            if previous.abs_diff(behind_ms) < 1000 {
                return;
            }
        }
        self.last_sent = Some((now, behind_ms));
        let _ = self.station.app.emit(
            EVENT_RADIO_TIMESHIFT,
            TimeshiftPayload {
                track_id: self.station.track_id,
                behind_ms,
                capacity_ms: lag.capacity.map(|c| c.as_millis() as u64),
            },
        );
    }
}

/// Sleep `pause` in short steps, giving up early when the reader went
/// away. Returns whether the ingest should go on.
fn pause_unless_stopped(buffer: &Buffer, pause: Duration) -> bool {
    let until = Instant::now() + pause;
    while Instant::now() < until {
        if buffer.ended().is_some() || crate::offline::is_offline() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    buffer.ended().is_none()
}

/// Whether the URL or the answer says HLS.
fn names_hls(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.to_ascii_lowercase().ends_with(".m3u8")
}

fn is_mpegurl(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| {
        let ct = ct
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        matches!(
            ct.as_str(),
            "application/vnd.apple.mpegurl"
                | "application/x-mpegurl"
                | "audio/mpegurl"
                | "audio/x-mpegurl"
        )
    })
}

/// Open a live station. Returns the source to decode and the extension to
/// hint the probe with (an HLS stream is known only once its first segment
/// is in). Blocks until the station has answered, like opening a file.
pub fn open(url: &str, station: Station) -> Result<(LiveSource, Option<&'static str>), String> {
    if crate::offline::is_offline() {
        return Err("offline mode is enabled".to_string());
    }
    if names_hls(url) {
        return open_hls(url, station);
    }
    let buffer = Arc::new(Buffer::new());
    let source = HttpMediaSource::open_with_icy(url, title_sink(&buffer))?;
    if is_mpegurl(source.content_type()) {
        // An `.m3u8` behind a URL that does not say so.
        drop(source);
        return open_hls(url, station);
    }
    if let Some(kbps) = source.icy_bitrate_kbps() {
        buffer.set_byte_rate(f64::from(kbps) * 125.0);
    }
    let ingest_buffer = Arc::clone(&buffer);
    let ingest_station = station.clone();
    let url = url.to_string();
    std::thread::Builder::new()
        .name("radio-ingest".into())
        .spawn(move || continuous_ingest(source, url, ingest_buffer, ingest_station))
        .map_err(|e| format!("radio ingest thread: {e}"))?;
    Ok((LiveSource::new(buffer, station), None))
}

/// The callback that records ICY titles into the buffer, at the offset
/// they arrived at.
fn title_sink(buffer: &Arc<Buffer>) -> super::http_source::TitleSink {
    let buffer = Arc::clone(buffer);
    Box::new(move |title| buffer.push_title(title))
}

/// Measures a continuous stream's byte rate when the server does not say
/// it (`icy-br`): over the 20 s that follow its first 10, past the burst a
/// server sends on connect.
struct RateMeter {
    started: Instant,
    window: Option<(Instant, usize)>,
    done: bool,
}

impl RateMeter {
    fn new(known: bool) -> Self {
        Self {
            started: Instant::now(),
            window: None,
            done: known,
        }
    }

    fn record(&mut self, bytes: usize, buffer: &Buffer) {
        if self.done {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.started) < Duration::from_secs(10) {
            return;
        }
        let (since, total) = self.window.get_or_insert((now, 0));
        *total += bytes;
        let elapsed = now.duration_since(*since);
        if elapsed >= Duration::from_secs(20) {
            buffer.set_byte_rate(*total as f64 / elapsed.as_secs_f64());
            self.done = true;
        }
    }
}

fn continuous_ingest(
    mut source: HttpMediaSource,
    url: String,
    buffer: Arc<Buffer>,
    station: Station,
) {
    let redacted = redact_url(&url);
    let mut block = vec![0u8; READ_BLOCK];
    let mut lag = LagReporter::new(station);
    let mut rate = RateMeter::new(source.icy_bitrate_kbps().is_some());
    loop {
        if buffer.ended().is_some() {
            return;
        }
        let reason = match source.read(&mut block) {
            Ok(n) if n > 0 => {
                buffer.push(&block[..n]);
                rate.record(n, &buffer);
                lag.tick(&buffer);
                continue;
            }
            Ok(_) => "the station closed the stream".to_string(),
            Err(e) => e.to_string(),
        };
        if buffer.ended().is_some() {
            return;
        }
        tracing::warn!(url = %redacted, %reason, "radio stream dropped, reconnecting");
        match reconnect(&url, &buffer) {
            Some(reopened) => source = reopened,
            None => {
                tracing::warn!(url = %redacted, %reason, "radio stream lost");
                buffer.finish(End::Failed(reason));
                return;
            }
        }
    }
}

fn reconnect(url: &str, buffer: &Arc<Buffer>) -> Option<HttpMediaSource> {
    for pause in RECONNECT_BACKOFF {
        if !pause_unless_stopped(buffer, pause) {
            return None;
        }
        match HttpMediaSource::open_with_icy(url, title_sink(buffer)) {
            Ok(source) => {
                tracing::info!(url = %redact_url(url), "radio stream reopened");
                return Some(source);
            }
            Err(err) => {
                tracing::debug!(url = %redact_url(url), %err, "radio reconnect failed")
            }
        }
    }
    None
}

// ── HLS ─────────────────────────────────────────────────────────────

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(super::http_source::USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        // Every request here is a playlist or one segment: bounded, unlike
        // a continuous stream, so a total deadline is the right tool.
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("http client build: {e}"))
}

fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    cap: u64,
) -> Result<(String, Vec<u8>), String> {
    if crate::offline::is_offline() {
        return Err("offline mode is enabled".to_string());
    }
    let response = client.get(url).send().map_err(|e| {
        format!(
            "http get: {}",
            waveflow_core::error::describe_http_error(&e)
        )
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("http status {status}"));
    }
    let final_url = response.url().to_string();
    let mut body = Vec::new();
    response
        .take(cap + 1)
        .read_to_end(&mut body)
        .map_err(|e| format!("http read: {e}"))?;
    if body.len() as u64 > cap {
        return Err(format!("answer larger than {cap} bytes"));
    }
    Ok((final_url, body))
}

/// Resolve a playlist entry against the playlist's own URL.
fn resolve(base: &str, entry: &str) -> Result<String, String> {
    let base = url::Url::parse(base).map_err(|e| format!("playlist url: {e}"))?;
    let joined = base
        .join(entry)
        .map_err(|e| format!("playlist entry: {e}"))?;
    match joined.scheme() {
        "http" | "https" => Ok(joined.to_string()),
        other => Err(format!("unsupported scheme in a playlist: {other}")),
    }
}

fn hls_error(err: HlsError) -> String {
    format!("hls: {err}")
}

/// Fetch the media playlist `url` names, following a multivariant one to
/// the variant to play. Returns the media playlist's URL and contents.
fn media_playlist(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<(String, hls::MediaPlaylist), String> {
    let mut url = url.to_string();
    // A multivariant playlist names media playlists; one level is the
    // whole of HLS, a second would be a loop.
    for _ in 0..2 {
        let (final_url, body) = fetch(client, &url, MAX_PLAYLIST_BYTES)?;
        match hls::parse(&String::from_utf8_lossy(&body)).map_err(hls_error)? {
            Playlist::Media(list) => return Ok((final_url, list)),
            Playlist::Multivariant(list) => {
                let pick = list.pick().ok_or_else(|| hls_error(HlsError::Empty))?;
                url = resolve(&final_url, pick)?;
            }
        }
    }
    Err("hls: a multivariant playlist names another".to_string())
}

/// What the segments are, once the first one has been read.
enum Unpack {
    Transport(TsDemuxer),
    Packed,
}

impl Unpack {
    /// Append `segment`'s audio to `out`, and say what it is.
    fn unpack(&mut self, segment: &[u8], out: &mut Vec<u8>) -> Option<AudioKind> {
        match self {
            Unpack::Transport(demuxer) => {
                demuxer.feed(segment, out);
                demuxer.audio_kind()
            }
            Unpack::Packed => {
                let audio = ts::strip_id3(segment);
                out.extend_from_slice(audio);
                ts::packed_kind(audio)
            }
        }
    }
}

/// Where an HLS ingest stands.
struct HlsState {
    client: reqwest::blocking::Client,
    media_url: String,
    /// The next segment to fetch, by media sequence.
    next: u64,
    unpack: Unpack,
    /// Seconds of audio appended, and the bytes they took: the rate.
    seconds: f64,
    bytes: usize,
}

impl HlsState {
    /// Fetch and append every segment of `list` from `next` on. Returns
    /// how many were appended.
    fn take_segments(
        &mut self,
        list: &hls::MediaPlaylist,
        buffer: &Buffer,
    ) -> Result<usize, String> {
        let first = list
            .segments
            .first()
            .map(|s| s.sequence)
            .unwrap_or(self.next);
        if self.next < first {
            // Fell out of the window (a long stall): the audio in between
            // is gone, and the stream picks up at what the server still has.
            tracing::warn!(missed = first - self.next, "hls: segments missed");
            self.next = first;
        }
        let mut taken = 0;
        let mut audio = Vec::new();
        let from = self.next;
        for segment in list.segments.iter().filter(|s| s.sequence >= from) {
            if buffer.ended().is_some() {
                return Ok(taken);
            }
            let url = resolve(&self.media_url, &segment.uri)?;
            let (_, body) = fetch(&self.client, &url, MAX_SEGMENT_BYTES)?;
            audio.clear();
            if self.unpack.unpack(&body, &mut audio).is_none() {
                return Err("hls: a segment carries no audio WaveFlow can decode".into());
            }
            buffer.push(&audio);
            self.next = segment.sequence + 1;
            self.seconds += segment.duration_s;
            self.bytes += audio.len();
            if self.seconds > 0.0 {
                buffer.set_byte_rate(self.bytes as f64 / self.seconds);
            }
            taken += 1;
        }
        Ok(taken)
    }
}

fn open_hls(url: &str, station: Station) -> Result<(LiveSource, Option<&'static str>), String> {
    let client = http_client()?;
    let (media_url, list) = media_playlist(&client, url)?;
    // Three segments from the live edge, the distance the specification
    // asks a client to keep, so the first poll has something to add.
    let start = list
        .segments
        .len()
        .saturating_sub(3)
        .min(list.segments.len().saturating_sub(1));
    let first = list
        .segments
        .get(start)
        .ok_or_else(|| hls_error(HlsError::Empty))?
        .clone();

    // The first segment says what the stream is, which the decoder's
    // probe is hinted with.
    let (_, body) = fetch(
        &client,
        &resolve(&media_url, &first.uri)?,
        MAX_SEGMENT_BYTES,
    )?;
    let mut unpack = if ts::is_transport_stream(&body) {
        Unpack::Transport(TsDemuxer::new())
    } else {
        Unpack::Packed
    };
    let mut audio = Vec::new();
    let kind = unpack
        .unpack(&body, &mut audio)
        .ok_or_else(|| "hls: the stream carries no audio WaveFlow can decode".to_string())?;
    let buffer = Arc::new(Buffer::new());
    // A segment lands whole every target duration: that much more in hand
    // just after one arrived is not being behind live.
    buffer.set_slack(Duration::from_secs_f64(
        list.target_duration_s.clamp(1.0, 30.0) + 1.0,
    ));
    buffer.push(&audio);
    let mut state = HlsState {
        client,
        media_url,
        next: first.sequence + 1,
        unpack,
        seconds: first.duration_s,
        bytes: audio.len(),
    };
    if state.seconds > 0.0 {
        buffer.set_byte_rate(state.bytes as f64 / state.seconds);
    }
    tracing::info!(
        url = %redact_url(url),
        kind = kind.extension(),
        target_s = list.target_duration_s,
        "hls stream opened"
    );

    let ingest_buffer = Arc::clone(&buffer);
    let ingest_station = station.clone();
    let source_url = url.to_string();
    std::thread::Builder::new()
        .name("radio-ingest".into())
        .spawn(move || {
            // The rest of the window the first fetch already listed.
            let mut failures = 0usize;
            let mut lag = LagReporter::new(ingest_station);
            let mut pending = Some(list);
            loop {
                if ingest_buffer.ended().is_some() {
                    return;
                }
                let list = match pending.take() {
                    Some(list) => Ok(list),
                    None => media_playlist(&state.client, &state.media_url).map(|(_, l)| l),
                };
                let outcome = list.and_then(|list| {
                    let taken = state.take_segments(&list, &ingest_buffer)?;
                    Ok((list, taken))
                });
                lag.tick(&ingest_buffer);
                let pause = match outcome {
                    Ok((list, taken)) => {
                        failures = 0;
                        if list.ended && state.next > list.segments.last().map_or(0, |s| s.sequence)
                        {
                            ingest_buffer.finish(End::Finished);
                            return;
                        }
                        // The specification's reload interval: the target
                        // duration after a change, half of it when nothing
                        // new had appeared.
                        let target =
                            Duration::from_secs_f64(list.target_duration_s.clamp(1.0, 30.0));
                        if taken > 0 {
                            target
                        } else {
                            target / 2
                        }
                    }
                    Err(reason) => {
                        tracing::warn!(url = %redact_url(&source_url), %reason, "hls: poll failed");
                        let Some(pause) = RECONNECT_BACKOFF.get(failures).copied() else {
                            ingest_buffer.finish(End::Failed(reason));
                            return;
                        };
                        failures += 1;
                        pause
                    }
                };
                if !pause_unless_stopped(&ingest_buffer, pause) {
                    return;
                }
            }
        })
        .map_err(|e| format!("radio ingest thread: {e}"))?;
    Ok((LiveSource::new(buffer, station), Some(kind.extension())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_is_recognised_by_its_url_or_its_type() {
        assert!(names_hls("https://a/live/index.m3u8"));
        assert!(names_hls("https://a/live/INDEX.M3U8?token=x"));
        assert!(!names_hls("https://a/stream.mp3"));
        assert!(!names_hls("https://a/m3u8/stream"));
        assert!(is_mpegurl(Some(
            "application/vnd.apple.mpegurl; charset=utf-8"
        )));
        assert!(is_mpegurl(Some("audio/x-mpegurl")));
        assert!(!is_mpegurl(Some("audio/mpeg")));
        assert!(!is_mpegurl(None));
    }

    #[test]
    fn playlist_entries_resolve_against_the_playlist() {
        assert_eq!(
            resolve("https://cdn.example/live/hi/index.m3u8", "seg-1.ts").unwrap(),
            "https://cdn.example/live/hi/seg-1.ts"
        );
        assert_eq!(
            resolve("https://cdn.example/live/master.m3u8", "/abs/x.m3u8").unwrap(),
            "https://cdn.example/abs/x.m3u8"
        );
        assert!(resolve("https://cdn.example/a.m3u8", "file:///etc/passwd").is_err());
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
}
