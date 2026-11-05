//! An enclosure read for the decoder, under the rules of [`super::fetch`].
//!
//! `audio::http_source` lets its client follow redirects and logs its address
//! through `redact_url`. Both are fine for a station a person picked and neither
//! is for an address out of a feed, so an episode has this instead: every request
//! goes through [`Fetcher::open`], which judges each hop before contacting it, and
//! what fails is one word from [`FetchFailure::code`]. Nothing in this file holds
//! an address in a message.
//!
//! ## On the decoder's thread
//!
//! The decoder reads on the thread it takes its commands on, and symphonia wants
//! a blocking `Read`. So a task pulls the body into a short queue, which is also
//! what makes a stall something that can be waited on, given up on and let go of,
//! rather than a read the whole player sits inside:
//!
//! - a read that gets nothing for [`STALL`] fails with `unreached`;
//! - a Stop or a load queued for the decoder calls [`release_starved_reader`], and
//!   a read that has been waiting for [`STALLED_AFTER`] then lets go, as a read on
//!   a network share does. A younger one is left alone: the command may yet be
//!   dropped as stale, and a healthy stream must survive that.
//!
//! The queue is short on purpose. Pulling the whole file ahead would spend a
//! listener's data on an hour they may never hear.
//!
//! ## Seeking
//!
//! A seek is a new `Range` request. It starts from where the last one landed, so
//! a measurement service in front of the file is not told about every jump, and
//! falls back to the enclosure's own address when that has expired. Every hop of
//! either is judged the same way. A jump forward this short is read through
//! instead ([`READ_THROUGH`]): a reopen costs a handshake.
//!
//! A long pause outlives the connection, since a host drops one that sends nothing
//! for a while. The read that finds that out reopens where it stopped.
//!
//! ## An address that does not last
//!
//! A bound server's episode has no enclosure of its own to fall back on, only a ticket
//! good for about an hour, which an episode easily is. So it opens from a [`Renew`],
//! and a request that answers 404 asks for another once and tries that. A second 404
//! is a real one.

use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_RANGES, CONTENT_RANGE, CONTENT_TYPE};
use reqwest::{Response, StatusCode};
use symphonia::core::io::MediaSource;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use url::Url;
use waveflow_core::playback::read_ahead::READ_RELEASED;
use waveflow_core::podcast::FetchFailure;

use super::fetch::{FetchError, Fetcher, Origin};

/// The whole of one opening, every hop included.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);

/// How long a read waits for bytes before it counts the host as gone.
const STALL: Duration = Duration::from_secs(30);

/// How long a read has to have been waiting for a command to let go of it.
const STALLED_AFTER: Duration = Duration::from_secs(1);

/// How often a waiting read looks whether it has been let go.
const WATCH: Duration = Duration::from_millis(100);

/// What is held ahead of the decoder, in the host's own chunks.
const QUEUED_CHUNKS: usize = 64;

/// A jump forward at most this long is read through rather than asked for.
const READ_THROUGH: u64 = 256 * 1024;

/// How many times in a row a body that broke off is reopened without a byte
/// coming of it, before the read gives up.
const MAX_REOPENS: u32 = 3;

/// Why an opening produced nothing. The message of each is the word after
/// `podcast: `, which is all a log or the webview ever gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamRefusal {
    /// `podcasts.enabled` is off. Nothing was contacted.
    Disabled,
    /// Offline mode is on. Nothing was contacted.
    Offline,
    Failed(FetchFailure),
    /// A bound server holds no file of this episode: it was never downloaded there,
    /// or it was deleted since.
    #[cfg_attr(not(feature = "sync_v2"), allow(dead_code))]
    NotStored,
    /// A bound server refused the account's token: it has to sign in again.
    #[cfg_attr(not(feature = "sync_v2"), allow(dead_code))]
    SignedOut,
    /// A command replaces this stream and it was still waiting.
    Released,
}

impl From<FetchError> for StreamRefusal {
    fn from(err: FetchError) -> Self {
        match err {
            FetchError::Offline => Self::Offline,
            FetchError::Failed(failure) => Self::Failed(failure),
        }
    }
}

