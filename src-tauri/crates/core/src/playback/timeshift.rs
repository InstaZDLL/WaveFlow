//! The bounded buffer between a live stream and the decoder.
//!
//! A live station is read by its own thread ([`super::ingest`]) into this
//! buffer, and the decoder reads the buffer. Pausing stops the decoder,
//! not the ingest, so the stream keeps arriving while nothing plays, and
//! playing again resumes where the pause was — behind the live edge by as
//! long as the pause lasted. Without it, a paused stream sat on an unread
//! socket: measured on two Icecast servers, both dropped the connection
//! within five minutes, and resuming played 8 to 12 s of stale audio before
//! the stream ended.
//!
//! **Compressed bytes, bounded.** What is kept is what the station sent —
//! 16 KB/s at 128 kbit/s, 40 KB/s at 320 kbit/s, so 29 MB and 72 MB for 30
//! minutes; decoded PCM would be 635 MB. Only what the decoder has not read
//! yet is kept (there is no rewinding past the pause point), so a station
//! listened to live costs nothing beyond the few seconds a server sends
//! ahead. When the unread part reaches the bound, the oldest is dropped
//! and the reader jumps forward: MP3 and ADTS resynchronise on the next
//! frame header (measured: at most one undecodable packet).
//!
//! **ICY titles are timed with the audio.** A `StreamTitle` is recorded at
//! the offset it arrived at and handed back when the reader passes that
//! offset, so a time-shifted listener sees the title of what they hear,
//! not of what the station plays now.
//!
//! The lock is held only to copy bytes in or out; the reader waits on a
//! condition variable. Neither side is the audio callback — the decoder
//! thread reads, the ingest thread writes — so blocking here is allowed.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Bytes per stored chunk: what one network read is copied into.
const CHUNK: usize = 64 * 1024;

/// How much a station may get ahead of the listener, in its own time.
pub const SPAN: Duration = Duration::from_secs(30 * 60);

/// Hard ceiling, whatever the station's bitrate: 30 min at 384 kbit/s.
pub const MAX_BYTES: usize = 96 * 1024 * 1024;

/// Floor, so a station whose bitrate is misreported low still gets a
/// useful pause.
pub const MIN_BYTES: usize = 4 * 1024 * 1024;

/// How the stream ended, once it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// The reader went away (a new track, a stop).
    Stopped,
    /// The station could not be read again after retrying.
    Failed(String),
    /// A finite stream (an HLS playlist with an end) was read whole.
    Finished,
}

#[derive(Debug, Default)]
struct State {
    /// Unread bytes, oldest first. The front chunk may be partly read.
    chunks: VecDeque<Vec<u8>>,
    /// How much of the front chunk has been read.
    front_read: usize,
    /// Bytes not read yet.
    unread: usize,
    /// Bytes ever written, and ever read or dropped: absolute offsets.
    written: u64,
    consumed: u64,
    /// Bytes dropped because the listener fell more than the bound behind.
    dropped: u64,
    /// Title changes at the offset they arrived at, oldest first.
    titles: VecDeque<(u64, Option<String>)>,
    /// A title change the reader passed and has not been handed yet.
    title_due: Option<Option<String>>,
    capacity: usize,
    /// The station's bytes per second, once known.
    byte_rate: Option<f64>,
    end: Option<End>,
    /// When the first byte was read.
    first_read: Option<Instant>,
    /// The least the reader has had in hand once settled: what a station
    /// keeps ahead of a listener who never paused (the burst a server
    /// sends on connect, the network's slack). Unread audio past this is
    /// how far behind live the listener is.
    floor: Option<usize>,
    /// How much the unread level swings above the floor by itself: an HLS
    /// stream arrives a segment at a time, so a listener who never paused
    /// has up to a segment more in hand just after one landed.
    slack: Duration,
}

/// How long after the first read the unread level counts toward the
/// floor: the server's burst arrives in that time.
const SETTLE: Duration = Duration::from_secs(5);

