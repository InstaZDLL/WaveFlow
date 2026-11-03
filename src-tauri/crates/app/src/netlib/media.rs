//! Opening a track on a share for the decoder, and keeping what it reads.
//!
//! Streamed, not copied first: a file plays as soon as its header is here. A
//! thread pulls it in large sequential reads ([`ReadAhead`], the same thing a
//! mounted share gets), and every byte also lands in the cache, whose entry is
//! published when the whole file is there. A seek or an index at the end of
//! the file goes straight to the share through a [`RangeReader`].
//!
//! ## When the link drops
//!
//! The transfer is usually well ahead of playback, so the rest of the track is
//! often here already and keeps playing. If not, a read waits at most
//! [`STALL`], the source is declared unreachable, and the error reaches the
//! decoder, which stops without advancing the queue. A Stop or a load does not
//! have to wait that out: [`release_starved_readers`].
//!
//! ## The next track
//!
//! The decoder opens the next track on its own thread, half a second before a
//! gapless end, with about a second of audio queued: an open that waited on a
//! share would cut the track playing. So [`prewarm`] fetches the next track
//! into the cache from the moment a track starts, and `audio::next_track`
//! offers the decoder a track on a share only once it is there. If it is not
//! ready in time, the end of the track starts it like any other load: a short
//! gap instead of a cut.

use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;
use tokio::sync::Notify;
use waveflow_core::network_locator;
use waveflow_core::playback::read_ahead::{ReadAhead, ReadAheadRelease};

use super::cache::{self, CacheSlot};
use super::fs::{Entry, NetError};
use super::reader::{RangeReader, ReadGuard, ReadMode};
use super::Source;
use crate::audio::stream_cache::CacheWriter;

/// How long a read waits on a share that stopped answering before playback
/// gives up on it.
pub const STALL: Duration = Duration::from_secs(15);

/// A file past this stays out of RAM and out of the cache: the same line a
/// mounted share gets for its pre-load.
const MAX_READ_AHEAD_BYTES: u64 = 512 * 1024 * 1024;

/// Bytes per request of a background download.
const PREWARM_BLOCK: u64 = 1024 * 1024;

/// What the transfer behind playback asks for first, doubling up to
/// [`PREWARM_BLOCK`]. A read only hands over its bytes when it ends, and the
/// decoder is waiting for the header: on a slow link a large first block would
/// hold the track back for as long as it takes to arrive.
const FIRST_TRANSFER_BLOCK: u64 = 64 * 1024;

/// A stream the decoder may be blocked on: its direct reads, and its transfer
/// unless it is too large to have one.
struct ActiveStream {
    direct: Weak<ReadGuard>,
    read_ahead: Option<ReadAheadRelease>,
}

fn active_streams() -> &'static Mutex<Vec<ActiveStream>> {
    static ACTIVE: OnceLock<Mutex<Vec<ActiveStream>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(Vec::new()))
}

fn direct_release() -> &'static Notify {
    static NOTIFY: OnceLock<Notify> = OnceLock::new();
    NOTIFY.get_or_init(Notify::new)
}

static RELEASE_COUNT: AtomicU64 = AtomicU64::new(0);

/// How many times readers were asked to let go. A probe reads a released read
/// as the end of the file and reports an unknown format, so a decoder that
/// fails to open a track compares this before and after: a difference means a
/// newer command replaced it, and the failure is nobody's to hear about.
pub fn release_count() -> u64 {
    RELEASE_COUNT.load(Ordering::Acquire)
}

/// Resolves when a command asks readers waiting on a share to let go.
pub(super) async fn released() {
    direct_release().notified().await;
}

/// Let go of every read waiting on a share. Called once a command that
/// replaces or stops what plays is queued for the decoder, beside the same
/// call for a live station.
pub fn release_starved_readers() {
    RELEASE_COUNT.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut streams) = active_streams().lock() {
        streams.retain(|stream| {
            let Some(direct) = stream.direct.upgrade() else {
                return false;
            };
            // A stream that is not waiting is left alone: the command asking
            // may yet be dropped as stale, and a healthy stream must survive.
            let starved = stream
                .read_ahead
                .as_ref()
                .is_some_and(ReadAheadRelease::is_starved);
            if starved || direct.is_stalled() {
                direct.release();
                if let Some(read_ahead) = &stream.read_ahead {
                    read_ahead.release();
                }
            } else {
                direct.request();
            }
            true
        });
    }
    direct_release().notify_waiters();
}