impl StreamRefusal {
    /// What the decoder is told. [`READ_RELEASED`] is the phrase it knows to
    /// keep quiet about.
    pub(crate) fn message(self) -> String {
        match self {
            Self::Disabled => "podcast: disabled".to_owned(),
            Self::Offline => "podcast: offline".to_owned(),
            Self::Failed(failure) => format!("podcast: {}", failure.code()),
            Self::NotStored => "podcast: not_stored".to_owned(),
            Self::SignedOut => "podcast: server_signed_out".to_owned(),
            Self::Released => READ_RELEASED.to_owned(),
        }
    }

    fn into_io(self) -> io::Error {
        let kind = match self {
            Self::Released => io::ErrorKind::ConnectionAborted,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(kind, self.message())
    }
}

/// Asked before every request, so a switch turned off, or offline mode, holds for
/// a seek as it does for the first byte.
pub(crate) type Gate =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), StreamRefusal>> + Send>> + Send + Sync>;

/// Mints a fresh address for what plays, when the one held no longer answers.
#[cfg_attr(not(feature = "sync_v2"), allow(dead_code))]
pub(crate) type Renew =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Url, StreamRefusal>> + Send>> + Send + Sync>;

/// Lets a command let go of a stream from another thread.
#[derive(Default)]
pub(crate) struct ReadGuard {
    /// A command asked. Forgotten when the read it found ends: it was healthy
    /// after all.
    requested: AtomicBool,
    /// For good: the decoder may make a few more reads before it gets to the
    /// command, and each would otherwise wait out the stall again.
    released: AtomicBool,
}

impl ReadGuard {
    /// A command replaces what plays. See the module doc.
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }

    fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    /// Resolves once a command has asked and this wait is old enough to call
    /// stalled. Never resolves otherwise, so it is one arm of a `select!`.
    async fn let_go(&self, since: Instant) {
        loop {
            if self.is_released() {
                return;
            }
            if self.requested.load(Ordering::Acquire) && since.elapsed() >= STALLED_AFTER {
                self.released.store(true, Ordering::Release);
                return;
            }
            tokio::time::sleep(WATCH).await;
        }
    }
}

/// The stream the decoder may be blocked on. One at a time: one decoder.
static CURRENT: Mutex<Option<Weak<ReadGuard>>> = Mutex::new(None);

fn set_current(guard: &Arc<ReadGuard>) {
    if let Ok(mut current) = CURRENT.lock() {
        *current = Some(Arc::downgrade(guard));
    }
}

/// Called once a command that replaces or stops what plays is queued for the
/// decoder, beside the same call for a live station and a network share.
pub fn release_starved_reader() {
    let guard = CURRENT
        .lock()
        .ok()
        .and_then(|current| current.as_ref().and_then(Weak::upgrade));
    if let Some(guard) = guard {
        guard.request();
    }
}

/// `Content-Range: bytes <start>-<end>/<total>` as a start and, unless it is `*`,
/// a total. Strictness is [`parse_content_range_start`]'s.
///
/// [`parse_content_range_start`]: crate::audio::http_source::parse_content_range_start
fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let start = crate::audio::http_source::parse_content_range_start(value)?;
    let total = value.rsplit('/').next()?.trim().parse().ok();
    Some((start, total))
}

fn request_headers(range_from: u64) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("audio/*,*/*;q=0.8"));
    if let Ok(range) = HeaderValue::from_str(&format!("bytes={range_from}-")) {
        headers.insert(reqwest::header::RANGE, range);
    }
    headers
}

/// Copies a body into the queue, and stops when nobody is reading it. A failure is
/// told as `Err(())`: a `reqwest::Error` quotes the address, so it goes no further.
///
/// Offline mode turned on stops the transfer where it is, as it stops everything
/// else. The read that finds out asks to reopen, and that is refused in its turn.
async fn pump(
    mut response: Response,
    chunks: mpsc::Sender<Result<Vec<u8>, ()>>,
    offline: fn() -> bool,
) {
    loop {
        if offline() {
            let _ = chunks.send(Err(())).await;
            return;
        }
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if chunks.send(Ok(chunk.to_vec())).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(_) => {
                let _ = chunks.send(Err(())).await;
                return;
            }
        }
    }
}

/// A body being read, or nothing once it ended or was parked past the end.
struct Body {
    chunks: mpsc::Receiver<Result<Vec<u8>, ()>>,
    task: JoinHandle<()>,
}