/// The buffer, shared by the ingest thread and the reader.
#[derive(Debug)]
pub struct Buffer {
    state: Mutex<State>,
    ready: Condvar,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

/// The capacity for a station sending `byte_rate` bytes per second.
pub fn capacity_for(byte_rate: Option<f64>) -> usize {
    match byte_rate {
        Some(rate) if rate.is_finite() && rate > 0.0 => {
            ((rate * SPAN.as_secs_f64()) as usize).clamp(MIN_BYTES, MAX_BYTES)
        }
        _ => MAX_BYTES,
    }
}

/// A snapshot of how far behind live the reader is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lag {
    /// Unread audio, in the station's time. `None` until its rate is known.
    pub unread: Option<Duration>,
    /// How far behind live the reader is: the unread audio past what a
    /// listener who never paused has in hand. Zero until that is known.
    pub behind: Duration,
    pub capacity: Option<Duration>,
    pub dropped_bytes: u64,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                capacity: MAX_BYTES,
                ..State::default()
            }),
            ready: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // Nothing panics while holding it; a poisoned lock still holds
        // consistent counters, so keep going rather than lose the stream.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Set the station's rate, which sizes the bound.
    pub fn set_byte_rate(&self, byte_rate: f64) {
        let mut s = self.lock();
        s.byte_rate = Some(byte_rate);
        s.capacity = capacity_for(Some(byte_rate));
        drop_overflow(&mut s);
    }

    /// Append what the station sent.
    pub fn push(&self, mut data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut s = self.lock();
        if s.end.is_some() {
            return;
        }
        s.written += data.len() as u64;
        s.unread += data.len();
        if let Some(last) = s.chunks.back_mut() {
            let room = CHUNK.saturating_sub(last.len());
            let take = room.min(data.len());
            last.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        while !data.is_empty() {
            let take = CHUNK.min(data.len());
            let mut chunk = Vec::with_capacity(CHUNK);
            chunk.extend_from_slice(&data[..take]);
            s.chunks.push_back(chunk);
            data = &data[take..];
        }
        drop_overflow(&mut s);
        drop(s);
        self.ready.notify_all();
    }

    /// The station's `StreamTitle` changed, at the current write offset.
    pub fn push_title(&self, title: Option<String>) {
        let mut s = self.lock();
        let at = s.written;
        s.titles.push_back((at, title));
    }

    /// End the stream; the reader drains what is left, then reads EOF.
    pub fn finish(&self, end: End) {
        let mut s = self.lock();
        if s.end.is_none() {
            s.end = Some(end);
        }
        drop(s);
        self.ready.notify_all();
    }

    /// End the stream if the reader is waiting on it with nothing to read:
    /// a load or a stop is on its way to the decoder, and must not queue
    /// behind a station that is reconnecting. A buffer with audio in hand
    /// is left alone — its reader is not blocked, and the command reaches
    /// it at the next packet.
    pub fn stop_if_starved(&self) {
        let mut s = self.lock();
        if s.unread == 0 && s.end.is_none() {
            s.end = Some(End::Stopped);
            drop(s);
            self.ready.notify_all();
        }
    }

    /// How much the unread level may rise above the floor without the
    /// listener being behind: the length of one HLS segment.
    pub fn set_slack(&self, slack: Duration) {
        self.lock().slack = slack;
    }

    /// Whether the stream has ended (the reader gone, or the station lost).
    pub fn ended(&self) -> Option<End> {
        self.lock().end.clone()
    }

    /// Read into `buf`, waiting up to `wait` for data. `Ok(0)` is the end
    /// of the stream; `Err(WouldBlock)` a wait that ran out, for the
    /// caller to check whether it should keep waiting.
    pub fn read(&self, buf: &mut [u8], wait: Duration) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now() + wait;
        let mut s = self.lock();
        while s.unread == 0 {
            match &s.end {
                Some(End::Failed(reason)) => {
                    return Err(std::io::Error::other(reason.clone()));
                }
                Some(_) => return Ok(0),
                None => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            s = self
                .ready
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        let mut copied = 0;
        while copied < buf.len() && s.unread > 0 {
            let front_read = s.front_read;
            let Some(front) = s.chunks.front() else { break };
            let front_len = front.len();
            let take = (front_len - front_read).min(buf.len() - copied);
            buf[copied..copied + take].copy_from_slice(&front[front_read..front_read + take]);
            copied += take;
            s.front_read += take;
            s.unread -= take;
            s.consumed += take as u64;
            if s.front_read == front_len {
                s.chunks.pop_front();
                s.front_read = 0;
            }
        }
        let now = Instant::now();
        let first = *s.first_read.get_or_insert(now);
        if now.duration_since(first) >= SETTLE {
            let unread = s.unread;
            s.floor = Some(s.floor.map_or(unread, |floor| floor.min(unread)));
        }
        pass_titles(&mut s);
        Ok(copied)
    }

    /// The title change the reader has passed since it last asked, if any.
    pub fn take_title(&self) -> Option<Option<String>> {
        self.lock().title_due.take()
    }

    pub fn lag(&self) -> Lag {
        let s = self.lock();
        let to_time = |bytes: usize| {
            s.byte_rate
                .filter(|r| *r > 0.0)
                .map(|r| Duration::from_secs_f64(bytes as f64 / r))
        };
        let behind = s
            .floor
            .and_then(|floor| to_time(s.unread.saturating_sub(floor)))
            .map(|over| over.saturating_sub(s.slack))
            .unwrap_or_default();
        Lag {
            unread: to_time(s.unread),
            behind,
            capacity: to_time(s.capacity),
            dropped_bytes: s.dropped,
        }
    }

    /// Bytes held — the memory the buffer costs, give or take a chunk.
    pub fn held_bytes(&self) -> usize {
        let s = self.lock();
        s.chunks.iter().map(Vec::capacity).sum()
    }
}

/// Drop the oldest unread bytes past the capacity, whole chunks first.
fn drop_overflow(s: &mut State) {
    while s.unread > s.capacity {
        let excess = s.unread - s.capacity;
        let front_read = s.front_read;
        let Some(front) = s.chunks.front() else { break };
        let left = front.len() - front_read;
        if left <= excess {
            s.chunks.pop_front();
            s.front_read = 0;
            s.unread -= left;
            s.consumed += left as u64;
            s.dropped += left as u64;
        } else {
            s.front_read += excess;
            s.unread -= excess;
            s.consumed += excess as u64;
            s.dropped += excess as u64;
        }
    }
    pass_titles(s);
}

/// Move the title changes the read offset has passed into `title_due`,
/// keeping only the latest: two changes skipped in one read leave the
/// second on screen.
fn pass_titles(s: &mut State) {
    while let Some((at, _)) = s.titles.front() {
        if *at > s.consumed {
            break;
        }
        let (_, title) = s.titles.pop_front().expect("front exists");
        s.title_due = Some(title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Duration = Duration::from_millis(0);

    #[test]
    fn what_is_pushed_is_read_back_in_order_across_chunks() {
        let buffer = Buffer::new();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        buffer.push(&data[..1000]);
        buffer.push(&data[1000..]);
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while out.len() < data.len() {
            let n = buffer.read(&mut buf, NOW).unwrap();
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, data);
        // Nothing unread is kept, and what was read is let go.
        assert!(matches!(
            buffer.read(&mut buf, NOW),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert_eq!(buffer.held_bytes(), 0);
    }

    #[test]
    fn a_reader_too_far_behind_loses_the_oldest_bytes_not_the_newest() {
        let buffer = Buffer::new();
        // 16 KB/s, a 128 kbit/s station: 30 minutes is 28.8 MB.
        buffer.set_byte_rate(16_000.0);
        let second = vec![7u8; 16_000];
        for _ in 0..(31 * 60) {
            buffer.push(&second);
        }
        let lag = buffer.lag();
        assert_eq!(lag.unread, Some(SPAN));
        assert_eq!(lag.dropped_bytes, 60 * 16_000);
        // The memory follows the bound, give or take a chunk.
        let held = buffer.held_bytes();
        assert!(
            (28_800_000..=28_800_000 + 2 * CHUNK).contains(&held),
            "{held}"
        );
    }

    #[test]
    fn the_bound_follows_the_bitrate_within_its_limits() {
        assert_eq!(capacity_for(Some(40_000.0)), 72_000_000);
        assert_eq!(capacity_for(Some(100.0)), MIN_BYTES);
        assert_eq!(capacity_for(Some(1_000_000.0)), MAX_BYTES);
        assert_eq!(capacity_for(None), MAX_BYTES);
    }

    #[test]
    fn titles_come_back_when_the_reader_reaches_them() {
        let buffer = Buffer::new();
        buffer.push(&[0u8; 100]);
        buffer.push_title(Some("Second song".into()));
        buffer.push(&[0u8; 100]);
        let mut buf = [0u8; 150];
        assert_eq!(buffer.read(&mut buf[..50], NOW).unwrap(), 50);
        assert_eq!(buffer.take_title(), None);
        assert_eq!(buffer.read(&mut buf, NOW).unwrap(), 150);
        assert_eq!(buffer.take_title(), Some(Some("Second song".into())));
        assert_eq!(buffer.take_title(), None);
    }

    #[test]
    fn a_dropped_stretch_hands_over_the_title_it_carried() {
        let buffer = Buffer::new();
        buffer.set_byte_rate(1.0);
        buffer.push(&[1u8; 10]);
        buffer.push_title(Some("Skipped into".into()));
        assert_eq!(buffer.take_title(), None);
        // Full: the first ten bytes go, and with them the place the title
        // changed at.
        buffer.push(&vec![0u8; MIN_BYTES]);
        assert_eq!(buffer.lag().dropped_bytes, 10);
        assert_eq!(buffer.take_title(), Some(Some("Skipped into".into())));
    }

    #[test]
    fn behind_is_what_is_unread_past_the_settled_floor_and_the_slack() {
        let buffer = Buffer::new();
        buffer.set_byte_rate(1_000.0);
        buffer.push(&[0u8; 4_000]);
        let mut buf = [0u8; 1_000];
        buffer.read(&mut buf, NOW).unwrap();
        // Settled: the reader had 3 s in hand when it was last served.
        buffer.lock().first_read = Some(Instant::now() - SETTLE);
        buffer.read(&mut buf, NOW).unwrap();
        assert_eq!(buffer.lag().behind, Duration::ZERO);
        // A 30 s pause: 30 s more arrive, none is read.
        buffer.push(&vec![0u8; 30_000]);
        assert_eq!(buffer.lag().behind, Duration::from_secs(30));
        // A segment's worth of it is how HLS arrives, not a delay.
        buffer.set_slack(Duration::from_secs(6));
        assert_eq!(buffer.lag().behind, Duration::from_secs(24));
    }

    #[test]
    fn the_end_is_read_after_what_is_left() {
        let buffer = Buffer::new();
        buffer.push(&[1, 2, 3]);
        buffer.finish(End::Failed("gone".into()));
        let mut buf = [0u8; 8];
        assert_eq!(buffer.read(&mut buf, NOW).unwrap(), 3);
        assert!(buffer.read(&mut buf, NOW).is_err());
        // Nothing more is taken once ended.
        buffer.push(&[4]);
        assert!(buffer.read(&mut buf, NOW).is_err());
        let finished = Buffer::new();
        finished.finish(End::Finished);
        assert_eq!(finished.read(&mut buf, NOW).unwrap(), 0);
    }

    #[test]
    fn a_waiting_reader_wakes_on_new_data() {
        let buffer = std::sync::Arc::new(Buffer::new());
        let writer = std::sync::Arc::clone(&buffer);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            writer.push(&[9u8; 4]);
        });
        let mut buf = [0u8; 8];
        assert_eq!(buffer.read(&mut buf, Duration::from_secs(5)).unwrap(), 4);
        handle.join().unwrap();
    }
}
