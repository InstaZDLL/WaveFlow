//! A file pulled into memory in the background while it is decoded.
//!
//! A track on a network share stutters when the decoder reads it in many
//! small reads and seeks over a slow link, so it is read into RAM in large
//! sequential reads instead. Reading it whole before decoding started froze
//! the decoder thread for the length of the transfer: a NAS over Wi-Fi took
//! seconds for one FLAC, the next track was opened that way a few seconds
//! before the current one ended, and with about a second of audio queued
//! the current track cut out while stop and seek waited.
//!
//! Here a thread fills the buffer and the decoder reads from it as it
//! grows. A read just ahead of the transfer waits for it, which a local
//! network rarely lets happen at playback speed. A read far ahead, a seek
//! into the middle or a container whose index sits at the end of the file
//! (an M4A with its `moov` last, read at open), goes to the file directly
//! instead of waiting for the whole transfer. Dropping the source stops
//! the transfer.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use symphonia::core::io::MediaSource;

/// Bytes taken from the file per read: large and sequential, the reads a
/// slow link handles best.
const CHUNK: usize = 512 * 1024;

/// How far ahead of the transfer a read waits for it rather than going to
/// the file directly: a couple of the transfer's own reads.
const NEAR: u64 = 2 * CHUNK as u64;

/// A second handle on the file, for reads far ahead of the transfer.
pub trait RandomAccess: Read + Seek + Send + Sync {}
impl<T: Read + Seek + Send + Sync> RandomAccess for T {}

struct Filled {
    data: Vec<u8>,
    /// The transfer has stopped: the file is all here, or `error` says why
    /// not.
    finished: bool,
    /// Kept as a kind and a message: an `io::Error` cannot be cloned out
    /// to every read that meets it.
    error: Option<(io::ErrorKind, String)>,
}

struct Shared {
    filled: Mutex<Filled>,
    grown: Condvar,
    /// Set when the source is dropped, so the transfer stops.
    abandoned: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Filled> {
        // The worker holds the lock only to append; a panic there leaves
        // a valid buffer, so a poisoned lock is still worth reading.
        self.filled
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A seekable [`MediaSource`] over bytes a background thread is still
/// reading.
pub struct ReadAhead {
    shared: Arc<Shared>,
    direct: Option<Box<dyn RandomAccess>>,
    pos: u64,
    len: u64,
}

impl ReadAhead {
    /// Start reading `reader`, which holds `len` bytes, on a thread of its
    /// own. `direct`, a second handle on the same bytes, serves the reads
    /// far ahead of the transfer; without it they wait for it.
    pub fn spawn(
        reader: impl Read + Send + 'static,
        direct: Option<Box<dyn RandomAccess>>,
        len: u64,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            filled: Mutex::new(Filled {
                data: Vec::with_capacity(usize::try_from(len).unwrap_or(0)),
                finished: false,
                error: None,
            }),
            grown: Condvar::new(),
            abandoned: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("wf-read-ahead".into())
            .spawn(move || fill(reader, &worker))?;
        Ok(Self {
            shared,
            direct,
            pos: 0,
            len,
        })
    }
}

fn fill(mut reader: impl Read, shared: &Shared) {
    let mut chunk = vec![0_u8; CHUNK];
    let outcome = loop {
        if shared.abandoned.load(Ordering::Relaxed) {
            break None;
        }
        match reader.read(&mut chunk) {
            Ok(0) => break None,
            Ok(n) => {
                shared.lock().data.extend_from_slice(&chunk[..n]);
                shared.grown.notify_all();
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => break Some((err.kind(), err.to_string())),
        }
    };
    let mut filled = shared.lock();
    filled.finished = true;
    filled.error = outcome;
    drop(filled);
    shared.grown.notify_all();
}

impl Read for ReadAhead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut filled = self.shared.lock();
        loop {
            let have = filled.data.len() as u64;
            if self.pos < have {
                let start = self.pos as usize;
                let n = buf.len().min((have - self.pos) as usize);
                buf[..n].copy_from_slice(&filled.data[start..start + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            if filled.finished {
                let Some((kind, message)) = filled.error.clone() else {
                    return Ok(0);
                };
                drop(filled);
                // The transfer failed (a link that dropped for a moment):
                // the rest is still worth reading from the file itself.
                return match self.direct.as_mut() {
                    Some(direct) => read_direct(direct, &mut self.pos, buf),
                    None => Err(io::Error::new(kind, message)),
                };
            }
            if self.pos >= have.saturating_add(NEAR) {
                if let Some(direct) = self.direct.as_mut() {
                    drop(filled);
                    return read_direct(direct, &mut self.pos, buf);
                }
            }
            filled = self
                .shared
                .grown
                .wait(filled)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// A read from the second handle, at `pos`.
fn read_direct(
    direct: &mut Box<dyn RandomAccess>,
    pos: &mut u64,
    buf: &mut [u8],
) -> io::Result<usize> {
    direct.seek(SeekFrom::Start(*pos))?;
    let n = direct.read(buf)?;
    *pos += n as u64;
    Ok(n)
}

impl Seek for ReadAhead {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        let Some(target) = target else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            ));
        };
        // Past what has arrived is fine: the next read waits for it.
        self.pos = target;
        Ok(target)
    }
}

impl MediaSource for ReadAhead {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        self.shared.abandoned.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
