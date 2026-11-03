//! A file on a share as a seekable reader, on a blocking thread.
//!
//! What lofty, the hash and a seek in playback all want. Bytes are held in a
//! few segments rather than a single window: a tag parse hops between the
//! head and the tail of a file, and a window would refetch at every hop.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

use super::fs::NetError;
use super::Source;

/// Bytes fetched by a read that finds nothing held.
const MISS_BYTES: u64 = 256 * 1024;

/// How many segments are kept. Enough for a head, a tail and the blocks of a
/// cover between them.
const MAX_SEGMENTS: usize = 16;

/// A read that held up this long counts as stalled, and may be let go by a
/// command that replaces what plays. A shorter one is a healthy seek, which a
/// stale load must not be able to kill.
const STALLED_AFTER: Duration = Duration::from_secs(1);

/// Lets a command let go of a [`RangeReader`] from another thread.
#[derive(Default)]
pub struct ReadGuard {
    released: AtomicBool,
    /// A command asked while a read was too young to call stalled. Forgotten
    /// when that read ends: it was healthy after all.
    requested: AtomicBool,
    reading_since: Mutex<Option<Instant>>,
}

impl ReadGuard {
    fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    /// Whether a read has been going on for long enough to be called stalled.
    pub fn is_stalled(&self) -> bool {
        self.reading_since
            .lock()
            .ok()
            .and_then(|since| *since)
            .is_some_and(|since| since.elapsed() >= STALLED_AFTER)
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    /// A command replaces what plays, and a read is going on that may yet
    /// turn out to be stalled.
    pub fn request(&self) {
        if self.reading_since.lock().is_ok_and(|since| since.is_some()) {
            self.requested.store(true, Ordering::Release);
        }
    }

    /// For good: see `ReadAheadRelease::release` for why not for one read.
    pub fn release(&self) {
        self.released.store(true, Ordering::Release);
    }

    fn reading(&self) -> Reading<'_> {
        if let Ok(mut since) = self.reading_since.lock() {
            *since = Some(Instant::now());
        }
        Reading(self)
    }
}

/// Marks a read as over however it ends.
struct Reading<'a>(&'a ReadGuard);

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        if let Ok(mut since) = self.0.reading_since.lock() {
            *since = None;
        }
        self.0.requested.store(false, Ordering::Release);
    }
}

/// How a read behaves when the share stops answering.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReadMode {
    /// A scan: fail, count the file, carry on or stop as the scan decides.
    Scan,
    /// Playback: fail at once when the source is known to be down, since the
    /// decoder thread is what is waiting, and let a newer command in.
    Playback,
}

pub struct RangeReader {
    source: Arc<Source>,
    path: String,
    len: u64,
    pos: u64,
    mode: ReadMode,
    guard: Arc<ReadGuard>,
    /// `(start, bytes)`, most recently used last.
    segments: Vec<(u64, Vec<u8>)>,
}

fn to_io(err: NetError) -> io::Error {
    let kind = match err {
        NetError::NotFound => io::ErrorKind::NotFound,
        NetError::Released => io::ErrorKind::ConnectionAborted,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, err.to_string())
}

impl RangeReader {
    pub fn new(source: Arc<Source>, path: String, len: u64, mode: ReadMode) -> Self {
        Self {
            source,
            path,
            len,
            pos: 0,
            mode,
            guard: Arc::default(),
            segments: Vec::new(),
        }
    }

    pub fn guard(&self) -> Arc<ReadGuard> {
        Arc::clone(&self.guard)
    }

    /// Fetch a range ahead of the reads that will want it, in one request.
    /// The hash asks for exactly two windows; fetching them block by block
    /// would cost several round trips per file for the same bytes.
    pub fn preload(&mut self, offset: u64, len: u64) -> io::Result<()> {
        if len == 0 || offset >= self.len {
            return Ok(());
        }
        let len = len.min(self.len - offset);
        let bytes = self.fetch(offset, len)?;
        self.hold(offset, bytes);
        Ok(())
    }

    fn fetch(&self, offset: u64, len: u64) -> io::Result<Vec<u8>> {
        let source = &self.source;
        let path = self.path.as_str();
        match self.mode {
            ReadMode::Scan => super::block_on(source.read_at(path, offset, len)).map_err(to_io),
            ReadMode::Playback => {
                if source.is_down() {
                    return Err(to_io(NetError::Unreachable(
                        "network source unreachable".into(),
                    )));
                }
                if self.guard.is_released() {
                    return Err(to_io(NetError::Released));
                }
                let _reading = self.guard.reading();
                super::block_on(async {
                    let read = source.read_patiently(path, offset, len, super::media::STALL);
                    tokio::pin!(read);
                    let stalled = tokio::time::sleep(STALLED_AFTER);
                    tokio::pin!(stalled);
                    loop {
                        tokio::select! {
                            result = &mut read => break result,
                            () = super::media::released() => {
                                if self.guard.is_released() {
                                    break Err(NetError::Released);
                                }
                            }
                            // A command that came before this read counted as
                            // stalled is honoured once it does.
                            () = &mut stalled, if !stalled.is_elapsed() => {
                                if self.guard.is_requested() {
                                    self.guard.release();
                                    break Err(NetError::Released);
                                }
                            }
                        }
                    }
                })
                .map_err(to_io)
            }
        }
    }

    fn hold(&mut self, start: u64, bytes: Vec<u8>) {
        if self.segments.len() >= MAX_SEGMENTS {
            self.segments.remove(0);
        }
        self.segments.push((start, bytes));
    }
}

impl Read for RangeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let pos = self.pos;
        let held = self
            .segments
            .iter()
            .position(|(start, bytes)| pos >= *start && pos < *start + bytes.len() as u64);
        let index = match held {
            Some(index) => index,
            None => {
                let want = MISS_BYTES.min(self.len - pos);
                let bytes = self.fetch(pos, want)?;
                if bytes.is_empty() {
                    // The file is shorter than it was listed as. A short read
                    // here would be taken for the end of the track.
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the file on the share is shorter than listed",
                    ));
                }
                self.hold(pos, bytes);
                self.segments.len() - 1
            }
        };
        let (start, bytes) = &self.segments[index];
        let from = (pos - start) as usize;
        let n = buf.len().min(bytes.len() - from);
        buf[..n].copy_from_slice(&bytes[from..from + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for RangeReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        let target = target.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )
        })?;
        self.pos = target;
        Ok(target)
    }
}

impl MediaSource for RangeReader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}