impl Drop for Body {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What one wait on the queue came to.
enum Next {
    Chunk(Vec<u8>),
    /// The body ended where the host ended it.
    Ended,
    /// The connection broke, or nothing came for [`STALL`].
    Broken,
    Released,
}

/// What the first request of a stream asks for.
enum Start {
    At(Url),
    Renewing(Renew),
}

/// A seekable [`MediaSource`] over an enclosure.
pub(crate) struct EnclosureSource {
    runtime: Handle,
    fetcher: Fetcher,
    gate: Gate,
    origin: Origin,
    /// The address out of the feed.
    enclosure: Url,
    /// Where the last request landed, which a seek tries first.
    landed: Url,
    /// How to get another address, when this one is made to run out.
    renew: Option<Renew>,
    guard: Arc<ReadGuard>,
    body: Option<Body>,
    /// What is left of the chunk being read.
    chunk: Vec<u8>,
    chunk_read: usize,
    pos: u64,
    len: Option<u64>,
    seekable: bool,
    /// How many reopens in a row have produced no byte.
    reopens: u32,
    content_type: Option<String>,
    /// Requests made so far, for a test to measure what a seek cost.
    #[cfg(test)]
    requests: u64,
}

impl EnclosureSource {
    /// Opens `enclosure` for `origin`. Blocking: for the decoder's thread, never
    /// for an async worker.
    ///
    /// Asks for a range from the first byte, since a `206` is the only proof a
    /// host can seek. Plenty do without saying `Accept-Ranges`, and some say it and
    /// do not.
    pub(crate) fn open(
        runtime: Handle,
        fetcher: Fetcher,
        gate: Gate,
        enclosure: Url,
        origin: Origin,
    ) -> Result<Self, StreamRefusal> {
        Self::open_from(runtime, fetcher, gate, Start::At(enclosure), origin)
    }

    /// [`Self::open`] for an address that does not last: `renew` gives the first one
    /// and any that come after. See the module doc.
    #[cfg_attr(not(feature = "sync_v2"), allow(dead_code))]
    pub(crate) fn open_renewing(
        runtime: Handle,
        fetcher: Fetcher,
        gate: Gate,
        renew: Renew,
        origin: Origin,
    ) -> Result<Self, StreamRefusal> {
        Self::open_from(runtime, fetcher, gate, Start::Renewing(renew), origin)
    }

    fn open_from(
        runtime: Handle,
        fetcher: Fetcher,
        gate: Gate,
        start: Start,
        origin: Origin,
    ) -> Result<Self, StreamRefusal> {
        let guard = Arc::new(ReadGuard::default());
        set_current(&guard);
        let started = Instant::now();
        let renew = match &start {
            Start::At(_) => None,
            Start::Renewing(renew) => Some(Arc::clone(renew)),
        };
        let (enclosure, response) = runtime.block_on(async {
            let open = async {
                gate().await?;
                let enclosure = match start {
                    Start::At(enclosure) => enclosure,
                    Start::Renewing(renew) => renew().await?,
                };
                let response = fetcher
                    .open(&enclosure, origin, request_headers(0))
                    .await
                    .map_err(StreamRefusal::from)?;
                Ok((enclosure, response))
            };
            tokio::select! {
                opened = tokio::time::timeout(OPEN_DEADLINE, open) => {
                    opened.unwrap_or(Err(StreamRefusal::Failed(FetchFailure::Unreached)))
                }
                () = guard.let_go(started) => Err(StreamRefusal::Released),
            }
        })?;
        guard.requested.store(false, Ordering::Release);

        // A feed names an MP3 and the host answers a web page: a login wall, an
        // expired link. Not a body to hand to a probe.
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if content_type
            .as_deref()
            .is_some_and(|value| value.trim_start().to_ascii_lowercase().starts_with("text/"))
        {
            return Err(StreamRefusal::Failed(FetchFailure::NotAudio));
        }

        let (seekable, len) = if response.status() == StatusCode::PARTIAL_CONTENT {
            match response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range)
            {
                Some((0, Some(total))) => (true, Some(total)),
                // A range it cannot total is a body with no known end.
                Some((0, None)) => (false, None),
                // Not the bytes it was asked for, and no reason to trust the rest.
                _ => {
                    return Err(StreamRefusal::Failed(FetchFailure::Status(
                        StatusCode::PARTIAL_CONTENT.as_u16(),
                    )))
                }
            }
        } else {
            let says_ranges = response
                .headers()
                .get(ACCEPT_RANGES)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().contains("bytes"));
            let len = response.content_length();
            (says_ranges && len.is_some(), len)
        };

