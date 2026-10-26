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

/// The byte rates taken for a station's: 800 B/s is 6.4 kbit/s, under any
/// station; 1 MB/s is 8 Mbit/s, past a lossless one. A figure outside —
/// a server's misreport, an HLS duration of almost nothing — is not
/// trusted with the bound or the lag.
const RATE_RANGE: std::ops::RangeInclusive<f64> = 800.0..=1_000_000.0;

/// Title changes kept while the reader has not reached them. A station
/// changes title once a song; a server sending one with every metadata
/// block is not to grow memory alongside a pause, so past this the oldest
/// change goes.
const MAX_TITLES: usize = 512;

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
    /// Time spent paused, pauses already over.
    paused_total: Duration,
    /// When the pause in progress began.
    paused_since: Option<Instant>,
}

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
    /// How far behind live the listener is: the time spent paused, the
    /// one way to fall behind, up to the capacity.
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
        if !RATE_RANGE.contains(&byte_rate) {
            return;
        }
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
        if s.titles.len() >= MAX_TITLES {
            s.titles.pop_front();
        }
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

    /// The listener paused, or played again. The time between is how far
    /// behind live they are from then on: the stream kept arriving and
    /// nothing read it.
    ///
    /// Counted here rather than read off the unread level: what a listener
    /// who never paused has in hand is the server's burst plus the
    /// network's slack, measured between 2.7 s (an Icecast mount at
    /// 128 kbit/s) and 17 s (an HLS stream of 10 s segments), and a pause
    /// in the first seconds would be taken for it.
    pub fn set_paused(&self, paused: bool) {
        self.set_paused_at(paused, Instant::now());
    }

    fn set_paused_at(&self, paused: bool, now: Instant) {
        let mut s = self.lock();
        match (paused, s.paused_since) {
            (true, None) => s.paused_since = Some(now),
            (false, Some(since)) => {
                s.paused_total += now.saturating_duration_since(since);
                s.paused_since = None;
            }
            _ => {}
        }
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
        pass_titles(&mut s);
        Ok(copied)
    }

    /// The title change the reader has passed since it last asked, if any.
    pub fn take_title(&self) -> Option<Option<String>> {
        self.lock().title_due.take()
    }

    pub fn lag(&self) -> Lag {
        self.lag_at(Instant::now())
    }

    fn lag_at(&self, now: Instant) -> Lag {
        let s = self.lock();
        let to_time = |bytes: usize| {
            s.byte_rate
                .filter(|r| *r > 0.0)
                .and_then(|r| Duration::try_from_secs_f64(bytes as f64 / r).ok())
        };
        let paused = s.paused_total
            + s.paused_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
        // Past the capacity the oldest audio went and the reader jumped
        // forward: no further behind than the buffer holds.
        let capacity = to_time(s.capacity);
        let behind = capacity.map_or(paused, |capacity| paused.min(capacity));
        Lag {
            unread: to_time(s.unread),
            behind,
            capacity,
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
    fn an_absurd_rate_or_a_flood_of_titles_is_not_trusted() {
        let buffer = Buffer::new();
        for rate in [f64::NAN, f64::INFINITY, 0.0, -1.0, 1e-300, 1e300] {
            buffer.set_byte_rate(rate);
        }
        buffer.push(&[0u8; 100]);
        assert_eq!(buffer.lag().unread, None);
        for i in 0..(MAX_TITLES * 3) {
            buffer.push_title(Some(format!("title {i}")));
        }
        assert_eq!(buffer.lock().titles.len(), MAX_TITLES);
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
        buffer.set_byte_rate(800.0);
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
    fn behind_is_the_time_spent_paused_even_in_the_first_seconds() {
        let buffer = Buffer::new();
        let t0 = Instant::now();
        // However much the station sent ahead, never pausing is live.
        buffer.push(&[0u8; 400_000]);
        assert_eq!(buffer.lag_at(t0).behind, Duration::ZERO);
        // Paused a second in, before anything settled, for 20 s.
        buffer.set_paused_at(true, t0 + Duration::from_secs(1));
        assert_eq!(
            buffer.lag_at(t0 + Duration::from_secs(11)).behind,
            Duration::from_secs(10)
        );
        buffer.set_paused_at(false, t0 + Duration::from_secs(21));
        let later = t0 + Duration::from_secs(300);
        assert_eq!(buffer.lag_at(later).behind, Duration::from_secs(20));
        // A second pause adds to it; a repeated call changes nothing.
        buffer.set_paused_at(true, later);
        buffer.set_paused_at(true, later + Duration::from_secs(4));
        buffer.set_paused_at(false, later + Duration::from_secs(5));
        buffer.set_paused_at(false, later + Duration::from_secs(9));
        assert_eq!(
            buffer.lag_at(later + Duration::from_secs(60)).behind,
            Duration::from_secs(25)
        );
    }

    #[test]
    fn behind_stops_at_the_capacity() {
        let buffer = Buffer::new();
        buffer.set_byte_rate(16_000.0);
        let t0 = Instant::now();
        buffer.set_paused_at(true, t0);
        assert_eq!(buffer.lag_at(t0 + Duration::from_secs(3_600)).behind, SPAN);
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
