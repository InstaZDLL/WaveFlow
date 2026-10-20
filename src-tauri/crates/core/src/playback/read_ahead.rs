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
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A reader handing out whatever the test sends it, and blocking in
    /// between: a transfer the test controls.
    struct Gated(mpsc::Receiver<Vec<u8>>, Vec<u8>);

    impl Read for Gated {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.1.is_empty() {
                match self.0.recv() {
                    Ok(next) => self.1 = next,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.1.len());
            buf[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    /// The point of it: the start of the file can be read while the rest
    /// is still on its way.
    #[test]
    fn the_start_is_readable_before_the_transfer_ends() {
        let (send, gate) = mpsc::channel();
        let mut source = ReadAhead::spawn(Gated(gate, Vec::new()), None, 8).unwrap();
        send.send(b"head".to_vec()).unwrap();

        let mut head = [0_u8; 4];
        source.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"head");

        send.send(b"tail".to_vec()).unwrap();
        drop(send);
        let mut rest = Vec::new();
        source.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"tail");
    }

    #[test]
    fn a_seek_ahead_of_the_transfer_waits_for_the_bytes() {
        let (send, gate) = mpsc::channel();
        let mut source = ReadAhead::spawn(Gated(gate, Vec::new()), None, 10).unwrap();
        source.seek(SeekFrom::Start(6)).unwrap();
        let reader = std::thread::spawn(move || {
            let mut four = [0_u8; 4];
            source.read_exact(&mut four).unwrap();
            four
        });
        send.send(b"012345".to_vec()).unwrap();
        send.send(b"6789".to_vec()).unwrap();
        assert_eq!(&reader.join().unwrap(), b"6789");
    }

    #[test]
    fn seeks_from_the_end_and_from_here_land_where_asked() {
        let mut source =
            ReadAhead::spawn(io::Cursor::new(b"0123456789".to_vec()), None, 10).unwrap();
        assert_eq!(source.seek(SeekFrom::End(-3)).unwrap(), 7);
        assert_eq!(source.seek(SeekFrom::Current(-2)).unwrap(), 5);
        let mut two = [0_u8; 2];
        source.read_exact(&mut two).unwrap();
        assert_eq!(&two, b"56");
        assert!(source.seek(SeekFrom::Current(-100)).is_err());
    }

    /// A read far ahead of the transfer (an index at the end of the file,
    /// a seek into the middle) goes to the file at once rather than
    /// waiting for everything before it.
    #[test]
    fn a_read_far_ahead_goes_to_the_file() {
        let data: Vec<u8> = (0..3 * NEAR as usize).map(|i| (i % 251) as u8).collect();
        let (_send, gate) = mpsc::channel::<Vec<u8>>();
        let direct: Box<dyn RandomAccess> = Box::new(io::Cursor::new(data.clone()));
        let mut source =
            ReadAhead::spawn(Gated(gate, Vec::new()), Some(direct), data.len() as u64).unwrap();

        // The transfer never delivers a byte, and the read still returns.
        source.seek(SeekFrom::End(-16)).unwrap();
        let mut tail = [0_u8; 16];
        source.read_exact(&mut tail).unwrap();
        assert_eq!(tail, data[data.len() - 16..]);
    }

    /// A transfer that fails gives what arrived, then the error rather
    /// than a quiet end of file.
    #[test]
    fn a_failed_transfer_surfaces_after_what_arrived() {
        let mut source = ReadAhead::spawn(Failing(false), None, 6).unwrap();
        let mut three = [0_u8; 3];
        source.read_exact(&mut three).unwrap();
        assert_eq!(&three, b"abc");
        let err = source.read(&mut three).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
        assert!(err.to_string().contains("link dropped"));
    }

    /// With a second handle, a transfer that fails is not the end: the
    /// rest is read from the file itself.
    #[test]
    fn a_failed_transfer_falls_back_to_the_file() {
        let direct: Box<dyn RandomAccess> = Box::new(io::Cursor::new(b"abcdef".to_vec()));
        let mut source = ReadAhead::spawn(Failing(false), Some(direct), 6).unwrap();
        let mut all = Vec::new();
        source.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"abcdef");
    }

    /// Three bytes, then a dropped link.
    struct Failing(bool);

    impl Read for Failing {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if std::mem::replace(&mut self.0, true) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "link dropped",
                ));
            }
            buf[..3].copy_from_slice(b"abc");
            Ok(3)
        }
    }
}