        let landed = response.url().clone();
        let mut source = Self {
            runtime,
            fetcher,
            gate,
            origin,
            enclosure,
            landed,
            renew,
            guard,
            body: None,
            chunk: Vec::new(),
            chunk_read: 0,
            pos: 0,
            len: if seekable { len } else { None },
            seekable,
            reopens: 0,
            content_type,
            #[cfg(test)]
            requests: 1,
        };
        source.start_body(response);
        Ok(source)
    }

    /// The answer's `Content-Type`, which picks the probe's hint.
    pub(crate) fn content_type(&self) -> Option<&str> {
        self.content_type.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn requests(&self) -> u64 {
        self.requests
    }

    /// The handle [`release_starved_reader`] reaches the current stream through.
    /// Tests run side by side, and the current stream may be another test's.
    #[cfg(test)]
    pub(crate) fn release_handle(&self) -> Arc<ReadGuard> {
        Arc::clone(&self.guard)
    }

    fn start_body(&mut self, response: Response) {
        let (sender, chunks) = mpsc::channel(QUEUED_CHUNKS);
        let task = self
            .runtime
            .spawn(pump(response, sender, self.fetcher.offline_check()));
        self.body = Some(Body { chunks, task });
        self.chunk.clear();
        self.chunk_read = 0;
    }

    /// Asks for the body from `target` on. The stream that plays stays as it is
    /// unless this works: a refused seek does not cost the listener the episode.
    fn reopen(&mut self, target: u64) -> Result<(), StreamRefusal> {
        let started = Instant::now();
        #[cfg(test)]
        {
            self.requests += 1;
        }
        let (answer, renewed) = self.runtime.block_on(async {
            let ask = async {
                (self.gate)().await?;
                let asked = match self
                    .fetcher
                    .open(&self.landed, self.origin, request_headers(target))
                    .await
                {
                    Ok(response) => Ok(response),
                    // Expired, or anything else that is not offline mode: a signed
                    // address does not last, the one in the feed does.
                    Err(FetchError::Failed(_)) if self.landed != self.enclosure => self
                        .fetcher
                        .open(&self.enclosure, self.origin, request_headers(target))
                        .await
                        .map_err(StreamRefusal::from),
                    Err(err) => Err(StreamRefusal::from(err)),
                };
                match (asked, &self.renew) {
                    // Nothing here tells an expired address from a missing file. What
                    // mints the next one does.
                    (Err(StreamRefusal::Failed(FetchFailure::Status(404))), Some(renew)) => {
                        let fresh = renew().await?;
                        let response = self
                            .fetcher
                            .open(&fresh, self.origin, request_headers(target))
                            .await
                            .map_err(StreamRefusal::from)?;
                        Ok((response, Some(fresh)))
                    }
                    (asked, _) => asked.map(|response| (response, None)),
                }
            };
            tokio::select! {
                answer = tokio::time::timeout(OPEN_DEADLINE, ask) => {
                    answer.unwrap_or(Err(StreamRefusal::Failed(FetchFailure::Unreached)))
                }
                () = self.guard.let_go(started) => Err(StreamRefusal::Released),
            }
        })?;
        if let Some(fresh) = renewed {
            self.enclosure = fresh;
        }
        // A 200 is the file from its first byte, and a 206 from anywhere else is
        // somebody else's bytes: either would play the wrong place.
        let starts_there = answer.status() == StatusCode::PARTIAL_CONTENT
            && answer
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range)
                .is_some_and(|(start, _)| start == target);
        if !starts_there {
            return Err(StreamRefusal::Failed(FetchFailure::Status(
                answer.status().as_u16(),
            )));
        }
        self.landed = answer.url().clone();
        self.start_body(answer);
        self.pos = target;
        Ok(())
    }

    fn next_chunk(&mut self) -> Next {
        let Some(body) = self.body.as_mut() else {
            return Next::Ended;
        };
        let guard = Arc::clone(&self.guard);
        let started = Instant::now();
        let next = self.runtime.block_on(async {
            tokio::select! {
                chunk = body.chunks.recv() => match chunk {
                    Some(Ok(chunk)) => Next::Chunk(chunk),
                    Some(Err(())) => Next::Broken,
                    None => Next::Ended,
                },
                () = tokio::time::sleep(STALL) => Next::Broken,
                () = guard.let_go(started) => Next::Released,
            }
        });
        // Whatever a command asked, this read is over: only one still waiting
        // when the next asks is stalled.
        self.guard.requested.store(false, Ordering::Release);
        next
    }

    /// Takes in the next chunk: `Ok(true)` when there is one, `Ok(false)` at the
    /// end of the body. A body that stops short of its length is a connection that
    /// went away, and is taken up again where it stopped.
    fn fill(&mut self) -> io::Result<bool> {
        loop {
            let broke = match self.next_chunk() {
                Next::Chunk(chunk) if chunk.is_empty() => continue,
                Next::Chunk(chunk) => {
                    self.chunk = chunk;
                    self.chunk_read = 0;
                    self.reopens = 0;
                    return Ok(true);
                }
                Next::Released => return Err(StreamRefusal::Released.into_io()),
                Next::Ended => {
                    if !self.seekable || self.len.is_some_and(|len| self.pos >= len) {
                        return Ok(false);
                    }
                    false
                }
                Next::Broken => true,
            };
            if !self.seekable && !broke {
                return Ok(false);
            }
            if !self.seekable || self.reopens >= MAX_REOPENS {
                return Err(StreamRefusal::Failed(FetchFailure::Unreached).into_io());
            }
            self.reopens += 1;
            self.reopen(self.pos).map_err(StreamRefusal::into_io)?;
        }
    }

    /// Reads and drops `count` bytes.
    fn read_through(&mut self, mut count: u64) -> io::Result<()> {
        while count > 0 {
            if self.chunk_read >= self.chunk.len() && !self.fill()? {
                return Ok(());
            }
            let held = (self.chunk.len() - self.chunk_read) as u64;
            let taken = held.min(count);
            self.chunk_read += taken as usize;
            self.pos += taken;
            count -= taken;
        }
        Ok(())
    }
}