/// Sequential reads for [`ReadAhead`], writing into the cache as they go.
struct Transfer {
    source: Arc<Source>,
    path: String,
    len: u64,
    pos: u64,
    block: u64,
    cache: Option<CacheWriter>,
}

impl Read for Transfer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want = (buf.len() as u64).min(self.block).min(self.len - self.pos);
        self.block = (self.block * 2).min(PREWARM_BLOCK);
        let bytes = super::block_on(
            self.source
                .read_patiently(&self.path, self.pos, want, STALL),
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the file on the share is shorter than listed",
            ));
        }
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        if let Some(cache) = self.cache.as_mut() {
            cache.write_at(self.pos, &bytes[..n]);
        }
        self.pos += n as u64;
        Ok(n)
    }
}

/// What the decoder reads for a locator. Installed into the core as its
/// [`NetworkOpener`](waveflow_core::playback::stream::NetworkOpener).
pub fn open(locator: &str) -> Result<Box<dyn MediaSource>, String> {
    let parsed = network_locator::parse(locator).ok_or_else(|| "bad locator".to_string())?;
    let cached = cache::slot(locator);
    let source = super::source(parsed.source);

    let Some(source) = source else {
        // Removed since the queue was built, but the bytes may still be here.
        return cached
            .as_ref()
            .and_then(open_cached)
            .ok_or_else(|| "network source unknown".to_string());
    };

    // A copy opens at once, without asking the share: the decoder opens the next
    // track on its own thread with a second of audio queued, and a stat on a
    // link that has just dropped would cut the track playing for its whole
    // timeout. So the check comes after, in the background. A copy that turns
    // out stale plays this once and is fetched afresh the next time. A share
    // that is down has nothing to check it against.
    if let Some(slot) = &cached {
        if let Some(file) = open_cached(slot) {
            verify_in_background(Arc::clone(&source), parsed.path.to_string(), slot.clone());
            return Ok(file);
        }
    }

    if source.is_down() {
        return Err(super::UNREACHABLE_MESSAGE.to_string());
    }

    let stat = super::block_on(async {
        tokio::select! {
            result = source.stat(parsed.path) => result,
            () = released() => Err(NetError::Released),
        }
    });
    let entry: Entry = match stat {
        Ok(entry) => entry,
        Err(NetError::Released) => {
            return Err(waveflow_core::playback::read_ahead::READ_RELEASED.to_string());
        }
        Err(NetError::NotFound) => return Err("file not found on the share".to_string()),
        Err(err) => {
            tracing::warn!(error = %err, "network track: stat failed");
            return Err(super::UNREACHABLE_MESSAGE.to_string());
        }
    };

    let direct = RangeReader::new(
        Arc::clone(&source),
        parsed.path.to_string(),
        entry.size,
        ReadMode::Playback,
    );
    let guard = Arc::downgrade(&direct.guard());
    if entry.size > MAX_READ_AHEAD_BYTES {
        register(guard, None);
        return Ok(Box::new(direct));
    }
    let transfer = Transfer {
        source: Arc::clone(&source),
        path: parsed.path.to_string(),
        len: entry.size,
        pos: 0,
        block: FIRST_TRANSFER_BLOCK,
        cache: cache::writer(locator, entry.size, entry.modified_ms),
    };
    let read_ahead = ReadAhead::spawn(transfer, Some(Box::new(direct)), entry.size)
        .map_err(|e| format!("network read-ahead: {e}"))?;
    register(guard, Some(read_ahead.release_handle()));
    Ok(Box::new(read_ahead))
}

fn register(direct: Weak<ReadGuard>, read_ahead: Option<ReadAheadRelease>) {
    if let Ok(mut streams) = active_streams().lock() {
        streams.push(ActiveStream { direct, read_ahead });
    }
}

/// Drop a copy the share no longer matches. Windows refuses while the copy is
/// open; the next play then finds it stale again and tries once more.
fn verify_in_background(source: Arc<Source>, path: String, slot: CacheSlot) {
    if source.is_down() {
        return;
    }
    super::spawn(async move {
        if let Ok(entry) = source.stat(&path).await {
            if !slot.matches(entry.size, entry.modified_ms) {
                tracing::debug!("network track: the cached copy is stale");
                slot.discard();
            }
        }
    });
}

fn open_cached(slot: &CacheSlot) -> Option<Box<dyn MediaSource>> {
    let path: PathBuf = slot.use_path()?;
    let file = std::fs::File::open(path).ok()?;
    Some(Box::new(file))
}

/// The one background download running, so a new one can take its place.
fn current_prewarm() -> &'static Mutex<Option<(String, Arc<AtomicBool>)>> {
    static CURRENT: OnceLock<Mutex<Option<(String, Arc<AtomicBool>)>>> = OnceLock::new();
    CURRENT.get_or_init(|| Mutex::new(None))
}

/// Fetch a track on a share into the cache in the background, for the decoder
/// to find when it gets there. One at a time: a new request stops the previous
/// one, whose working file goes with it. `on_ready` runs once it is cached.
///
/// Not a task-registry row: it lasts as long as one file's transfer, nothing
/// waits on it, and the user neither started it nor needs to stop it.
pub fn prewarm(locator: String, on_ready: impl FnOnce() + Send + 'static) {
    let cancel = Arc::new(AtomicBool::new(false));
    if let Ok(mut current) = current_prewarm().lock() {
        if let Some((running, previous)) = current.as_ref() {
            if *running == locator && !previous.load(Ordering::Acquire) {
                return;
            }
            previous.store(true, Ordering::Release);
        }
        *current = Some((locator.clone(), Arc::clone(&cancel)));
    }
    let spawned = std::thread::Builder::new()
        .name("wf-net-prewarm".into())
        .spawn(move || {
            match fetch_into_cache(&locator, &cancel) {
                Ok(true) => on_ready(),
                Ok(false) => {}
                Err(err) => tracing::debug!(error = %err, "network track: prewarm stopped"),
            }
            if let Ok(mut current) = current_prewarm().lock() {
                if current
                    .as_ref()
                    .is_some_and(|(_, c)| Arc::ptr_eq(c, &cancel))
                {
                    *current = None;
                }
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "network track: prewarm thread failed");
    }
}

/// Stop the background download, if one runs: a profile switch makes it another
/// profile's file in another profile's cache.
pub fn stop_prewarm() {
    if let Ok(mut current) = current_prewarm().lock() {
        if let Some((_, cancel)) = current.take() {
            cancel.store(true, Ordering::Release);
        }
    }
}

fn fetch_into_cache(locator: &str, cancel: &AtomicBool) -> Result<bool, NetError> {
    let parsed =
        network_locator::parse(locator).ok_or_else(|| NetError::Protocol("bad locator".into()))?;
    let source =
        super::source(parsed.source).ok_or_else(|| NetError::Protocol("unknown source".into()))?;
    if source.is_down() {
        return Ok(false);
    }
    let entry = super::block_on(source.stat(parsed.path))?;
    if let Some(slot) = cache::slot(locator) {
        if slot.matches(entry.size, entry.modified_ms) {
            return Ok(true);
        }
        slot.discard();
    }
    if entry.size > MAX_READ_AHEAD_BYTES {
        return Ok(false);
    }
    let Some(mut writer) = cache::writer(locator, entry.size, entry.modified_ms) else {
        return Ok(false);
    };
    let started = Instant::now();
    let mut pos = 0;
    while pos < entry.size {
        if cancel.load(Ordering::Acquire) {
            return Ok(false);
        }
        let want = PREWARM_BLOCK.min(entry.size - pos);
        let bytes = super::block_on(source.read_at(parsed.path, pos, want))?;
        if bytes.is_empty() {
            return Err(NetError::Protocol("shorter than listed".into()));
        }
        writer.write_at(pos, &bytes);
        pos += bytes.len() as u64;
    }
    tracing::debug!(
        bytes = entry.size,
        ms = started.elapsed().as_millis() as u64,
        "network track: next track cached"
    );
    Ok(true)
}