impl Read for EnclosureSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.chunk_read >= self.chunk.len() && !self.fill()? {
            return Ok(0);
        }
        let held = &self.chunk[self.chunk_read..];
        let n = held.len().min(buf.len());
        buf[..n].copy_from_slice(&held[..n]);
        self.chunk_read += n;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for EnclosureSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        if !self.seekable {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "podcast: not_seekable",
            ));
        }
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            SeekFrom::End(delta) => self.len.and_then(|len| len.checked_add_signed(delta)),
        }
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if target == self.pos {
            return Ok(target);
        }
        // The end of the file is a legal place to land, where a reader goes to find
        // out there is nothing left and an MP4 keeps its index, but no request can
        // ask for it: `bytes=<len>-` has no byte to answer with.
        if self.len.is_some_and(|len| target >= len) {
            self.body = None;
            self.chunk.clear();
            self.chunk_read = 0;
            self.pos = target;
            return Ok(target);
        }
        if target > self.pos && target - self.pos <= READ_THROUGH && self.body.is_some() {
            self.read_through(target - self.pos)?;
            return Ok(target);
        }
        self.reopen(target).map_err(StreamRefusal::into_io)?;
        Ok(target)
    }
}

impl MediaSource for EnclosureSource {
    fn is_seekable(&self) -> bool {
        self.seekable
    }

    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

/// What the probe is told a body holds, from its `Content-Type` or else what the
/// feed declared. Symphonia finds the format without one, later.
pub(crate) fn ext_hint(content_type: Option<&str>) -> Option<&'static str> {
    let essence = content_type?.split(';').next()?.trim().to_ascii_lowercase();
    match essence.as_str() {
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" => Some("mp3"),
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" | "audio/aac" | "audio/mp4a-latm" => Some("m4a"),
        "audio/ogg" | "audio/vorbis" | "application/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/wav" | "audio/x-wav" | "audio/wave" => Some("wav"),
        _ => None,
    }
}
