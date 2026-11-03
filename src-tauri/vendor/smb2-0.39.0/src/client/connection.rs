//! Connection state and message exchange with actor-based routing.
//!
//! The [`Connection`] type manages a single TCP connection to an SMB server.
//! A background receiver task owns the transport's read half, demultiplexes
//! incoming frames by `MessageId`, and routes each response to the matching
//! per-request `oneshot::Sender`. A writer task owns the write half, so a
//! caller hands it whole frames and never touches the socket. Callers use
//! [`Connection::execute`] / [`Connection::execute_compound`], each of which
//! owns its `oneshot::Receiver` locally.
//!
//! Decisions and their why (the IDs cited below, like decision E3):
//! `docs/connection-actor.md`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use futures_util::future::{select, Either};
use log::{debug, error, info, trace, warn, Level};
use tokio::sync::{mpsc, oneshot, watch, Semaphore};

/// One in-flight request: who is waiting, what they asked for, and since when.
///
/// The command and timestamp exist purely so a request that never comes back can
/// be NAMED. Without them a wedged connection is only observable as "the caller
/// is still awaiting", which is exactly the dead end a 2026-07-31 Cmdr incident
/// hit: three requests appear to have gone unanswered while small operations on
/// the same connection kept flowing, and nothing recorded enough to confirm it.
struct Waiter {
    tx: oneshot::Sender<Routed>,
    command: Command,
    /// When the waiter was inserted, which is BEFORE the bytes reach the
    /// transport. A request can sit here having never been sent.
    registered_at: std::time::Instant,
    /// When the transport accepted the frame, or `None` while it is still
    /// queued for the writer task.
    ///
    /// The whole point of the split: "registered 20 minutes ago, never sent"
    /// and "sent 20 minutes ago, unanswered" are opposite diagnoses, and
    /// collapsing them into one timestamp is what sent three investigations
    /// after an innocent server.
    sent_at: Option<std::time::Instant>,
    /// Last sign of life for this request: the send, then every interim
    /// STATUS_PENDING the server sends. Separate from the timestamps above
    /// because the response deadline wants "how long since the server last
    /// said anything", and a request still in the send queue has not asked
    /// it anything yet.
    last_activity: std::time::Instant,
    /// The `AsyncId` the server assigned in its interim STATUS_PENDING, if it
    /// sent one.
    ///
    /// Load-bearing for CANCEL and nothing else. Once a request has been given
    /// an `AsyncId`, MS-SMB2 § 3.2.4.24 says a cancel must carry it with
    /// `SMB2_FLAGS_ASYNC_COMMAND` set; one sent against the `MessageId`
    /// instead matches nothing and the server keeps the request. Every long
    /// poll gets one (a CHANGE_NOTIFY the server intends to hold is exactly
    /// what interim responses are for), so without this a retired
    /// subscription could never actually be retired.
    async_id: Option<u64>,
}

/// What the receiver task hands a waiter: the response, and when its frame
/// came off the wire.
///
/// The instant exists for the read-ahead and write-behind windows, which
/// measure the link from it. The moment a caller gets round to looking at a
/// response says how fast the caller is, not the link: a consumer slow to take
/// chunks reads the link low, and one catching up after a stall takes a burst
/// of long-arrived chunks at once and reads it high. Crate-private on purpose,
/// so [`Frame`] keeps its all-public shape.
struct Routed {
    result: Result<Frame>,
    /// `None` for an error the connection fanned out itself, which never
    /// arrived at all.
    arrived_at: Option<rt::Instant>,
}

/// A registered waiter that deregisters itself if its caller goes away.
///
/// Consumers abort in-flight requests as a matter of course (a user cancels a
/// copy; a `select!` arm loses). Before this was RAII, only the response
/// deadline ever removed a waiter, so every abandoned request stayed in the
/// map for the life of the connection. Two things broke: `outstanding_requests`
/// reported long-dead requests as in flight, and `reserve_credits`' "is
/// anything outstanding that could bring a grant back?" check could never
/// again be false, so genuine starvation waited out the full deadline instead
/// of failing fast.
pub(crate) struct WaiterGuard {
    inner: Arc<Inner>,
    msg_id: MessageId,
    /// The connection generation this request was registered under, captured
    /// here rather than read back later: after a revival the connection's own
    /// counter has moved on, and a `MessageId` means nothing outside the
    /// session that issued it.
    generation: u64,
    /// Taken when the response is claimed; `None` afterwards so `Drop` knows
    /// there is nothing left to clean up.
    rx: Option<oneshot::Receiver<Routed>>,
    /// A response [`has_landed`](Self::has_landed) took off `rx` and nobody
    /// has claimed yet. Every way of claiming one looks here first.
    landed: Option<Routed>,
    /// When the response's frame came off the wire, once it has landed.
    arrived_at: Option<rt::Instant>,
}

impl WaiterGuard {
    /// When the response's frame came off the wire: stamped by the receiver
    /// task as it read the frame, however long the caller took to look.
    /// `None` until a response has been claimed or seen by
    /// [`has_landed`](Self::has_landed), and for an error that never arrived
    /// (the connection died under it).
    pub(crate) fn arrived_at(&self) -> Option<rt::Instant> {
        self.arrived_at
    }

    /// Whether the response has been routed here, without claiming it: it
    /// waits in the guard for [`recv`](Self::recv) or
    /// [`try_recv`](Self::try_recv), and [`arrived_at`](Self::arrived_at)
    /// says when it came in. Synchronous and idempotent, so a caller can ask
    /// about every request it has out without anything to cancel.
    ///
    /// For a read-ahead [`FileDownload`](crate::FileDownload), which takes
    /// chunks in file order and so hears of an answer the server sent early
    /// only once the ones ahead of it are taken.
    pub(crate) fn has_landed(&mut self) -> bool {
        if self.landed.is_none() {
            if let Some(Ok(routed)) = self.rx.as_mut().map(oneshot::Receiver::try_recv) {
                // Spent: a oneshot polled again after its value panics.
                self.rx = None;
                self.arrived_at = routed.arrived_at;
                self.landed = Some(routed);
            }
        }
        self.landed.is_some()
    }

    fn claim(&mut self, routed: Routed) -> Result<Frame> {
        self.arrived_at = routed.arrived_at;
        routed.result
    }

    /// The id this guard is holding a slot for.
    pub(crate) fn msg_id(&self) -> MessageId {
        self.msg_id
    }

    /// The session generation `msg_id` belongs to. What a CANCEL for it has to
    /// be stamped with, so a revival in between can be spotted.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The `AsyncId` the server assigned this request, if it has sent an
    /// interim STATUS_PENDING. What a CANCEL for it has to carry.
    pub(crate) fn async_id(&self) -> Option<u64> {
        self.inner.async_id_for(self.msg_id)
    }

    /// Take an answer that has ALREADY arrived, without waiting for one.
    ///
    /// The last look before a caller walks away from a request it is about to
    /// retire: a response that landed in the moment between "this has been
    /// parked long enough" and "drop the guard" is a real answer, and throwing
    /// it away would turn a routine handover into lost events.
    pub(crate) fn try_recv(&mut self) -> Option<Result<Frame>> {
        if let Some(routed) = self.landed.take() {
            return Some(self.claim(routed));
        }
        let rx = self.rx.as_mut()?;
        match rx.try_recv() {
            Ok(routed) => Some(self.claim(routed)),
            Err(oneshot::error::TryRecvError::Empty) => None,
            Err(oneshot::error::TryRecvError::Closed) => Some(Err(Error::Disconnected)),
        }
    }

    /// Await this request's response.
    ///
    /// Takes `&mut self` on purpose: the guard, not the future, owns the map
    /// entry, so a caller whose future is dropped mid-await still deregisters.
    pub(crate) async fn recv(&mut self) -> Result<Frame> {
        if let Some(routed) = self.landed.take() {
            return self.claim(routed);
        }
        let Some(rx) = self.rx.as_mut() else {
            return Err(Error::Disconnected);
        };
        match rx.await {
            Ok(routed) => self.claim(routed),
            Err(_canceled) => Err(Error::Disconnected),
        }
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        // Routing removes the entry when the response lands, and the response
        // deadline removes it when it gives up, so this is usually a no-op.
        // It is idempotent by design: MessageIds are never reused, so a late
        // removal can't evict somebody else's waiter.
        if self
            .inner
            .waiters
            .lock()
            .unwrap()
            .remove(&self.msg_id)
            .is_some()
        {
            let mut abandoned = self.inner.abandoned.lock().unwrap();
            if abandoned.len() >= ABANDONED_ID_MEMORY {
                abandoned.pop_front();
            }
            abandoned.push_back(self.msg_id);
            trace!(
                "waiter deregistered without a response: msg_id={}",
                self.msg_id.0
            );
        }
    }
}

/// How often the receiver task sweeps for requests that have gone unanswered,
/// and how long a request may be outstanding before it is called out.
///
/// The threshold has to sit far enough below [`RESPONSE_TIMEOUT`] that at
/// least one sweep lands between "this is taking a while" and "the caller gave
/// up" — otherwise the only log line explaining a wedge arrives after the
/// evidence is gone. `the_default_deadlines_are_layered` pins that.
const STALE_WAITER_SWEEP: std::time::Duration = std::time::Duration::from_secs(10);
const STALE_WAITER_AFTER: std::time::Duration = std::time::Duration::from_secs(15);

/// How long a transfer's measured rate seeds the next window the same way,
/// for downloads and uploads alike.
///
/// Long enough to span the gaps in a folder copy (the consumer writing one
/// file locally before opening the next), short enough that a link which
/// changed since (Wi-Fi roaming, a VPN coming up) is measured afresh. A stale
/// hint costs at most one window at the start of one transfer: the first
/// delivery replaces it with a measurement.
const RATE_HINT_TTL: Duration = Duration::from_secs(30);

/// The largest answer whose round trip still counts toward the connection's
/// RTT estimate (`Inner::note_round_trip`): a `stat`'s or a CLOSE's, not a
/// payload whose own time on the wire would ride along.
const ROUND_TRIP_SAMPLE_MAX: usize = 4096;

/// What a transfer left on the connection, unless it's older than
/// [`RATE_HINT_TTL`].
fn recent<T: Copy>(noted: &StdMutex<Option<(rt::Instant, T)>>) -> Option<T> {
    let noted = *noted.lock().unwrap();
    noted
        .filter(|(at, _)| at.elapsed() < RATE_HINT_TTL)
        .map(|(_, value)| value)
}

/// How often a send parked on credits rechecks whether anything is still
/// outstanding. Short enough that "the last response landed while we waited"
/// surfaces quickly, long enough to cost nothing.
const CREDIT_STARVATION_RECHECK: Duration = Duration::from_millis(250);

/// How long a request may go without any sign of life from the server before
/// the caller gives up with [`Error::Timeout`].
///
/// This is a **silence** budget, not a duration budget, which is what lets it
/// be short: the clock restarts on every interim STATUS_PENDING (MS-SMB2
/// § 3.2.5.1.5) and again the moment the frame reaches the wire, so an
/// operation the server has acknowledged gets as long as it needs and only
/// total silence ever trips it. Long-poll commands are exempt entirely (see
/// [`is_long_poll`]).
///
/// 30 s is chosen to clear the slowest thing a healthy server does without
/// saying anything: waking a spun-down NAS disk before it can answer the first
/// CREATE, which runs 10–20 s on consumer hardware. Past that, waiting is no
/// longer diagnosis, it is a frozen transfer nobody is watching recover.
pub(crate) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one frame may take to reach the socket before its caller gives up
/// with [`Error::SendTimeout`].
///
/// This bounds getting ONTO the wire, which no other deadline in the crate
/// does: they all start once the server has been asked. Unlike the response
/// deadline it has no refresh to lean on, so it has to cover a whole
/// worst-case frame in one go: 20 s pushes a 1 MB `MaxWriteSize` frame at
/// 50 KB/s, or an 8 MB one at 400 KB/s — links far worse than any Wi-Fi a
/// transfer would be attempted over.
///
/// Kept below [`RESPONSE_TIMEOUT`] on purpose: a socket that has stopped
/// accepting writes must surface as [`Error::SendTimeout`] and name the send
/// side, not expire upstream and leave an innocent server holding the blame.
const SEND_TIMEOUT: Duration = Duration::from_secs(20);

/// A send slower than this is worth counting even though it succeeded — it is
/// the early warning for the state that used to wedge.
///
/// A threshold on the FRAME, reported per sweep: the writer tallies how many
/// crossed it and how bad the worst one was, and [`sweep_report`] says so once.
/// ❌ Never make it a gate on a log line per frame again. Every frame in a bulk
/// copy over a slow link crosses it — the queue in front of a 6.7 MB/s link is
/// tens of seconds deep by arithmetic — so per-frame it emitted six or seven
/// WARN lines a second for a sixteen-minute transfer, which is the same flood
/// the sweeper's own lines caused (2026-08-26).
const SLOW_SEND_REPORT: Duration = Duration::from_secs(5);

/// How long the server may say nothing, while work is outstanding, before the
/// client asks it directly with an SMB2 ECHO.
///
/// A response deadline on its own cannot tell a slow server from a dead one,
/// which is why [`RESPONSE_TIMEOUT`] has to be sized for the slowest healthy
/// case and is therefore a poor detector of the dead one. ECHO separates them:
/// it touches no disk, no share, and no open handle, so an answer means the
/// server is processing requests and the slow operation deserves more room.
///
/// 5 s of silence, not 5 s on a timer: a connection with responses flowing
/// never probes at all, so a busy transfer pays nothing for this. It only ever
/// fires on the shape that used to wedge — requests outstanding, wire quiet.
///
/// ❌ A missed probe is never a verdict on its own. It means "no extension"
/// and nothing more: a real NAS drops ECHO probes precisely when it is busy
/// writing, which is exactly when a transfer is running (measured on a QNAP
/// TS-464 under write load, 2026-08-02), so anything that tears a connection
/// down over a missed probe kills the healthy transfers this exists to
/// protect.
const KEEPALIVE_AFTER: Duration = Duration::from_secs(5);

/// How many probe thresholds of silence before a connection stops counting as
/// provably alive and starts counting as unresponsive.
///
/// Two would be the tightest reading that still makes sense (one round to
/// notice the silence, one to answer it), but a probe round that runs late on
/// a loaded client would then read as a dead server. One number for both
/// readings of this clock — [`Inner::liveness_is_proven`] and
/// [`Inner::unresponsive_for`] — so no tuning pass can open a gap between
/// "no longer provably alive" and "unresponsive".
const LIVENESS_WINDOW_PROBES: u32 = 3;

/// How much longer a request may go unanswered when the connection is provably
/// alive, as a multiple of the configured response deadline.
///
/// This is what the keepalive buys. The base deadline is a *silence* budget
/// sized for a server we know nothing about; once ECHO says the server is
/// processing requests, silence on one request stops being evidence of death
/// and becomes evidence of a slow operation, which is exactly the thing we
/// must not kill (a large write to a loaded spinning-disk NAS).
///
/// It is a multiple, not its own constant, so a consumer who retunes
/// [`Connection::set_response_timeout`] gets a proportionate ceiling instead of
/// two numbers that quietly stop relating to each other. At the 30 s default
/// the ceiling is 3 minutes: past that, "alive but has not answered this one
/// request" is a server-side stall that waiting cannot fix, and reconnecting
/// beats waiting.
const ALIVE_DEADLINE_FACTOR: u32 = 6;

/// How long one long-poll request may sit registered with the server before
/// the client retires it and issues a fresh one.
///
/// **This is a self-healing cycle, not a detector, and it cannot be anything
/// else.** A CHANGE_NOTIFY the server has silently forgotten and one watching a
/// directory nobody has touched produce the identical observation: nothing. No
/// amount of correlating it against other traffic tells them apart, because a
/// healthy watch is *defined* by hours of silence — which is also why
/// [`is_long_poll`] exempts these from the response deadline to begin with. So
/// the only sound answer is to stop relying on a single subscription surviving
/// indefinitely: re-issue it periodically and a server-side loss heals itself
/// within one cycle, with no verdict to get wrong and no healthy watch ever
/// ended.
///
/// It has to exist because the connection-level bound cannot cover this case.
/// [`Connection::await_long_poll`] ends a watch when the whole wire goes quiet,
/// which catches a dead session — but a server that keeps answering everything
/// else while one subscription is dead is, by that reading, perfectly healthy.
/// Measured against a QNAP TS-464 on 2026-08-03: two CHANGE_NOTIFY requests
/// outstanding for 6,186 s with no response, while `fs_info` on the same
/// connection round-tripped in 4 ms and every ECHO probe was answered.
///
/// 10 minutes trades healing latency against wire chatter: a lost subscription
/// costs at most one cycle of missed events, and the cycle itself costs three
/// frames (one replacement CHANGE_NOTIFY plus a CANCEL for each retired
/// request) per watched directory. ❌ Don't shorten it to "detect faster" — it
/// detects nothing, and each cycle is one more handover an event can slip
/// through.
const LONG_POLL_REFRESH: Duration = Duration::from_secs(600);

/// The unit the write budget is counted in, so one `u32` permit count can
/// express tens of megabytes.
const WRITE_BUDGET_UNIT: u64 = 64 * 1024;

/// How many bytes of WRITE payload may be outstanding across the WHOLE
/// connection at once — launched but not yet acknowledged — until a consumer
/// says otherwise with [`Connection::set_write_budget`].
///
/// This is the bound a per-upload window alone cannot give. Each upload's
/// write-behind window (`write_behind.rs`, at most 4 MiB) says nothing about
/// how many uploads there are, so N concurrent ones multiply it. When that
/// window was a fixed 32 frames, a file manager copying 10 files at once over
/// a 1 MiB `MaxWriteSize` reached 320 MiB of payload queued in this process,
/// and every byte of it was latency the consumer paid for: on a 6.7 MB/s link,
/// ~48 s between handing a chunk over and it reaching the socket (a 2026-08-26
/// Cmdr report, measured on the user's own link). Cancel waits it out,
/// progress reporting runs ahead of it, and the memory is real. A fixed
/// [`WriteBehind`](crate::WriteBehind) window can still get there.
///
/// 32 MiB, chosen against both ends rather than either:
///
/// - **Fast link**: at ~100 MB/s (saturated gigabit) this is 0.3 s of buffer,
///   many times the bandwidth-delay product of a LAN, so the pipe still never
///   runs dry. The measured few-large curve is flat from a 4-wide window up
///   (`docs/notes/transfer-concurrency-window-bench-2026-08-02.md` in the Cmdr
///   repo), so there is no throughput here to lose.
/// - **Slow link**: at 6.7 MB/s it is ~5 s of buffer instead of ~48.
/// - **Memory**: 32 MiB of `Vec<u8>` is a bound a consumer can reason about,
///   where `concurrency x window x max_write` is not.
///
/// A 10 GbE link or a memory-constrained host is exactly where that reasoning
/// runs out, which is why the number is a default rather than a constant of
/// nature: [`Connection::set_write_budget`] moves it.
///
/// ❌ Don't make this adaptive without a measurement that demands it. A fixed
/// bound is predictable and testable; a control loop over a noisy throughput
/// estimate can oscillate, and the numbers above say a well-chosen constant
/// already sits far from both edges.
///
/// A frame larger than the whole budget is clamped to the whole budget rather
/// than refused (`Connection::clamped_units`), so a server negotiating a
/// `MaxWriteSize` above this can always still send one frame.
const DEFAULT_WRITE_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

/// [`WRITE_BUDGET_UNIT`]s needed to cover `bytes`, at least one and never more
/// than a `u32` (which is what `Semaphore::acquire_many` takes).
fn budget_units(bytes: u64) -> u32 {
    u32::try_from(bytes.div_ceil(WRITE_BUDGET_UNIT).max(1)).unwrap_or(u32::MAX)
}

/// How many write-budget permits the semaphore currently holds, and how many it
/// should hold.
///
/// The two differ only while a reduction is outstanding: `Semaphore` can only
/// take back permits that are FREE, so shrinking a budget that is fully spent
/// removes what it can now and the rest as frames complete. `Inner::settle_write_budget`
/// is what pays the difference down, and every reserve calls it first.
#[derive(Debug, Clone, Copy)]
struct WriteBudgetUnits {
    /// What the semaphore holds right now (free plus held).
    issued: u32,
    /// What it should hold, per the last `set_write_budget`.
    target: u32,
}

/// One step toward the connection-wide write budget, for a caller that
/// pipelines WRITE frames of its own.
pub(crate) enum WriteBudgetStep<T> {
    /// The budget is yours. Move the permit INTO the frame's future, so every
    /// path that ends the frame — drained, aborted, cancelled, dropped —
    /// returns it with no explicit release anywhere.
    ///
    /// `None` only if the budget semaphore is closed, which nothing in this
    /// crate does; send the frame unaccounted rather than dropping it.
    Granted(Option<tokio::sync::OwnedSemaphorePermit>),
    /// One of your OWN in-flight frames finished first. Account for it, then
    /// ask again for the frame you still want to send.
    Drained(T),
}

/// Get write budget for one frame without deadlocking the connection.
///
/// ❌ **This ordering is the whole deadlock argument. Don't reorder it, and
/// don't hand-roll a second copy.** A caller parked on the budget is not
/// polling its own in-flight frames, so the frames that would return the budget
/// can never complete and the wait can never end. So, in order:
///
/// 1. Take the budget if it is free. No wait, no risk.
/// 2. Otherwise free our OWN first, by waiting on one of our in-flight frames.
///    That is the only budget we can be sure will come back.
/// 3. Only when we hold nothing at all — `in_flight` is empty — wait on the
///    budget itself, where waiting cannot be part of a cycle.
///
/// Every holder is in the same loop, draining, so a wait at step 3 always ends
/// while the server answers. An empty `in_flight` falls through step 2 for
/// free: a `FuturesUnordered` with nothing in it yields `None` at once.
pub(crate) async fn reserve_write_budget_or_drain<S>(
    conn: &Connection,
    bytes: u64,
    in_flight: &mut S,
) -> WriteBudgetStep<S::Item>
where
    S: futures_util::stream::Stream + Unpin,
{
    use futures_util::stream::StreamExt;

    if let Some(permit) = conn.try_reserve_write_budget(bytes) {
        return WriteBudgetStep::Granted(Some(permit));
    }
    if let Some(finished) = in_flight.next().await {
        return WriteBudgetStep::Drained(finished);
    }
    WriteBudgetStep::Granted(conn.reserve_write_budget(bytes).await)
}

/// How many frames may be queued for the writer task before callers block.
///
/// Backpressure, not a buffer: each transfer already caps itself with its own
/// window, so this only bites when many streams pile on at once, and blocking
/// there is better than growing an unbounded queue of `MaxWriteSize` frames.
const WRITE_QUEUE_DEPTH: usize = 256;

/// How many abandoned MessageIds to remember for response classification.
///
/// Only needs to span one round trip (a response already in flight when its
/// caller gave up), so this is generous.
const ABANDONED_ID_MEMORY: usize = 512;

/// One frame waiting for the socket.
struct WriteJob {
    bytes: Vec<u8>,
    /// For the log line and the [`Error::SendTimeout`]; the first sub-op's
    /// command for a compound.
    command: Command,
    queued_at: std::time::Instant,
    done: oneshot::Sender<Result<()>>,
}

/// A request built and registered, with its bytes as they go on the wire.
struct PreparedRequest {
    msg_id: MessageId,
    guard: WaiterGuard,
    wire: Vec<u8>,
    shape: WireShape,
}

/// What happened to a request's bytes on the way to the wire, for the
/// dispatch `TRACE` line. A value, not a string, since every request makes
/// one and `TRACE` is usually off.
#[derive(Clone, Copy)]
enum WireShape {
    Plain {
        signed: bool,
        len: usize,
    },
    Compressed {
        signed: bool,
        from: usize,
        to: usize,
    },
    Encrypted {
        len: usize,
    },
}

impl std::fmt::Display for WireShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plain { signed, len } => write!(f, "signed={signed}, len={len}"),
            Self::Compressed { signed, from, to } => {
                write!(f, "signed={signed}, compressed {from}->{to} bytes")
            }
            Self::Encrypted { len } => write!(f, "encrypted, len={len}"),
        }
    }
}

/// A request [`Connection::dispatch_without_waiting`] put in the writer
/// task's queue.
///
/// [`response`](Self::response) waits for it to reach the wire and for its
/// answer. Dropping it doesn't take the request back: it goes out anyway,
/// and its answer is discarded when it lands (its credits still bank), the
/// same as for any request whose caller went away.
pub(crate) struct QueuedRequest {
    conn: Connection,
    command: Command,
    msg_id: MessageId,
    guard: WaiterGuard,
    frame: QueuedFrame,
}

impl QueuedRequest {
    /// Wait for the request to reach the wire, then for its answer, under
    /// the connection's usual response deadline.
    pub(crate) async fn response(self) -> Result<Frame> {
        let QueuedRequest {
            conn,
            command,
            msg_id,
            mut guard,
            frame,
        } = self;
        frame.settle().await?;
        conn.inner.mark_sent(&[msg_id]);
        conn.await_response_in_place(&mut guard, command).await
    }
}

/// The `MessageId` of a plain frame's first request, for the writer's `TRACE`
/// line; `None` for an encrypted or compressed frame, whose header is opaque.
fn first_message_id(frame: &[u8]) -> Option<u64> {
    if frame.len() < 32 || frame[0..4] != [0xFE, b'S', b'M', b'B'] {
        return None;
    }
    Some(u64::from_le_bytes(frame[24..32].try_into().ok()?))
}

/// The end of one socket's life, shared by the two tasks that hold its halves.
///
/// The writer task and the receiver task each own a handle to the same
/// transport (`TcpTransport` keeps both halves in one object), so the socket
/// closes only once BOTH have exited. Neither one exiting may leave the other
/// parked: a server hang-up used to end the receiver while the writer waited
/// for a frame that could never come, and the socket sat in `CLOSE_WAIT` for as
/// long as the `Connection` lived.
///
/// So each task ends it on the way out, whatever the way out is (see
/// [`EndsSocketLife`]), and each exits once the other has. [`fan_error_to_waiters`]
/// ends it too, which is what makes "the connection is dead" and "the socket is
/// closed" the same statement: a consumer holding a dead connection until its
/// next operation fails holds no socket meanwhile.
///
/// One per generation. A revival hands its tasks a fresh one, so ending the
/// dead socket's life can never reach the new socket's tasks.
#[derive(Clone)]
struct SocketLife(Arc<watch::Sender<bool>>);

impl SocketLife {
    fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    /// Tell both tasks to let go of the socket. Idempotent.
    fn end(&self) {
        self.0.send_replace(true);
    }

    /// Resolves once [`end`](Self::end) has been called, at once if it already
    /// has been.
    async fn ended(&self) {
        let mut rx = self.0.subscribe();
        // Can't fail: `self` holds the sender.
        let _ = rx.wait_for(|ended| *ended).await;
    }
}

/// Ends a [`SocketLife`] when dropped, so a task holding one of the socket's
/// halves ends it however the task stops: returning, panicking, or being
/// aborted.
struct EndsSocketLife(SocketLife);

impl Drop for EndsSocketLife {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// The only task that touches the transport's write half.
///
/// Callers hand over whole frames and wait for an ack, which buys three
/// things the old "every caller locks the socket" shape could not give:
///
/// 1. **A dropped caller cannot desynchronize the stream.** `TcpTransport::send`
///    writes a 4-byte length header and then the body; a caller cancelled
///    between the two used to leave a header with no body on the wire, and
///    every later frame landed inside it. A frame handed to this task is sent
///    whole or not at all, and consumers abort these futures routinely (a user
///    cancelling a copy).
/// 2. **A stuck write is bounded and named.** It is this task's own deadline,
///    not an invisible queue behind a lock.
/// 3. **The backlog is observable** (`send_queue_depth`), so a wedge reports
///    itself instead of looking like server silence.
///
/// A write that times out or errors tears the connection down: bytes may have
/// reached the socket, so the stream can no longer be trusted. A frame
/// rejected before any byte was written (oversized) is the caller's problem
/// alone and leaves the connection alive.
///
/// Exits once the socket's [`SocketLife`] ends, so a connection declared dead
/// lets go of the socket straight away. A frame already being written finishes
/// (or times out) first; the ones still queued answer `Error::Disconnected`.
async fn writer_loop(
    sender: Arc<dyn TransportSend>,
    mut rx: mpsc::Receiver<WriteJob>,
    inner: Weak<Inner>,
    life: SocketLife,
) {
    let _ends_life = EndsSocketLife(life.clone());
    let mut ended = pin!(life.ended());
    loop {
        // `ended` first: once the socket is finished with, nothing queued
        // behind it should still reach the wire.
        let job = match select(ended.as_mut(), pin!(rx.recv())).await {
            Either::Left(_) => return,
            Either::Right((Some(job), _)) => job,
            Either::Right((None, _)) => return, // every `write_tx` dropped
        };
        let Some(strong) = inner.upgrade() else {
            return; // last Connection clone dropped
        };
        let deadline = *strong.send_timeout.lock().unwrap();
        let len = job.bytes.len();
        let started = std::time::Instant::now();

        let result = match deadline {
            Some(d) => match rt::timeout(d, sender.send(&job.bytes)).await {
                Ok(r) => r,
                Err(_) => Err(Error::SendTimeout {
                    command: job.command,
                    bytes: len,
                    waited: job.queued_at.elapsed(),
                }),
            },
            None => sender.send(&job.bytes).await,
        };

        let wrote_in = started.elapsed();
        let queued_for = started.saturating_duration_since(job.queued_at);
        if result.is_ok() {
            // Tallied, not logged. The sweeper reports the whole interval in
            // one line — see [`SLOW_SEND_REPORT`] for what a line each cost.
            // Splitting queue time from write time is still the whole
            // diagnostic, so the tally keeps both: time in the queue means an
            // earlier frame is slow, time in the write means this socket is.
            strong.send_tally.record(len, queued_for, wrote_in);
        }
        trace!(
            "send: cmd={:?}, msg_id={}, {} bytes, {:?} queued + {:?} writing",
            job.command,
            first_message_id(&job.bytes).map_or_else(|| "?".to_string(), |id| id.to_string()),
            len,
            queued_for,
            wrote_in
        );

        // The transport's raw io error becomes the connection-lost error it
        // is: the caller must not mistake it for a local `Error::Io`, which
        // says nothing about the connection.
        let result = match result {
            Err(Error::Io(source)) => Err(Error::SendFailed {
                command: job.command,
                source,
            }),
            other => other,
        };
        let fatal = matches!(
            result,
            Err(Error::SendTimeout { .. }) | Err(Error::SendFailed { .. })
        );
        if fatal {
            if let Err(ref e) = result {
                strong.metrics.send_failures.fetch_add(1, Ordering::Relaxed);
                error!(
                    "send failed after {:?}: cmd={:?}, {} bytes: {e}; tearing the connection down \
                     because a partly-written frame leaves the stream out of sync",
                    wrote_in, job.command, len
                );
            }
            // Before answering the caller, so it finds the connection already
            // dead: a caller that reacts by checking `is_disconnected` or by
            // reviving must not see the dead socket as live.
            fan_error_to_waiters(&strong, &Error::Disconnected);
            let _ = job.done.send(result);
            return;
        }
        let _ = job.done.send(result);
    }
}

/// Commands whose whole job is to wait for something that may never happen.
///
/// CHANGE_NOTIFY sits open until the watched directory changes — hours is
/// normal, and timing it out would break the watcher rather than protect it.
///
/// Being exempt from the *request's* deadline is exactly why these need the
/// keepalive: with no clock of their own, a dead server would leave a watcher
/// waiting for an event that could never arrive. They are bounded by the
/// connection instead of by themselves — see
/// [`Connection::await_long_poll`] — which is why a connection holding a
/// watcher open sends a periodic ECHO.
fn is_long_poll(command: Command) -> bool {
    matches!(command, Command::ChangeNotify)
}

/// One outstanding request, named well enough to diagnose from.
#[derive(Debug, Clone, Copy)]
struct Outstanding {
    msg_id: MessageId,
    command: Command,
    /// Since the waiter was registered, which is BEFORE the bytes reach the
    /// transport.
    age: std::time::Duration,
}

/// A request the server has been asked and has not answered.
#[derive(Debug, Clone, Copy)]
struct Unanswered {
    request: Outstanding,
    /// Since the transport accepted the frame. The only clock that can be read
    /// as "the server is quiet", which is why this bucket is the only one that
    /// blames it.
    sent_age: std::time::Duration,
}

/// Everything in flight, split by which diagnosis it supports.
///
/// Three populations, three different things to say about them, and conflating
/// any two of them is a misdiagnosis: the server owing an answer, the send
/// queue owing a socket, and a long poll waiting for an event that may never
/// come. Separated from the logging so the split itself can be tested: which
/// bucket a request lands in is the decision, and the rest is formatting.
#[derive(Debug, Default)]
struct OutstandingSplit {
    /// Sent, past the threshold, unanswered. The server owes us a response.
    unanswered: Vec<Unanswered>,
    /// Past the threshold and never sent. Waiting on the send queue.
    queued: Vec<Outstanding>,
    /// Long polls, at any age. Not stale by construction — see [`is_long_poll`].
    parked: Vec<Outstanding>,
}

fn classify_outstanding(inner: &Inner, threshold: std::time::Duration) -> OutstandingSplit {
    let now = std::time::Instant::now();
    let mut split = OutstandingSplit::default();
    for (id, w) in inner.waiters.lock().unwrap().iter() {
        let request = Outstanding {
            msg_id: *id,
            command: w.command,
            age: now.saturating_duration_since(w.registered_at),
        };
        if is_long_poll(w.command) {
            split.parked.push(request);
        } else if request.age >= threshold {
            match w.sent_at {
                Some(sent) => split.unanswered.push(Unanswered {
                    request,
                    sent_age: now.saturating_duration_since(sent),
                }),
                None => split.queued.push(request),
            }
        }
    }
    split
}

/// Whether the send side is still moving.
///
/// The queue depth alone cannot tell a slow link from a wedged writer: both
/// look like hundreds of frames waiting. Only "is anything still reaching the
/// socket" separates them, and that difference is the whole reason one of these
/// is a warning and the other is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendSide {
    /// Frames are still reaching the socket, or have recently enough that a
    /// slow link explains the gap. A deep queue here is the link being slow,
    /// which is the transfer working.
    Draining,
    /// Nothing has reached the socket for longer than a whole frame is allowed
    /// to take. Whatever is wrong is on our side of the wire.
    Stalled,
}

/// How long the send side may produce nothing before a sweep calls it stalled.
///
/// Deliberately the send deadline itself, because that is already the crate's
/// answer to "how long may one frame take to reach the socket", and a second,
/// tighter opinion here would warn about links [`SEND_TIMEOUT`] was explicitly
/// sized to accept.
///
/// **Why anything shorter is wrong.** `wire_bytes_sent` only ticks when a WHOLE
/// frame is out, so between completions it stands still by construction, and
/// the gap between them is the per-frame write time. Judging a single sweep
/// interval would therefore call any link slower than one frame per
/// [`STALE_WAITER_SWEEP`] a wedge: ~105 KB/s at a 1 MiB `MaxWriteSize`, ~840
/// KB/s at the 8 MiB one Windows and several NAS boxes negotiate. `SEND_TIMEOUT`
/// is sized for 50 KB/s and 400 KB/s respectively, so that shorter window would
/// warn across the entire band between the two — ordinary Wi-Fi, and precisely
/// the false positive the collapse exists to remove. Tying it to the send
/// deadline closes the gap by construction: a link the send path tolerates is a
/// link this never calls stalled, and one it doesn't tolerate tears itself down
/// with [`Error::SendTimeout`] anyway.
///
/// A consumer who sets `send_timeout` to `None` has removed the teardown, not
/// the diagnosis — which makes this the ONLY thing left that can name a wedged
/// send side, so it falls back to the default rather than going quiet.
/// `set_stale_request_warning(None)` is the knob for silence.
///
/// Floored at two sweeps so a verdict always rests on more than one look.
fn stall_tolerance(send_timeout: Option<Duration>) -> Duration {
    send_timeout
        .unwrap_or(SEND_TIMEOUT)
        .max(STALE_WAITER_SWEEP * 2)
}

/// What one sweep learned about the send side.
#[derive(Debug, Clone, Copy)]
struct SendSideReading {
    state: SendSide,
    /// How long since a whole frame last reached the socket, as of this sweep.
    ///
    /// Measured from the sweep that SAW the counter move, not from the move
    /// itself, so it can lag reality by up to one sweep — conservative in the
    /// right direction: it can only ever delay a verdict, never rush one.
    silent_for: Duration,
    /// Wall time since the previous sweep: the window [`SendActivity`] covers.
    /// Read from the clock rather than assumed to be [`STALE_WAITER_SWEEP`],
    /// because a process that went unscheduled did not stop time.
    window: Duration,
}

/// The sweeper's memory of the send side.
#[derive(Debug)]
struct SendProgress {
    /// `metrics.wire_bytes_sent` as of the previous sweep.
    ///
    /// Deliberately the same reading `CreditInfo::send_queue_depth` documents
    /// for consumers ("steadily non-zero while `wire_bytes_sent` stands still
    /// means the send side is stuck"), so the crate's own warning and a
    /// consumer's dashboard can't disagree.
    wire_bytes_sent: u64,
    /// When a sweep last saw that number change.
    last_progress: Instant,
    /// When the previous sweep ran.
    last_sweep: Instant,
}

/// What the writer task has done since the sweeper last drained it.
///
/// Atomics rather than a lock: the writer touches this once per frame on the
/// hot path, and the sweeper reads it once every [`STALE_WAITER_SWEEP`].
#[derive(Debug, Default)]
struct SendTally {
    frames: AtomicU64,
    bytes: AtomicU64,
    slow_frames: AtomicU64,
    worst_queued_nanos: AtomicU64,
    worst_writing_nanos: AtomicU64,
}

impl SendTally {
    /// Note one frame that reached the socket.
    fn record(&self, bytes: usize, queued_for: Duration, wrote_in: Duration) {
        self.frames.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        if queued_for >= SLOW_SEND_REPORT || wrote_in >= SLOW_SEND_REPORT {
            self.slow_frames.fetch_add(1, Ordering::Relaxed);
        }
        self.worst_queued_nanos
            .fetch_max(queued_for.as_nanos() as u64, Ordering::Relaxed);
        self.worst_writing_nanos
            .fetch_max(wrote_in.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Take everything since the last drain and start a fresh interval.
    fn drain(&self) -> SendActivity {
        SendActivity {
            frames: self.frames.swap(0, Ordering::Relaxed),
            bytes: self.bytes.swap(0, Ordering::Relaxed),
            slow_frames: self.slow_frames.swap(0, Ordering::Relaxed),
            worst_queued: Duration::from_nanos(self.worst_queued_nanos.swap(0, Ordering::Relaxed)),
            worst_writing: Duration::from_nanos(
                self.worst_writing_nanos.swap(0, Ordering::Relaxed),
            ),
        }
    }
}

/// What the send side actually achieved over one sweep interval.
#[derive(Debug, Clone, Copy, Default)]
struct SendActivity {
    frames: u64,
    bytes: u64,
    /// Of those, how many crossed [`SLOW_SEND_REPORT`] queueing or writing.
    slow_frames: u64,
    worst_queued: Duration,
    worst_writing: Duration,
}

/// Periodically report what the connection is doing: the send side's progress,
/// and any request that has gone unanswered, for as long as the connection
/// lives.
///
/// A separate task rather than a check inside `receiver_loop`: the loop is parked
/// in `transport_recv.receive()` exactly when a wedged connection most needs
/// reporting, and racing that read with a timer would mean dropping a read that
/// is not necessarily cancel-safe. Holds a `Weak` so it exits once the last
/// `Connection` clone drops.
fn spawn_connection_sweeper(inner: &Arc<Inner>) {
    let weak = Arc::downgrade(inner);
    let handle = rt::spawn(async move {
        loop {
            rt::sleep(STALE_WAITER_SWEEP).await;
            let Some(inner) = weak.upgrade() else {
                return; // connection dropped
            };
            if inner.disconnected.load(Ordering::Acquire) {
                return;
            }
            sweep_connection(&inner);
        }
    });
    if let Some(old) = inner.sweeper_task.lock().unwrap().replace(handle) {
        old.abort();
    }
}

/// How a long poll's wait ended.
///
/// A long poll is the one request in the crate that can finish without the
/// server having said anything, so "did we get a frame" is not a yes/no
/// question here.
pub(crate) enum LongPollOutcome {
    /// The server answered.
    Answered(Frame),
    /// The request has been registered with the server longer than the refresh
    /// interval, so it has been given up on. **Nothing is wrong** — see
    /// [`LONG_POLL_REFRESH`].
    ///
    /// Its waiter is already deregistered. The caller owes the server two
    /// things, in this order: a replacement request (so the wire is never left
    /// unarmed, which is what the pipelined `Watcher` exists to guarantee) and
    /// then a CANCEL carrying these ids, without which the server accumulates
    /// one abandoned subscription per cycle for the life of the watch.
    RefreshDue {
        /// The retired request's id.
        msg_id: MessageId,
        /// The `AsyncId` the server gave it, which its CANCEL must carry
        /// (MS-SMB2 § 3.2.4.24).
        async_id: Option<u64>,
        /// The session generation those ids belong to, which its CANCEL must
        /// also carry: a revival in between makes them name nothing.
        generation: u64,
    },
}

/// What one probe round learned about the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeOutcome {
    /// It answered. It is processing requests, so the liveness clock it just
    /// refreshed can be trusted.
    Alive,
    /// The probe reached the wire and nothing came back. Costs the connection
    /// its deadline extension until a frame does arrive, and nothing else.
    Silent,
    /// Nothing was asked, so nothing was learned. ❌ Never treat this as a
    /// failure — see [`Connection::echo_probe`].
    Skipped,
    /// The connection failed underneath the probe and is already being torn
    /// down by whoever noticed.
    Broken,
}

/// Ask a quiet server whether it is still there.
///
/// This is what makes an aggressive response deadline safe. A deadline alone
/// cannot tell "the server is slow" from "the server is dead", so it has to be
/// sized for the slowest healthy case, which makes it a poor detector of the
/// dead one — and a 2026-07-31 QNAP wedge is what that costs. ECHO separates
/// them: an answer means the connection has earned more patience for the
/// operation that is running long.
///
/// **The answer is the loop's only product.** It refreshes the liveness clock
/// and nothing else; every decision that can cost a caller something is the
/// response deadline's, in [`Connection::await_response`]. See
/// [`KEEPALIVE_AFTER`] for why a missed probe must never be a verdict.
///
/// A separate task rather than work inside `receiver_loop`, for the same
/// reason as the stale-waiter sweeper: the receive loop is parked inside
/// `transport_recv.receive()` exactly when a wedged connection most needs
/// probing. Holds a `Weak` so it exits once the last `Connection` clone drops.
fn spawn_keepalive(inner: &Arc<Inner>) {
    let weak = Arc::downgrade(inner);
    let handle = rt::spawn(async move { keepalive_loop(weak).await });
    if let Some(old) = inner.keepalive_task.lock().unwrap().replace(handle) {
        old.abort();
    }
}

/// Spawn the four background tasks a live connection needs, retiring any
/// previous generation's.
///
/// Shared by construction and revival so the two can never drift: a revived
/// connection has to end up with exactly the plumbing a fresh one has, and
/// three of these four tasks exit for good when `disconnected` flips — so a
/// revival that only replaced the transport would come back with no keepalive
/// and no stale-request warning, silently.
fn spawn_plumbing(
    inner: &Arc<Inner>,
    sender: Box<dyn TransportSend>,
    receiver: Box<dyn TransportReceive>,
    write_rx: mpsc::Receiver<WriteJob>,
) {
    let sender: Arc<dyn TransportSend> = Arc::from(sender);

    // A fresh life for the fresh socket. The retired one is ended here too,
    // though a revival has normally ended it already by declaring the old
    // socket dead.
    let life = SocketLife::new();
    std::mem::replace(&mut *inner.socket_life.lock().unwrap(), life.clone()).end();

    // Every task holds a `Weak`, so none of them can keep the connection
    // alive. ❌ Never hand one an `Arc<Inner>` to keep: only `Inner::drop`
    // aborts these tasks, so a task holding `Inner` alive is a cycle, and
    // dropping every `Connection` would leave the socket open until the
    // SERVER hangs up (never, on a server that doesn't reap idle sessions).
    let weak = Arc::downgrade(inner);
    let writer_life = life.clone();
    let writer = rt::spawn(async move {
        writer_loop(sender, write_rx, weak, writer_life).await;
    });
    if let Some(old) = inner.writer_task.lock().unwrap().replace(writer) {
        old.abort();
    }

    // Before the receiver task exists, so it can't count a frame into the
    // counter being retired.
    let feed = inner.adopt_receive_progress(receiver.receive_progress());
    let weak = Arc::downgrade(inner);
    let handle = rt::spawn(async move {
        receiver_loop(receiver, weak, feed, life).await;
    });
    if let Some(old) = inner.receiver_task.lock().unwrap().replace(handle) {
        old.abort();
    }

    spawn_connection_sweeper(inner);
    spawn_keepalive(inner);
}

async fn keepalive_loop(weak: Weak<Inner>) {
    loop {
        // Re-read the cadence every round so `set_keepalive` takes effect
        // without a restart. While probing is off we still wake on the default
        // interval, so turning it back on doesn't need one either.
        //
        // ❌ Don't "save wakeups" by sleeping longer while the connection is
        // idle. The sleep is chosen before the check, so a coarse idle
        // interval delays the NEXT round too — including the one that would
        // notice a consumer's `set_keepalive` — and a setter that takes
        // effect five seconds later is a setter nobody can reason about. One
        // timer per second per connection is not the cost worth chasing.
        let tick = match weak.upgrade() {
            Some(inner) => {
                let after = (*inner.keepalive_after.lock().unwrap()).unwrap_or(KEEPALIVE_AFTER);
                Inner::keepalive_tick(after)
            }
            None => return, // last Connection clone dropped
        };
        rt::sleep(tick).await;

        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.disconnected.load(Ordering::Acquire) {
            return;
        }
        // The guaranteed heartbeat behind the stall witness. Every wait loop
        // corrects for a stall too, but only this one runs on a cadence of its
        // own, so only this one notices a stall on a connection with nothing
        // outstanding — and the clocks have to be right BEFORE the next
        // request arrives, not after it has already been misjudged.
        inner.forgive_scheduling_stall();
        let Some(after) = *inner.keepalive_after.lock().unwrap() else {
            continue;
        };
        // Nothing on the wire (no work to protect), or the server has spoken
        // recently enough (there is nothing to ask about). Either way a probe
        // would cost a round trip and buy nothing, which is why a busy
        // connection never sends one.
        match inner.quiet_for() {
            Some(quiet) if quiet >= after => {}
            _ => continue,
        }

        let conn = Connection {
            inner: Arc::clone(&inner),
            pin: None,
        };
        // An answer refreshes `last_frame_at` on its way through the receiver
        // task, which is the whole product of this loop: a request on a
        // connection proven alive that way gets [`ALIVE_DEADLINE_FACTOR`]
        // times the response deadline. A miss leaves the clock stale, which
        // costs the connection that extension and nothing else.
        //
        // ❌ Never conclude anything harsher from here. The one outcome worth
        // acting on is a connection somebody else has already torn down, and
        // only because there is nothing left to probe.
        if conn.echo_probe(after).await == ProbeOutcome::Broken {
            return;
        }
    }
}

/// `Write x318, Read x2`, busiest first — the shape of a backlog in one
/// phrase, which is what a reader needs before they need message ids.
fn command_mix(requests: &[Outstanding]) -> String {
    let mut counts: Vec<(Command, usize)> = Vec::new();
    for r in requests {
        match counts.iter_mut().find(|(c, _)| *c == r.command) {
            Some(entry) => entry.1 += 1,
            None => counts.push((r.command, 1)),
        }
    }
    // Then by command code, so the same backlog reads the same sweep to sweep
    // and a reader can diff two lines.
    counts.sort_by_key(|(c, n)| (std::cmp::Reverse(*n), u16::from(*c)));
    counts
        .iter()
        .map(|(c, n)| format!("{c:?} x{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The per-request detail the collapsed lines stand in for.
fn describe_requests(requests: &[Outstanding], verb: &str) -> String {
    requests
        .iter()
        .map(|r| format!("{:?}/{} {verb} {:?}", r.command, r.msg_id.0, r.age))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A throughput a reader can judge without doing arithmetic.
fn format_rate(bytes: u64, over: Duration) -> String {
    let seconds = over.as_secs_f64();
    if seconds <= 0.0 {
        return "unknown rate".to_string();
    }
    let per_second = bytes as f64 / seconds;
    if per_second >= 1_000_000.0 {
        format!("{:.1} MB/s", per_second / 1_000_000.0)
    } else if per_second >= 1_000.0 {
        format!("{:.0} KB/s", per_second / 1_000.0)
    } else {
        format!("{per_second:.0} B/s")
    }
}

/// What the send side achieved over the sweep window, in one clause.
fn describe_activity(activity: &SendActivity, window: Duration) -> String {
    let mut clause = format!(
        "{} frame(s) at {} reached the socket in the last {:?}",
        activity.frames,
        format_rate(activity.bytes, window),
        window
    );
    if activity.slow_frames > 0 {
        // The split is the diagnostic: time in the queue means an earlier
        // frame is slow, time in the write means this socket is.
        clause.push_str(&format!(
            ", {} of them slower than {:?} (worst {:?} queued + {:?} writing)",
            activity.slow_frames, SLOW_SEND_REPORT, activity.worst_queued, activity.worst_writing
        ));
    }
    clause
}

/// What one sweep has to say, as records for the caller to log.
///
/// Pure so the VOLUME can be tested and not just the wording: "one collapsed
/// line for the send side, never one per frame" is a property of this
/// function's return length, and a regression to per-frame lines shows up as a
/// longer `Vec`.
///
/// The populations get different levels, because they are different claims:
///
/// - **Unanswered** (sent, and the server has said nothing since): WARN,
///   always. The server owes us a response and is not producing one; there is
///   no reading of that which is healthy.
/// - **The send side** (frames queued for the socket, and what the writer got
///   out): the level depends entirely on [`SendSide`], and that split is the
///   whole point. A deep queue that is DRAINING is a slow link with a lot
///   buffered in front of it — the frames are moving at exactly the rate the
///   link can carry, the transfer is working, and calling that a warning is the
///   crate crying wolf about physics. It gets INFO: worth a line, since it is
///   what explains a transfer that feels slow, but not a fault. A send side
///   that has produced nothing for longer than [`stall_tolerance`] is the
///   genuine wedge, and keeps WARN. INFO rather than DEBUG on purpose:
///   consumers ship INFO in their diagnostic bundles, and a bundle that cannot
///   show the link was the bottleneck is a bundle that sends the next
///   investigation after the server again.
/// - **Parked** long polls: TRACE normally (waiting for an event is what they
///   are for), promoted to WARN alongside a genuine wedge, since an
///   investigation wants the whole in-flight picture at the level it is
///   reading.
///
/// An idle-and-healthy connection says nothing at all: the send-side record
/// needs either a backlog past the threshold or a frame that was actually slow.
fn sweep_report(
    split: &OutstandingSplit,
    reading: &SendSideReading,
    activity: &SendActivity,
    queue_depth: usize,
) -> Vec<(Level, String)> {
    let mut records = Vec::new();

    for u in &split.unanswered {
        records.push((
            Level::Warn,
            format!(
                "outstanding request: cmd={:?}, msg_id={}, sent {:?} ago, no response",
                u.request.command, u.request.msg_id.0, u.sent_age
            ),
        ));
    }

    // One line for the whole send side. The 2026-08-26 incident that made this
    // a collapse rather than a loop: 282 × 66 MB copied to a NAS over 6.7 MB/s
    // Wi-Fi kept ~320 one-MiB frames queued at all times, and a line each per
    // sweep made 4,487 of the 4,744 lines in the user's diagnostic bundle this
    // one message — a bundle covering 2 minutes 30 seconds, with everything
    // that would have explained anything already rotated out.
    let backlog = split.queued.iter().map(|q| q.age).max().map(|oldest| {
        format!(
            "{} request(s) waiting for the socket ({}), oldest queued {oldest:?} ago, \
             {queue_depth} frame(s) in the queue",
            split.queued.len(),
            command_mix(&split.queued)
        )
    });
    match (&backlog, reading.state) {
        // The line that names a send-side wedge instead of blaming the server:
        // nothing was asked, so nothing can be expected back.
        (Some(backlog), SendSide::Stalled) => records.push((
            Level::Warn,
            format!(
                "send queue is NOT draining: not one byte has reached the socket in {:?}, \
                 with {backlog}",
                reading.silent_for
            ),
        )),
        (Some(backlog), SendSide::Draining) => records.push((
            Level::Info,
            format!(
                "send queue is deep but draining: {backlog}; {}",
                describe_activity(activity, reading.window)
            ),
        )),
        // No backlog worth naming, but the writer is working harder than it
        // should have to. The early warning, once per sweep.
        (None, _) if activity.slow_frames > 0 => records.push((
            Level::Info,
            format!(
                "send side is slow but moving: {}",
                describe_activity(activity, reading.window)
            ),
        )),
        (None, _) => {}
    }

    if !split.parked.is_empty() {
        let wedged = !split.unanswered.is_empty()
            || (!split.queued.is_empty() && reading.state == SendSide::Stalled);
        let parked = describe_requests(&split.parked, "parked");
        records.push(if wedged {
            (
                Level::Warn,
                format!(
                    "also outstanding, but waiting for events by design rather than stalled: \
                     {parked}"
                ),
            )
        } else {
            // The healthy shape, and by far the common one. Nothing is wrong,
            // so nothing above TRACE should say anything.
            (
                Level::Trace,
                format!("long polls outstanding (waiting for events, as designed): {parked}"),
            )
        });
    }

    records
}

/// Report what one look at a live connection found: what the send side is
/// doing, and anything outstanding longer than `STALE_WAITER_AFTER`.
///
/// Deliberately re-reports on every sweep: a connection that keeps serving small
/// requests while a large write hangs looks healthy by every other measure, so
/// the repetition is the signal. What it may NOT do is repeat per request or
/// per frame — see [`sweep_report`] for the collapse and for how each
/// population earns its level.
///
/// ❌ **Long polls are not warned about.** The premise of every line here is
/// "this should have come back by now", and for a CHANGE_NOTIFY that premise is
/// false by construction — [`is_long_poll`] exempts it from the deadline
/// precisely because hours of silence is the healthy case, so warning about it
/// every sweep is the sweeper contradicting the deadline. A file manager
/// watching two panes produced 5,911 WARN lines in six hours that way, about
/// requests nothing was ever going to answer, and noise at that volume costs
/// the genuine lines their meaning. They stay observable two ways instead: at
/// TRACE every sweep, and named in full whenever a REAL stale request is
/// warned about, since a wedge investigation wants the whole in-flight picture.
fn sweep_connection(inner: &Inner) {
    // Read before anything can return early, so the send side's clock and the
    // writer's tally stay honest across a consumer toggling the warning off
    // and back on. Both are consumed by whoever looks, so exactly one look per
    // sweep is the contract.
    let tolerance = stall_tolerance(*inner.send_timeout.lock().unwrap());
    let reading = inner.observe_send_side(Instant::now(), tolerance);
    let activity = inner.send_tally.drain();

    let Some(threshold) = *inner.stale_request_after.lock().unwrap() else {
        return; // consumer turned the warning off
    };
    let split = classify_outstanding(inner, threshold);
    let queue_depth = inner.send_queue_depth.load(Ordering::Relaxed);
    for (level, message) in sweep_report(&split, &reading, &activity, queue_depth) {
        log::log!(level, "{message}");
    }
    // The frame-by-frame detail the collapsed send-side line stands in for.
    // Built lazily, unlike the parked list above: there can be hundreds of
    // these, and nobody with TRACE off should pay to format them.
    if !split.queued.is_empty() {
        trace!(
            "not yet on the wire, frame by frame: {}",
            describe_requests(&split.queued, "waiting")
        );
    }
}

use crate::client::credits::{self, CreditPool, CreditReservation};
use crate::client::read_ahead;
use crate::client::write_behind;
use crate::crypto::compression::{compress_message, decompress_message, CompressedMessage};
use crate::crypto::encryption::{self, Cipher, NonceGenerator};
use crate::crypto::kdf::PreauthHasher;
use crate::crypto::signing::{self, SigningAlgorithm};
use crate::error::{Error, Result};
use crate::msg::echo::EchoRequest;
use crate::msg::header::Header;
use crate::msg::negotiate::{
    NegotiateContext, NegotiateRequest, NegotiateResponse, CIPHER_AES_128_CCM, CIPHER_AES_128_GCM,
    CIPHER_AES_256_CCM, CIPHER_AES_256_GCM, COMPRESSION_LZ4, HASH_ALGORITHM_SHA512,
    SIGNING_AES_CMAC, SIGNING_AES_GMAC, SIGNING_HMAC_SHA256,
};
use crate::msg::transform::{
    CompressionTransformHeader, TransformHeader, COMPRESSION_ALGORITHM_LZ4,
    COMPRESSION_PROTOCOL_ID, SMB2_COMPRESSION_FLAG_NONE, TRANSFORM_PROTOCOL_ID,
};
use crate::pack::{Guid, Pack, ReadCursor, Unpack, WriteCursor};
use crate::rt;
use crate::transport::{ReceiveProgress, TcpTransport, TransportReceive, TransportSend};
use crate::types::flags::{Capabilities, HeaderFlags, SecurityMode};
use crate::types::status::NtStatus;
use crate::types::{
    Command, CreditCharge, Dialect, FileId, MessageId, OplockLevel, SessionId, TreeId,
};

// ── Reconnect ──────────────────────────────────────────────────────────────

/// How to bring a dead connection back on a fresh socket.
///
/// The crate can detect a dead session on its own ([`Error::ServerUnresponsive`],
/// [`Error::Disconnected`]) but it cannot re-establish one: negotiate is its
/// job, authentication needs credentials it deliberately does not keep, and
/// dialing needs an address. A consumer supplies all three by installing a
/// reviver with [`Connection::set_reviver`]; [`SmbClient`](crate::SmbClient)
/// installs one for you when [`ClientConfig::auto_reconnect`](crate::ClientConfig::auto_reconnect)
/// is set.
///
/// Both methods are called with the revival lock held, so exactly one runs at
/// a time per connection and neither needs to be reentrant. Neither is given
/// its own deadline: the whole revival, dial and authentication together, runs
/// under one [`ReconnectPolicy::total_budget`] timeout, so an implementation
/// that parks forever is still bounded.
#[async_trait::async_trait]
pub trait SessionReviver: Send + Sync {
    /// Open a fresh transport to the server.
    ///
    /// Return the two halves of a brand-new connection. ❌ Never hand back the
    /// halves of the connection being revived: it is being torn down around
    /// you.
    async fn dial(&self) -> Result<(Box<dyn TransportSend>, Box<dyn TransportReceive>)>;

    /// Re-run NEGOTIATE and SESSION_SETUP on the revived connection.
    ///
    /// Called after the new transport is installed and every scrap of the dead
    /// session's state has been cleared, so this sees a connection in exactly
    /// the state a freshly constructed one is in. Tree connects are the
    /// consumer's business: the tree ids of the old session are gone, and only
    /// the consumer knows which shares still matter.
    async fn reauthenticate(&self, conn: &mut Connection) -> Result<()>;
}

/// Bounds on bringing a connection back.
///
/// The entire point of this machinery is killing a transfer that hangs
/// forever, so a reconnect loop that can spin forever would reintroduce the
/// bug in a new costume. Every field here exists to make that unreachable:
/// attempts are counted, backoff is capped, and the whole thing runs under one
/// wall-clock timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Dial attempts per revival, including the first. `0` disables reviving.
    pub max_attempts: u32,
    /// Pause before the second attempt. Doubles each round, capped at
    /// [`max_backoff`](Self::max_backoff).
    pub initial_backoff: Duration,
    /// Ceiling on the backoff, so a server that is refusing connections is not
    /// hammered and the wait between attempts stays legible.
    pub max_backoff: Duration,
    /// Hard wall-clock ceiling on one revival, dial and authentication and
    /// every backoff included.
    ///
    /// This is the bound that matters: the whole revival runs inside a single
    /// timeout of this length, so no attempt, however wedged, can outlive it.
    pub total_budget: Duration,
    /// How long a failed revival's verdict stands before another caller is
    /// allowed to try again.
    ///
    /// Without it, each of a deep pipeline's callers pays the full
    /// [`total_budget`](Self::total_budget) in turn against a server that is
    /// plainly gone.
    pub failure_cooldown: Duration,
}

impl Default for ReconnectPolicy {
    /// The shipping bounds.
    ///
    /// Sized for the failures that actually recover — a Wi-Fi roam (1–5 s), a
    /// share flapping, a switch relearning a port — and deliberately NOT for a
    /// full NAS reboot (30–90 s). Sitting on a frozen transfer for minutes on
    /// the chance the box comes back is the behavior this whole effort exists
    /// to delete; surfacing a typed error at a minute lets the consumer retry
    /// the file, ask the user, or give up, all of which beat a silent stall.
    ///
    /// Four attempts at 0.5 s → 1 s → 2 s of backoff leave the budget almost
    /// entirely to the dials themselves.
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(8),
            total_budget: Duration::from_secs(60),
            failure_cooldown: Duration::from_secs(10),
        }
    }
}

/// A consumer's hook for [`ReconnectEvent`]s. See
/// [`Connection::on_reconnect`].
pub type ReconnectObserver = Arc<dyn Fn(ReconnectEvent) + Send + Sync>;

/// Something worth telling the consumer about a reconnect, as it happens.
///
/// A counter says how flaky a link has been to whoever polls it later; a log
/// line is for a human reading a bug report. Neither can put "reconnected to
/// the NAS, resuming" in front of someone watching a progress dialog, which is
/// why this is pushed. See [`Connection::on_reconnect`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReconnectEvent {
    /// A revival attempt is about to dial.
    Started {
        /// 1-based attempt number within this revival.
        attempt: u32,
        /// The ceiling from [`ReconnectPolicy::max_attempts`].
        of: u32,
    },
    /// The connection is live and authenticated again.
    ///
    /// Handles, tree connects, and file ids from the old session are gone; the
    /// consumer re-establishes what it still needs.
    Succeeded {
        /// How many attempts it took.
        attempts: u32,
        /// Wall clock from the first dial to the new session being ready.
        took: Duration,
    },
    /// The revival gave up. The connection is dead and stays dead.
    Failed {
        /// How many attempts were made.
        attempts: u32,
        /// Wall clock spent before giving up.
        took: Duration,
        /// What the last attempt failed with, for a human.
        reason: String,
    },
}

/// Parameters established during negotiate.
#[derive(Debug, Clone)]
pub struct NegotiatedParams {
    /// The dialect both sides agreed on.
    pub dialect: Dialect,
    /// Maximum read size the server supports.
    pub max_read_size: u32,
    /// Maximum write size the server supports.
    pub max_write_size: u32,
    /// Maximum transact size the server supports.
    pub max_transact_size: u32,
    /// The server's GUID.
    pub server_guid: Guid,
    /// Whether the server requires signing.
    pub signing_required: bool,
    /// Server capabilities.
    pub capabilities: Capabilities,
    /// Whether AES-GMAC signing was negotiated (SMB 3.1.1).
    pub gmac_negotiated: bool,
    /// The cipher negotiated for encryption (SMB 3.x).
    pub cipher: Option<Cipher>,
    /// Whether compression was negotiated with the server.
    pub compression_supported: bool,
}

/// A received SMB2 sub-response, post-decrypt / post-decompress / post-header-parse.
///
/// This is what `Connection::execute` / `execute_with_credits` return on
/// success (and what each inner `Result` in `execute_compound`'s return
/// vector wraps). The three fields cover every downstream parse need:
///
/// - `header`: the parsed SMB2 header. Includes `status`, `command`,
///   `message_id`, `credits`, `tree_id`, etc.
/// - `body`: the sub-frame bytes after the header (i.e.
///   `raw[Header::SIZE..]`). Most callers unpack this via `ReadCursor` +
///   `Unpack`.
/// - `raw`: the full sub-frame bytes, header included. Kept for preauth
///   hash updates and any caller that wants to re-verify signatures or
///   inspect the original wire bytes.
///
/// Callers receive one `Frame` per matched `MessageId`. Frames are owned;
/// the receiver task allocates fresh `Vec`s for `body` / `raw` as it splits
/// compound frames, so you can store or mutate them freely.
#[derive(Debug)]
pub struct Frame {
    /// Parsed SMB2 header of this sub-response.
    pub header: Header,
    /// Sub-frame bytes after the header (body portion only).
    pub body: Vec<u8>,
    /// Full sub-frame bytes including the header.
    pub raw: Vec<u8>,
}

/// One sub-operation in a compound request, as passed to
/// [`Connection::execute_compound`].
///
/// Each `CompoundOp` describes a single SMB2 operation (CREATE, READ,
/// CLOSE, etc.) that the receiver side pairs with a [`Frame`] response
/// by `MessageId`. The server MAY split compound responses into multiple
/// transport frames — the receiver task handles that transparently; each
/// sub-op still gets routed to its own waiter by msg_id.
///
/// Field-by-field:
///
/// - `command`: the SMB2 command code (`Create`, `Read`, `Write`, etc.).
/// - `body`: the packed request body as a `&dyn Pack`. Typical callers
///   pass `&MyRequest { ... }` — the trait object lets one compound
///   chain hold heterogeneous request types.
/// - `tree_id`: the `TreeId` to stamp into the header, or `None` when
///   the op predates tree connect (for example, SESSION_SETUP in a
///   compound). For ordinary file ops, pass `Some(tree.tree_id)`.
/// - `credit_charge`: the number of credits (and consecutive MessageIds)
///   this op consumes. Most ops use `CreditCharge(1)`. Large READ / WRITE
///   ops consume `ceil(payload_size / 65536)` — see the docs on
///   [`execute_with_credits`](Connection::execute_with_credits) for details.
pub struct CompoundOp<'a> {
    /// The SMB2 command code.
    pub command: Command,
    /// The packed request body, as a `&dyn Pack`.
    pub body: &'a dyn Pack,
    /// `Some(tree_id)` for tree-scoped ops, `None` for connection-level ones.
    pub tree_id: Option<TreeId>,
    /// Credit charge (and consecutive-MessageId count) for this sub-op.
    pub credit_charge: CreditCharge,
}

impl<'a> CompoundOp<'a> {
    /// Build a `CompoundOp` with the default single-credit charge.
    ///
    /// Equivalent to setting `credit_charge: CreditCharge(1)`. For reads
    /// or writes larger than 64 KB, construct the struct directly with
    /// the right charge.
    pub fn new(command: Command, body: &'a dyn Pack, tree_id: Option<TreeId>) -> Self {
        Self {
            command,
            body,
            tree_id,
            credit_charge: CreditCharge(1),
        }
    }
}

/// Crypto state shared between the caller thread (sending) and receiver task
/// (verifying signatures, decrypting).
///
/// Uses `std::sync::Mutex` because the critical sections are short and never
/// hold the lock across an `.await`. Mutation is rare (once at session setup),
/// reads happen once per frame on either side.
struct CryptoState {
    signing_key: Option<Vec<u8>>,
    signing_algorithm: Option<SigningAlgorithm>,
    should_sign: bool,
    encryption_key: Option<Vec<u8>>,
    decryption_key: Option<Vec<u8>>,
    encryption_cipher: Option<Cipher>,
    should_encrypt: bool,
    nonce_gen: Option<NonceGenerator>,
    session_id: SessionId,
}

impl CryptoState {
    fn new() -> Self {
        Self {
            signing_key: None,
            signing_algorithm: None,
            should_sign: false,
            encryption_key: None,
            decryption_key: None,
            encryption_cipher: None,
            should_encrypt: false,
            nonce_gen: None,
            session_id: SessionId::NONE,
        }
    }
}

/// Where a connection reads its inbound byte counts from.
///
/// The counts belong to the TRANSPORT, because only the transport sees a frame
/// while it is still arriving, and a revival replaces the transport. So the
/// connection keeps the current one's counter plus what the replaced ones had
/// counted, which is what lets [`Connection::inbound`] report one total that
/// never runs backwards.
struct Inbound {
    /// The current transport's counter, or one the receiver task keeps from
    /// whole frames when the transport can't see inside one.
    progress: Arc<ReceiveProgress>,
    /// Bytes counted by transports this connection has since replaced.
    retired_bytes: u64,
}

/// Shared connection state, held in an `Arc` by every `Connection` clone and
/// (weakly) by the background tasks.
///
/// All connection-wide state lives here, which is what makes `Connection`
/// `Clone`: each clone sees the same credits, session id, negotiated params,
/// and crypto state. `execute` / `execute_compound` own their per-call
/// `oneshot::Receiver`s locally, so a clone carries no bookkeeping of its own;
/// `Connection` is only a handle to `Arc<Inner>`.
struct Inner {
    /// Per-request routing: msg_id → oneshot sender waiting for its response.
    waiters: StdMutex<HashMap<MessageId, Waiter>>,
    /// How long a request may go unanswered before the sweeper warns, or `None`
    /// to stay silent. Consumers with a legitimately slow server tune or disable
    /// it; see `Connection::set_stale_request_warning`.
    stale_request_after: StdMutex<Option<std::time::Duration>>,
    /// The sweeper's memory of the send side: what lets one sweep tell "the
    /// queue is deep because the link is slow" from "the queue is deep because
    /// nothing is moving" — two states the depth alone cannot separate, and
    /// only one of which is worth waking anyone up for.
    send_progress: StdMutex<SendProgress>,
    /// What the writer task has done since the sweeper last looked.
    send_tally: SendTally,
    /// How long a request may go unanswered before its caller gives up, or
    /// `None` to wait indefinitely. See `Connection::set_response_timeout`.
    response_timeout: StdMutex<Option<std::time::Duration>>,
    /// When the server last put a frame on the wire for us, or `None` if it
    /// never has.
    ///
    /// The connection's liveness clock, and deliberately fed by EVERY frame —
    /// a response, an interim STATUS_PENDING, an oplock break, even a stray —
    /// because all of them prove the same thing: the server is processing.
    /// The keepalive only has to manufacture a frame when nothing else is
    /// producing one.
    ///
    /// `None` rather than "the connection's birth" on purpose: a server that
    /// has never said anything has not proven anything, and the deadline
    /// extension must never be granted on an assumption.
    last_frame_at: StdMutex<Option<std::time::Instant>>,
    /// What the server has put on the wire so far, counted as it lands rather
    /// than when a frame completes. See [`Inbound`].
    inbound: StdMutex<Inbound>,
    /// When one of this connection's own loops was last scheduled: the
    /// process's liveness clock, as opposed to the server's.
    ///
    /// Every other clock here measures wall time, which silently assumes we
    /// were running to hear the silence we are measuring. This is the witness
    /// that says whether we were. See [`Inner::forgive_scheduling_stall`].
    last_scheduled_at: StdMutex<std::time::Instant>,
    /// How much server silence, with work outstanding, triggers an ECHO probe,
    /// or `None` to never probe. See `Connection::set_keepalive`.
    keepalive_after: StdMutex<Option<Duration>>,
    /// How long a long-poll request may stay registered with the server before
    /// it is retired for a fresh one, or `None` to keep one forever. See
    /// `Connection::set_long_poll_refresh`.
    long_poll_refresh: StdMutex<Option<Duration>>,
    /// The server's credit budget. Every send reserves its `CreditCharge`
    /// here before the bytes go out; the receiver task banks the grant off
    /// every frame (orphans included). See `credits.rs`.
    credits: CreditPool,
    /// Next message id to allocate. Incremented by caller on send.
    next_message_id: AtomicU64,
    /// Crypto state for signing / encryption.
    crypto: StdMutex<CryptoState>,
    /// Set to true when the receiver task exits (transport error / EOF).
    /// New `execute` / `execute_compound` calls short-circuit to
    /// `Err(Disconnected)` once this flips so they don't register waiters
    /// into a dead map.
    disconnected: AtomicBool,

    /// Queue into the writer task. Callers hand over a WHOLE frame and wait
    /// for an ack; they never touch the socket themselves.
    ///
    /// Two things fall out of that. A caller dropped mid-send can no longer
    /// leave half a frame on the wire (the frame is one message, sent or
    /// not), and a stuck write is the writer task's problem to time out
    /// rather than a lock every other caller silently queues behind.
    ///
    /// Swapped wholesale when the connection is revived, together with the
    /// writer task that drains it. That is what makes a frame built for the
    /// dead session unable to reach the new socket: its queue no longer has a
    /// reader, so the send fails instead of landing on a stranger.
    write_tx: StdMutex<mpsc::Sender<WriteJob>>,
    /// MessageIds whose caller went away before the response landed, newest
    /// last, capped at [`ABANDONED_ID_MEMORY`].
    ///
    /// Deregistering on drop (which is what stops waiters leaking) would
    /// otherwise make a cancelled request's late response indistinguishable
    /// from a frame we never asked for. Consumers cancel constantly — a user
    /// aborting a copy — so `responses_stray` would fill with routine noise
    /// and stop meaning "protocol anomaly". A late response arrives within one
    /// round trip of the cancellation, so a small ring covers it.
    abandoned: StdMutex<VecDeque<MessageId>>,
    /// Frames handed to the writer task and not yet acked. A gauge for
    /// diagnostics and for the stale-waiter warning, which reports it so a
    /// backlog names itself.
    send_queue_depth: AtomicUsize,
    /// Permits for the outstanding-WRITE-payload budget, counted in
    /// [`WRITE_BUDGET_UNIT`]s. [`DEFAULT_WRITE_BUDGET_BYTES`] worth to begin
    /// with; [`Connection::set_write_budget`] moves it.
    ///
    /// A permit is held by the in-flight WRITE future itself, so it comes back
    /// when the response is drained, the writer is aborted, or the future is
    /// dropped mid-flight. ❌ Don't release it by hand anywhere: every cancel
    /// path already returns it for free, and a manual release would double-count.
    write_budget: Arc<Semaphore>,
    /// What `write_budget` holds versus what it should hold. See
    /// [`WriteBudgetUnits`] and `Inner::settle_write_budget`.
    write_budget_units: StdMutex<WriteBudgetUnits>,
    /// How long one frame may take to reach the socket before its caller
    /// gives up with [`Error::SendTimeout`], or `None` to wait forever.
    send_timeout: StdMutex<Option<Duration>>,
    /// Handle for the writer task, aborted with the receiver task when the
    /// last `Connection` clone drops.
    writer_task: StdMutex<Option<rt::TaskHandle>>,
    /// Handle for the ECHO keepalive task. Aborted with the others when the
    /// last `Connection` clone drops.
    keepalive_task: StdMutex<Option<rt::TaskHandle>>,
    /// Handle for the background receiver task. Aborted when the last clone
    /// of `Connection` drops (via `Inner`'s `Drop`). The transport's read
    /// half's EOF also stops the task; the abort is a safety net.
    receiver_task: StdMutex<Option<rt::TaskHandle>>,
    /// Handle for the stale-request sweeper. Held so a revival can retire the
    /// old one instead of accumulating a sweeper per generation — the sweeper
    /// exits on `disconnected`, and a revival clears that flag.
    sweeper_task: StdMutex<Option<rt::TaskHandle>>,
    /// The current socket's [`SocketLife`], so declaring the connection dead
    /// can close it. Replaced (and the old one ended) by every revival.
    socket_life: StdMutex<SocketLife>,

    /// How to dial a fresh socket and re-authenticate on it, or `None` when
    /// the consumer has not armed auto-reconnect. See [`SessionReviver`].
    reviver: StdMutex<Option<Arc<dyn SessionReviver>>>,
    /// Serializes revival attempts, so a pipeline of 32 callers that all
    /// discover the same dead session dials once rather than 32 times.
    revive_lock: tokio::sync::Mutex<()>,
    /// Bounds on a revival: attempts, backoff, and the total wall clock it may
    /// consume. See [`ReconnectPolicy`].
    reconnect_policy: StdMutex<ReconnectPolicy>,
    /// Successful revivals. Doubles as the "did somebody else already fix
    /// this while I queued for the lock" check, which is why it is bumped only
    /// on success.
    revivals: AtomicU64,
    /// When the last revival gave up, and why.
    ///
    /// Without this, every one of a deep pipeline's callers would serially pay
    /// the full reconnect budget against a server that is plainly not coming
    /// back — 32 × 60 s of stall, which is the unbounded hang again wearing a
    /// different hat. Inside the cooldown the stored verdict is returned at
    /// once. See [`ReconnectPolicy::failure_cooldown`].
    last_revive_failure: StdMutex<Option<(Instant, Error)>>,
    /// Called on every reconnect lifecycle event, if a consumer asked to be
    /// told. See [`Connection::on_reconnect`].
    reconnect_observer: StdMutex<Option<ReconnectObserver>>,
    /// The session the next SESSION_SETUP replaces, or `0`: the one this
    /// connection had before it was revived, or one a consumer named with
    /// [`Connection::set_previous_session_id`].
    ///
    /// SESSION_SETUP carries it as `PreviousSessionId` (MS-SMB2 § 2.2.5), which
    /// is how a client says "this is me again" after a disconnect. Without it
    /// the server has no way to connect the new session to the old one, so it
    /// holds the dead session's state (its open handles, and the share locks
    /// they take) until its own timeout. With it, the server ends the old
    /// session before answering, which closes those handles; Windows 8.1 and
    /// later close its durable opens too (MS-SMB2 product notes <337>, <351>).
    previous_session_id: AtomicU64,
    /// The session most recently established on this connection.
    ///
    /// Exists so a session established behind a consumer's back — by a
    /// revival, which re-authenticates without anyone asking — is reachable.
    /// A consumer still holding the *old* `Session` would otherwise activate
    /// share encryption with keys the current server has never seen, and every
    /// frame after that fails to decrypt.
    session: StdMutex<Option<Arc<crate::client::Session>>>,

    /// This connection's identity to the server: chosen when the connection
    /// is built, and unchanged for its whole life, revivals included
    /// (MS-SMB2 § 2.2.3, `ClientGuid`).
    ///
    /// ❌ Don't mint a fresh one per NEGOTIATE. A server matches the client
    /// guid when deciding whether a durable open may be claimed back
    /// (MS-SMB2 § 3.3.5.9.12), so a connection that reintroduces itself when
    /// it comes back can never resume anything: Samba 4.x answers a reconnect
    /// from a "different" client with `STATUS_OBJECT_NAME_NOT_FOUND` (observed
    /// against the `smb-guest` fixture, 2026-08-02, which grants a 60 s durable
    /// handle and then refuses to give it back). `Inner` survives a revival, so
    /// holding the value here is exactly what a reclaim needs.
    ///
    /// ❌ Nor share one value across connections, which is what a process-wide
    /// guid does. The spec calls it a property of the *client*, and a client
    /// doing multi-channel needs it that way, but this crate opens independent
    /// connections rather than channels. Samba reads a guid it already knows as
    /// "another channel for a client I have" and hands the socket to the smbd
    /// process owning the first connection (`smbXsrv_connection_pass`, keyed at
    /// negprot before any capability is negotiated, so declining
    /// `SMB2_GLOBAL_CAP_MULTI_CHANNEL` doesn't opt out). When connects overlap,
    /// that hand-off loses the negotiate's credit grant: the next request is
    /// rejected as over-credit and its caller waits out the 30 s watchdog. See
    /// `docs/notes/samba-client-guid-connection-pass.md`.
    client_guid: Guid,

    /// Server name (hostname or IP) used for UNC paths. Set at construction
    /// and never mutated.
    server_name: String,
    /// Negotiated parameters, populated by `negotiate` and re-established by
    /// every revival.
    ///
    /// ❌ Not a `OnceLock`: a connection revived on a fresh socket renegotiates
    /// from scratch, and a rebooted server may come back with a smaller
    /// `MaxWriteSize` or a different dialect. Keeping the first negotiation's
    /// numbers would size every chunk against a server that no longer exists.
    params: StdMutex<Option<NegotiatedParams>>,
    /// Estimated round-trip time: NEGOTIATE's, lowered by the quickest small
    /// request answered since (`Inner::note_round_trip`).
    estimated_rtt: StdMutex<Option<Duration>>,
    /// The last link rate a download measured on this connection, in bytes
    /// per second, and when. Seeds the next download's read-ahead
    /// window, so a folder of 1 MiB files doesn't pay a round trip per file
    /// to rediscover the link. See [`RATE_HINT_TTL`].
    read_rate_hint: StdMutex<Option<(rt::Instant, f64)>>,
    /// The same for uploads: the last rate a writer's WRITEs were confirmed
    /// at. Kept apart from the download rate because most links are
    /// asymmetric, a home uplink often ten times slower than its downlink.
    write_rate_hint: StdMutex<Option<(rt::Instant, f64)>>,
    /// Whether this server refused `FileIdBothDirectoryInformation` (class 37)
    /// in a QUERY_DIRECTORY, so a listing that opts into index numbers asks
    /// for the default class from the start instead of paying the refusal on
    /// every directory. Learned from the first refusal rather than guessed
    /// from the server, and erased on a revival with the rest of what the old
    /// server told us.
    listing_ids_refused: AtomicBool,
    /// How late READ answers came for the last download on this connection,
    /// and when: what the next download's learned headroom starts from.
    /// Same expiry and revival rules as the rate. See [`RATE_HINT_TTL`].
    read_lateness_hint: StdMutex<Option<(rt::Instant, read_ahead::Lateness)>>,
    /// The same for WRITE confirmations.
    write_lateness_hint: StdMutex<Option<(rt::Instant, read_ahead::Lateness)>>,
    /// Whether compression is active on this connection (negotiated).
    compression_enabled: AtomicBool,
    /// Whether the client wants compression (from config).
    compression_requested: AtomicBool,
    /// Preauth integrity hash (for SMB 3.1.1 key derivation). Mutated during
    /// negotiate and session setup; both happen on one task before any clone
    /// is expected to observe it.
    preauth_hasher: StdMutex<PreauthHasher>,
    /// Tree IDs that have DFS capability (auto-set `SMB2_FLAGS_DFS_OPERATIONS`).
    dfs_trees: StdMutex<HashSet<TreeId>>,
    /// An IPC$ tree kept open for DFS referrals.
    ///
    /// MS-SMB2 § 3.2.4.20.3 lets a referral ride any existing tree connect and
    /// only asks for IPC$ when there is none, so holding one turns every
    /// referral after the first from three frames into one. Erased with the
    /// rest of the session on a revival, because the tree id belongs to it.
    ///
    /// Kept with the pin it was connected under, so a tree connect that
    /// finishes as a revival erases the cache can't leave the dead session's
    /// id behind for the new one.
    ipc_tree: StdMutex<Option<(SessionPin, TreeId)>>,
    /// Which tree each oplocked handle belongs to.
    ///
    /// Only durable opens take an oplock, and only so the server will grant
    /// durability at all. The map exists because an oplock break notification
    /// arrives with no usable tree id and the acknowledgment must carry the
    /// one the open belongs to (MS-SMB2 § 2.2.24.1) — and an unacknowledged
    /// break makes the *other* client wait out the server's break timeout,
    /// which is around 35 s on both Samba and Windows.
    oplock_trees: StdMutex<HashMap<FileId, TreeId>>,
    /// Counters for diagnostics. Snapshotted via [`Inner::metrics_snapshot`].
    /// Survives connection teardown — counters are read off the still-alive
    /// `Arc<Inner>` after the receiver task has exited.
    metrics: Metrics,
}

impl Inner {
    /// Bring the write-budget semaphore in line with the target, and report the
    /// target in units.
    ///
    /// Growing is immediate. Shrinking can only take back permits that are FREE
    /// right now, so a reduction made while a transfer is running lands as those
    /// frames complete — which is why every reserve calls this before asking for
    /// permits, rather than `set_write_budget` doing it once and hoping.
    fn settle_write_budget(&self) -> u32 {
        let mut units = self.write_budget_units.lock().unwrap();
        // `std::cmp::Ordering` spelled out: `Ordering` in this module is the
        // atomic one.
        match units.issued.cmp(&units.target) {
            std::cmp::Ordering::Less => {
                let grow = units.target - units.issued;
                self.write_budget.add_permits(grow as usize);
                units.issued = units.target;
            }
            std::cmp::Ordering::Greater => {
                let shrink = units.issued - units.target;
                let taken = self.write_budget.forget_permits(shrink as usize);
                units.issued -= u32::try_from(taken).unwrap_or(u32::MAX);
            }
            std::cmp::Ordering::Equal => {}
        }
        units.target
    }

    fn new(write_tx: mpsc::Sender<WriteJob>, server_name: String) -> Self {
        Self {
            waiters: StdMutex::new(HashMap::new()),
            stale_request_after: StdMutex::new(Some(STALE_WAITER_AFTER)),
            send_progress: StdMutex::new(SendProgress {
                wire_bytes_sent: 0,
                last_progress: Instant::now(),
                last_sweep: Instant::now(),
            }),
            send_tally: SendTally::default(),
            response_timeout: StdMutex::new(Some(RESPONSE_TIMEOUT)),
            last_frame_at: StdMutex::new(None),
            inbound: StdMutex::new(Inbound {
                progress: Arc::new(ReceiveProgress::new()),
                retired_bytes: 0,
            }),
            last_scheduled_at: StdMutex::new(std::time::Instant::now()),
            keepalive_after: StdMutex::new(Some(KEEPALIVE_AFTER)),
            long_poll_refresh: StdMutex::new(Some(LONG_POLL_REFRESH)),
            credits: CreditPool::new(),
            next_message_id: AtomicU64::new(0),
            crypto: StdMutex::new(CryptoState::new()),
            disconnected: AtomicBool::new(false),
            write_tx: StdMutex::new(write_tx),
            abandoned: StdMutex::new(VecDeque::new()),
            send_queue_depth: AtomicUsize::new(0),
            write_budget: Arc::new(Semaphore::new(
                budget_units(DEFAULT_WRITE_BUDGET_BYTES) as usize
            )),
            write_budget_units: StdMutex::new(WriteBudgetUnits {
                issued: budget_units(DEFAULT_WRITE_BUDGET_BYTES),
                target: budget_units(DEFAULT_WRITE_BUDGET_BYTES),
            }),
            send_timeout: StdMutex::new(Some(SEND_TIMEOUT)),
            writer_task: StdMutex::new(None),
            keepalive_task: StdMutex::new(None),
            receiver_task: StdMutex::new(None),
            sweeper_task: StdMutex::new(None),
            socket_life: StdMutex::new(SocketLife::new()),
            reviver: StdMutex::new(None),
            revive_lock: tokio::sync::Mutex::new(()),
            reconnect_policy: StdMutex::new(ReconnectPolicy::default()),
            revivals: AtomicU64::new(0),
            last_revive_failure: StdMutex::new(None),
            reconnect_observer: StdMutex::new(None),
            previous_session_id: AtomicU64::new(0),
            session: StdMutex::new(None),
            client_guid: random_guid(),
            server_name,
            params: StdMutex::new(None),
            estimated_rtt: StdMutex::new(None),
            read_rate_hint: StdMutex::new(None),
            write_rate_hint: StdMutex::new(None),
            listing_ids_refused: AtomicBool::new(false),
            read_lateness_hint: StdMutex::new(None),
            write_lateness_hint: StdMutex::new(None),
            compression_enabled: AtomicBool::new(false),
            compression_requested: AtomicBool::new(true),
            preauth_hasher: StdMutex::new(PreauthHasher::new()),
            dfs_trees: StdMutex::new(HashSet::new()),
            ipc_tree: StdMutex::new(None),
            oplock_trees: StdMutex::new(HashMap::new()),
            metrics: Metrics::default(),
        }
    }

    /// Send raw wire bytes through the transport and bump the
    /// `wire_bytes_sent` counter. The single funnel for every outbound
    /// frame — keeps `wire_bytes_sent` from drifting as new send sites
    /// are added.
    async fn send_and_count(self: &Arc<Self>, bytes: &[u8], command: Command) -> Result<()> {
        let (job, frame) = self.write_job(bytes.to_vec(), command);
        // Snapshot the queue rather than holding the lock across the await: a
        // revival may swap it underneath us, and enqueuing into the retired
        // queue is exactly right — that frame belongs to the dead session and
        // must not reach the new socket.
        let write_tx = self.write_tx.lock().unwrap().clone();
        if write_tx.send(job).await.is_err() {
            // The writer task is gone, so the connection is dead.
            return Err(Error::Disconnected);
        }
        frame.settle().await
    }

    /// A frame ready for the writer task, and the handle that waits for its
    /// outcome. The frame counts in `send_queue_depth` from here until that
    /// handle is settled or dropped.
    fn write_job(self: &Arc<Self>, bytes: Vec<u8>, command: Command) -> (WriteJob, QueuedFrame) {
        let (done_tx, done_rx) = oneshot::channel();
        let queued_at = std::time::Instant::now();
        let frame = QueuedFrame {
            inner: Arc::clone(self),
            done: done_rx,
            len: bytes.len(),
            command,
            queued_at,
        };
        self.send_queue_depth.fetch_add(1, Ordering::Relaxed);
        let job = WriteJob {
            bytes,
            command,
            queued_at,
            done: done_tx,
        };
        (job, frame)
    }
}

/// A frame handed to the writer task, until the writer says how it went.
///
/// Counts in `send_queue_depth` for exactly as long as it lives, so a caller
/// dropped while it waits (consumers cancel routinely) can't leave the gauge
/// counting a frame forever.
struct QueuedFrame {
    inner: Arc<Inner>,
    done: oneshot::Receiver<Result<()>>,
    len: usize,
    command: Command,
    queued_at: std::time::Instant,
}

impl QueuedFrame {
    /// Wait for the writer task, and count the bytes once they're out.
    async fn settle(mut self) -> Result<()> {
        let outcome = (&mut self.done).await;
        let (len, command) = (self.len, self.command);
        let waited = self.queued_at.elapsed();
        match outcome {
            // The writer task counts the bytes it actually wrote, so a frame
            // that never made it doesn't inflate `wire_bytes_sent` — the one
            // counter that says whether we are talking to the server at all.
            Ok(Ok(())) => {
                self.inner
                    .metrics
                    .wire_bytes_sent
                    .fetch_add(len as u64, Ordering::Relaxed);
                if waited >= SLOW_SEND_REPORT {
                    warn!(
                        "slow send: cmd={command:?}, {len} bytes took {waited:?} to reach the socket"
                    );
                }
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            // Writer task died between accepting the job and answering.
            Err(_) => Err(Error::Disconnected),
        }
    }
}

impl Drop for QueuedFrame {
    fn drop(&mut self) {
        self.inner.send_queue_depth.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Inner {
    /// How the send side looks as of `now`, and advance the sweeper's memory.
    ///
    /// ❌ Only the connection sweeper may call this: it consumes the previous
    /// reading, so a second caller would leave the sweeper measuring silence
    /// from its own last look instead of from the last frame out.
    ///
    /// `wire_bytes_sent` only ticks once a WHOLE frame has reached the socket,
    /// which is what makes it the right witness — a writer parked mid-frame
    /// moves nothing, and neither does this counter — and also why the verdict
    /// needs [`stall_tolerance`] rather than "did it move since last time".
    fn observe_send_side(&self, now: Instant, tolerance: Duration) -> SendSideReading {
        let sent = self.metrics.wire_bytes_sent.load(Ordering::Relaxed);
        let mut progress = self.send_progress.lock().unwrap();
        if sent != progress.wire_bytes_sent {
            progress.wire_bytes_sent = sent;
            progress.last_progress = now;
        }
        let silent_for = now.saturating_duration_since(progress.last_progress);
        let window = now.saturating_duration_since(progress.last_sweep);
        progress.last_sweep = now;
        SendSideReading {
            state: if silent_for >= tolerance {
                SendSide::Stalled
            } else {
                SendSide::Draining
            },
            silent_for,
            window,
        }
    }

    /// Note that `msg_id`'s bytes have reached the transport.
    ///
    /// Also restarts the response deadline: the clock measures the server's
    /// silence, and the server has only now been asked.
    fn mark_sent(&self, msg_ids: &[MessageId]) {
        let now = std::time::Instant::now();
        let mut waiters = self.waiters.lock().unwrap();
        for id in msg_ids {
            if let Some(w) = waiters.get_mut(id) {
                w.sent_at = Some(now);
                w.last_activity = now;
            }
        }
    }

    /// Lower the round-trip estimate to `waiter`'s, if its request was a small
    /// one answered with a small frame.
    ///
    /// NEGOTIATE's round trip overstates the link whenever the first exchange
    /// on a connection is slow: a server starting a process for it, a waking
    /// NAS, a proxy dialing the far end (175 ms against a 66 ms link on the
    /// read-ahead benchmark rig). The adaptive windows budget
    /// `rate × (RTT + headroom)`, so an overstated RTT keeps a slow link a
    /// chunk or two deeper than it needs, and every listing waits behind it.
    /// Any small request's answer is a round trip too, so the smallest one
    /// wins, as TCP's minimum RTT does.
    ///
    /// Only what can't understate counts: timed from registration (before the
    /// bytes reach the socket, so a queue only adds), never a READ or WRITE
    /// (their payload's own time on the wire rides along) or a request the
    /// server parked with STATUS_PENDING, and only answers of at most
    /// [`ROUND_TRIP_SAMPLE_MAX`] bytes. Every sample is at least the real
    /// round trip, so the minimum can only come down toward it.
    fn note_round_trip(&self, waiter: &Waiter, response_len: usize) {
        let small = matches!(
            waiter.command,
            Command::Create
                | Command::Close
                | Command::QueryInfo
                | Command::SetInfo
                | Command::Echo
                | Command::TreeConnect
                | Command::TreeDisconnect
                | Command::Flush
        );
        if !small || waiter.async_id.is_some() || response_len > ROUND_TRIP_SAMPLE_MAX {
            return;
        }
        let sample = waiter.registered_at.elapsed();
        let mut rtt = self.estimated_rtt.lock().unwrap();
        if let Some(current) = rtt.as_mut() {
            *current = (*current).min(sample);
        }
    }

    /// Reserve `charge` credits for a request that is about to be sent.
    ///
    /// The reservation is refunded on drop, so a request that fails to sign,
    /// encrypt, or reach the transport gives its credits back; `commit` it
    /// once the bytes are out.
    ///
    /// Waiting is bounded four ways, because an unbounded wait would trade
    /// the over-spend hang for a starvation hang:
    ///
    /// 1. Nothing outstanding and not enough on hand: no grant can ever
    ///    arrive, so fail immediately rather than wait out the deadline.
    /// 2. The charge is wider than a window the server has stopped growing
    ///    (`CreditPool::can_never_fund`): no grant can ever be enough, and on
    ///    the fair pool every request queued behind it would wait too, so fail
    ///    immediately. Checked again while waiting, since the verdict can
    ///    land mid-wait.
    /// 3. The connection dies: `CreditPool::close` wakes every waiter.
    /// 4. Otherwise the deadline from
    ///    [`Connection::set_credit_wait_timeout`] applies.
    async fn reserve_credits(&self, charge: u16, command: Command) -> Result<CreditReservation> {
        if let Some(reservation) = self.credits.try_reserve(charge) {
            return Ok(reservation);
        }
        if self.credits.is_closed() || self.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        if self.waiters.lock().unwrap().is_empty() {
            // Every credit the server will ever return rides on a response,
            // and there is no request outstanding to carry one.
            return Err(self.starvation(charge, Duration::ZERO));
        }
        if self.credits.can_never_fund(charge) {
            debug!(
                "credits: {:?} needs {} credit(s), more than the server's whole window of {:?}; not waiting",
                command,
                charge,
                self.credits.ceiling()
            );
            return Err(self.starvation(charge, Duration::ZERO));
        }

        self.metrics.credit_waits.fetch_add(1, Ordering::Relaxed);
        warn!(
            "credits: {:?} needs {} credit(s) but only {} are available; waiting for the server to grant more",
            command,
            charge,
            self.credits.available()
        );

        let started = std::time::Instant::now();
        let deadline = started + self.credits.wait_timeout();
        let mut reserving = Box::pin(self.credits.reserve(charge));
        loop {
            // `select` polls the reservation first, so credits that land in
            // the same tick as a recheck are taken rather than declared lost.
            let recheck = Box::pin(rt::sleep(CREDIT_STARVATION_RECHECK));
            match select(reserving, recheck).await {
                Either::Left((res, _)) => {
                    return match res {
                        Ok(reservation) => {
                            debug!(
                                "credits: {:?} acquired {} credit(s) after {:?}",
                                command,
                                charge,
                                started.elapsed()
                            );
                            Ok(reservation)
                        }
                        // The pool only closes on connection teardown.
                        Err(_) => Err(Error::Disconnected),
                    };
                }
                Either::Right((_, still_reserving)) => {
                    let nothing_outstanding = self.waiters.lock().unwrap().is_empty();
                    if nothing_outstanding
                        || self.credits.can_never_fund(charge)
                        || std::time::Instant::now() >= deadline
                    {
                        self.metrics
                            .credit_starvations
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(self.starvation(charge, started.elapsed()));
                    }
                    reserving = still_reserving;
                }
            }
        }
    }

    /// Record that the server put a frame on the wire just now.
    fn note_server_spoke(&self) {
        *self.last_frame_at.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// Start reading inbound counts from a new transport's counter, retiring
    /// the previous one's total so the connection's never runs backwards.
    ///
    /// Returns the counter the receiver task has to feed itself, which is a
    /// fresh one when the transport can't publish its own.
    fn adopt_receive_progress(
        &self,
        published: Option<Arc<ReceiveProgress>>,
    ) -> Option<Arc<ReceiveProgress>> {
        let (progress, feed) = match published {
            Some(progress) => (progress, None),
            None => {
                let own = Arc::new(ReceiveProgress::new());
                (Arc::clone(&own), Some(own))
            }
        };
        let mut inbound = self.inbound.lock().unwrap();
        inbound.retired_bytes += inbound.progress.snapshot().bytes;
        inbound.progress = progress;
        feed
    }

    /// When the server last put anything on the wire, a byte of a frame still
    /// arriving included, or `None` if it never has.
    ///
    /// **Every inbound byte is a sign of life, not just every whole frame.**
    /// TCP delivers in order, so while a large response is arriving nothing
    /// else can: an 8 MB READ at 200 KB/s holds the wire for 40 s, and the
    /// ECHO replies that would prove the server alive queue up behind it. With
    /// only whole frames on this clock, that read looked like 40 s of silence
    /// and ended in `declare_unresponsive` against a server that was talking
    /// the whole time.
    ///
    /// Folds the transport's clock into `last_frame_at` rather than reading it
    /// alongside, because `last_frame_at` is the one the stall correction and
    /// a revival adjust. Taking the later of the two keeps both: a corrected
    /// reading sits ahead of any byte that landed before the stall.
    fn last_heard(&self) -> Option<std::time::Instant> {
        let progress = Arc::clone(&self.inbound.lock().unwrap().progress);
        let byte_at = progress.snapshot().last_byte_at;
        let mut last = self.last_frame_at.lock().unwrap();
        if let Some(byte_at) = byte_at {
            if last.is_none_or(|t| byte_at > t) {
                *last = Some(byte_at);
            }
        }
        *last
    }

    /// How long since the server last said anything, or `None` if it never
    /// has.
    fn server_silent_for(&self) -> Option<Duration> {
        let now = std::time::Instant::now();
        self.last_heard().map(|t| now.saturating_duration_since(t))
    }

    /// How long the wire has been quiet while the server had something to
    /// answer, or `None` if nothing is on the wire.
    ///
    /// Measured from the LATER of "the server last spoke" and "the oldest
    /// unanswered request went out", because both readings are wrong on their
    /// own: a server cannot be silent about a question it has not been asked
    /// yet, and a connection that has never heard a word is quiet from the
    /// moment we first asked rather than not-yet-quiet forever.
    ///
    /// `sent_at.is_some()` is load-bearing. A request still queued for the
    /// writer task has not asked the server anything, so probing on its behalf
    /// would measure the wrong side of the wire — the send deadline owns that
    /// case, and conflating the two is the misdiagnosis the `sent_at` split
    /// exists to prevent.
    fn quiet_for(&self) -> Option<Duration> {
        let now = std::time::Instant::now();
        let oldest_sent = {
            let waiters = self.waiters.lock().unwrap();
            waiters.values().filter_map(|w| w.sent_at).min()?
        };
        let reference = match self.last_heard() {
            Some(spoke) => spoke.max(oldest_sent),
            None => oldest_sent,
        };
        Some(now.saturating_duration_since(reference))
    }

    /// Freeze every liveness clock across a stretch this PROCESS spent
    /// unscheduled, and report how long that was.
    ///
    /// Every clock on a connection measures wall time, which quietly assumes
    /// we were running to hear the silence we are measuring. A system sleep,
    /// an App Nap, or a machine starved by a parallel build breaks that
    /// assumption: the wire looks silent for minutes because nobody was
    /// listening, and [`unresponsive_for`](Self::unresponsive_for) then
    /// convicts a server that was answering the whole time. Cmdr saw three of
    /// these in twelve minutes on 2026-08-08 (62 s, 175 s, 355 s, a concurrent
    /// `cargo build` starving the app), each ending in a declared-dead session
    /// against a healthy NAS; a closed laptop lid is the same shape.
    ///
    /// The witness is cadence: this connection's loops wake on a
    /// [`keepalive_tick`](Self::keepalive_tick) or faster, so a gap since the
    /// last one that exceeds `after` on top of the tick is not a slow server,
    /// it is us not running. Forgiving it pushes every clock forward by the
    /// gap, which is the exact statement "nothing aged while we were gone".
    ///
    /// Idempotent and safe to call from any loop: the gap is measured from the
    /// shared witness and calling this updates it, so whichever loop reaches a
    /// verdict first does the correction and the rest see nothing to forgive.
    /// ❌ Don't leave it to the keepalive task alone — every wait loop wakes
    /// from the same stall at the same moment, and one that read its clock
    /// before the keepalive got scheduled would declare the death this exists
    /// to prevent.
    ///
    /// ⚠️ This forgives, it never exonerates. The clocks restart, the probes
    /// restart with them, and a server that really is dead is declared dead
    /// one budget later. Suppressing the verdict instead would trade a false
    /// death for a permanent hang.
    fn forgive_scheduling_stall(&self) -> Option<Duration> {
        // The threshold rides on the probe cadence whether or not probing is
        // armed: "we were not running" is a fact about this process, and
        // `set_keepalive(None)` says nothing about it either way.
        let after = (*self.keepalive_after.lock().unwrap()).unwrap_or(KEEPALIVE_AFTER);
        let now = std::time::Instant::now();
        let stall = {
            let mut witness = self.last_scheduled_at.lock().unwrap();
            let gap = now
                .saturating_duration_since(*witness)
                .saturating_sub(Self::keepalive_tick(after));
            *witness = now;
            (gap >= after).then_some(gap)
        }?;
        // Shifting forward, rather than resetting to now, keeps whatever the
        // clocks legitimately read BEFORE the stall: a request the server had
        // already owed us for 10 s is still 10 s overdue afterwards.
        let shift = |t: &mut std::time::Instant| *t = t.checked_add(stall).unwrap_or(now);
        // Folds in the latest partial-frame byte first, so the shift starts
        // from the last thing the server actually said.
        self.last_heard();
        if let Some(spoke) = self.last_frame_at.lock().unwrap().as_mut() {
            shift(spoke);
        }
        for waiter in self.waiters.lock().unwrap().values_mut() {
            shift(&mut waiter.registered_at);
            shift(&mut waiter.last_activity);
            if let Some(sent) = waiter.sent_at.as_mut() {
                shift(sent);
            }
        }
        self.metrics
            .scheduling_stalls
            .fetch_add(1, Ordering::Relaxed);
        // INFO, not WARN: nothing is broken and nobody has anything to fix.
        // It earns the level by being the one line that explains a burst of
        // timeouts nobody could otherwise account for.
        info!(
            "this process was not scheduled for {stall:?} (system sleep, App Nap, or a starved \
             machine). Nothing can be concluded about the server from silence nobody was \
             listening to, so every liveness clock on this connection moves forward with it"
        );
        Some(stall)
    }

    /// Has this connection's own cadence been missed, so the clocks may still
    /// be carrying a stall nobody has corrected yet?
    ///
    /// The read-only half of [`forgive_scheduling_stall`](Self::forgive_scheduling_stall),
    /// for a reader that must not move the witness itself.
    fn loops_are_behind(&self) -> bool {
        let after = (*self.keepalive_after.lock().unwrap()).unwrap_or(KEEPALIVE_AFTER);
        let witness = *self.last_scheduled_at.lock().unwrap();
        std::time::Instant::now()
            .saturating_duration_since(witness)
            .saturating_sub(Self::keepalive_tick(after))
            >= after
    }

    /// How often the keepalive loop wakes to check the liveness clock.
    ///
    /// Derived from the probe threshold rather than configured separately: the
    /// keepalive's numbers only make sense as a set, and one knob that scales
    /// them together cannot be tuned into an inconsistent state.
    fn keepalive_tick(after: Duration) -> Duration {
        (after / 5).clamp(Duration::from_millis(25), Duration::from_secs(1))
    }

    /// Is the connection provably alive right now?
    ///
    /// "Provably" is the whole point: a frame arrived recently enough that if
    /// the server had gone quiet, an ECHO probe would have gone out and gone
    /// unanswered. Anything weaker would extend a deadline on an assumption,
    /// which is how a hang comes back.
    ///
    /// Always false when the keepalive is off — with nothing refreshing the
    /// clock, a stale reading means "quiet connection", not "dead server", and
    /// a fresh one is luck rather than evidence.
    fn liveness_is_proven(&self) -> bool {
        let Some(after) = *self.keepalive_after.lock().unwrap() else {
            return false;
        };
        match self.server_silent_for() {
            Some(silent) => silent < after * LIVENESS_WINDOW_PROBES,
            None => false,
        }
    }

    /// The other reading of the same clock: how long the server has been
    /// silent while owing us something, when that silence is evidence rather
    /// than an assumption.
    ///
    /// `Some` only when the keepalive is armed and the connection has been
    /// quiet for longer than [`liveness_is_proven`](Self::liveness_is_proven)
    /// tolerates. The keepalive is what makes the silence mean anything: it
    /// has been asking a question that needs no disk and no share, so "busy
    /// with a slow write" stops explaining the quiet. With probing off the
    /// same reading is just a quiet connection, and this returns `None`.
    ///
    /// Measured with [`quiet_for`](Self::quiet_for) rather than
    /// [`server_silent_for`](Self::server_silent_for), so it never counts
    /// silence from before we asked anything, and so a server that has said
    /// nothing at all since the first request still counts — which
    /// `server_silent_for` cannot express.
    ///
    /// A request that runs out of deadline while this reads `None` is ONE
    /// stalled operation, not a dead link, and must not be reported as one.
    ///
    /// ⚠️ The check is that probing is ARMED, not that a probe actually reached
    /// the wire. A probe with no credit on hand is skipped
    /// (`keepalive_probes_skipped`), so a fully credit-starved connection can
    /// reach this verdict on silence nobody put a question to. ❌ Don't
    /// "tighten" it to require a sent probe without a design pass: credits run
    /// out only when many requests are outstanding and the server has answered
    /// none of them, which is the wedge shape itself, and downgrading that to
    /// `Error::Timeout` would leave the connection standing while every waiter
    /// burns its own deadline one at a time — exactly what
    /// `declare_unresponsive` exists to replace. The premise is weaker there
    /// than the doc above claims; the verdict is still the useful one.
    fn unresponsive_for(&self) -> Option<Duration> {
        let after = (*self.keepalive_after.lock().unwrap())?;
        let quiet = self.quiet_for()?;
        (quiet >= after * LIVENESS_WINDOW_PROBES).then_some(quiet)
    }

    /// How long ago `msg_id`'s frame reached the wire, or `None` if it is no
    /// longer outstanding or has not been sent yet.
    ///
    /// The clock a long poll's refresh cycle runs on: "how long has the server
    /// been holding this subscription". Deliberately NOT `last_activity`,
    /// which an interim STATUS_PENDING restarts — a long poll gets exactly one
    /// of those, right at the start, and a clock that restarted there would
    /// measure the same thing while claiming to measure registration age.
    fn waiter_sent_age(&self, msg_id: MessageId) -> Option<Duration> {
        let now = std::time::Instant::now();
        self.waiters
            .lock()
            .unwrap()
            .get(&msg_id)
            .and_then(|w| w.sent_at)
            .map(|t| now.saturating_duration_since(t))
    }

    /// The `AsyncId` the server assigned `msg_id`, if it has sent an interim
    /// STATUS_PENDING for it.
    fn async_id_for(&self, msg_id: MessageId) -> Option<u64> {
        self.waiters.lock().unwrap().get(&msg_id)?.async_id
    }

    /// How long `msg_id` has gone without a sign of life, or `None` if it is
    /// no longer outstanding (its response has been routed).
    fn waiter_idle_for(&self, msg_id: MessageId) -> Option<Duration> {
        let now = std::time::Instant::now();
        self.waiters
            .lock()
            .unwrap()
            .get(&msg_id)
            .map(|w| now.saturating_duration_since(w.last_activity))
    }

    fn starvation(&self, charge: u16, waited: Duration) -> Error {
        Error::CreditStarvation {
            needed: charge,
            available: self.credits.available(),
            waited,
        }
    }

    /// Snapshot the counters into a plain-value `MetricsSnapshot`, for
    /// `Connection::diagnostics()`.
    pub(crate) fn metrics_snapshot(&self) -> crate::client::diagnostics::MetricsSnapshot {
        self.metrics.snapshot()
    }
}

/// Per-connection counters, all `AtomicU64`, all `Relaxed` reads/writes.
///
/// Lives on [`Inner`] and outlives the receiver task; a snapshot taken
/// after the connection has torn down returns the final values at the
/// moment of death.
///
/// Each field's meaning, and the disjoint partition of the receive-side
/// routing branches, is documented on the matching field of
/// [`MetricsSnapshot`](crate::client::diagnostics::MetricsSnapshot). The
/// design rationale is in the `diagnostics` module docs.
#[derive(Default)]
pub(crate) struct Metrics {
    // Send path
    pub requests_sent: AtomicU64,
    pub compound_requests_sent: AtomicU64,
    pub wire_bytes_sent: AtomicU64,
    pub explicit_cancels_sent: AtomicU64,

    // Receive path: four disjoint routing outcomes
    pub responses_routed_ok: AtomicU64,
    pub responses_routed_err: AtomicU64,
    pub responses_late_after_drop: AtomicU64,
    pub responses_stray: AtomicU64,
    pub wire_bytes_received: AtomicU64,

    // Protocol events
    pub status_pending_loops: AtomicU64,
    pub unsolicited_notifications_received: AtomicU64,
    pub signature_failures: AtomicU64,
    pub decrypt_failures: AtomicU64,
    pub decompress_failures: AtomicU64,
    pub malformed_frames: AtomicU64,
    pub session_expired_events: AtomicU64,

    // Credit accounting
    /// Sends that had to park because the connection's whole credit budget
    /// was in flight. A steady trickle is normal on a saturated pipeline; a
    /// flood means the server's window is too small for the chunk size.
    pub credit_waits: AtomicU64,
    /// Sends that gave up waiting for a grant. Non-zero means a server went
    /// silent while its socket stayed open.
    pub credit_starvations: AtomicU64,
    /// Requests abandoned because the server went silent for longer than the
    /// response timeout.
    pub response_timeouts: AtomicU64,
    /// Frames the transport could not write: a send that timed out or errored.
    /// Non-zero means a wedge on OUR side of the wire, not the server's.
    pub send_failures: AtomicU64,
    /// ECHO probes that reached the wire.
    pub keepalive_probes_sent: AtomicU64,
    /// Probe rounds that never asked anything, so they say nothing about the
    /// server: no credit on hand, or the connection was already dying.
    pub keepalive_probes_skipped: AtomicU64,
    /// Probes sent and left unanswered. Two in a row tear the connection down.
    pub keepalive_failures: AtomicU64,
    /// Requests that outlived the base response deadline because the
    /// connection was provably alive. This is the counter that says the
    /// keepalive is earning its place: each tick is a slow-but-healthy
    /// operation that the deadline alone would have killed.
    pub response_deadline_extensions: AtomicU64,
    /// Long-poll requests retired and re-issued on the refresh cycle. Says a
    /// handover happened, never that anything was wrong.
    pub long_poll_refreshes: AtomicU64,
    /// Stretches this PROCESS spent unscheduled, caught by the keepalive loop
    /// overrunning its own cadence. Says nothing about the server.
    pub scheduling_stalls: AtomicU64,

    // Reconnect
    /// Dials made trying to bring this connection back, across all revivals.
    /// Divided by `reconnects_succeeded + reconnects_failed` it says how hard
    /// each revival had to work.
    pub reconnect_attempts: AtomicU64,
    /// Revivals that ended with a live, authenticated session.
    ///
    /// The counter that answers "is this link quietly flaky?" — a transfer
    /// that completes with a non-zero value here survived something, and a
    /// consumer that wants to say so has the evidence without subscribing to
    /// anything.
    pub reconnects_succeeded: AtomicU64,
    /// Revivals that gave up. Each one surfaced as
    /// [`Error::ReconnectFailed`](crate::Error::ReconnectFailed) to its caller.
    pub reconnects_failed: AtomicU64,

    // Caller-observed outcomes
    pub requests_returned_err: AtomicU64,
}

impl Metrics {
    fn snapshot(&self) -> crate::client::diagnostics::MetricsSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        crate::client::diagnostics::MetricsSnapshot {
            requests_sent: self.requests_sent.load(Relaxed),
            compound_requests_sent: self.compound_requests_sent.load(Relaxed),
            wire_bytes_sent: self.wire_bytes_sent.load(Relaxed),
            explicit_cancels_sent: self.explicit_cancels_sent.load(Relaxed),
            responses_routed_ok: self.responses_routed_ok.load(Relaxed),
            responses_routed_err: self.responses_routed_err.load(Relaxed),
            responses_late_after_drop: self.responses_late_after_drop.load(Relaxed),
            responses_stray: self.responses_stray.load(Relaxed),
            wire_bytes_received: self.wire_bytes_received.load(Relaxed),
            status_pending_loops: self.status_pending_loops.load(Relaxed),
            unsolicited_notifications_received: self
                .unsolicited_notifications_received
                .load(Relaxed),
            signature_failures: self.signature_failures.load(Relaxed),
            decrypt_failures: self.decrypt_failures.load(Relaxed),
            decompress_failures: self.decompress_failures.load(Relaxed),
            malformed_frames: self.malformed_frames.load(Relaxed),
            session_expired_events: self.session_expired_events.load(Relaxed),
            credit_waits: self.credit_waits.load(Relaxed),
            credit_starvations: self.credit_starvations.load(Relaxed),
            response_timeouts: self.response_timeouts.load(Relaxed),
            send_failures: self.send_failures.load(Relaxed),
            keepalive_probes_sent: self.keepalive_probes_sent.load(Relaxed),
            keepalive_probes_skipped: self.keepalive_probes_skipped.load(Relaxed),
            keepalive_failures: self.keepalive_failures.load(Relaxed),
            response_deadline_extensions: self.response_deadline_extensions.load(Relaxed),
            long_poll_refreshes: self.long_poll_refreshes.load(Relaxed),
            scheduling_stalls: self.scheduling_stalls.load(Relaxed),
            reconnect_attempts: self.reconnect_attempts.load(Relaxed),
            reconnects_succeeded: self.reconnects_succeeded.load(Relaxed),
            reconnects_failed: self.reconnects_failed.load(Relaxed),
            requests_returned_err: self.requests_returned_err.load(Relaxed),
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Last `Arc<Inner>` dropping: abort every background task still
        // alive. The two that hold the transport let go of it as they stop,
        // which is what closes the socket. The writer would also stop on its
        // own once `write_tx` drops, but not while it is parked inside a send.
        if let Some(handle) = self.receiver_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.writer_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.keepalive_task.lock().unwrap().take() {
            handle.abort();
        }
        if let Some(handle) = self.sweeper_task.lock().unwrap().take() {
            handle.abort();
        }
    }
}

/// Low-level connection with actor-based response routing.
///
/// Manages credit tracking, message ID sequencing, preauth integrity hash,
/// message signing, and encryption. A background receiver task owns the
/// transport's read half and routes each incoming frame to the
/// `oneshot::Sender` registered for its `MessageId`. Callers go through
/// [`execute`](Self::execute) / [`execute_compound`](Self::execute_compound)
/// which register the waiter, send the frame, and await the matching
/// `oneshot::Receiver` — all owned locally by the future, so dropping the
/// future mid-flight is safe (the late arrival is discarded on the receiver
/// task when the `Sender` fails to deliver).
///
/// `Connection` is `Clone`; cloning is a cheap `Arc::clone` bump. All clones
/// share the same receiver task, credits, and waiters map, so concurrent
/// `execute` calls on different clones multiplex over the same SMB session.
#[derive(Clone)]
pub struct Connection {
    /// Shared state (credits, waiters, crypto, transport sender, negotiated
    /// params, receiver task) behind `Arc<Inner>`. `clone()` bumps this.
    inner: Arc<Inner>,
    /// `Some` on a clone a file handle owns: the session it may send on. See
    /// [`pinned_to_session`](Self::pinned_to_session). Per clone, unlike
    /// everything in `inner`.
    pin: Option<SessionPin>,
}

/// The session a [`Connection`] clone (or a [`Tree`](crate::Tree)) was pinned
/// to: which connection, and the generation and session id it had at the
/// time.
///
/// Generation and session id both, because neither covers the other. The
/// generation ticks only once a WHOLE revival has succeeded, so it still
/// matches through the new session's NEGOTIATE and SESSION_SETUP; the session
/// id is wiped at the start of a revival, so it catches that window. And a
/// second `Session::setup` on the same socket replaces the session id without
/// a revival at all. The connection (its `ClientGuid`, one per connection for
/// life) because a tree id means nothing on another connection either: a DFS
/// target's tree sent on the primary names whatever the primary's session gave
/// that id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionPin {
    connection: Guid,
    generation: u64,
    session_id: SessionId,
}

impl Connection {
    /// Create a connection from an existing transport (for testing with mock).
    pub fn from_transport(
        sender: Box<dyn TransportSend>,
        receiver: Box<dyn TransportReceive>,
        server_name: impl Into<String>,
    ) -> Self {
        let (write_tx, write_rx) = mpsc::channel(WRITE_QUEUE_DEPTH);
        let inner = Arc::new(Inner::new(write_tx, server_name.into()));
        spawn_plumbing(&inner, sender, receiver, write_rx);
        Self { inner, pin: None }
    }

    /// A clone that sends only on the session this connection has right now,
    /// and refuses every request with [`Error::HandleInvalidated`] once that
    /// session is gone.
    ///
    /// For anything holding a tree id or file id across awaits: a revival
    /// happens in place, so the clone keeps a working connection, but the new
    /// session numbers its handles from scratch and the same ids can name a
    /// different file there. Take the pin BEFORE the CREATE, and send
    /// everything the handle does through that one clone.
    ///
    /// Pinning a pinned clone keeps the original pin, so a stale clone can't
    /// be pinned again into looking current.
    pub(crate) fn pinned_to_session(&self) -> Connection {
        let pin = self.pin.unwrap_or_else(|| self.current_pin());
        Connection {
            inner: Arc::clone(&self.inner),
            pin: Some(pin),
        }
    }

    /// A clone pinned to `pin` as well as to whatever this one is pinned to,
    /// for sending a [`Tree`](crate::Tree)'s requests: they're refused once
    /// either session is gone. `None` (a tree built by hand, never connected)
    /// adds nothing.
    pub(crate) fn pinned_to(&self, pin: Option<SessionPin>) -> Connection {
        let pin = match (self.pin, pin) {
            (Some(mine), Some(theirs)) if mine != theirs => {
                // Two different pins can't both be current, so keep the one
                // that isn't: the requests must fail if either is stale.
                Some(if self.pin_is_current(mine) {
                    theirs
                } else {
                    mine
                })
            }
            (mine, theirs) => mine.or(theirs),
        };
        Connection {
            inner: Arc::clone(&self.inner),
            pin,
        }
    }

    /// This clone's pin, if it has one.
    pub(crate) fn pin(&self) -> Option<SessionPin> {
        self.pin
    }

    /// The session this connection has right now.
    fn current_pin(&self) -> SessionPin {
        SessionPin {
            connection: self.inner.client_guid,
            generation: self.generation(),
            session_id: self.session_id(),
        }
    }

    fn pin_is_current(&self, pin: SessionPin) -> bool {
        pin.session_id != SessionId(0) && pin == self.current_pin()
    }

    /// Whether this clone may still send: always on an unpinned one, and on a
    /// pinned one while its session is the current one.
    pub(crate) fn pinned_session_is_current(&self) -> bool {
        self.session_for_request().is_ok()
    }

    /// The session id a request from this clone goes out with, or the
    /// refusal of a pinned clone whose session is gone.
    ///
    /// Read once and stamped into the header, never re-read: if a revival
    /// slips in after this check, the request still carries the dead session's
    /// id, which the new session refuses. Re-reading would hand the stale tree
    /// and file ids the new session's credentials.
    fn session_for_request(&self) -> Result<SessionId> {
        let Some(pin) = self.pin else {
            return Ok(self.session_id());
        };
        if self.pin_is_current(pin) {
            return Ok(pin.session_id);
        }
        Err(Error::HandleInvalidated {
            opened_on: pin.generation,
            current: self.generation(),
        })
    }

    /// Connect to an SMB server over TCP.
    ///
    /// `timeout` bounds the whole attempt, name resolution included, and every
    /// address the name resolves to gets a chance inside it. To tune how those
    /// attempts are spread, use
    /// [`connect_with`](Self::connect_with).
    pub async fn connect(addr: &str, timeout: Duration) -> Result<Self> {
        Self::connect_with(
            addr,
            crate::transport::ConnectOptions::with_timeout(timeout),
        )
        .await
    }

    /// [`connect`](Self::connect) with the connect budget under the caller's
    /// control. See [`ConnectOptions`](crate::transport::ConnectOptions).
    pub async fn connect_with(addr: &str, opts: crate::transport::ConnectOptions) -> Result<Self> {
        // ❌ Never `split(':')` here: it reads an IPv6 literal as its first
        // group (`[::1]:445` → `[`), and this name goes out as the server half
        // of the UNC path in every TREE_CONNECT. One derivation, shared with
        // `SmbClient::unc_for`, so the two can't disagree.
        let server_name = crate::client::host_of(addr).to_string();
        let transport = TcpTransport::connect_with(addr, opts).await?;
        info!("connection: connected addr={addr:?}");
        let transport = Arc::new(transport);
        Ok(Self::from_transport(
            Box::new(Arc::clone(&transport)),
            Box::new(transport),
            server_name,
        ))
    }

    /// Perform the SMB2 NEGOTIATE exchange.
    pub async fn negotiate(&mut self) -> Result<()> {
        debug!("negotiate: sending request, dialects={:?}", Dialect::ALL);
        let client_guid = self.inner.client_guid;

        let mut negotiate_contexts = vec![
            NegotiateContext::PreauthIntegrity {
                hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                salt: generate_salt(),
            },
            NegotiateContext::Encryption {
                ciphers: vec![
                    CIPHER_AES_128_GCM,
                    CIPHER_AES_128_CCM,
                    CIPHER_AES_256_GCM,
                    CIPHER_AES_256_CCM,
                ],
            },
            NegotiateContext::Signing {
                algorithms: vec![SIGNING_AES_GMAC, SIGNING_AES_CMAC, SIGNING_HMAC_SHA256],
            },
        ];

        if self.inner.compression_requested.load(Ordering::Acquire) {
            negotiate_contexts.push(NegotiateContext::Compression {
                flags: 0,
                algorithms: vec![COMPRESSION_LZ4],
            });
        }

        let request = NegotiateRequest {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            capabilities: Capabilities::new(
                Capabilities::DFS | Capabilities::LEASING | Capabilities::LARGE_MTU,
            ),
            client_guid,
            dialects: Dialect::ALL.to_vec(),
            negotiate_contexts,
        };

        // NEGOTIATE is exempt from the credit window rather than funded by it
        // (MS-SMB2 § 3.2.5.1.1): the server has granted nothing yet, and the
        // response is what opens the budget. Reserving here instead would mean
        // seeding a permit in the pool, which anything racing the handshake
        // could spend — see `CreditReservation::exempt`.
        let reservation = CreditReservation::exempt();
        let mut header = Header::new_request(Command::Negotiate);
        let msg_id = self.allocate_msg_id(1);
        header.message_id = msg_id;
        header.credits = 1;
        let req_bytes = pack_message(&header, &request);

        // Update preauth hash with request bytes.
        self.inner.preauth_hasher.lock().unwrap().update(&req_bytes);

        let guard = self.register_waiter(msg_id, Command::Negotiate)?;

        let rtt_start = std::time::Instant::now();
        self.inner
            .send_and_count(&req_bytes, Command::Negotiate)
            .await?;
        reservation.commit();
        self.inner.mark_sent(&[msg_id]);

        // Through the response deadline like every other request: a server
        // that accepts the socket and never answers would otherwise hang the
        // handshake (and any reconnect built on it) forever.
        let frame = self.await_response(guard, Command::Negotiate).await?;
        *self.inner.estimated_rtt.lock().unwrap() = Some(rtt_start.elapsed());

        // Preauth hash update with response bytes.
        self.inner.preauth_hasher.lock().unwrap().update(&frame.raw);

        let resp_header = &frame.header;
        if !resp_header.is_response() {
            return Err(Error::invalid_data("expected a response but got a request"));
        }
        if resp_header.command != Command::Negotiate {
            return Err(Error::invalid_data(format!(
                "expected Negotiate response, got {:?}",
                resp_header.command
            )));
        }

        if resp_header.status != NtStatus::SUCCESS {
            return Err(Error::Protocol {
                status: resp_header.status,
                command: Command::Negotiate,
            });
        }

        // Parse the body.
        let mut cursor = ReadCursor::new(&frame.body);
        let resp = NegotiateResponse::unpack(&mut cursor)?;

        if !Dialect::ALL.contains(&resp.dialect_revision) {
            return Err(Error::invalid_data(format!(
                "server selected dialect 0x{:04X} which we did not offer",
                u16::from(resp.dialect_revision)
            )));
        }
        if resp.max_read_size < 65536 {
            return Err(Error::invalid_data(format!(
                "MaxReadSize {} is below minimum 65536",
                resp.max_read_size
            )));
        }
        if resp.max_write_size < 65536 {
            return Err(Error::invalid_data(format!(
                "MaxWriteSize {} is below minimum 65536",
                resp.max_write_size
            )));
        }

        let mut gmac_negotiated = false;
        let mut cipher = None;
        let mut compression_supported = false;

        for ctx in &resp.negotiate_contexts {
            match ctx {
                NegotiateContext::Signing { algorithms }
                    if algorithms.contains(&SIGNING_AES_GMAC) =>
                {
                    gmac_negotiated = true;
                }
                NegotiateContext::Encryption { ciphers } => {
                    if let Some(&c) = ciphers.first() {
                        cipher = match c {
                            CIPHER_AES_128_CCM => Some(Cipher::Aes128Ccm),
                            CIPHER_AES_128_GCM => Some(Cipher::Aes128Gcm),
                            CIPHER_AES_256_CCM => Some(Cipher::Aes256Ccm),
                            CIPHER_AES_256_GCM => Some(Cipher::Aes256Gcm),
                            _ => None,
                        };
                    }
                }
                NegotiateContext::Compression { algorithms, .. }
                    if algorithms.contains(&COMPRESSION_LZ4) =>
                {
                    compression_supported = true;
                }
                _ => {}
            }
        }

        let signing_required = resp.security_mode.signing_required();
        let compression_enabled =
            self.inner.compression_requested.load(Ordering::Acquire) && compression_supported;
        self.inner
            .compression_enabled
            .store(compression_enabled, Ordering::Release);

        // Overwrites: a revived connection renegotiates from scratch on a
        // fresh socket, and the numbers it comes back with are the only ones
        // that describe the server we are now talking to.
        *self.inner.params.lock().unwrap() = Some(NegotiatedParams {
            dialect: resp.dialect_revision,
            max_read_size: resp.max_read_size,
            max_write_size: resp.max_write_size,
            max_transact_size: resp.max_transact_size,
            server_guid: resp.server_guid,
            signing_required,
            capabilities: resp.capabilities,
            gmac_negotiated,
            cipher,
            compression_supported,
        });

        info!(
            "negotiate: dialect={}, signing_required={}, capabilities={:?}",
            resp.dialect_revision, signing_required, resp.capabilities
        );
        debug!(
            "negotiate: max_read={}, max_write={}, max_transact={}, server_guid={:?}, cipher={:?}, gmac={}, compression={}",
            resp.max_read_size, resp.max_write_size, resp.max_transact_size,
            resp.server_guid, cipher, gmac_negotiated, compression_enabled
        );

        Ok(())
    }

    /// Get the estimated round-trip time: NEGOTIATE's, lowered by the
    /// quickest small request (a `stat`, a CLOSE, an ECHO) answered since.
    pub fn estimated_rtt(&self) -> Option<Duration> {
        *self.inner.estimated_rtt.lock().unwrap()
    }

    /// The link rate the last download on this connection measured, if it's
    /// recent enough to still describe the link.
    pub(crate) fn read_rate_hint(&self) -> Option<f64> {
        recent(&self.inner.read_rate_hint)
    }

    /// How fast this connection's link carries downloads, in bytes per
    /// second, or `None` when nothing recent was measured.
    ///
    /// Measured by streaming downloads ([`Tree::download`](crate::Tree::download)
    /// and anything else built on [`FileDownload`](crate::FileDownload)), and
    /// shared by every clone of this connection. It's the link's capacity
    /// rather than what one download achieved: it's taken from READ answers
    /// that queued behind each other on the wire, timed as they arrived, so
    /// a short download or a consumer slow to take chunks doesn't drag it
    /// down. What makes it `None`, and how far to trust it otherwise:
    ///
    /// - **Only a download of two or more chunks measures it.** One READ's
    ///   rate is mostly its round trip, so small files and compound reads
    ///   leave it untouched.
    /// - **It expires 30 seconds after the last measurement.** A running
    ///   download refreshes it with every chunk, so it only goes stale when
    ///   the connection stops downloading. Long enough to span the gaps in a
    ///   folder copy, short enough that a link that changed since (Wi-Fi
    ///   roaming, a VPN coming up) is measured afresh.
    /// - **A reconnect clears it**, since the new socket may run over a
    ///   different path.
    /// - **It errs low, never high.** Several transfers sharing the
    ///   connection, a busy server disk, or a download whose READs never
    ///   overlapped (then it falls back to the pace they did arrive at) all
    ///   read lower than the link.
    ///
    /// To decide between one compound read and a streamed download, use
    /// [`quick_read_limit`](Self::quick_read_limit), which is built on this.
    #[must_use]
    pub fn download_rate_hint(&self) -> Option<u64> {
        self.read_rate_hint().map(|rate| rate as u64)
    }

    /// The largest file worth reading in one READ on this connection right
    /// now, in bytes: the size cut-off between one compound
    /// CREATE + READ + CLOSE and a streamed [`Tree::download`](crate::Tree::download).
    ///
    /// One READ costs a single round trip, but nothing else on the connection
    /// moves while it arrives and it reports no progress. A streamed download
    /// costs about two more round trips (CREATE and the first READ each wait
    /// for an answer) and never blocks the connection for more than about a
    /// chunk. So a file is worth one READ when the link moves it in about
    /// 250 ms, a fixed budget for how long a listing may wait behind one
    /// frame. It doesn't follow the read-ahead window's learned headroom: on a
    /// quiet LAN that shrinks to tens of milliseconds, and a cut-off that
    /// shrank with it would stream small files at a round trip or two each.
    ///
    /// - With a [`download_rate_hint`](Self::download_rate_hint), it's what the
    ///   connection moves in 250 ms at that rate.
    /// - Never less than one [`DOWNLOAD_CHUNK_SIZE`](crate::DOWNLOAD_CHUNK_SIZE)
    ///   (512 KiB): up to one chunk, a download is one READ anyway, so the
    ///   compound read is strictly cheaper. That's also the answer with no
    ///   hint.
    /// - Never more than the server's `MaxReadSize`, which bounds one READ.
    ///   Before NEGOTIATE, assumes the 64 KiB every dialect allows.
    /// - Never more than half the window funds next to the CREATE and CLOSE,
    ///   once the server has shown a [`credit_ceiling`](Self::credit_ceiling).
    ///   This one beats the one-chunk floor, and can be 0 on a tiny window,
    ///   meaning stream everything: a chain the window can't fund waits out
    ///   the credit deadline or fails with [`Error::CreditStarvation`].
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &smb2::Tree) -> Result<(), smb2::Error> {
    /// let size: u64 = 3 * 1024 * 1024; // from a directory listing
    /// let data = if size <= conn.quick_read_limit() {
    ///     tree.read_file_compound_sized(conn, "photo.jpg", size).await?
    /// } else {
    ///     tree.download(conn, "photo.jpg").await?.collect().await?
    /// };
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn quick_read_limit(&self) -> u64 {
        let max_read = self.params().map_or(65536, |p| p.max_read_size);
        let limit = read_ahead::quick_read_limit(self.read_rate_hint(), max_read);
        self.fundable_payload(limit, 2)
    }

    /// Record a download's measured link rate for the next one.
    pub(crate) fn note_read_rate(&self, bytes_per_sec: f64) {
        *self.inner.read_rate_hint.lock().unwrap() = Some((rt::Instant::now(), bytes_per_sec));
    }

    /// What the next download on this connection starts from.
    pub(crate) fn read_link_hint(&self) -> read_ahead::LinkHint {
        read_ahead::LinkHint {
            rtt: self.estimated_rtt(),
            rate: self.read_rate_hint(),
            lateness: recent(&self.inner.read_lateness_hint),
        }
    }

    /// Record what a download's window has learned about the link so far.
    pub(crate) fn note_read(&self, window: &read_ahead::Window) {
        if let Some(rate) = window.rate_to_share() {
            self.note_read_rate(rate);
        }
        if let Some(lateness) = window.lateness_to_share() {
            *self.inner.read_lateness_hint.lock().unwrap() = Some((rt::Instant::now(), lateness));
        }
    }

    /// The rate the last upload on this connection had its WRITEs confirmed
    /// at, if it's recent enough to still describe the link.
    pub(crate) fn write_rate_hint(&self) -> Option<f64> {
        recent(&self.inner.write_rate_hint)
    }

    /// How fast this connection's link carries uploads, in bytes per second,
    /// or `None` when nothing recent was measured.
    ///
    /// The upload twin of [`download_rate_hint`](Self::download_rate_hint),
    /// measured apart from it because most links are asymmetric. Every
    /// pipelined upload measures it ([`FileWriter`](crate::FileWriter),
    /// [`FileUpload`](crate::FileUpload), [`Tree::write_file_pipelined`](crate::Tree::write_file_pipelined),
    /// [`Tree::write_file_streamed`](crate::Tree::write_file_streamed)), from
    /// WRITE confirmations that queued behind each other, timed as they
    /// arrived, and it's shared by every clone of this connection. The same
    /// rules as the download rate:
    ///
    /// - **Only an upload of two or more WRITEs measures it.** One WRITE's
    ///   rate is mostly its round trip, so small files and compound writes
    ///   leave it untouched.
    /// - **It expires 30 seconds after the last measurement**, and **a
    ///   reconnect clears it**.
    /// - **It errs low, never high.** A WRITE's answer shares the downlink
    ///   with anything else the connection receives, and a producer too slow
    ///   to keep WRITEs queued leaves only the pace they did arrive at. See
    ///   the [`write_behind` module docs](crate::client::write_behind).
    ///
    /// To decide between one compound write and a streamed upload, use
    /// [`quick_write_limit`](Self::quick_write_limit), which is built on this.
    #[must_use]
    pub fn upload_rate_hint(&self) -> Option<u64> {
        self.write_rate_hint().map(|rate| rate as u64)
    }

    /// The largest file worth writing as one frame on this connection right
    /// now, in bytes: the size cut-off between one compound
    /// CREATE + WRITE + FLUSH ([`Tree::write_file_compound`](crate::Tree::write_file_compound),
    /// its CLOSE standalone) and a streamed [`FileWriter`](crate::FileWriter).
    ///
    /// The upload twin of [`quick_read_limit`](Self::quick_read_limit), for
    /// the same reason: one frame costs a single round trip, but nothing else
    /// on the connection moves while it crawls up the link, and it reports no
    /// progress. A streamed upload costs a few more round trips (CREATE, and
    /// the FLUSH and CLOSE after the last WRITE) and never makes the
    /// connection wait behind more than about a chunk. So a file is worth one
    /// frame when the uplink moves it in about 250 ms, the same fixed budget
    /// as for reads, whatever headroom the write-behind window has learned:
    ///
    /// - With an [`upload_rate_hint`](Self::upload_rate_hint), it's what the
    ///   connection moves up in 250 ms at that rate.
    /// - Never less than one [`UPLOAD_CHUNK_SIZE`](crate::UPLOAD_CHUNK_SIZE)
    ///   (512 KiB): up to one chunk, a streamed upload is one WRITE anyway,
    ///   plus the round trips. That's also the answer with no hint.
    /// - Never more than [`compound_write_limit`](Self::compound_write_limit):
    ///   `MaxWriteSize`, lowered to what half the credit window funds. That
    ///   one beats the one-chunk floor and can be 0 on a tiny window, meaning
    ///   stream everything.
    ///
    /// A consumer that promises "one frame, all or nothing" up front (for
    /// example, to skip writing through a temporary name) can decide by this
    /// and still send the frame later if the limit has moved in between: the
    /// promise stays safe as long as the size fits `compound_write_limit`,
    /// which this never exceeds.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # async fn example(conn: &mut smb2::client::Connection, tree: &std::sync::Arc<smb2::Tree>, data: &[u8]) -> Result<(), smb2::Error> {
    /// if data.len() as u64 <= conn.quick_write_limit() {
    ///     tree.write_file_compound(conn, "photo.jpg", data).await?;
    /// } else {
    ///     let mut writer = tree.create_file_writer(conn.clone(), "photo.jpg").await?;
    ///     writer.write_chunk(data).await?;
    ///     writer.finish().await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn quick_write_limit(&self) -> u64 {
        write_behind::quick_write_limit(self.write_rate_hint(), self.compound_write_limit())
    }

    /// Record an upload's measured rate for the next one.
    pub(crate) fn note_write_rate(&self, bytes_per_sec: f64) {
        *self.inner.write_rate_hint.lock().unwrap() = Some((rt::Instant::now(), bytes_per_sec));
    }

    /// What the next upload on this connection starts from.
    pub(crate) fn write_link_hint(&self) -> read_ahead::LinkHint {
        read_ahead::LinkHint {
            rtt: self.estimated_rtt(),
            rate: self.write_rate_hint(),
            lateness: recent(&self.inner.write_lateness_hint),
        }
    }

    /// Record what an upload's window has learned about the link so far.
    pub(crate) fn note_write(&self, window: &read_ahead::Window) {
        if let Some(rate) = window.rate_to_share() {
            self.note_write_rate(rate);
        }
        if let Some(lateness) = window.lateness_to_share() {
            *self.inner.write_lateness_hint.lock().unwrap() = Some((rt::Instant::now(), lateness));
        }
    }

    /// Get the negotiated parameters, or `None` before NEGOTIATE has run.
    ///
    /// Returns an owned copy rather than a borrow because the parameters are
    /// replaced whenever the connection is revived on a fresh socket; every
    /// field is a plain scalar, so the copy costs nothing.
    pub fn params(&self) -> Option<NegotiatedParams> {
        self.inner.params.lock().unwrap().clone()
    }

    /// Get a clone of the preauth hasher's current state.
    ///
    /// The hasher lives behind a lock (shared across `Connection` clones
    /// now that the type is `Clone`). Callers that want to derive per-session
    /// keys — see `session.rs` — take a snapshot via this method and feed
    /// their own session-specific updates into it without disturbing the
    /// shared connection-level hasher. Returning an owned clone is ~a few
    /// hundred bytes of SHA-512 state; cheaper than the actual KDF it feeds.
    pub fn preauth_hasher(&self) -> PreauthHasher {
        self.inner.preauth_hasher.lock().unwrap().clone()
    }

    /// Run a closure with a mutable borrow of the preauth hasher.
    ///
    /// The hasher lives behind a lock now that `Connection` is `Clone`; a
    /// naked `&mut PreauthHasher` can no longer be handed out. Closure-based
    /// access keeps the lock scoped to the caller's update.
    #[doc(hidden)] // unused outside the crate; kept for crate-internal parity.
    pub fn with_preauth_hasher_mut<R>(&self, f: impl FnOnce(&mut PreauthHasher) -> R) -> R {
        let mut h = self.inner.preauth_hasher.lock().unwrap();
        f(&mut h)
    }

    /// Set the session ID.
    pub fn set_session_id(&mut self, id: SessionId) {
        self.inner.crypto.lock().unwrap().session_id = id;
    }

    /// Get the current session ID.
    pub fn session_id(&self) -> SessionId {
        self.inner.crypto.lock().unwrap().session_id
    }

    /// Activate signing with the given key and algorithm.
    pub fn activate_signing(&mut self, key: Vec<u8>, algorithm: SigningAlgorithm) {
        debug!(
            "signing: activated, algo={:?}, key_len={}",
            algorithm,
            key.len()
        );
        let mut c = self.inner.crypto.lock().unwrap();
        c.signing_key = Some(key);
        c.signing_algorithm = Some(algorithm);
        c.should_sign = true;
    }

    /// Activate encryption with the given keys and cipher.
    pub fn activate_encryption(&mut self, enc_key: Vec<u8>, dec_key: Vec<u8>, cipher: Cipher) {
        debug!(
            "encryption: activated, cipher={:?}, enc_key_len={}, dec_key_len={}",
            cipher,
            enc_key.len(),
            dec_key.len()
        );
        let mut c = self.inner.crypto.lock().unwrap();
        c.encryption_key = Some(enc_key);
        c.decryption_key = Some(dec_key);
        c.encryption_cipher = Some(cipher);
        c.nonce_gen = Some(NonceGenerator::new());
        c.should_encrypt = true;
    }

    /// Whether encryption is active on this connection.
    pub fn should_encrypt(&self) -> bool {
        self.inner.crypto.lock().unwrap().should_encrypt
    }

    /// Credits on hand: granted by the server and not yet spent on a request
    /// that is still in flight.
    ///
    /// This is the budget a new request can draw on right now, so it drops as
    /// requests go out and climbs as their responses come back. It is *not*
    /// the size of the server's window — subtract it from that and you have
    /// what the pipeline is currently holding.
    pub fn credits(&self) -> u16 {
        self.inner.credits.available()
    }

    /// How many concurrent compound reads of `bytes` each the credit window
    /// can carry. Always at least 1.
    ///
    /// `bytes` is a per-operation size in bytes, clamped to the server's
    /// `MaxReadSize` because one READ can't return more than that. The answer
    /// counts the whole CREATE+READ+CLOSE chain
    /// [`Tree::read_file_compound_sized`](crate::Tree::read_file_compound_sized)
    /// sends, since the server charges every sub-request.
    ///
    /// This is an estimate of steady state, not a reading of unspent credits
    /// (that is [`credits`](Self::credits)): it measures the window the client
    /// steers the server toward, or the server's
    /// [`credit_ceiling`](Self::credit_ceiling) once it has shown one below
    /// that, and other work on the same connection draws on the same pool. Use
    /// it to size a batch of concurrent reads; launching more than this many
    /// just parks the extras in `reserve_credits` until earlier ones finish.
    pub fn credit_capacity_for(&self, bytes: u64) -> usize {
        let max_read = self.params().map(|p| p.max_read_size).unwrap_or(65536) as u64;
        let read_charge = credits::charge_for_payload(bytes.min(max_read));
        // Plus one credit each for the CREATE and the CLOSE riding along.
        credits::capacity_for_charge(read_charge.saturating_add(2), self.credit_ceiling())
    }

    /// The most credits this connection's window will ever hold, once the
    /// server has shown it: `None` while the server may still grow it.
    ///
    /// Every request asks the server to grow the window, and servers ramp up to
    /// their own maximum (Samba's `smb2 max credits`, 8,192 by default). A
    /// response that grants no more than its request consumed, although the
    /// request asked for more, marks the ceiling; growth past it clears it
    /// again. A request charging more than the ceiling can never be funded, so
    /// it fails at once with [`Error::CreditStarvation`] instead of waiting out
    /// [`set_credit_wait_timeout`](Self::set_credit_wait_timeout). Resets with
    /// the connection.
    ///
    /// The size cut-offs this crate hands out already account for it:
    /// [`quick_read_limit`](Self::quick_read_limit),
    /// [`quick_write_limit`](Self::quick_write_limit),
    /// [`compound_write_limit`](Self::compound_write_limit), and
    /// [`credit_capacity_for`](Self::credit_capacity_for). So do the chunks
    /// every chunked transfer sends.
    #[must_use]
    pub fn credit_ceiling(&self) -> Option<u16> {
        self.inner.credits.ceiling()
    }

    /// The largest `data` [`Tree::write_file_compound`](crate::Tree::write_file_compound)
    /// (or [`Tree::write_file_compound_exclusive`](crate::Tree::write_file_compound_exclusive))
    /// sends comfortably on this connection right now, in bytes.
    ///
    /// The server's `MaxWriteSize`, which bounds one WRITE, lowered once the
    /// server has shown a [`credit_ceiling`](Self::credit_ceiling) to what
    /// half the window funds next to the CREATE and FLUSH riding along. Half,
    /// because a chain charging the whole window can only be funded once
    /// everything else on the connection has been answered, and a directory
    /// watcher's long poll never is on cue. Can be 0 on a tiny window, meaning
    /// stream everything. Before NEGOTIATE, assumes the 64 KiB every dialect
    /// allows.
    ///
    /// A consumer that promises "one frame carries all the data" for a
    /// compound write should decide by this, up front: a bigger chain can
    /// wait out the credit deadline or fail with [`Error::CreditStarvation`].
    #[must_use]
    pub fn compound_write_limit(&self) -> u64 {
        let max_write = self.params().map_or(65536, |p| p.max_write_size);
        self.fundable_payload(u64::from(max_write), 2)
    }

    /// Whether this server has refused a listing in
    /// `FileIdBothDirectoryInformation` (see `ListOptions::index_numbers`).
    pub(crate) fn listing_ids_refused(&self) -> bool {
        self.inner.listing_ids_refused.load(Ordering::Acquire)
    }

    /// Record that this server refuses `FileIdBothDirectoryInformation`.
    pub(crate) fn note_listing_ids_refused(&self) {
        self.inner
            .listing_ids_refused
            .store(true, Ordering::Release);
    }

    /// What the crate may charge at once for something it sizes itself: half
    /// the ceiling, once the server has shown one (`credits.rs`).
    pub(crate) fn comfortable_charge(&self) -> Option<u16> {
        self.inner.credits.comfortable_charge()
    }

    /// `max` bytes, lowered to what half the window funds once the server has
    /// shown a ceiling, after `riders` credits for the other requests in the
    /// same compound. 0 when the riders alone take that budget.
    pub(crate) fn fundable_payload(&self, max: u64, riders: u16) -> u64 {
        match self.inner.credits.comfortable_charge() {
            None => max,
            Some(charge) => max.min(u64::from(charge.saturating_sub(riders)) * 65536),
        }
    }

    /// A chunk of at most `max` bytes that the window funds comfortably, never
    /// under one credit's 64 KiB. What every chunked transfer sizes its
    /// requests by, so a server with a small window slows a transfer down
    /// rather than failing it.
    pub(crate) fn fundable_chunk(&self, max: u32) -> u32 {
        let floor = max.min(65536);
        (self.fundable_payload(u64::from(max), 0) as u32).max(floor)
    }

    /// How long a send waits for the server to grant credits before failing
    /// with [`Error::CreditStarvation`].
    ///
    /// Defaults to 30 s. A send only waits at all once the connection's whole
    /// credit budget is in flight, which on a healthy server clears in
    /// milliseconds; the deadline exists so a server that stops answering
    /// surfaces as an error instead of a wait that never ends. Raise it for a
    /// server that is legitimately slow under heavy load.
    ///
    /// There is deliberately no way to wait forever: that is the failure this
    /// bound exists to prevent.
    pub fn set_credit_wait_timeout(&self, after: Duration) {
        self.inner.credits.set_wait_timeout(after);
    }

    /// The `MessageId` that will be assigned to the next request.
    ///
    /// Starts at 0 on a fresh connection and increments by `credit_charge`
    /// per allocation. Pre-first-send this is `0`; after a single
    /// single-credit `execute` it's `1`.
    pub fn next_message_id(&self) -> u64 {
        self.inner.next_message_id.load(Ordering::Acquire)
    }

    /// Get the server name.
    pub fn server_name(&self) -> &str {
        &self.inner.server_name
    }

    /// Set whether the client wants compression.
    pub fn set_compression_requested(&mut self, requested: bool) {
        self.inner
            .compression_requested
            .store(requested, Ordering::Release);
    }

    /// Whether compression is active on this connection.
    pub fn compression_enabled(&self) -> bool {
        self.inner.compression_enabled.load(Ordering::Acquire)
    }

    /// Send a single SMB2 request and wait for its response.
    ///
    /// Takes `&self` so multiple clones of a `Connection` can call `execute`
    /// concurrently from different tasks — the receiver task routes each
    /// response to its own `oneshot::Sender` by `MessageId`. Cancellation
    /// by drop is safe by construction: if the caller's future is dropped
    /// before the response arrives, the `oneshot::Receiver` drops, and
    /// the receiver task discards the late frame silently on arrival
    /// (credits still apply).
    ///
    /// Equivalent to `execute_with_credits(command, body, tree_id, CreditCharge(1))`.
    /// For large READ / WRITE ops (> 64 KB payload), use `execute_with_credits`
    /// with a charge of `ceil(payload_size / 65536)` — each credit consumed
    /// also consumes one consecutive `MessageId`, and gaps in the id
    /// sequence cause the server to drop the connection.
    pub async fn execute(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
    ) -> Result<Frame> {
        self.execute_with_credits(command, body, tree_id, CreditCharge(1))
            .await
    }

    /// Crate-internal variant of [`execute`] that also returns the plaintext
    /// request bytes that were packed on the wire (before any encryption).
    ///
    /// Only `session.rs` needs this: its SESSION_SETUP rounds feed the
    /// *request* bytes into the session-local preauth hasher for key
    /// derivation, and the signed/encrypted wire form would break the
    /// hash because preauth covers the plaintext. Rather than forcing
    /// session.rs to re-pack messages with a predicted msg_id, we let
    /// `execute_with_credits_capturing_request` hand them back.
    pub(crate) async fn execute_capturing_request(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
    ) -> Result<(Frame, Vec<u8>)> {
        self.execute_with_credits_capturing_request(command, body, tree_id, CreditCharge(1))
            .await
    }

    /// See [`Self::execute_capturing_request`].
    pub(crate) async fn execute_with_credits_capturing_request(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        credit_charge: CreditCharge,
    ) -> Result<(Frame, Vec<u8>)> {
        let result = self
            .execute_with_credits_capturing_request_inner(command, body, tree_id, credit_charge)
            .await;
        if result.is_err() {
            self.inner
                .metrics
                .requests_returned_err
                .fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn execute_with_credits_capturing_request_inner(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        credit_charge: CreditCharge,
    ) -> Result<(Frame, Vec<u8>)> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        self.session_for_request()?;
        let charge = credit_charge.0.max(1);
        let mut reservation = self.inner.reserve_credits(charge, command).await?;
        let session_id = self.session_for_request()?;
        let msg_id = self.allocate_msg_id(charge as u64);

        let mut header = Header::new_request(command);
        header.message_id = msg_id;
        reservation.stamp(&mut header, charge);
        header.session_id = session_id;
        if let Some(tid) = tree_id {
            header.tree_id = Some(tid);
        }

        let (should_sign, should_encrypt) = {
            let c = self.inner.crypto.lock().unwrap();
            (c.should_sign, c.should_encrypt)
        };

        if should_sign && !should_encrypt {
            header.flags.set_signed();
        }
        if self.should_set_dfs_flag(tree_id) {
            header.flags |= HeaderFlags::new(HeaderFlags::DFS_OPERATIONS);
        }

        let mut msg_bytes = pack_message(&header, body);
        let captured = msg_bytes.clone();

        let guard = self.register_waiter(msg_id, command)?;

        let wire_bytes = if should_encrypt {
            match self.encrypt_bytes(&msg_bytes) {
                Ok(enc) => enc,
                Err(e) => return Err(e),
            }
        } else {
            if should_sign {
                let c = self.inner.crypto.lock().unwrap();
                if let (Some(key), Some(algo)) = (&c.signing_key, &c.signing_algorithm) {
                    if let Err(e) =
                        signing::sign_message(&mut msg_bytes, key, *algo, msg_id.0, false)
                    {
                        drop(c);
                        return Err(e);
                    }
                }
            }
            msg_bytes
        };

        self.inner.send_and_count(&wire_bytes, command).await?;
        reservation.commit();
        self.inner.mark_sent(&[msg_id]);
        // TRACE, not DEBUG: per-request frame plumbing. Fires for every request, so at
        // DEBUG it floods a consumer during high-throughput ops (e.g. a recursive
        // directory scan). Lifecycle/errors stay at DEBUG. See AGENTS.md § Logging.
        trace!(
            "execute_cap: cmd={:?}, msg_id={}, credit_charge={}, tree_id={:?}, signed={}, encrypted={}",
            command, msg_id.0, charge, tree_id, should_sign, should_encrypt
        );
        let frame = self.await_response(guard, command).await?;
        Ok((frame, captured))
    }

    /// Send a single SMB2 request with a caller-specified credit charge.
    ///
    /// Same semantics as [`execute`](Self::execute) — see that method's doc
    /// for the concurrency / cancellation invariants — but lets the caller
    /// set `credit_charge` directly. Use `CreditCharge(ceil(payload_size /
    /// 65536))` for READ / WRITE ops larger than 64 KB.
    ///
    /// On the wire this is the same as `send_request_with_credits` +
    /// `receive_response` — the difference is that this method owns its
    /// `oneshot::Receiver` locally (not in a caller-shared FIFO), so
    /// it's safe to call from multiple tasks on clones of the same
    /// `Connection`.
    pub async fn execute_with_credits(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        credit_charge: CreditCharge,
    ) -> Result<Frame> {
        let result = self
            .execute_with_credits_inner(command, body, tree_id, credit_charge)
            .await;
        if result.is_err() {
            self.inner
                .metrics
                .requests_returned_err
                .fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn execute_with_credits_inner(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        credit_charge: CreditCharge,
    ) -> Result<Frame> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        // Checked before spending credits, and again once they're spent: the
        // wait for them can outlast a revival.
        self.session_for_request()?;
        let charge = credit_charge.0.max(1);
        // Spend the credits BEFORE allocating a MessageId: the two advance
        // together (MS-SMB2 § 3.2.4.1.6 consumes `CreditCharge` sequence
        // numbers per request), and a reservation that has to wait must not
        // leave a hole in the sequence window meanwhile.
        let mut reservation = self.inner.reserve_credits(charge, command).await?;
        let session_id = self.session_for_request()?;
        let msg_id = self.allocate_msg_id(charge as u64);

        let mut header = Header::new_request(command);
        header.message_id = msg_id;
        reservation.stamp(&mut header, charge);
        header.session_id = session_id;
        if let Some(tid) = tree_id {
            header.tree_id = Some(tid);
        }

        let (should_sign, should_encrypt) = {
            let c = self.inner.crypto.lock().unwrap();
            (c.should_sign, c.should_encrypt)
        };

        if should_sign && !should_encrypt {
            header.flags.set_signed();
        }
        if self.should_set_dfs_flag(tree_id) {
            header.flags |= HeaderFlags::new(HeaderFlags::DFS_OPERATIONS);
        }

        let mut msg_bytes = pack_message(&header, body);

        // Register waiter BEFORE send so the receiver task can match any
        // fast-arriving response. `register_waiter` atomically rechecks
        // `disconnected` under the waiters lock, so a receiver-task
        // teardown between the early fast-path check above and this
        // insertion returns `Err(Disconnected)` instead of leaving a
        // ghost Sender that never gets routed.
        let guard = self.register_waiter(msg_id, command)?;

        // Build the wire bytes with encryption / signing / compression.
        let wire_bytes = if should_encrypt {
            match self.encrypt_bytes(&msg_bytes) {
                Ok(enc) => enc,
                Err(e) => return Err(e),
            }
        } else {
            if should_sign {
                let c = self.inner.crypto.lock().unwrap();
                if let (Some(key), Some(algo)) = (&c.signing_key, &c.signing_algorithm) {
                    if let Err(e) =
                        signing::sign_message(&mut msg_bytes, key, *algo, msg_id.0, false)
                    {
                        drop(c);
                        return Err(e);
                    }
                }
            }
            if self.compression_enabled() && msg_bytes.len() > Header::SIZE {
                if let Some(compressed) = compress_message(&msg_bytes, Header::SIZE) {
                    let framed = build_compressed_frame(&compressed);
                    match self.inner.send_and_count(&framed, command).await {
                        Ok(()) => {
                            reservation.commit();
                            self.inner.mark_sent(&[msg_id]);
                            // TRACE: per-request frame plumbing (see execute_cap above).
                            trace!(
                                "execute: cmd={:?}, msg_id={}, credit_charge={}, tree_id={:?}, signed={}, compressed {}->{} bytes",
                                command, msg_id.0, charge, tree_id, should_sign,
                                msg_bytes.len(), framed.len()
                            );
                            return self.await_response(guard, command).await;
                        }
                        Err(e) => {
                            self.remove_waiter(msg_id);
                            return Err(e);
                        }
                    }
                }
            }
            msg_bytes
        };

        if let Err(e) = self.inner.send_and_count(&wire_bytes, command).await {
            self.remove_waiter(msg_id);
            return Err(e);
        }
        reservation.commit();
        self.inner.mark_sent(&[msg_id]);
        // TRACE: per-request frame plumbing (see execute_cap above).
        trace!(
            "execute: cmd={:?}, msg_id={}, credit_charge={}, tree_id={:?}, signed={}, encrypted={}, len={}",
            command, msg_id.0, charge, tree_id, should_sign, should_encrypt, wire_bytes.len()
        );
        self.await_response(guard, command).await
    }

    /// Send a request and return its response receiver without awaiting it.
    ///
    /// Same wire-level work as [`execute`](Self::execute) — allocate
    /// `MessageId`, register waiter, sign / encrypt, send bytes — but
    /// stops as soon as `transport.send().await` returns and hands back
    /// the `oneshot::Receiver` for the response. Use this for pipelining:
    /// dispatch the next request before awaiting the previous response,
    /// keeping the wire continuously armed.
    ///
    /// **Eager-send guarantee**: when this future resolves to `Ok(rx)`,
    /// the request bytes have been handed to the transport. The caller
    /// can rely on "after this `.await` completes, the request is on
    /// the wire."
    ///
    /// The returned `Receiver` follows the same drop-safety contract as
    /// [`execute`]'s internal one: dropping it without awaiting causes
    /// the receiver task to discard the late response silently when it
    /// arrives (credits still apply).
    ///
    /// Currently used by [`Watcher`](crate::Watcher) to pre-issue the
    /// next CHANGE_NOTIFY before awaiting the current one. Other call
    /// sites should prefer [`execute`] / [`execute_with_credits`] unless
    /// they specifically need the pipelining shape.
    pub(crate) async fn dispatch(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
    ) -> Result<WaiterGuard> {
        self.dispatch_with_credits(command, body, tree_id, CreditCharge(1))
            .await
    }

    /// Variant of [`dispatch`](Self::dispatch) with a caller-specified credit charge.
    pub(crate) async fn dispatch_with_credits(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        credit_charge: CreditCharge,
    ) -> Result<WaiterGuard> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        self.session_for_request()?;
        let charge = credit_charge.0.max(1);
        let reservation = self.inner.reserve_credits(charge, command).await?;
        self.dispatch_reserved(command, body, tree_id, charge, reservation)
            .await
    }

    /// [`dispatch_with_credits`](Self::dispatch_with_credits) for a caller that
    /// has already taken its credits.
    ///
    /// Split out for the keepalive, which must never *wait* for credits: a
    /// probe parked on a grant would park exactly when the pipeline is
    /// deepest, and a liveness check that can be starved is not one. It takes
    /// its credit with `try_reserve` or skips the round, and this is the entry
    /// point that lets it.
    async fn dispatch_reserved(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        charge: u16,
        mut reservation: CreditReservation,
    ) -> Result<WaiterGuard> {
        let request = self.prepare_request(command, body, tree_id, charge, &mut reservation)?;
        self.inner.send_and_count(&request.wire, command).await?;
        reservation.commit();
        self.inner.mark_sent(&[request.msg_id]);
        trace!(
            "dispatch: cmd={:?}, msg_id={}, credit_charge={}, tree_id={:?}, {}",
            command,
            request.msg_id.0,
            charge,
            tree_id,
            request.shape
        );
        Ok(request.guard)
    }

    /// Put a request on its way from code that can't await, which is `Drop`.
    ///
    /// Everything happens before this returns: the credit, the frame, and its
    /// place in the writer task's queue, behind every frame already handed
    /// over and ahead of every one handed over after. So a request the
    /// caller sends next reaches the server after this one. The returned
    /// [`QueuedRequest`] waits for the write and the answer, for a caller
    /// that can; dropping it lets the request go out anyway.
    ///
    /// `Ok(None)` when the request would have had to wait: no credit on hand,
    /// or the writer's queue full. Nothing was sent then.
    pub(crate) fn dispatch_without_waiting(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
    ) -> Result<Option<QueuedRequest>> {
        let Some(mut reservation) = self.inner.credits.try_reserve(1) else {
            return Ok(None);
        };
        // The queue slot before the message id: an id allocated and then
        // never sent leaves a hole the server's sequence window can't move
        // past (Samba advances it only from its lowest unused id), and on a
        // live connection that stalls everything after it.
        let write_tx = self.inner.write_tx.lock().unwrap().clone();
        let Ok(slot) = write_tx.try_reserve() else {
            return Ok(None);
        };
        let request = self.prepare_request(command, body, tree_id, 1, &mut reservation)?;
        let (job, frame) = self.inner.write_job(request.wire, command);
        // Lands behind every frame already handed over and ahead of every
        // one handed over after. A writer gone meanwhile drops it, and the
        // frame's ack reports `Disconnected`.
        slot.send(job);
        // Spent from here on: the frame goes out, or the connection dies
        // with the session that funded it. Refunding it now would let the
        // next request spend a credit this one is about to use.
        reservation.commit();
        trace!(
            "dispatch: cmd={:?}, msg_id={}, queued without waiting, tree_id={:?}, {}",
            command,
            request.msg_id.0,
            tree_id,
            request.shape
        );
        Ok(Some(QueuedRequest {
            conn: self.clone(),
            command,
            msg_id: request.msg_id,
            guard: request.guard,
            frame,
        }))
    }

    /// Everything [`dispatch_reserved`](Self::dispatch_reserved) does before
    /// the send: the message id, the header, the waiter, and the bytes as
    /// they go on the wire (signed, encrypted, or compressed).
    fn prepare_request(
        &self,
        command: Command,
        body: &dyn Pack,
        tree_id: Option<TreeId>,
        charge: u16,
        reservation: &mut CreditReservation,
    ) -> Result<PreparedRequest> {
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        let session_id = self.session_for_request()?;
        let msg_id = self.allocate_msg_id(charge as u64);

        let mut header = Header::new_request(command);
        header.message_id = msg_id;
        reservation.stamp(&mut header, charge);
        header.session_id = session_id;
        if let Some(tid) = tree_id {
            header.tree_id = Some(tid);
        }

        let (should_sign, should_encrypt) = {
            let c = self.inner.crypto.lock().unwrap();
            (c.should_sign, c.should_encrypt)
        };

        if should_sign && !should_encrypt {
            header.flags.set_signed();
        }
        if self.should_set_dfs_flag(tree_id) {
            header.flags |= HeaderFlags::new(HeaderFlags::DFS_OPERATIONS);
        }

        let mut msg_bytes = pack_message(&header, body);

        let guard = self.register_waiter(msg_id, command)?;

        if should_encrypt {
            let wire = self.encrypt_bytes(&msg_bytes)?;
            let shape = WireShape::Encrypted { len: wire.len() };
            return Ok(PreparedRequest {
                msg_id,
                guard,
                wire,
                shape,
            });
        }
        if should_sign {
            let c = self.inner.crypto.lock().unwrap();
            if let (Some(key), Some(algo)) = (&c.signing_key, &c.signing_algorithm) {
                signing::sign_message(&mut msg_bytes, key, *algo, msg_id.0, false)?;
            }
        }
        if self.compression_enabled() && msg_bytes.len() > Header::SIZE {
            if let Some(compressed) = compress_message(&msg_bytes, Header::SIZE) {
                let wire = build_compressed_frame(&compressed);
                let shape = WireShape::Compressed {
                    signed: should_sign,
                    from: msg_bytes.len(),
                    to: wire.len(),
                };
                return Ok(PreparedRequest {
                    msg_id,
                    guard,
                    wire,
                    shape,
                });
            }
        }
        let shape = WireShape::Plain {
            signed: should_sign,
            len: msg_bytes.len(),
        };
        Ok(PreparedRequest {
            msg_id,
            guard,
            wire: msg_bytes,
            shape,
        })
    }

    /// Ask the server directly whether it is still processing requests.
    ///
    /// SMB2 ECHO (MS-SMB2 § 2.2.28) is a four-byte request that touches no
    /// share, no handle, and no disk. That is what makes an answer mean
    /// something specific: the server is processing requests, full stop.
    ///
    /// Bounded on both legs, and the two are told apart deliberately:
    ///
    /// - **Could not ask** → [`ProbeOutcome::Skipped`]. No credit on hand is
    ///   the common case, and it happens precisely when the pipeline is
    ///   deepest, so it is counted and logged separately rather than filed
    ///   under "the server said nothing".
    /// - **Asked, no answer** → [`ProbeOutcome::Silent`]. Evidence, not a
    ///   verdict: it leaves the liveness clock stale, which is what withholds
    ///   the deadline extension.
    ///
    /// Getting the probe onto the wire is the send deadline's business
    /// ([`set_send_timeout`](Self::set_send_timeout) bounds it and tears the
    /// connection down on breach), so there is no second timeout wrapped
    /// around it here: a stuck socket already has an owner, and blaming the
    /// server for it is the misdiagnosis `sent_at` exists to prevent. The wait
    /// for the *answer* is this method's own, and is always bounded.
    async fn echo_probe(&self, budget: Duration) -> ProbeOutcome {
        if self.inner.disconnected.load(Ordering::Acquire) {
            return ProbeOutcome::Broken;
        }
        let Some(reservation) = self.inner.credits.try_reserve(1) else {
            self.inner
                .metrics
                .keepalive_probes_skipped
                .fetch_add(1, Ordering::Relaxed);
            debug!(
                "keepalive: no credit on hand for an ECHO probe, so this connection has no \
                 liveness signal until a grant comes back"
            );
            return ProbeOutcome::Skipped;
        };
        // No `tree_id`: ECHO is connection-scoped and needs no share. It does
        // carry the session id and gets signed like anything else, because a
        // session that requires signing rejects what isn't signed.
        let mut guard = match self
            .dispatch_reserved(Command::Echo, &EchoRequest, None, 1, reservation)
            .await
        {
            Ok(guard) => guard,
            Err(e) => {
                debug!("keepalive: could not send an ECHO probe: {e}");
                // Only stop probing if the connection is genuinely gone. A
                // send that failed for any other reason (a crypto error, say)
                // is a round we learned nothing from, and a keepalive that
                // quietly retires on a live connection is worse than no
                // keepalive, because nothing says it stopped.
                return if self.inner.disconnected.load(Ordering::Acquire) {
                    ProbeOutcome::Broken
                } else {
                    self.inner
                        .metrics
                        .keepalive_probes_skipped
                        .fetch_add(1, Ordering::Relaxed);
                    ProbeOutcome::Skipped
                };
            }
        };
        self.inner
            .metrics
            .keepalive_probes_sent
            .fetch_add(1, Ordering::Relaxed);
        match rt::timeout(budget, guard.recv()).await {
            Ok(Ok(_frame)) => ProbeOutcome::Alive,
            // The connection died under us; whoever noticed is tearing it down.
            Ok(Err(Error::Disconnected)) | Ok(Err(Error::ServerUnresponsive { .. })) => {
                ProbeOutcome::Broken
            }
            // Any other error came out of a frame the server actually sent —
            // STATUS_NETWORK_SESSION_EXPIRED is the realistic one. It is a bad
            // answer, but it is an answer, and that is the only question this
            // probe asks. ❌ Don't read it as death: a consumer re-running
            // `Session::setup` on this connection would otherwise be left with
            // a keepalive that had silently retired.
            Ok(Err(e)) => {
                debug!("keepalive: the server answered the probe with an error, which still proves it is processing requests: {e}");
                ProbeOutcome::Alive
            }
            Err(_elapsed) => {
                self.inner
                    .metrics
                    .keepalive_failures
                    .fetch_add(1, Ordering::Relaxed);
                debug!(
                    "keepalive: an ECHO probe went unanswered within {budget:?}; this connection \
                     has no liveness proof until a frame arrives, so a slow request on it gets \
                     the plain response deadline"
                );
                ProbeOutcome::Silent
            }
        }
    }

    /// Send a compound SMB2 request (multiple operations in one transport
    /// frame) and return the per-sub-op responses.
    ///
    /// Takes `&self`. Each [`CompoundOp`] is assigned its own `MessageId`
    /// and its own `oneshot::Sender` registered in the waiters map. The
    /// server MAY split the compound response into multiple transport
    /// frames (MS-SMB2 § 3.3.4.1.3) — the receiver task's per-`MessageId`
    /// routing handles that transparently; each sub-op's waiter resolves
    /// independently.
    ///
    /// Return shape (per decision E3 in `docs/connection-actor.md`):
    ///
    /// - Outer `Result`: `Err` if the compound didn't make it onto the wire
    ///   (encryption failed, signing failed, transport send failed, or the
    ///   connection was already disconnected). On this path no waiter
    ///   observes a response — we clean them up before returning.
    /// - Inner `Vec<Result<Frame>>`: one entry per sub-op, in the same
    ///   order as `ops`. `Ok(frame)` with the server's response, including
    ///   non-success statuses encoded in `frame.header.status`. `Err` when
    ///   a sub-op hit a waiter-level error (session expired, signature
    ///   verify failure, connection dropped mid-await). Compound partial
    ///   failure is protocol-normal — for example, CREATE may succeed but
    ///   a later READ fail — so callers typically match on each inner
    ///   result individually.
    pub async fn execute_compound(&self, ops: &[CompoundOp<'_>]) -> Result<Vec<Result<Frame>>> {
        self.inner
            .metrics
            .compound_requests_sent
            .fetch_add(1, Ordering::Relaxed);
        let result = self.execute_compound_inner(ops).await;
        if result.is_err() {
            self.inner
                .metrics
                .requests_returned_err
                .fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    async fn execute_compound_inner(&self, ops: &[CompoundOp<'_>]) -> Result<Vec<Result<Frame>>> {
        if ops.is_empty() {
            return Err(Error::invalid_data(
                "compound request must have at least one operation",
            ));
        }
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        self.session_for_request()?;

        let (should_sign, should_encrypt) = {
            let c = self.inner.crypto.lock().unwrap();
            (c.should_sign, c.should_encrypt)
        };

        // One frame, but the server charges every sub-request, so reserve the
        // whole chain up front — reserving per sub-op could let another task
        // slip in between and leave this compound half-funded on the wire.
        let total_charge = ops
            .iter()
            .map(|op| op.credit_charge.0.max(1))
            .fold(0u16, |acc, c| acc.saturating_add(c));
        let mut reservation = self
            .inner
            .reserve_credits(total_charge, ops[0].command)
            .await?;

        let session_id = self.session_for_request()?;
        let mut message_ids: Vec<MessageId> = Vec::with_capacity(ops.len());
        let mut sub_requests: Vec<Vec<u8>> = Vec::with_capacity(ops.len());

        for (i, op) in ops.iter().enumerate() {
            let charge = op.credit_charge.0.max(1);
            let msg_id = self.allocate_msg_id(charge as u64);

            let mut header = Header::new_request(op.command);
            header.message_id = msg_id;
            reservation.stamp(&mut header, charge);
            header.session_id = session_id;
            header.tree_id = op.tree_id;

            if i > 0 {
                header.flags.set_related();
            }
            if should_sign && !should_encrypt {
                header.flags.set_signed();
            }
            if self.should_set_dfs_flag(op.tree_id) {
                header.flags |= HeaderFlags::new(HeaderFlags::DFS_OPERATIONS);
            }

            message_ids.push(msg_id);
            sub_requests.push(pack_message(&header, op.body));
        }

        // 8-byte align all but the last sub-request, then wire up
        // `NextCommand` offsets.
        let last_idx = sub_requests.len() - 1;
        for sub_req in sub_requests.iter_mut().take(last_idx) {
            let rem = sub_req.len() % 8;
            if rem != 0 {
                let pad = 8 - rem;
                let new_len = sub_req.len() + pad;
                sub_req.resize(new_len, 0);
            }
        }
        for sub_req in sub_requests.iter_mut().take(last_idx) {
            let next_cmd = sub_req.len() as u32;
            sub_req[20..24].copy_from_slice(&next_cmd.to_le_bytes());
        }

        if should_sign && !should_encrypt {
            let c = self.inner.crypto.lock().unwrap();
            if let (Some(key), Some(algo)) = (&c.signing_key, &c.signing_algorithm) {
                for (i, sub_req) in sub_requests.iter_mut().enumerate() {
                    signing::sign_message(sub_req, key, *algo, message_ids[i].0, false)?;
                }
            }
        }

        // Register one oneshot::Receiver per sub-op BEFORE the send,
        // collected in the same order as `ops` / `message_ids`. On any
        // registration error, unregister the ones we already inserted.
        // The guards deregister on drop, so an error anywhere below unwinds
        // every sub-op's map entry without a manual rollback list.
        let mut guards: Vec<WaiterGuard> = Vec::with_capacity(message_ids.len());
        for (idx, id) in message_ids.iter().enumerate() {
            guards.push(self.register_waiter(*id, ops[idx].command)?);
        }

        let total_len: usize = sub_requests.iter().map(|r| r.len()).sum();
        let mut compound_buf = Vec::with_capacity(total_len);
        for sub_req in &sub_requests {
            compound_buf.extend_from_slice(sub_req);
        }

        let send_result = if should_encrypt {
            match self.encrypt_bytes(&compound_buf) {
                Ok(enc) => self.inner.send_and_count(&enc, ops[0].command).await,
                Err(e) => return Err(e),
            }
        } else {
            self.inner
                .send_and_count(&compound_buf, ops[0].command)
                .await
        };
        send_result?;
        reservation.commit();
        self.inner.mark_sent(&message_ids);

        // TRACE: per-request frame plumbing (see execute_cap above).
        trace!(
            "execute_compound: {} operations, total_len={}, msg_ids={:?}, signed={}, encrypted={}",
            ops.len(),
            compound_buf.len(),
            message_ids.iter().map(|m| m.0).collect::<Vec<_>>(),
            should_sign,
            should_encrypt,
        );

        // Collect per-sub-op results in submission order. Each `rx.await`
        // resolves independently — the receiver task splits the response
        // frame by `NextCommand` and routes each sub-response to its own
        // waiter, so we can await them sequentially without blocking any
        // of them (they may already all be resolved by the time we loop).
        let mut results: Vec<Result<Frame>> = Vec::with_capacity(guards.len());
        for (idx, guard) in guards.into_iter().enumerate() {
            results.push(self.await_response(guard, ops[idx].command).await);
        }
        Ok(results)
    }

    /// Send a CANCEL request for an outstanding operation.
    ///
    /// `async_id` is not optional in practice: once the server has sent an
    /// interim STATUS_PENDING it has assigned the request an `AsyncId`, and a
    /// cancel that does not carry it matches nothing (MS-SMB2 § 3.2.4.24).
    /// [`outstanding_requests`](Self::outstanding_requests) reports the id for
    /// each in-flight request, which is where a consumer gets it.
    ///
    /// Takes `&self`: cancelling touches only shared connection state, and a
    /// caller who has just decided to abandon a request usually holds nothing
    /// exclusively.
    pub async fn send_cancel(
        &self,
        original_msg_id: MessageId,
        async_id: Option<u64>,
        generation: u64,
    ) -> Result<()> {
        use crate::msg::cancel::CancelRequest;

        // A CANCEL names its target by `MessageId` alone, and a `MessageId` is
        // only meaningful inside the session that issued it: a revival resets
        // the counter to zero, so an id from a dead session either names some
        // unrelated live request or names nothing at all. Both conditions below
        // are load-bearing, and neither covers the other:
        //
        // - The generation moved: the caller obtained these ids from a session
        //   that has since been replaced.
        // - There is no session id yet: `install_transport` wipes it and
        //   `revivals` ticks only once the WHOLE revival has succeeded, so the
        //   generation still matches for the entire NEGOTIATE / SESSION_SETUP
        //   handshake. That is the worst possible moment to send one, because
        //   the server's credit window is exactly zero wide until it grants,
        //   and an over-spent frame is discarded in silence -- leaving the
        //   client to wait out a response deadline for an answer that was
        //   never coming.
        //
        // Not an error: nothing is wrong, the request being cancelled died
        // with its session and the server has already forgotten it.
        if generation != self.generation() || self.session_id() == SessionId(0) {
            debug!(
                "send_cancel: skipping the CANCEL for msg_id={} -- it belongs to \
                 generation {} and the connection is on {}{}",
                original_msg_id.0,
                generation,
                self.generation(),
                if self.session_id() == SessionId(0) {
                    " with no session established"
                } else {
                    ""
                }
            );
            return Ok(());
        }
        // A pinned clone whose session is gone: the same "died with its
        // session" as above, caught by the session id when no revival moved
        // the generation.
        let Ok(session_id) = self.session_for_request() else {
            debug!(
                "send_cancel: skipping the CANCEL for msg_id={} -- this handle's session is gone",
                original_msg_id.0
            );
            return Ok(());
        };

        self.inner
            .metrics
            .explicit_cancels_sent
            .fetch_add(1, Ordering::Relaxed);

        let (should_sign, should_encrypt) = {
            let c = self.inner.crypto.lock().unwrap();
            (c.should_sign, c.should_encrypt)
        };

        let mut header = Header::new_request(Command::Cancel);
        header.message_id = original_msg_id;
        header.credit_charge = CreditCharge(0);
        header.credits = 0;
        header.session_id = session_id;

        if let Some(aid) = async_id {
            header.flags.set_async();
            header.async_id = Some(aid);
            header.tree_id = None;
        }
        if should_sign && !should_encrypt {
            header.flags.set_signed();
        }

        let body = CancelRequest;
        let mut msg_bytes = pack_message(&header, &body);

        if should_encrypt {
            let encrypted = self.encrypt_bytes(&msg_bytes)?;
            self.inner
                .send_and_count(&encrypted, Command::Cancel)
                .await?;
            trace!(
                "send_cancel: msg_id={}, async_id={:?}, encrypted",
                original_msg_id.0,
                async_id
            );
        } else {
            if should_sign {
                let c = self.inner.crypto.lock().unwrap();
                if let (Some(key), Some(algo)) = (&c.signing_key, &c.signing_algorithm) {
                    // `is_cancel = true`: the AES-GMAC nonce carries a bit for
                    // it (MS-SMB2 § 3.1.4.1), and a server negotiating GMAC
                    // rejects a CANCEL signed without it — silently, since a
                    // cancel has no success response. So the request stays
                    // outstanding and the client believes it was cancelled.
                    signing::sign_message(&mut msg_bytes, key, *algo, original_msg_id.0, true)?;
                }
            }
            self.inner
                .send_and_count(&msg_bytes, Command::Cancel)
                .await?;
            trace!(
                "send_cancel: msg_id={}, async_id={:?}, signed={}",
                original_msg_id.0,
                async_id,
                should_sign
            );
        }
        Ok(())
    }

    /// Encrypt plaintext into a TRANSFORM_HEADER + ciphertext frame.
    fn encrypt_bytes(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut c = self.inner.crypto.lock().unwrap();
        let enc_key = c
            .encryption_key
            .as_ref()
            .ok_or_else(|| Error::invalid_data("encryption active but no encryption key"))?
            .clone();
        let cipher = c
            .encryption_cipher
            .ok_or_else(|| Error::invalid_data("encryption active but no cipher"))?;
        let session_id = c.session_id.0;
        let nonce = c
            .nonce_gen
            .as_mut()
            .ok_or_else(|| Error::invalid_data("encryption active but no nonce generator"))?
            .next(cipher);
        drop(c);

        let (transform_header, ciphertext) =
            encryption::encrypt_message(plaintext, &enc_key, cipher, &nonce, session_id)?;

        let mut encrypted = transform_header;
        encrypted.extend_from_slice(&ciphertext);

        trace!(
            "encrypt: plaintext={} bytes, encrypted={} bytes, nonce={:02X?}",
            plaintext.len(),
            encrypted.len(),
            &nonce[..cipher.nonce_len()]
        );

        Ok(encrypted)
    }

    /// Register a tree as DFS-enabled.
    pub fn register_dfs_tree(&mut self, tree_id: TreeId) {
        self.inner.dfs_trees.lock().unwrap().insert(tree_id);
    }

    /// Deregister a tree from DFS tracking.
    pub fn deregister_dfs_tree(&mut self, tree_id: TreeId) {
        self.inner.dfs_trees.lock().unwrap().remove(&tree_id);
    }

    /// The IPC$ tree this connection keeps open for DFS referrals, if any.
    ///
    /// Only one connected on the session this connection has now.
    pub(crate) fn cached_ipc_tree(&self) -> Option<TreeId> {
        let cached = *self.inner.ipc_tree.lock().unwrap();
        cached
            .filter(|(pin, _)| self.pin_is_current(*pin))
            .map(|(_, tree_id)| tree_id)
    }

    /// Remember (or, with `None`, forget) the IPC$ tree for DFS referrals,
    /// connected on the session this clone is pinned to (the current one, if
    /// unpinned).
    ///
    /// Forget it the moment the server says the tree is gone: a cached id the
    /// session no longer has turns every later referral into the same error.
    pub(crate) fn set_cached_ipc_tree(&self, tree_id: Option<TreeId>) {
        let pin = self.pin.unwrap_or_else(|| self.current_pin());
        *self.inner.ipc_tree.lock().unwrap() = tree_id.map(|id| (pin, id));
    }

    fn should_set_dfs_flag(&self, tree_id: Option<TreeId>) -> bool {
        tree_id.is_some_and(|id| self.inner.dfs_trees.lock().unwrap().contains(&id))
    }

    /// Allocate `charge` consecutive MessageIds and return the first.
    ///
    /// Also bumps the `requests_sent` metric — this is the single funnel
    /// every send path (`negotiate`, `execute`, `execute_with_credits`,
    /// `execute_capturing_request`, `dispatch`, `execute_compound`'s loop)
    /// goes through, so counting here can't drift as new send sites land.
    /// `send_cancel` reuses an existing msg_id and is counted separately by
    /// `explicit_cancels_sent`.
    fn allocate_msg_id(&self, charge: u64) -> MessageId {
        let first = self
            .inner
            .next_message_id
            .fetch_add(charge, Ordering::SeqCst);
        self.inner
            .metrics
            .requests_sent
            .fetch_add(1, Ordering::Relaxed);
        MessageId(first)
    }

    /// Register a waiter in the shared map and return the Receiver.
    ///
    /// Atomically checks `disconnected` under the waiters lock. If the
    /// connection died between `send_request`'s fast-path check and
    /// this call, returns `Err(Disconnected)` without inserting —
    /// prevents a TOCTOU where the receiver task has already drained
    /// the waiters map but we'd insert a new entry that no one will
    /// ever route to, leaving the caller hanging on `rx.await`.
    ///
    /// `fan_error_to_waiters` sets `disconnected = true` under the
    /// same lock, making the two paths strictly ordered.
    fn register_waiter(&self, msg_id: MessageId, command: Command) -> Result<WaiterGuard> {
        let mut waiters = self.inner.waiters.lock().unwrap();
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Err(Error::Disconnected);
        }
        let (tx, rx) = oneshot::channel();
        let now = std::time::Instant::now();
        waiters.insert(
            msg_id,
            Waiter {
                tx,
                command,
                registered_at: now,
                sent_at: None,
                last_activity: now,
                async_id: None,
            },
        );
        trace!("register_waiter: msg_id={}", msg_id.0);
        Ok(WaiterGuard {
            inner: Arc::clone(&self.inner),
            msg_id,
            generation: self.inner.revivals.load(Ordering::Acquire),
            rx: Some(rx),
            landed: None,
            arrived_at: None,
        })
    }

    /// Set how long a request may go unanswered before the background sweeper
    /// logs a warning naming it, or `None` to stay silent.
    ///
    /// Defaults to 15 s, which leaves at least one sweep between the warning
    /// and the point where
    /// [`set_response_timeout`](Self::set_response_timeout) gives up, so a
    /// wedge is always named in the log before its evidence disappears. Raise
    /// it for a server that is legitimately slow under load, or disable it
    /// entirely if your application surfaces
    /// [`ConnectionDiagnostics::outstanding`](crate::client::diagnostics::ConnectionDiagnostics)
    /// itself and doesn't want the log line.
    pub fn set_stale_request_warning(&self, after: Option<std::time::Duration>) {
        *self.inner.stale_request_after.lock().unwrap() = after;
    }

    /// Requests sent and not yet answered, oldest first.
    ///
    /// The same data the sweeper warns from, for consumers that would rather
    /// render it than read logs.
    pub fn outstanding_requests(&self) -> Vec<crate::client::diagnostics::OutstandingRequest> {
        let now = std::time::Instant::now();
        let mut out: Vec<_> = self
            .inner
            .waiters
            .lock()
            .unwrap()
            .iter()
            .map(|(id, w)| crate::client::diagnostics::OutstandingRequest {
                command: w.command,
                message_id: id.0,
                age: now.saturating_duration_since(w.registered_at),
                sent_age: w.sent_at.map(|t| now.saturating_duration_since(t)),
                async_id: w.async_id,
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.age));
        out
    }

    /// What the server has put on the wire so far, counted as the bytes land,
    /// a frame still arriving included.
    ///
    /// For a live rate while one large response arrives, and for whether the
    /// server is talking at all right now. Cheap: a lock or two held only long
    /// enough to copy a few numbers out. See
    /// [`InboundProgress`](crate::client::diagnostics::InboundProgress) for
    /// what each field means and why none of it is data you have.
    pub fn inbound(&self) -> crate::client::diagnostics::InboundProgress {
        let (snapshot, retired) = {
            let inbound = self.inner.inbound.lock().unwrap();
            (inbound.progress.snapshot(), inbound.retired_bytes)
        };
        crate::client::diagnostics::InboundProgress {
            bytes_received: retired + snapshot.bytes,
            frame: snapshot.frame,
            since_last_byte: self.inner.server_silent_for(),
        }
    }

    /// Whether the server is still there, by this connection's own clocks,
    /// right now.
    ///
    /// Pollable, and it never acts: reading it tears nothing down. It is the
    /// same evidence the response deadline acts on, so
    /// [`Liveness::Unresponsive`](crate::client::diagnostics::Liveness::Unresponsive)
    /// here is what a request that runs out of budget would turn into
    /// [`Error::ServerUnresponsive`], readable before any request has paid for
    /// it. See [`Liveness`](crate::client::diagnostics::Liveness).
    pub fn liveness(&self) -> crate::client::diagnostics::Liveness {
        use crate::client::diagnostics::Liveness;
        if self.inner.disconnected.load(Ordering::Acquire) {
            return Liveness::Disconnected;
        }
        let Some(silent_for) = self.inner.quiet_for() else {
            return Liveness::Idle;
        };
        // A poll that lands right after this process was frozen would read
        // the freeze as the server's silence. The wait loops correct for that
        // before they read a clock, and until one of them has run, the
        // verdict is withheld. ❌ Don't call `forgive_scheduling_stall` from
        // here instead: this runs on the consumer's thread, and resetting the
        // shared witness from a thread that kept running would hide a starved
        // runtime from the loops that have to notice it.
        if let Some(silent_for) = self.inner.unresponsive_for() {
            if !self.inner.loops_are_behind() {
                return Liveness::Unresponsive { silent_for };
            }
        }
        let window = (*self.inner.keepalive_after.lock().unwrap())
            .unwrap_or(KEEPALIVE_AFTER)
            .saturating_mul(LIVENESS_WINDOW_PROBES);
        match self.inner.server_silent_for() {
            Some(heard) if heard < window => Liveness::Alive,
            _ => Liveness::Quiet { silent_for },
        }
    }

    /// Await a response, giving up if the server goes silent.
    ///
    /// The deadline measures silence, not elapsed time: every interim
    /// STATUS_PENDING restarts it, so an operation the server has acknowledged
    /// gets as long as it needs. Long-poll commands are exempt, and
    /// [`set_response_timeout(None)`](Self::set_response_timeout) waits
    /// forever the way the crate used to.
    ///
    /// It also consults the connection, not just this one request. A request
    /// on a connection the keepalive has just proven alive gets a ceiling
    /// [`ALIVE_DEADLINE_FACTOR`] times the base, because on such a connection
    /// silence about one operation stops being evidence of death and becomes
    /// evidence of a slow operation. That is the entire reason the keepalive
    /// exists: without it there is one number for two situations, and every
    /// value of it is wrong for one of them.
    ///
    /// The same reading names the failure when the budget does run out. A
    /// connection that kept answering has one stalled operation on it
    /// ([`Error::Timeout`]); a connection that answered nothing at all, ECHO
    /// probes included, is a dead link ([`Error::ServerUnresponsive`]) and is
    /// torn down so every other waiter learns at once. Reaching that verdict
    /// always costs a request its full deadline first, so it can never lose
    /// anything the plain deadline was not already losing.
    pub(crate) async fn await_response(
        &self,
        mut guard: WaiterGuard,
        command: Command,
    ) -> Result<Frame> {
        let timeout = *self.inner.response_timeout.lock().unwrap();
        let Some(timeout) = timeout else {
            return guard.recv().await;
        };
        if is_long_poll(command) {
            // No refresh on this path. Retiring a subscription is only useful
            // to whoever can issue its replacement, and a caller who reached a
            // long poll through `execute` has already handed us the only
            // handle to it. `Watcher` owns that loop and takes
            // `await_long_poll_refreshable` instead.
            return match self
                .await_long_poll(guard, command, Some(timeout), None)
                .await?
            {
                LongPollOutcome::Answered(frame) => Ok(frame),
                LongPollOutcome::RefreshDue { .. } => unreachable!(
                    "the refresh is switched off on this path, so nothing can ask for one"
                ),
            };
        }
        self.await_response_in_place(&mut guard, command).await
    }

    /// [`await_response`](Self::await_response) for a guard the caller keeps.
    ///
    /// Cancel-safe: dropping this future mid-wait leaves the guard registered
    /// and its response, when it lands, waiting in the guard. That is what lets
    /// a read-ahead [`FileDownload`](crate::FileDownload) wait on its head READ
    /// in a `select` against its own dispatch timer, or be dropped by a
    /// consumer's `select!`, without losing a chunk. Every call measures the
    /// same per-request silence clock (it lives in the waiter, not in this
    /// future), so waiting in several goes loses no deadline; a restart only
    /// postpones the next check by up to one tick. Not for long polls, which
    /// have their own bound.
    pub(crate) async fn await_response_in_place(
        &self,
        guard: &mut WaiterGuard,
        command: Command,
    ) -> Result<Frame> {
        let msg_id = guard.msg_id();
        let timeout = *self.inner.response_timeout.lock().unwrap();
        let Some(timeout) = timeout else {
            return guard.recv().await;
        };
        debug_assert!(
            !is_long_poll(command),
            "long polls wait through await_long_poll"
        );
        // Bounded even when the connection is healthy: "alive" is a reason for
        // more patience, never for unlimited patience. A server answering ECHO
        // that still has not answered THIS is stalled in a way waiting cannot
        // fix, and reconnecting beats waiting.
        let alive_ceiling = timeout.saturating_mul(ALIVE_DEADLINE_FACTOR);

        // Check often enough that a short timeout is honored promptly, rarely
        // enough that the default costs one wakeup a second per request.
        let tick = (timeout / 4).clamp(Duration::from_millis(25), Duration::from_secs(1));
        let mut receiving = Box::pin(guard.recv());
        let mut extended = false;
        loop {
            let idle_check = Box::pin(rt::sleep(tick));
            match select(receiving, idle_check).await {
                Either::Left((frame, _)) => return frame,
                Either::Right((_, still_receiving)) => {
                    receiving = still_receiving;
                    // Before any clock on this connection is read: a tick that
                    // came back late because nothing was scheduled has to be
                    // taken off every clock first, or this request is about to
                    // be timed out over silence nobody was listening to.
                    self.inner.forgive_scheduling_stall();
                    // `None` means the response has been routed and the next
                    // poll will produce it — never a timeout.
                    if let Some(idle) = self.inner.waiter_idle_for(msg_id) {
                        if idle < timeout {
                            continue;
                        }
                        if idle < alive_ceiling && self.inner.liveness_is_proven() {
                            if !extended {
                                extended = true;
                                self.inner
                                    .metrics
                                    .response_deadline_extensions
                                    .fetch_add(1, Ordering::Relaxed);
                                debug!(
                                    "slow but alive: cmd={:?}, msg_id={}, silent for {:?}, but the \
                                     server is answering on this connection; waiting up to {:?}",
                                    command, msg_id.0, idle, alive_ceiling
                                );
                            }
                            continue;
                        }
                        // Read the connection's verdict BEFORE dropping out of
                        // the waiters map: `quiet_for` measures from the
                        // oldest outstanding request, so a lone waiter that
                        // deregisters first takes the evidence with it.
                        let verdict = self.inner.unresponsive_for();
                        self.remove_waiter(msg_id);
                        self.inner
                            .metrics
                            .response_timeouts
                            .fetch_add(1, Ordering::Relaxed);
                        // Two different diagnoses, and the error has to say
                        // which: a connection that went quiet is a network or
                        // server death, while one that kept answering has a
                        // single stalled operation on it.
                        if let Some(silent_for) = verdict {
                            return Err(self.declare_unresponsive(silent_for, command, msg_id));
                        }
                        warn!(
                            "no response: cmd={:?}, msg_id={}, silent for {:?}; giving up — \
                             nothing here says the connection itself is gone, so this is one \
                             stalled operation",
                            command, msg_id.0, idle
                        );
                        return Err(Error::Timeout);
                    }
                }
            }
        }
    }

    /// Wait for a long-poll command, with a refresh cycle underneath it.
    ///
    /// The entry point for whoever owns the subscription loop — in this crate,
    /// [`Watcher`](crate::Watcher). It can end two ways a plain
    /// [`await_response`](Self::await_response) cannot express: the server
    /// answered, or the request has been registered long enough that it should
    /// be replaced. See [`LONG_POLL_REFRESH`] for why the second one exists at
    /// all, and [`LongPollOutcome::RefreshDue`] for what the caller owes.
    pub(crate) async fn await_long_poll_refreshable(
        &self,
        guard: WaiterGuard,
        command: Command,
    ) -> Result<LongPollOutcome> {
        let budget = *self.inner.response_timeout.lock().unwrap();
        let refresh = *self.inner.long_poll_refresh.lock().unwrap();
        self.await_long_poll(guard, command, budget, refresh).await
    }

    /// Wait for a long-poll command, bounded by the connection rather than by
    /// the request, and retired on a cycle rather than trusted forever.
    ///
    /// A CHANGE_NOTIFY has no deadline of its own by design: it waits for an
    /// event that may never come, and hours of silence is the healthy case.
    /// That leaves it as the one wait in the crate with nothing to end it, so
    /// two separate things have to stand in for a deadline, and neither
    /// substitutes for the other:
    ///
    /// - **`budget` ends a watch on a DEAD CONNECTION.** The clock is
    ///   [`Inner::quiet_for`], not the request's own idle time: a watcher open
    ///   for an hour is not a symptom of anything, while a wire that has gone
    ///   completely silent is. Only the keepalive makes that reading mean
    ///   anything, so with probing off a quiet connection is just a quiet
    ///   connection and this waits forever, deliberately.
    /// - **`refresh` retires a subscription the connection cannot speak for.**
    ///   A server answering everything else while it has quietly forgotten one
    ///   CHANGE_NOTIFY is, to every reading above, perfectly healthy — and it
    ///   is what a QNAP TS-464 did for 6,186 s on 2026-08-03. ❌ This is not a
    ///   detector and cannot be built into one: a forgotten subscription and an
    ///   untouched directory are the same observation. It just declines to
    ///   trust one request indefinitely.
    ///
    /// The death verdict is checked first. A refresh on a corpse would replace
    /// a subscription on a session that has nothing left to subscribe to, and
    /// hide the error the consumer needs.
    async fn await_long_poll(
        &self,
        mut guard: WaiterGuard,
        command: Command,
        budget: Option<Duration>,
        refresh: Option<Duration>,
    ) -> Result<LongPollOutcome> {
        let msg_id = guard.msg_id();
        // Nothing to wake up for: no death bound and no refresh means the wait
        // is unbounded.
        let Some(shortest) = [budget, refresh].into_iter().flatten().min() else {
            return guard.recv().await.map(LongPollOutcome::Answered);
        };
        let tick = (shortest / 4).clamp(Duration::from_millis(25), Duration::from_secs(1));
        loop {
            // A fresh `recv()` future per tick rather than one long-lived one.
            // Cancelling it loses nothing — the `oneshot` holds the value until
            // a poll takes it — and unlike a pinned future held across the loop
            // it leaves the guard free to be inspected and retired below.
            match rt::timeout(tick, guard.recv()).await {
                Ok(frame) => return frame.map(LongPollOutcome::Answered),
                Err(_tick_elapsed) => {}
            }
            // Same reason as in `await_response`: a tick this loop spent
            // unscheduled is not silence the server owes an answer for, and
            // this is the loop that would otherwise convict it.
            self.inner.forgive_scheduling_stall();
            // The response has been routed and the next poll will produce
            // it — never a timeout.
            if self.inner.waiter_idle_for(msg_id).is_none() {
                continue;
            }
            // The same verdict an ordinary request's deadline reaches, on the
            // connection's clock instead of the request's, and held to the
            // response deadline's budget rather than to the shorter window that
            // merely withholds an extension.
            if let Some(quiet) =
                budget.and_then(|b| self.inner.unresponsive_for().filter(|q| *q >= b))
            {
                self.remove_waiter(msg_id);
                self.inner
                    .metrics
                    .response_timeouts
                    .fetch_add(1, Ordering::Relaxed);
                return Err(self.declare_unresponsive(quiet, command, msg_id));
            }
            let Some(registered_for) =
                refresh.and_then(|r| self.inner.waiter_sent_age(msg_id).filter(|age| *age >= r))
            else {
                continue;
            };
            // One last look before walking away. A response that landed in the
            // moment between "parked long enough" and "retire it" is a real
            // answer, and discarding it would turn a routine handover into lost
            // events.
            if let Some(frame) = guard.try_recv() {
                return frame.map(LongPollOutcome::Answered);
            }
            let async_id = guard.async_id();
            let generation = guard.generation();
            // Dropping the guard deregisters the waiter, so a response that
            // arrives after this counts as late-after-drop rather than as a
            // frame nobody asked for.
            drop(guard);
            self.inner
                .metrics
                .long_poll_refreshes
                .fetch_add(1, Ordering::Relaxed);
            debug!(
                "long poll refresh: cmd={:?}, msg_id={}, registered {:?} ago; retiring it for \
                 a fresh one (routine, not a fault)",
                command, msg_id.0, registered_for
            );
            return Ok(LongPollOutcome::RefreshDue {
                msg_id,
                async_id,
                generation,
            });
        }
    }

    /// The link is dead: end every wait on it at once and mark it so.
    ///
    /// Called only when a wait has already run out of budget AND the server
    /// has put nothing at all on the wire, ECHO probes included. Both halves
    /// matter. The budget is what keeps this from costing anything the plain
    /// deadline was not already costing, and the connection-wide silence is
    /// what makes "dead link" a better description than "stalled operation" —
    /// telling the other waiters now beats each discovering it a deadline at a
    /// time, and marking it dead is what gives
    /// [`reconnect_if_needed`](Self::reconnect_if_needed) something to revive.
    ///
    /// ❌ This is deliberately not something the keepalive can reach on its
    /// own. A missed probe means "no extension" and nothing more; see
    /// [`KEEPALIVE_AFTER`] for the NAS that made that distinction necessary.
    ///
    /// **Logged at WARN, and it earns the level.** The connection recovers
    /// itself, so this is not an ERROR — but it is not routine either: the
    /// wire is still up, so this is a server that accepted a session and
    /// stopped serving it, and every request outstanding on it just failed.
    /// The reader has somewhere to look (the server, or the path to it), which
    /// is what separates a WARN from an INFO. Silence we merely failed to
    /// listen to never reaches here; see
    /// [`forgive_scheduling_stall`](Inner::forgive_scheduling_stall).
    fn declare_unresponsive(
        &self,
        silent_for: Duration,
        command: Command,
        msg_id: MessageId,
    ) -> Error {
        let err = Error::ServerUnresponsive { silent_for };
        warn!(
            "the server has put nothing on the wire for {:?} while work was outstanding and every \
             ECHO probe went unanswered; declaring the session dead (waiting on cmd={:?}, \
             msg_id={}) and failing {} other waiter(s). Reconnecting is the recovery and it is \
             automatic; if this repeats against one server, it is wedging sessions instead of \
             closing them, so restart its SMB service or check the path to it",
            silent_for,
            command,
            msg_id.0,
            self.inner.waiters.lock().unwrap().len()
        );
        fan_error_to_waiters(&self.inner, &err);
        err
    }

    /// How long a request may go without any sign of life from the server
    /// before its caller gives up with [`Error::Timeout`], or `None` to wait
    /// indefinitely.
    ///
    /// Defaults to 30 s. The clock measures silence rather than elapsed time:
    /// it starts when the frame reaches the wire and every interim
    /// `STATUS_PENDING` restarts it, so an operation the server has
    /// acknowledged is never cut short however long it runs. CHANGE_NOTIFY,
    /// whose job is to wait for an event that may never come, is exempt at any
    /// setting.
    ///
    /// This is what keeps a server that stops answering while its TCP socket
    /// stays open from hanging a caller forever. Pass `None` only if your
    /// application imposes its own deadline.
    pub fn set_response_timeout(&self, after: Option<Duration>) {
        *self.inner.response_timeout.lock().unwrap() = after;
    }

    /// The response deadline currently in force. See
    /// [`set_response_timeout`](Self::set_response_timeout).
    pub fn response_timeout(&self) -> Option<Duration> {
        *self.inner.response_timeout.lock().unwrap()
    }

    /// How long one frame may take to reach the socket before its caller
    /// gives up with [`Error::SendTimeout`], or `None` to wait indefinitely.
    ///
    /// Defaults to 20 s. This bounds getting ONTO the wire, which
    /// [`set_response_timeout`](Self::set_response_timeout) does not: that
    /// clock only starts once the server has been asked. A socket that stops
    /// accepting writes while TCP stays `ESTABLISHED` is invisible to every
    /// other deadline in the crate, and produced a permanent, silent wedge
    /// before this existed.
    ///
    /// Raise it for a link so slow that a `MaxWriteSize` frame legitimately
    /// takes longer. ❌ Don't set it below the time a full-size write needs on
    /// the slowest link you support, or healthy transfers will be cut off.
    ///
    /// When it fires, the connection is torn down: a write abandoned partway
    /// leaves a partial frame on the wire, so the stream can't be resynced.
    /// `None` restores the old unbounded behavior.
    pub fn set_send_timeout(&self, after: Option<Duration>) {
        *self.inner.send_timeout.lock().unwrap() = after;
    }

    /// How many bytes of WRITE payload may be outstanding across the whole
    /// connection at once: handed to the crate but not yet acknowledged by the
    /// server.
    ///
    /// Defaults to 32 MiB. This is the bound a per-upload window cannot give:
    /// the [`WriteBehind`](crate::WriteBehind) window caps ONE upload and says
    /// nothing about how many share the connection, so N concurrent uploads
    /// multiply it. Ten files each keeping 32 WRITEs of a 1 MiB `MaxWriteSize`
    /// in flight reach 320 MiB queued in-process, which on a 6.7 MB/s link is
    /// ~48 s between handing a chunk over and it reaching the socket. The
    /// consumer pays that in cancel latency, in progress reporting that runs
    /// ahead of reality, and in plain memory.
    ///
    /// The default is sized for a LAN, so move it when your link or your host
    /// is not one:
    ///
    /// - **Raise it on a very fast link.** At 10 GbE (~1.2 GB/s) 32 MiB is
    ///   ~25 ms of buffer, and a round trip plus server-side write latency can
    ///   exceed that, leaving the pipe briefly dry. A few hundred MiB costs
    ///   nothing but memory there.
    /// - **Lower it on a memory-constrained host, or where cancel has to feel
    ///   instant.** The floor is one frame (see below), so a small embedded
    ///   consumer can go well under the default.
    /// - **Leave it alone otherwise.** On a saturated gigabit link the default
    ///   is 0.3 s of buffer, many times the bandwidth-delay product, and the
    ///   measured throughput curve is flat well below it.
    ///
    /// The budget is counted in 64 KiB granules, so `bytes` rounds up to one;
    /// [`write_budget`](Self::write_budget) reports what the rounding produced.
    /// A budget smaller than a single `MaxWriteSize` frame is not an error and
    /// never wedges: an oversized frame is clamped to the whole budget and gets
    /// it to itself, which is honest degradation rather than a writer waiting
    /// forever for permits that will never exist.
    ///
    /// Raising it takes effect at once. Lowering it takes effect as the frames
    /// already outstanding complete, because the budget those hold has already
    /// been spent.
    ///
    /// ❌ Don't reach for this to throttle a transfer's SPEED. It bounds
    /// buffering, not bandwidth; a link that can carry the bytes still will,
    /// and a budget set low enough to slow one down mostly costs you the
    /// pipelining that makes this crate worth using.
    pub fn set_write_budget(&self, bytes: u64) {
        self.inner.write_budget_units.lock().unwrap().target = budget_units(bytes);
        self.inner.settle_write_budget();
    }

    /// The current outstanding-WRITE-payload budget in bytes, rounded up to the
    /// 64 KiB granule the budget is counted in. See
    /// [`set_write_budget`](Self::set_write_budget).
    #[must_use]
    pub fn write_budget(&self) -> u64 {
        u64::from(self.inner.write_budget_units.lock().unwrap().target) * WRITE_BUDGET_UNIT
    }

    /// Reserve write budget for `bytes`, waiting until it is free.
    ///
    /// The returned permit must be moved INTO the in-flight WRITE future, so
    /// that draining, aborting, or dropping that future returns the budget
    /// without any explicit release. See [`Inner::write_budget`].
    ///
    /// ❌ Never await this while holding a permit whose future you are not
    /// polling: that is the one way to deadlock this budget. Go through
    /// [`reserve_write_budget_or_drain`], which encodes the safe ordering, or
    /// reproduce it exactly.
    ///
    /// `None` means the budget semaphore is closed, which nothing in this crate
    /// does; the caller sends the frame unaccounted rather than dropping it.
    pub(crate) async fn reserve_write_budget(
        &self,
        bytes: u64,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let units = self.clamped_units(bytes);
        Arc::clone(&self.inner.write_budget)
            .acquire_many_owned(units)
            .await
            .ok()
    }

    /// Reserve write budget for `bytes` if it is free right now.
    pub(crate) fn try_reserve_write_budget(
        &self,
        bytes: u64,
    ) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let units = self.clamped_units(bytes);
        Arc::clone(&self.inner.write_budget)
            .try_acquire_many_owned(units)
            .ok()
    }

    /// Units for `bytes`, never more than the whole budget holds.
    ///
    /// Clamping is load-bearing: `acquire_many` for more permits than the
    /// semaphore will EVER hold waits forever, so a server negotiating a
    /// `MaxWriteSize` above the budget — or a consumer setting a budget below
    /// one frame — would wedge that frame. Clamped, it simply gets the whole
    /// budget to itself, which is the honest degradation. ❌ Don't reach for
    /// `available_permits()` here: that is what is free right now, not what
    /// exists, so under contention it would clamp to a number smaller than the
    /// caller needs and under-charge the budget.
    fn clamped_units(&self, bytes: u64) -> u32 {
        // Settling here rather than only in `set_write_budget` is what makes a
        // reduction land once the frames holding the old budget complete.
        let total = self.inner.settle_write_budget();
        budget_units(bytes).min(total)
    }

    /// How long the server may say nothing, while a request is on the wire,
    /// before the client asks it directly with an SMB2 ECHO. `None` turns the
    /// keepalive off.
    ///
    /// **The keepalive tells the response deadline whether the server is
    /// alive; an alive server gets more time.** That is the whole feature. It
    /// is on by default (5 s) because it can only ever hand time out, never
    /// take it away: a request on a connection ECHO has just proven alive runs
    /// to [`set_response_timeout`](Self::set_response_timeout) × 6 instead of
    /// × 1, and a probe the server ignores simply leaves that extension
    /// ungranted. ❌ A missed probe never ends anything on its own — a busy
    /// NAS drops probes exactly while a transfer is running.
    ///
    /// It measures **silence**, not elapsed time, so a connection with
    /// responses flowing never probes at all and a busy transfer pays nothing
    /// for it. A connection with nothing on the wire is not probed either —
    /// there is no work to protect, and the next request's own deadlines
    /// cover it.
    ///
    /// Two things follow from turning it off, both by design:
    ///
    /// - **No extension**, ever. With nothing refreshing the liveness clock a
    ///   fresh reading is luck rather than evidence, and a deadline extended
    ///   on luck is the hang coming back.
    /// - **No [`crate::Error::ServerUnresponsive`]**, and a long-poll
    ///   CHANGE_NOTIFY waits indefinitely again. Both of those rest on
    ///   "nobody could get a word out of this server", which is a claim only
    ///   probing can support.
    ///
    /// Independent of [`ClientConfig::auto_reconnect`](crate::client::ClientConfig::auto_reconnect):
    /// this decides how patient a deadline is, that decides whether a dead
    /// session is re-dialed. Neither switches the other on.
    pub fn set_keepalive(&self, after: Option<Duration>) {
        *self.inner.keepalive_after.lock().unwrap() = after;
    }

    /// How long one long-poll request (CHANGE_NOTIFY) may stay registered with
    /// the server before the client retires it and issues a fresh one. `None`
    /// keeps a single subscription open indefinitely.
    ///
    /// Defaults to 10 minutes. **It is a self-healing cycle, not a detector.**
    /// A subscription the server has silently dropped and a directory nobody
    /// has touched look identical from here — both are silence — so nothing can
    /// tell them apart and any attempt to would end healthy watches. Re-issuing
    /// on a cycle needs no such judgment: a lost subscription comes back within
    /// one interval, and a live one is never disturbed by more than a handover.
    ///
    /// This is the only bound a long poll has of its own.
    /// [`set_response_timeout`](Self::set_response_timeout) exempts it (waiting
    /// for an event that may never come is the job), and the connection-level
    /// bound behind [`set_keepalive`](Self::set_keepalive) only fires when the
    /// whole wire goes quiet — so a server answering everything except this one
    /// subscription is invisible to both. A QNAP TS-464 held two CHANGE_NOTIFY
    /// requests for 6,186 s that way while answering `fs_info` in 4 ms
    /// (2026-08-03).
    ///
    /// Watch [`MetricsSnapshot::long_poll_refreshes`](crate::client::diagnostics::MetricsSnapshot::long_poll_refreshes)
    /// to see cycles happening. On a healthy watch it climbs at roughly one per
    /// interval per watched directory; it says a refresh happened, never that
    /// anything was wrong.
    ///
    /// Shorten it if a stale watch is expensive for your application, but
    /// ❌ don't shorten it hoping to detect faster: there is nothing to detect,
    /// and each cycle is one more handover an event can slip through. `None`
    /// suits a consumer that re-creates its own watchers periodically.
    pub fn set_long_poll_refresh(&self, after: Option<Duration>) {
        *self.inner.long_poll_refresh.lock().unwrap() = after;
    }

    /// The current long-poll refresh interval. See
    /// [`set_long_poll_refresh`](Self::set_long_poll_refresh).
    pub fn long_poll_refresh(&self) -> Option<Duration> {
        *self.inner.long_poll_refresh.lock().unwrap()
    }

    /// Frames handed to the writer task and not yet written.
    ///
    /// A gauge, not a gate. Persistently non-zero while
    /// [`MetricsSnapshot::wire_bytes_sent`](crate::client::diagnostics::MetricsSnapshot::wire_bytes_sent)
    /// stands still means the send side is stuck.
    pub fn send_queue_depth(&self) -> usize {
        self.inner.send_queue_depth.load(Ordering::Relaxed)
    }

    // ── Reconnect ─────────────────────────────────────────────────────

    /// Arm auto-reconnect by supplying the two things this crate deliberately
    /// does not keep: an address to dial and credentials to authenticate with.
    ///
    /// Without a reviver a dead connection stays dead and
    /// [`reconnect_if_needed`](Self::reconnect_if_needed) is a no-op that
    /// reports [`Error::Disconnected`].
    pub fn set_reviver(&self, reviver: Option<Arc<dyn SessionReviver>>) {
        *self.inner.reviver.lock().unwrap() = reviver;
    }

    /// Whether a reviver is installed.
    pub fn can_reconnect(&self) -> bool {
        self.inner.reviver.lock().unwrap().is_some()
    }

    /// Note that `file_id` holds an oplock on `tree_id`, so a break
    /// notification can be acknowledged on the right tree.
    pub(crate) fn register_oplock(&self, file_id: FileId, tree_id: TreeId) {
        self.inner
            .oplock_trees
            .lock()
            .unwrap()
            .insert(file_id, tree_id);
    }

    /// Drop the oplock bookkeeping for a handle that is being closed.
    pub(crate) fn forget_oplock(&self, file_id: FileId) {
        self.inner.oplock_trees.lock().unwrap().remove(&file_id);
    }

    /// The session the next session setup here replaces, or `0`.
    ///
    /// A revival sets it to the session it's replacing, and
    /// [`set_previous_session_id`](Self::set_previous_session_id) lets a
    /// consumer name one; a successful setup clears it.
    /// [`Session::setup`](crate::Session::setup) sends it as
    /// `PreviousSessionId`, which is what lets the server close the old
    /// session's handles at once rather than at its own timeout.
    pub fn previous_session_id(&self) -> SessionId {
        SessionId(self.inner.previous_session_id.load(Ordering::Acquire))
    }

    /// Announce `id` as `PreviousSessionId` on the next session setup here, so
    /// the server ends that session and closes everything it still has open.
    ///
    /// This is for a consumer that replaces a dead connection with a new one
    /// rather than reviving it in place (which announces its old session by
    /// itself): pass the old connection's [`session_id`](Self::session_id)
    /// before [`Session::setup`](crate::Session::setup).
    /// [`ClientConfig::previous_session_id`](crate::ClientConfig::previous_session_id)
    /// does the same for [`SmbClient::connect`](crate::SmbClient::connect).
    /// The setup that succeeds retires it; a refused one keeps it for the
    /// retry. `SessionId(0)` clears it.
    ///
    /// ❌ **Only a session you've given up on.** A server that finds the id
    /// logged in as the same user ends it, whether or not it was dead (MS-SMB2
    /// § 3.3.5.5.3), so naming a session that's still in use (a sibling
    /// connection's, say) cuts it off mid-request. A different user's session,
    /// or an id the server doesn't know, is ignored, and so is any id on a
    /// Samba guest login. The caveats are on the `ClientConfig` field.
    pub fn set_previous_session_id(&self, id: SessionId) {
        self.inner
            .previous_session_id
            .store(id.0, Ordering::Release);
    }

    /// Record the session that has just been established here.
    ///
    /// Called by [`Session::setup`](crate::Session::setup); consumers should
    /// not need it.
    pub fn adopt_session(&self, session: &crate::client::Session) {
        // The old session is superseded; announcing it again on a later setup
        // would point at something twice-dead.
        self.inner.previous_session_id.store(0, Ordering::Release);
        *self.inner.session.lock().unwrap() = Some(Arc::new(session.snapshot()));
    }

    /// The session currently established on this connection, or `None` before
    /// authentication.
    ///
    /// ❌ Don't cache this across a possible reconnect: a revival replaces it,
    /// and the previous session's keys decrypt nothing.
    pub fn current_session(&self) -> Option<Arc<crate::client::Session>> {
        self.inner.session.lock().unwrap().clone()
    }

    /// Replace the bounds on a revival. See [`ReconnectPolicy`].
    pub fn set_reconnect_policy(&self, policy: ReconnectPolicy) {
        *self.inner.reconnect_policy.lock().unwrap() = policy;
    }

    /// The bounds currently in force.
    pub fn reconnect_policy(&self) -> ReconnectPolicy {
        *self.inner.reconnect_policy.lock().unwrap()
    }

    /// Be told about every reconnect as it happens.
    ///
    /// The observer runs on the task driving the revival. It must not block
    /// and must not call back into this connection: sending from inside it
    /// deadlocks against the revival lock. Forward to a channel and return.
    pub fn on_reconnect(&self, observer: Option<ReconnectObserver>) {
        *self.inner.reconnect_observer.lock().unwrap() = observer;
    }

    /// Whether the connection is currently torn down.
    ///
    /// A revived connection reports `false` again — this is the live state,
    /// not a latch.
    pub fn is_disconnected(&self) -> bool {
        self.inner.disconnected.load(Ordering::Acquire)
    }

    /// How many times this connection has come back on a fresh socket.
    ///
    /// Increments only on a *successful* revival, so it doubles as the
    /// identity of the current session: a handle, tree id, or message id
    /// obtained under an older generation belongs to a session that no longer
    /// exists.
    pub fn generation(&self) -> u64 {
        self.inner.revivals.load(Ordering::Acquire)
    }

    /// Bring the connection back if it has died, or do nothing if it is fine.
    ///
    /// The whole pipeline discovers a dead session at once, so this is written
    /// for a stampede: the first caller in dials, everyone else waits on the
    /// same attempt and returns as soon as it lands. Nothing here can spin —
    /// see [`ReconnectPolicy`] for every bound.
    ///
    /// On success the connection is negotiated and authenticated again, and
    /// **everything scoped to the old session is gone**: tree ids, file ids,
    /// message ids, and the credit window. The caller re-establishes what it
    /// still needs. That is why this is not called for you inside `execute`:
    /// re-issuing an arbitrary request against a new session is a data-safety
    /// decision only the layer that knows the operation's semantics can make.
    ///
    /// Errors:
    ///
    /// - [`Error::Disconnected`] when the connection is dead and no reviver is
    ///   installed.
    /// - [`Error::ReconnectFailed`] when every attempt failed, or the budget
    ///   ran out, or a previous revival failed recently enough that its
    ///   verdict still stands (see [`ReconnectPolicy::failure_cooldown`]).
    pub async fn reconnect_if_needed(&self) -> Result<()> {
        if !self.is_disconnected() {
            return Ok(());
        }
        let seen = self.inner.revivals.load(Ordering::Acquire);
        let _serialized = self.inner.revive_lock.lock().await;

        // Somebody else fixed it while we queued for the lock. This is the
        // common case under a deep pipeline and it must be cheap.
        if self.inner.revivals.load(Ordering::Acquire) != seen {
            return Ok(());
        }
        if !self.is_disconnected() {
            return Ok(());
        }
        if let Some(verdict) = self.recent_revive_failure() {
            return Err(verdict);
        }
        self.revive().await
    }

    /// The stored verdict of a recent failed revival, if it still stands.
    fn recent_revive_failure(&self) -> Option<Error> {
        let cooldown = self.reconnect_policy().failure_cooldown;
        let held = self.inner.last_revive_failure.lock().unwrap();
        let (at, err) = held.as_ref()?;
        (at.elapsed() < cooldown).then(|| match err {
            Error::ReconnectFailed {
                attempts,
                waited,
                cause,
                reason,
            } => Error::ReconnectFailed {
                attempts: *attempts,
                waited: *waited,
                cause: *cause,
                reason: reason.clone(),
            },
            _ => Error::Disconnected,
        })
    }

    /// Dial, install, and re-authenticate, under one hard wall-clock bound.
    ///
    /// Called with the revival lock held.
    async fn revive(&self) -> Result<()> {
        let Some(reviver) = self.inner.reviver.lock().unwrap().clone() else {
            return Err(Error::Disconnected);
        };
        let policy = self.reconnect_policy();
        if policy.max_attempts == 0 {
            return Err(Error::Disconnected);
        }

        let started = Instant::now();
        // ONE timeout around the entire loop. Every attempt, every backoff,
        // and any dial or authentication that parks are all inside it, so
        // there is no path — not a wedged socket, not a reviver that never
        // returns — by which this outlives the budget. ❌ Don't move the bound
        // inside the loop: a per-attempt timeout multiplies by the attempt
        // count and stops being a bound.
        let outcome = rt::timeout(
            policy.total_budget,
            self.revive_attempts(&reviver, &policy, started),
        )
        .await;

        match outcome {
            Ok(Ok(attempts)) => {
                let took = started.elapsed();
                self.inner.revivals.fetch_add(1, Ordering::Release);
                *self.inner.last_revive_failure.lock().unwrap() = None;
                self.inner
                    .metrics
                    .reconnects_succeeded
                    .fetch_add(1, Ordering::Relaxed);
                info!(
                    "reconnect: session re-established on a new socket after {attempts} \
                     attempt(s) in {took:?}; every tree id and file handle from the old \
                     session is invalid"
                );
                self.announce(ReconnectEvent::Succeeded { attempts, took });
                Ok(())
            }
            Ok(Err((attempts, cause))) => Err(self.give_up(attempts, started, cause)),
            Err(_) => {
                let attempts = self
                    .inner
                    .metrics
                    .reconnect_attempts
                    .load(Ordering::Relaxed);
                // The connection is mid-rebuild and nobody is going to finish
                // it; leave it unambiguously dead rather than half-alive.
                self.mark_dead();
                Err(self.give_up(
                    u32::try_from(attempts)
                        .unwrap_or(u32::MAX)
                        .min(policy.max_attempts),
                    started,
                    Error::Timeout,
                ))
            }
        }
    }

    /// The attempt loop. Returns the attempt count on success, or the last
    /// failure alongside it.
    async fn revive_attempts(
        &self,
        reviver: &Arc<dyn SessionReviver>,
        policy: &ReconnectPolicy,
        started: Instant,
    ) -> std::result::Result<u32, (u32, Error)> {
        let mut backoff = policy.initial_backoff;
        let mut last = Error::Disconnected;

        for attempt in 1..=policy.max_attempts {
            if attempt > 1 {
                warn!(
                    "reconnect: attempt {attempt} of {} after {:?}, backing off {backoff:?}",
                    policy.max_attempts,
                    started.elapsed()
                );
                rt::sleep(backoff).await;
                backoff = (backoff * 2).min(policy.max_backoff);
            }
            self.inner
                .metrics
                .reconnect_attempts
                .fetch_add(1, Ordering::Relaxed);
            self.announce(ReconnectEvent::Started {
                attempt,
                of: policy.max_attempts,
            });

            match self.revive_once(reviver).await {
                Ok(()) => return Ok(attempt),
                Err(e) => {
                    warn!("reconnect: attempt {attempt} failed: {e}");
                    // A half-built session must never look usable. Whatever
                    // went wrong, the connection goes back to unambiguously
                    // dead before the next attempt or before we give up.
                    self.mark_dead();
                    last = e;
                }
            }
        }
        Err((policy.max_attempts, last))
    }

    /// One attempt: fresh socket, wiped state, new session.
    async fn revive_once(&self, reviver: &Arc<dyn SessionReviver>) -> Result<()> {
        let (sender, receiver) = reviver.dial().await?;
        self.install_transport(sender, receiver);
        let mut conn = self.clone();
        reviver.reauthenticate(&mut conn).await
    }

    /// Swap in a fresh transport and erase every trace of the dead session.
    ///
    /// Order is the whole correctness argument:
    ///
    /// 1. Tear down first, under the waiters lock, so `disconnected` is true
    ///    and no caller can register into a half-built connection.
    /// 2. Reset the per-session state. ❌ Nothing here may be carried over: a
    ///    stale credit window over-spends the new server's budget, a stale
    ///    message id makes the server drop the connection for a sequence gap,
    ///    and stale signing keys make every frame fail verification.
    /// 3. Rebuild the plumbing.
    /// 4. Clear `disconnected` LAST, which is what reopens the gate.
    fn install_transport(
        &self,
        sender: Box<dyn TransportSend>,
        receiver: Box<dyn TransportReceive>,
    ) {
        let inner = &self.inner;

        // Anything still registered was asked of a server that is gone.
        fan_error_to_waiters(inner, &Error::Disconnected);

        inner.credits.reset();
        inner.next_message_id.store(0, Ordering::Release);
        {
            // Captured before the wipe: the next SESSION_SETUP announces it so
            // the server can tie the new session to the old one.
            let mut crypto = inner.crypto.lock().unwrap();
            inner
                .previous_session_id
                .store(crypto.session_id.0, Ordering::Release);
            *crypto = CryptoState::new();
        }
        *inner.preauth_hasher.lock().unwrap() = PreauthHasher::new();
        *inner.params.lock().unwrap() = None;
        *inner.session.lock().unwrap() = None;
        *inner.last_frame_at.lock().unwrap() = None;
        *inner.estimated_rtt.lock().unwrap() = None;
        *inner.read_rate_hint.lock().unwrap() = None;
        *inner.write_rate_hint.lock().unwrap() = None;
        *inner.read_lateness_hint.lock().unwrap() = None;
        *inner.write_lateness_hint.lock().unwrap() = None;
        inner.abandoned.lock().unwrap().clear();
        inner.dfs_trees.lock().unwrap().clear();
        *inner.ipc_tree.lock().unwrap() = None;
        inner.oplock_trees.lock().unwrap().clear();
        inner.compression_enabled.store(false, Ordering::Release);
        inner.listing_ids_refused.store(false, Ordering::Release);
        // ❌ `send_queue_depth` is deliberately NOT reset: a caller parked
        // between its increment and its decrement would underflow the gauge
        // into a nonsense number. It drains on its own.
        //
        // The send side's clocks DO restart: the new socket has sent nothing
        // and owes nothing, and carrying the dead one's silence over would let
        // the first sweep after a revival call the fresh connection wedged.
        {
            let now = Instant::now();
            let mut progress = inner.send_progress.lock().unwrap();
            progress.wire_bytes_sent = inner.metrics.wire_bytes_sent.load(Ordering::Relaxed);
            progress.last_progress = now;
            progress.last_sweep = now;
        }
        inner.send_tally.drain();

        let (write_tx, write_rx) = mpsc::channel(WRITE_QUEUE_DEPTH);
        *inner.write_tx.lock().unwrap() = write_tx;
        spawn_plumbing(inner, sender, receiver, write_rx);

        inner.disconnected.store(false, Ordering::Release);
    }

    /// Tear the connection down: every waiter told, every new send refused,
    /// and the socket closed.
    ///
    /// The `Connection` stays usable as a disconnected one: a reviver, if one
    /// is armed, can bring it back on a fresh socket.
    ///
    /// Public because a consumer that has decided a connection is finished
    /// (a user cancelling, a share unmounted) should be able to say so
    /// without waiting for a deadline to notice.
    pub fn mark_dead(&self) {
        fan_error_to_waiters(&self.inner, &Error::Disconnected);
    }

    /// Record and report a revival that gave up.
    fn give_up(&self, attempts: u32, started: Instant, cause: Error) -> Error {
        let took = started.elapsed();
        let reason = cause.to_string();
        let err = Error::ReconnectFailed {
            attempts,
            waited: took,
            cause: cause.kind(),
            reason: reason.clone(),
        };
        self.inner
            .metrics
            .reconnects_failed
            .fetch_add(1, Ordering::Relaxed);
        error!("reconnect: gave up after {attempts} attempt(s) in {took:?}: {reason}");
        *self.inner.last_revive_failure.lock().unwrap() = Some((
            Instant::now(),
            Error::ReconnectFailed {
                attempts,
                waited: took,
                cause: cause.kind(),
                reason: reason.clone(),
            },
        ));
        self.announce(ReconnectEvent::Failed {
            attempts,
            took,
            reason,
        });
        err
    }

    /// Hand an event to the consumer's observer, if there is one.
    fn announce(&self, event: ReconnectEvent) {
        let observer = self.inner.reconnect_observer.lock().unwrap().clone();
        if let Some(observer) = observer {
            observer(event);
        }
    }

    /// Remove a waiter from the map (used on send error).
    fn remove_waiter(&self, msg_id: MessageId) {
        self.inner.waiters.lock().unwrap().remove(&msg_id);
        trace!("remove_waiter: msg_id={}", msg_id.0);
    }

    #[cfg(test)]
    pub(crate) fn set_test_params(&mut self, params: NegotiatedParams) {
        *self.inner.params.lock().unwrap() = Some(params);
    }

    /// Stand in for the round trip NEGOTIATE would have measured.
    #[cfg(test)]
    pub(crate) fn set_estimated_rtt(&self, rtt: Option<Duration>) {
        *self.inner.estimated_rtt.lock().unwrap() = rtt;
    }

    #[cfg(test)]
    pub(crate) fn set_credits(&self, credits: u16) {
        self.inner.credits.set_available(credits);
    }

    /// Stage a server that has stopped growing its window at `ceiling`.
    #[cfg(test)]
    pub(crate) fn set_credit_ceiling(&self, ceiling: u16) {
        self.inner.credits.set_ceiling(ceiling);
    }

    #[cfg(test)]
    pub(crate) fn set_next_message_id(&mut self, id: u64) {
        self.inner.next_message_id.store(id, Ordering::Release);
    }

    /// Snapshot the diagnostics counters on this connection.
    ///
    /// Crate-internal; the public surface is [`Self::diagnostics`].
    pub(crate) fn metrics(&self) -> crate::client::diagnostics::MetricsSnapshot {
        self.inner.metrics_snapshot()
    }

    /// Capture a snapshot of this connection's state and counters.
    ///
    /// **Eventually consistent.** Each field is loaded independently —
    /// `credits.available` and `credits.in_flight` are sampled at slightly
    /// different moments, so their sum is *not* invariant. Documented on
    /// [`crate::client::diagnostics::CreditInfo`].
    ///
    /// **Survives teardown.** Counters live on the `Arc<Inner>` that
    /// outlives the receiver task; calling this on a torn-down connection
    /// (`disconnected: true`) returns final values.
    ///
    /// **Lock order.** Internally takes the `crypto`, `waiters`,
    /// `dfs_trees`, and `estimated_rtt` locks one at a time, in that
    /// order, then the ones [`inbound`](Self::inbound) and
    /// [`liveness`](Self::liveness) read, and each only as long as it takes to
    /// copy primitives out. No lock is held across an `.await`.
    pub fn diagnostics(&self) -> crate::client::diagnostics::ConnectionDiagnostics {
        use crate::client::diagnostics::{
            CompressionInfo, ConnectionDiagnostics, CreditInfo, EncryptionInfo, NegotiatedSummary,
            SigningInfo,
        };

        // ── 1. crypto lock: signing / encryption snapshot ────────────────
        let (signing, encryption) = {
            let c = self.inner.crypto.lock().unwrap();
            (
                SigningInfo {
                    active: c.should_sign,
                    algorithm: c.signing_algorithm,
                },
                EncryptionInfo {
                    active: c.should_encrypt,
                    cipher: c.encryption_cipher,
                },
            )
        };

        // ── 2. waiters lock: in-flight count ────────────────────────────
        let in_flight = self.inner.waiters.lock().unwrap().len();

        // ── 3. dfs_trees lock: cloned snapshot ──────────────────────────
        let dfs_trees: Vec<TreeId> = self
            .inner
            .dfs_trees
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect();

        // ── 4. estimated_rtt lock: cloned snapshot ──────────────────────
        let rtt_estimate = *self.inner.estimated_rtt.lock().unwrap();

        // Wait-free reads.
        let credits = CreditInfo {
            available: self.inner.credits.available(),
            in_flight,
            next_message_id: self.inner.next_message_id.load(Ordering::Acquire),
            send_queue_depth: self.inner.send_queue_depth.load(Ordering::Relaxed),
        };
        let disconnected = self.inner.disconnected.load(Ordering::Acquire);
        let compression = CompressionInfo {
            requested: self.inner.compression_requested.load(Ordering::Acquire),
            negotiated: self.inner.compression_enabled.load(Ordering::Acquire),
        };

        let negotiated = self.params().map(|p| NegotiatedSummary {
            dialect: p.dialect,
            max_read_size: p.max_read_size,
            max_write_size: p.max_write_size,
            max_transact_size: p.max_transact_size,
            server_guid: p.server_guid,
            signing_required: p.signing_required,
            capabilities: p.capabilities,
            gmac_negotiated: p.gmac_negotiated,
            cipher: p.cipher,
            compression_supported: p.compression_supported,
        });

        ConnectionDiagnostics {
            server: self.inner.server_name.clone(),
            negotiated,
            credits,
            signing,
            encryption,
            compression,
            rtt_estimate,
            disconnected,
            dfs_trees,
            session: None, // populated by SmbClient when assembling the full tree
            metrics: self.metrics(),
            outstanding: self.outstanding_requests(),
            inbound: self.inbound(),
            liveness: self.liveness(),
        }
    }
}

// `Connection`'s teardown lives on `Inner::drop`: the receiver task is
// aborted when the last clone drops (the last `Arc<Inner>` goes away), which
// can only happen because the task itself holds a `Weak`.

/// Receiver task loop: owns the transport receive half, routes each frame
/// to its waiter.
///
/// `feed` is the counter to record each whole frame into, for a transport that
/// can't publish its own ([`TransportReceive::receive_progress`] said `None`).
///
/// Holds the connection strongly only while it handles a frame, never while
/// it waits for one: a strong reference held across `receive()` is a cycle
/// through this task (see [`spawn_plumbing`]). A connection whose last clone
/// drops mid-frame loses that frame's response, which is exactly what its
/// callers already gave up on by dropping it.
async fn receiver_loop(
    transport_recv: Box<dyn TransportReceive>,
    weak: Weak<Inner>,
    feed: Option<Arc<ReceiveProgress>>,
    life: SocketLife,
) {
    let _ends_life = EndsSocketLife(life.clone());
    let mut ended = pin!(life.ended());
    loop {
        // Dropping a half-read frame is fine here: `ended` means the
        // connection is already dead, and nobody reads this socket again.
        let received = match select(ended.as_mut(), pin!(transport_recv.receive())).await {
            Either::Left(_) => return,
            Either::Right((received, _)) => received,
        };
        let Some(inner) = weak.upgrade() else {
            return; // last Connection clone dropped
        };
        // Before any decrypting or verifying: what the windows measure is the
        // link, and that work is the same for every frame.
        let arrived_at = rt::Instant::now();
        let raw = match received {
            Ok(bytes) => bytes,
            Err(e) => {
                debug!("receiver_loop: transport error: {}, shutting down", e);
                let count = inner.waiters.lock().unwrap().len();
                fan_error_to_waiters(&inner, &e);
                // Idle teardown (no in-flight requests) is routine: the server
                // or OS reaps a session that's been quiet long enough. Real
                // disconnects with pending waiters stay at WARN because they
                // affect callers. The decrypt / decompress / malformed-frame
                // teardowns below stay WARN regardless of waiter count — those
                // are protocol corruption, always worth surfacing.
                if count == 0 {
                    debug!("receiver_loop: idle teardown (no waiters)");
                } else {
                    warn!(
                        "receiver_loop: exiting after fan-error to {} waiters",
                        count
                    );
                }
                return;
            }
        };
        inner
            .metrics
            .wire_bytes_received
            .fetch_add(raw.len() as u64, Ordering::Relaxed);
        if let Some(feed) = &feed {
            feed.record_whole_frame(raw.len());
        }
        // The connection's liveness clock, fed before anything can reject the
        // frame. Even a frame we go on to discard proves the server is
        // processing requests, which is the only thing this clock claims.
        inner.note_server_spoke();
        trace!("receiver_loop: received {} bytes", raw.len());
        trace!(
            "receiver_loop: tick, waiters={}",
            inner.waiters.lock().unwrap().len()
        );

        // Decrypt if TRANSFORM_HEADER. Per decision E6: on an
        // unrecoverable frame error (decrypt auth tag mismatch, decompress
        // failure, malformed sub-frame structure) we tear the connection
        // down instead of log-and-continue. The msg_id isn't recoverable
        // from an unparseable frame, so there's no targeted waiter to
        // notify; log-and-continue would leave the matching waiter
        // hanging forever. Teardown fans Err(Disconnected) to every
        // pending waiter; the caller reconnects.
        let (decoded, was_encrypted) = if raw.len() >= 4 && raw[0..4] == TRANSFORM_PROTOCOL_ID {
            match decrypt_frame(&raw, &inner) {
                Ok(plain) => (plain, true),
                Err(e) => {
                    inner
                        .metrics
                        .decrypt_failures
                        .fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "receiver_loop: decrypt failed: {}; tearing down connection",
                        e
                    );
                    let count = inner.waiters.lock().unwrap().len();
                    fan_error_to_waiters(&inner, &e);
                    warn!(
                        "receiver_loop: exiting after fan-error to {} waiters",
                        count
                    );
                    return;
                }
            }
        } else {
            (raw, false)
        };

        // Decompress if COMPRESSION_HEADER.
        let decoded = if decoded.len() >= 4 && decoded[0..4] == COMPRESSION_PROTOCOL_ID {
            match decompress_response(&decoded) {
                Ok(plain) => plain,
                Err(e) => {
                    inner
                        .metrics
                        .decompress_failures
                        .fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "receiver_loop: decompress failed: {}; tearing down connection",
                        e
                    );
                    let count = inner.waiters.lock().unwrap().len();
                    fan_error_to_waiters(&inner, &e);
                    warn!(
                        "receiver_loop: exiting after fan-error to {} waiters",
                        count
                    );
                    return;
                }
            }
        } else {
            decoded
        };

        // Split by NextCommand.
        let sub_frames = match split_compound(&decoded) {
            Ok(subs) => subs,
            Err(e) => {
                inner
                    .metrics
                    .malformed_frames
                    .fetch_add(1, Ordering::Relaxed);
                warn!(
                    "receiver_loop: malformed frame: {}; tearing down connection",
                    e
                );
                let count = inner.waiters.lock().unwrap().len();
                fan_error_to_waiters(&inner, &e);
                warn!(
                    "receiver_loop: exiting after fan-error to {} waiters",
                    count
                );
                return;
            }
        };

        // Produce a list of routable entries for this transport frame.
        // SubFrameAction::Skip frames (oplock break, STATUS_PENDING) are
        // dropped silently. A parse error from prepare_sub_frame is fatal:
        // the compound split succeeded (framing looked valid) but a header
        // inside is corrupt — the connection is out of sync and we can't
        // recover. Tear down so pending waiters see Err(Disconnected)
        // rather than hanging forever.
        let mut routable: Vec<(MessageId, Result<Frame>)> = Vec::new();
        for sub in sub_frames {
            match prepare_sub_frame(&sub, was_encrypted, &inner) {
                Ok(SubFrameAction::Route(msg_id, result)) => routable.push((msg_id, result)),
                Ok(SubFrameAction::Skip) => { /* notification / STATUS_PENDING */ }
                Ok(SubFrameAction::AckOplockBreak(brk, tree_id)) => {
                    acknowledge_oplock_break(&inner, brk, tree_id);
                }
                Err(e) => {
                    inner
                        .metrics
                        .malformed_frames
                        .fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "receiver_loop: sub-frame parse failed: {}; tearing down connection",
                        e
                    );
                    let count = inner.waiters.lock().unwrap().len();
                    fan_error_to_waiters(&inner, &e);
                    warn!(
                        "receiver_loop: exiting after fan-error to {} waiters",
                        count
                    );
                    return;
                }
            }
        }

        if routable.is_empty() {
            continue;
        }

        for (msg_id, result) in routable {
            let waiter = inner.waiters.lock().unwrap().remove(&msg_id);
            if let (Some(w), Ok(frame)) = (&waiter, &result) {
                inner.note_round_trip(w, frame.raw.len());
            }
            let maybe_tx = waiter.map(|w| w.tx);
            match maybe_tx {
                Some(tx) => {
                    let was_err = result.is_err();
                    // Success routing is per-response frame plumbing → TRACE (floods at
                    // DEBUG during scans). The error variant stays DEBUG: it's low-volume
                    // and a real signal. See AGENTS.md § Logging.
                    match &result {
                        Ok(frame) => trace!(
                            "recv: routed msg_id={}, status={:?}, cmd={:?}, credits_granted={}, len={}",
                            msg_id.0,
                            frame.header.status,
                            frame.header.command,
                            frame.header.credits,
                            frame.raw.len()
                        ),
                        Err(e) => debug!("recv: routed error msg_id={}, err={}", msg_id.0, e),
                    }
                    // Bump the routing counter BEFORE handing the response to
                    // the caller. The caller's `await` resumes as soon as
                    // `tx.send` lands, so a fetch_add ordered after the send
                    // races: tests that snapshot metrics right after the
                    // awaited call would see the increment land late. If the
                    // send fails (caller dropped its Receiver), rebalance:
                    // subtract from ok/err and credit `late_after_drop`. The
                    // rebalance window is only observable to another thread
                    // snapshotting mid-send during a caller-drop, which is
                    // benign (eventually consistent counters by design).
                    let counter = if was_err {
                        &inner.metrics.responses_routed_err
                    } else {
                        &inner.metrics.responses_routed_ok
                    };
                    counter.fetch_add(1, Ordering::Relaxed);
                    let routed = Routed {
                        result,
                        arrived_at: Some(arrived_at),
                    };
                    if tx.send(routed).is_err() {
                        // Caller's oneshot::Receiver was dropped — typical
                        // spawn/abort pattern. Counted distinctly from
                        // stray frames (None branch below).
                        counter.fetch_sub(1, Ordering::Relaxed);
                        inner
                            .metrics
                            .responses_late_after_drop
                            .fetch_add(1, Ordering::Relaxed);
                        trace!("recv: late arrival for dropped waiter, msg_id={}", msg_id.0);
                    }
                }
                None => {
                    // No waiter. Two very different situations, and the
                    // partition only stays meaningful if they're told apart:
                    // a request whose caller gave up (routine — consumers
                    // cancel), versus a frame for an id we never had
                    // outstanding (a protocol anomaly worth looking at).
                    let was_abandoned = inner
                        .abandoned
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|id| *id == msg_id);
                    if was_abandoned {
                        inner
                            .metrics
                            .responses_late_after_drop
                            .fetch_add(1, Ordering::Relaxed);
                        trace!("recv: late arrival for dropped waiter, msg_id={}", msg_id.0);
                    } else {
                        inner
                            .metrics
                            .responses_stray
                            .fetch_add(1, Ordering::Relaxed);
                        match &result {
                            Ok(frame) => debug!(
                                "recv: orphan dropped, msg_id={}, status={:?}, cmd={:?}",
                                msg_id.0, frame.header.status, frame.header.command
                            ),
                            Err(e) => debug!(
                                "recv: orphan dropped (error) msg_id={}, err={}",
                                msg_id.0, e
                            ),
                        }
                    }
                }
            }
        }

        // Let a caller we just woke run before we read the next frame. On
        // tokio it sits in this worker's LIFO slot, which nothing else may
        // steal, and a socket that never drains never makes us yield on our
        // own: a side `stat` once waited 333 ms behind the rest of a download
        // that way. Released first, since we hold `inner` only per frame.
        drop(inner);
        rt::yield_now().await;
    }
}

/// Outcome of preparing a single sub-frame.
#[derive(Debug)]
pub(crate) enum SubFrameAction {
    /// Route this response to the waiter for `msg_id`.
    ///
    /// The inner `Result` lets us deliver a per-sub-op error (signature
    /// verification failure, session expired) targeted at its matching
    /// waiter without disturbing others.
    Route(MessageId, std::result::Result<Frame, Error>),
    /// Skip silently — not forwarded to any waiter.
    /// Used for STATUS_PENDING interim responses (keep the waiter alive) and
    /// unsolicited notifications there is nothing to do about.
    Skip,
    /// An oplock break the server is waiting on an answer to.
    AckOplockBreak(crate::msg::oplock_break::OplockBreak, TreeId),
}

/// Prepare a routable sub-frame from raw bytes.
///
/// Returns `Ok(SubFrameAction::Route(...))` for a normal response (possibly
/// carrying a sub-op error), `Ok(SubFrameAction::Skip)` for oplock/PENDING
/// frames that the caller should drop silently, and `Err(e)` for
/// unrecoverable errors where the connection is now out of sync
/// (header parse failure on a sub-frame the compound-splitter claimed was
/// valid — the receiver loop fans the error to all waiters and exits).
fn prepare_sub_frame(sub: &[u8], was_encrypted: bool, inner: &Inner) -> Result<SubFrameAction> {
    // Parse the header. A failure here means split_compound produced a
    // chunk that doesn't start with a valid SMB2 header — the framing is
    // corrupt and we can't know where the next sub-frame begins. Fatal
    // to the connection.
    let mut cursor = ReadCursor::new(sub);
    let header = match Header::unpack(&mut cursor) {
        Ok(h) => h,
        Err(e) => {
            return Err(Error::invalid_data(format!(
                "sub-frame header parse failed: {}",
                e
            )));
        }
    };

    // Bank the grant off every frame, orphans included: the server has
    // released those credits regardless of whether anyone is still waiting for
    // the response that carried them. Interim STATUS_PENDING frames count too.
    //
    // Nothing is taken from the unspent pool here. The charge was already
    // spent when the request went out (see `Inner::reserve_credits`); charging
    // again on receipt would double-count, and charging *only* on receipt is
    // what let concurrent senders each spend the same credits. The request's
    // share of the WINDOW is settled here, though, which is how the pool learns
    // whether the server is still growing it (see `credits.rs`).
    inner.credits.answer(header.message_id, header.credits);

    // Oplock break notification: MessageId=UNSOLICITED.
    if header.message_id == MessageId::UNSOLICITED {
        inner
            .metrics
            .unsolicited_notifications_received
            .fetch_add(1, Ordering::Relaxed);
        if header.command == Command::OplockBreak {
            // Only a handle this crate opened durably ever holds an oplock, so
            // a break we can place is one of ours; anything else (a lease
            // break, whose body has a different shape entirely) is not ours to
            // answer. ❌ Never guess the tree id: an acknowledgment on the
            // wrong tree is rejected, and the other client waits out the
            // server's break timeout exactly as if we had said nothing.
            let parsed = crate::msg::oplock_break::OplockBreak::unpack(&mut ReadCursor::new(
                &sub[Header::SIZE..],
            ));
            if let Ok(brk) = parsed {
                let tree = inner
                    .oplock_trees
                    .lock()
                    .unwrap()
                    .get(&brk.file_id)
                    .copied();
                if let Some(tree_id) = tree {
                    return Ok(SubFrameAction::AckOplockBreak(brk, tree_id));
                }
                debug!(
                    "recv: oplock break for a handle we hold no oplock on, file_id={:?}",
                    brk.file_id
                );
            }
        }
        debug!(
            "recv: skipping unsolicited notification, cmd={:?}",
            header.command
        );
        return Ok(SubFrameAction::Skip);
    }

    // STATUS_PENDING is an interim response — don't forward, keep waiter.
    if header.status.is_pending() {
        inner
            .metrics
            .status_pending_loops
            .fetch_add(1, Ordering::Relaxed);
        // "Still working on it" is a sign of life: restart the caller's
        // response deadline (MS-SMB2 § 3.2.5.1.5). Without this, a legitimately
        // slow operation the server has acknowledged would be timed out.
        if let Some(waiter) = inner.waiters.lock().unwrap().get_mut(&header.message_id) {
            waiter.last_activity = std::time::Instant::now();
            // Remember the id a CANCEL for this request will have to carry
            // (MS-SMB2 § 3.2.4.24). The interim response is the only place the
            // server ever states it.
            if let Some(async_id) = header.async_id {
                waiter.async_id = Some(async_id);
            }
        }
        trace!(
            "recv: STATUS_PENDING (interim), cmd={:?}, msg_id={}",
            header.command,
            header.message_id.0
        );
        return Ok(SubFrameAction::Skip);
    }

    // Verify signature if signing is active and not encrypted. Oplock breaks
    // and interim STATUS_PENDING frames have already returned above: they are the only
    // exemptions MS-SMB2 § 3.2.5.1.3 allows. ❌ Never gate on the header's
    // `SMB2_FLAGS_SIGNED`: that bit arrives in the same untrusted bytes as the
    // payload, so an on-path attacker clears it and rewrites the response.
    // An unsigned frame carries a zero signature and fails verification here.
    let (should_sign, signing_key, signing_algorithm) = {
        let c = inner.crypto.lock().unwrap();
        (c.should_sign, c.signing_key.clone(), c.signing_algorithm)
    };
    if should_sign && !was_encrypted && sub.len() >= Header::SIZE {
        // The `is_cancel` bit is part of the AES-GMAC nonce (MS-SMB2
        // § 3.1.4.1), so a frame whose command is CANCEL has to be verified
        // with it set or the MAC can never match. In practice that means
        // the error response a server sends when it REJECTS a cancel — the
        // one frame that says the cancel did not take.
        let is_cancel = header.command == Command::Cancel;
        if let (Some(key), Some(algo)) = (signing_key, signing_algorithm) {
            if let Err(e) =
                signing::verify_signature(sub, &key, algo, header.message_id.0, is_cancel)
            {
                inner
                    .metrics
                    .signature_failures
                    .fetch_add(1, Ordering::Relaxed);
                warn!(
                    "recv: sub-frame produced error for msg_id={}, reason=signature verify failed: {}",
                    header.message_id.0, e
                );
                return Ok(SubFrameAction::Route(header.message_id, Err(e)));
            }
        }
    }

    // Special status handling: session expired → error.
    if header.status == NtStatus::NETWORK_SESSION_EXPIRED {
        inner
            .metrics
            .session_expired_events
            .fetch_add(1, Ordering::Relaxed);
        warn!(
            "recv: session expired (STATUS_NETWORK_SESSION_EXPIRED), cmd={:?}, msg_id={}",
            header.command, header.message_id.0
        );
        warn!(
            "recv: sub-frame produced error for msg_id={}, reason=session expired",
            header.message_id.0
        );
        return Ok(SubFrameAction::Route(
            header.message_id,
            Err(Error::SessionExpired),
        ));
    }

    let body = if sub.len() > Header::SIZE {
        sub[Header::SIZE..].to_vec()
    } else {
        Vec::new()
    };
    let raw = sub.to_vec();
    let msg_id = header.message_id;
    Ok(SubFrameAction::Route(
        msg_id,
        Ok(Frame { header, body, raw }),
    ))
}

/// Fan the given error (as best we can clone it) to every pending waiter
/// and clear the waiters map. Marks the connection as disconnected so
/// new sends fail-fast.
///
/// `disconnected` is set UNDER the waiters lock so `register_waiter` sees
/// either "still alive → insert succeeds" or "dead → insert rejected",
/// never "inserted but already drained" (which would leave the caller
/// hanging on `rx.await`).
///
/// Also closes the socket, by ending its [`SocketLife`]: a dead connection
/// holds no socket, however long a consumer keeps it around. A revival
/// dials a new one.
fn fan_error_to_waiters(inner: &Inner, e: &Error) {
    let drained: Vec<(MessageId, Waiter)> = {
        let mut waiters = inner.waiters.lock().unwrap();
        inner.disconnected.store(true, Ordering::Release);
        waiters.drain().collect()
    };
    inner.socket_life.lock().unwrap().end();
    // Sends parked on credits are waiting for a grant that can no longer
    // arrive. Wake them now instead of letting each burn its full deadline.
    inner.credits.close();
    for (_id, waiter) in drained {
        let _ = waiter.tx.send(Routed {
            result: Err(clone_err_for_waiters(e)),
            arrived_at: None,
        });
    }
}

/// Best-effort error clone: `Error` isn't `Clone` (Io holds std::io::Error),
/// so most causes collapse to `Error::Disconnected` — a waiter only needs to
/// know the connection died.
///
/// [`Error::ServerUnresponsive`] is the exception, because it answers a
/// question `Disconnected` cannot: the socket is fine and the server is
/// answering nothing. A consumer deciding between "reconnect" and "this file
/// needs retrying" is choosing on exactly that.
fn clone_err_for_waiters(e: &Error) -> Error {
    match e {
        Error::ServerUnresponsive { silent_for } => Error::ServerUnresponsive {
            silent_for: *silent_for,
        },
        _ => Error::Disconnected,
    }
}

fn decrypt_frame(data: &[u8], inner: &Inner) -> Result<Vec<u8>> {
    let c = inner.crypto.lock().unwrap();
    let dec_key = c
        .decryption_key
        .as_ref()
        .ok_or_else(|| Error::invalid_data("received encrypted message but no decryption key"))?
        .clone();
    let cipher = c
        .encryption_cipher
        .ok_or_else(|| Error::invalid_data("received encrypted message but no cipher"))?;
    drop(c);

    if data.len() < TransformHeader::SIZE {
        return Err(Error::invalid_data(
            "encrypted message too short for TransformHeader",
        ));
    }

    let transform_header = &data[..TransformHeader::SIZE];
    let ciphertext = &data[TransformHeader::SIZE..];
    let plaintext = encryption::decrypt_message(transform_header, ciphertext, &dec_key, cipher)?;
    Ok(plaintext)
}

/// Split a preprocessed frame into sub-frames by `NextCommand` offsets.
/// Returns the raw byte slices (as owned Vec<u8>) for each sub-frame.
pub(crate) fn split_compound(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut results = Vec::new();
    let mut offset = 0usize;

    loop {
        if offset + Header::SIZE > data.len() {
            return Err(Error::invalid_data(format!(
                "compound response truncated at offset {}: need {} bytes for header, but only {} remain",
                offset,
                Header::SIZE,
                data.len() - offset,
            )));
        }

        if !results.is_empty() && !offset.is_multiple_of(8) {
            return Err(Error::invalid_data(format!(
                "compound response at offset {} is not 8-byte aligned -- must disconnect",
                offset,
            )));
        }

        // Parse NextCommand directly from header bytes 20..24.
        let next_cmd = u32::from_le_bytes(data[offset + 20..offset + 24].try_into().unwrap());
        let sub_end = if next_cmd > 0 {
            offset + next_cmd as usize
        } else {
            data.len()
        };

        if sub_end > data.len() {
            return Err(Error::invalid_data(format!(
                "compound NextCommand offset {} at position {} exceeds response length {}",
                next_cmd,
                offset,
                data.len(),
            )));
        }

        results.push(data[offset..sub_end].to_vec());
        if next_cmd == 0 {
            break;
        }
        offset += next_cmd as usize;
    }
    Ok(results)
}

/// Await a per-request `oneshot::Receiver` and translate the three
/// outcomes into a `Result<Frame>`:
///
/// Pack a header + body into raw SMB2 message bytes.
pub(crate) fn pack_message(header: &Header, body: &dyn Pack) -> Vec<u8> {
    let mut cursor = WriteCursor::new();
    header.pack(&mut cursor);
    body.pack(&mut cursor);
    cursor.into_inner()
}

/// A cryptographically random GUID.
///
/// Used for the client GUID at negotiate and for the `CreateGuid` that proves
/// ownership of a durable handle. ❌ It must stay unpredictable: a guessable
/// `CreateGuid` would let another client on the same server claim our open.
pub(crate) fn random_guid() -> Guid {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("failed to generate random GUID");
    Guid {
        data1: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        data2: u16::from_le_bytes([bytes[4], bytes[5]]),
        data3: u16::from_le_bytes([bytes[6], bytes[7]]),
        data4: [
            bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ],
    }
}

fn generate_salt() -> Vec<u8> {
    let mut salt = vec![0u8; 32];
    getrandom::fill(&mut salt).expect("failed to generate random salt");
    salt
}

fn build_compressed_frame(compressed: &CompressedMessage) -> Vec<u8> {
    let header = CompressionTransformHeader {
        original_compressed_segment_size: compressed.original_size,
        compression_algorithm: COMPRESSION_ALGORITHM_LZ4,
        flags: SMB2_COMPRESSION_FLAG_NONE,
        offset_or_length: compressed.offset,
    };
    let mut cursor = WriteCursor::new();
    header.pack(&mut cursor);
    let mut frame = cursor.into_inner();
    frame.extend_from_slice(&compressed.uncompressed_prefix);
    frame.extend_from_slice(&compressed.compressed_data);
    frame
}

fn decompress_response(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < CompressionTransformHeader::SIZE {
        return Err(Error::invalid_data(
            "compressed response too short for CompressionTransformHeader",
        ));
    }
    let mut cursor = ReadCursor::new(data);
    let header = CompressionTransformHeader::unpack(&mut cursor)?;
    if header.compression_algorithm != COMPRESSION_ALGORITHM_LZ4 {
        return Err(Error::invalid_data(format!(
            "unsupported compression algorithm 0x{:04X}, only LZ4 (0x{:04X}) is supported",
            header.compression_algorithm, COMPRESSION_ALGORITHM_LZ4
        )));
    }
    if header.flags != SMB2_COMPRESSION_FLAG_NONE {
        return Err(Error::invalid_data(format!(
            "unsupported compression flags 0x{:04X}, only unchained (0x0000) is supported",
            header.flags
        )));
    }
    let offset = header.offset_or_length as usize;
    let remaining = &data[CompressionTransformHeader::SIZE..];
    if offset > remaining.len() {
        return Err(Error::invalid_data(format!(
            "compression offset {} exceeds remaining data length {}",
            offset,
            remaining.len()
        )));
    }
    let uncompressed_prefix = &remaining[..offset];
    let compressed_data = &remaining[offset..];
    decompress_message(
        uncompressed_prefix,
        compressed_data,
        header.original_compressed_segment_size,
    )
}

// Arc-based TransportSend/TransportReceive for TcpTransport sharing.
#[async_trait::async_trait]
impl<T: TransportSend> TransportSend for Arc<T> {
    async fn send(&self, data: &[u8]) -> Result<()> {
        (**self).send(data).await
    }
}

#[async_trait::async_trait]
impl<T: TransportReceive> TransportReceive for Arc<T> {
    async fn receive(&self) -> Result<Vec<u8>> {
        (**self).receive().await
    }

    // ❌ Don't drop this forward. The default answers `None`, and that isn't
    // an error anyone sees: `Connection::connect` wraps `TcpTransport` in an
    // `Arc`, so a missing forward silently puts every TCP connection back to
    // treating a frame still arriving as silence.
    fn receive_progress(&self) -> Option<Arc<ReceiveProgress>> {
        (**self).receive_progress()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SignatureFailure;
    use crate::msg::negotiate::{NegotiateContext, HASH_ALGORITHM_SHA512};
    use crate::transport::MockTransport;
    use crate::types::flags::HeaderFlags;

    /// Pack a set of SMB2 sub-responses into one compound transport frame
    /// by wiring up `NextCommand` offsets and 8-byte-padding each sub
    /// except the last. Used by compound execute tests below.
    use crate::client::test_helpers::build_compound_response_frame;

    /// Build a canned negotiate response with the given dialect.
    fn build_negotiate_response(dialect: Dialect) -> Vec<u8> {
        let resp_header = {
            let mut h = Header::new_request(Command::Negotiate);
            h.flags.set_response();
            h.credits = 32;
            h
        };
        let resp_body = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: dialect,
            server_guid: Guid::ZERO,
            capabilities: Capabilities::new(Capabilities::DFS | Capabilities::LEASING),
            max_transact_size: 65536,
            max_read_size: 65536,
            max_write_size: 65536,
            system_time: 132_000_000_000_000_000,
            server_start_time: 131_000_000_000_000_000,
            security_buffer: vec![0x60, 0x00],
            negotiate_contexts: if dialect == Dialect::Smb3_1_1 {
                vec![NegotiateContext::PreauthIntegrity {
                    hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                    salt: vec![0xBB; 32],
                }]
            } else {
                vec![]
            },
        };
        pack_message(&resp_header, &resp_body)
    }

    #[tokio::test]
    async fn negotiate_stores_params_correctly() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();

        let params = conn.params().unwrap();
        assert_eq!(params.dialect, Dialect::Smb3_1_1);
        assert_eq!(params.max_read_size, 65536);
        assert_eq!(params.max_write_size, 65536);
        assert_eq!(params.max_transact_size, 65536);
        assert!(!params.signing_required);
    }

    #[tokio::test]
    async fn negotiate_updates_credits() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb3_0));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();

        // Server granted 32 credits, minus 1 consumed for our request.
        assert_eq!(conn.credits(), 32);
    }

    #[tokio::test]
    async fn credit_capacity_shrinks_as_the_operation_grows() {
        let mock = Arc::new(MockTransport::new());
        let conn =
            crate::client::test_helpers::setup_connection_with_max_read(&mock, 8 * 1024 * 1024);

        // 4 MiB costs 64 credits for the READ plus one each for CREATE and
        // CLOSE, so a 512-credit window carries seven of them at once.
        assert_eq!(conn.credit_capacity_for(4 * 1024 * 1024), 7);
        // A full-window read costs 130, which is where the wedge came from.
        assert_eq!(conn.credit_capacity_for(8 * 1024 * 1024), 3);
        // A small file charges the minimum: one credit each for all three.
        assert_eq!(conn.credit_capacity_for(4096), 170);
    }

    /// Once the server has shown where its window stops, every size cut-off
    /// this crate hands out fits in half of it, so nothing it picks the size
    /// of can be refused for credits.
    #[tokio::test]
    async fn size_cut_offs_fit_a_window_the_server_stopped_growing() {
        let mock = Arc::new(MockTransport::new());
        let mut conn =
            crate::client::test_helpers::setup_connection_with_max_read(&mock, 8 * 1024 * 1024);
        let mut params = conn.params().unwrap();
        params.max_write_size = 8 * 1024 * 1024;
        conn.set_test_params(params);
        conn.note_read_rate(1e12);
        assert_eq!(conn.quick_read_limit(), 8 * 1024 * 1024);
        assert_eq!(conn.compound_write_limit(), 8 * 1024 * 1024);
        assert_eq!(conn.fundable_chunk(8 * 1024 * 1024), 8 * 1024 * 1024);

        conn.set_credit_ceiling(64);

        // Half of 64 is 32: the READ gets 30 next to CREATE and CLOSE, the
        // WRITE 30 next to CREATE and FLUSH, and a lone chunk all 32.
        assert_eq!(conn.quick_read_limit(), 30 * 65536);
        assert_eq!(conn.compound_write_limit(), 30 * 65536);
        assert_eq!(conn.fundable_chunk(8 * 1024 * 1024), 32 * 65536);
        assert_eq!(conn.credit_capacity_for(512 * 1024), 6);

        // A window too small for any compound streams everything, and a
        // chunk never drops below one credit.
        conn.set_credit_ceiling(4);
        assert_eq!(conn.quick_read_limit(), 0);
        assert_eq!(conn.compound_write_limit(), 0);
        assert_eq!(conn.fundable_chunk(8 * 1024 * 1024), 2 * 65536);
        conn.set_credit_ceiling(1);
        assert_eq!(conn.fundable_chunk(8 * 1024 * 1024), 65536);
        assert_eq!(conn.fundable_chunk(1000), 1000);
    }

    #[tokio::test]
    async fn credit_capacity_is_never_zero() {
        let mock = Arc::new(MockTransport::new());
        let conn =
            crate::client::test_helpers::setup_connection_with_max_read(&mock, 8 * 1024 * 1024);

        assert!(conn.credit_capacity_for(0) >= 1);
        assert!(conn.credit_capacity_for(u64::MAX) >= 1);
    }

    // ── Credit accounting ──────────────────────────────────────────────

    /// The window is the server's, and it is spent the moment a request goes
    /// on the wire — not when the answer comes back. Sending more than the
    /// server granted is a protocol violation it is entitled to punish, and
    /// at least one NAS punishes it by going silent forever.
    #[tokio::test]
    async fn concurrent_requests_cannot_outspend_the_credit_window() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // The server's entire window is four credits, and no response is ever
        // queued, so nothing replenishes it.
        conn.set_credits(4);

        // Four concurrent requests charging two credits each: eight credits
        // asked of a four-credit window.
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let c = conn.clone();
            tasks.push(tokio::spawn(async move {
                let body = crate::msg::echo::EchoRequest;
                c.execute_with_credits(Command::Echo, &body, Some(TreeId(1)), CreditCharge(2))
                    .await
            }));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let sent = mock.sent_count();
        for t in tasks {
            t.abort();
        }
        assert!(
            sent <= 2,
            "over-spent the four-credit window: {sent} requests of charge 2 reached the wire"
        );
    }

    /// The gate must not turn an over-spend hang into a starvation hang. A
    /// server that goes quiet for any reason stops granting credits, and a
    /// send parked on those credits has to give up and say so.
    #[tokio::test]
    async fn a_server_that_stops_granting_credits_errors_instead_of_hanging() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(2);
        conn.set_credit_wait_timeout(std::time::Duration::from_millis(300));

        // Takes the whole window and is never answered, so the server looks
        // alive (the request is outstanding) but grants nothing.
        let holder = conn.clone();
        let held = tokio::spawn(async move {
            let body = crate::msg::echo::EchoRequest;
            holder
                .execute_with_credits(Command::Echo, &body, Some(TreeId(1)), CreditCharge(2))
                .await
        });
        while mock.sent_count() < 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let body = crate::msg::echo::EchoRequest;
        let blocked = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            conn.execute_with_credits(Command::Echo, &body, Some(TreeId(1)), CreditCharge(2)),
        )
        .await
        .expect("the send must give up on its own, not hang until the test times out");

        held.abort();
        assert!(
            matches!(blocked, Err(Error::CreditStarvation { needed: 2, .. })),
            "expected a typed starvation error, got {blocked:?}"
        );
        assert_eq!(
            conn.metrics().credit_starvations,
            1,
            "starvation is counted so a consumer can see it without reading logs"
        );
        assert_eq!(
            mock.sent_count(),
            1,
            "the starved request must not reach the wire"
        );
    }

    // ── Response deadline ──────────────────────────────────────────────

    /// The failure that started all this: a server that accepts a request and
    /// then says nothing while its TCP socket stays open. Correct credit
    /// accounting removes the cause; this removes the symptom whatever the
    /// cause turns out to be.
    #[tokio::test]
    async fn a_silent_server_cannot_hang_a_caller_forever() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);
        conn.set_response_timeout(Some(std::time::Duration::from_millis(200)));

        let body = crate::msg::echo::EchoRequest;
        // No response is ever queued.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            conn.execute(Command::Echo, &body, Some(TreeId(1))),
        )
        .await
        .expect("the caller must give up on its own, not hang until the test times out");

        assert!(
            matches!(result, Err(Error::Timeout)),
            "expected a timeout, got {result:?}"
        );
        assert_eq!(conn.metrics().response_timeouts, 1);
        assert!(
            conn.outstanding_requests().is_empty(),
            "the abandoned request must not leak a waiter"
        );
    }

    /// The handshake is a request like any other: a server that accepts the
    /// TCP connection and then never answers the NEGOTIATE must not hang the
    /// caller. NEGOTIATE used to wait on a bare `guard.recv()`, so a reconnect
    /// against such a server never returned (70 s and counting, observed from
    /// Cmdr, which held its reconnect lock the whole time).
    #[tokio::test]
    async fn a_server_that_never_answers_negotiate_cannot_hang_the_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (conn, accepted) = tokio::join!(
            Connection::connect(&addr, std::time::Duration::from_secs(5)),
            listener.accept()
        );
        let mut conn = conn.unwrap();
        // Held open and silent for the whole test.
        let _server = accepted.unwrap().0;
        conn.set_response_timeout(Some(std::time::Duration::from_millis(200)));

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), conn.negotiate())
            .await
            .expect("the handshake must give up on its own, not hang until the test times out");

        assert!(
            matches!(
                result,
                Err(Error::Timeout) | Err(Error::ServerUnresponsive { .. })
            ),
            "expected a timeout, got {result:?}"
        );
        assert_eq!(conn.metrics().response_timeouts, 1);
    }

    /// A server that answers STATUS_PENDING is working, not dead. Timing it
    /// out would break every legitimately slow operation.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_interim_pending_response_restarts_the_deadline() {
        let mock = Arc::new(MockTransport::new());
        // Plain mode: auto-rewrite pairs one queued response per sent request,
        // and this test queues five responses for a single request.
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);
        conn.set_response_timeout(Some(std::time::Duration::from_millis(300)));

        let c = conn.clone();
        let call = tokio::spawn(async move {
            let body = crate::msg::echo::EchoRequest;
            c.execute(Command::Echo, &body, Some(TreeId(1))).await
        });
        while mock.sent_count() < 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // Keep saying "still working" for well past the deadline.
        for _ in 0..4 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let mut h = Header::new_request(Command::Echo);
            h.flags.set_response();
            h.credits = 1;
            h.status = NtStatus::PENDING;
            h.message_id = MessageId(0);
            mock.queue_response(pack_message(&h, &crate::msg::echo::EchoResponse));
        }

        mock.queue_response(build_echo_response_with_msg_id(MessageId(0)));
        let result = call.await.unwrap();
        assert!(
            result.is_ok(),
            "an acknowledged operation must not be timed out: {result:?}"
        );
        assert_eq!(conn.metrics().response_timeouts, 0);
    }

    /// A send half that takes `delay` to accept the first frame, the way a
    /// full-size write crawls onto a slow link.
    struct SlowFirstSend {
        inner: Arc<MockTransport>,
        delay: Duration,
        seen: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TransportSend for SlowFirstSend {
        async fn send(&self, data: &[u8]) -> Result<()> {
            if self.seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(self.delay).await;
            }
            self.inner.send(data).await
        }
    }

    /// A frame that took a long time to reach the wire gets the FULL response
    /// deadline afterwards.
    ///
    /// The clock measures the server's silence, and until `mark_sent` the
    /// server has not been asked anything at all. Without that refresh a big
    /// write that spent most of its send deadline crawling onto a slow link
    /// would arrive at `await_response` with its budget already spent and be
    /// declared unanswered by a server that never saw it — the exact
    /// misdiagnosis the `registered_at` / `sent_at` split exists to prevent.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_send_does_not_eat_the_response_deadline() {
        let mock = Arc::new(MockTransport::new());
        let deadline = Duration::from_millis(300);
        let conn = Connection::from_transport(
            Box::new(SlowFirstSend {
                inner: Arc::clone(&mock),
                // Twice the response deadline: measured from registration this
                // request is long dead, measured from the send it is healthy.
                delay: deadline * 2,
                seen: std::sync::atomic::AtomicUsize::new(0),
            }),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(512);
        conn.set_response_timeout(Some(deadline));
        conn.set_send_timeout(None); // the send side is not what's under test

        let c = conn.clone();
        let call = tokio::spawn(async move {
            let body = crate::msg::echo::EchoRequest;
            c.execute(Command::Echo, &body, Some(TreeId(1))).await
        });

        // Answer promptly once the frame is actually on the wire.
        while mock.sent_count() < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(deadline / 3).await;
        mock.queue_response(build_echo_response_with_msg_id(MessageId(0)));

        let result = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .expect("the call must resolve, not hang")
            .expect("task panicked");
        assert!(
            result.is_ok(),
            "the server answered well inside the deadline it was given: {result:?}"
        );
        assert_eq!(
            conn.metrics().response_timeouts,
            0,
            "time spent getting onto the wire is the send deadline's problem, not the server's"
        );
    }

    /// The default deadlines only work as a set: each layer has to NAME a
    /// problem before the layer above it gives up on it, or the log line that
    /// explains a wedge is never written.
    #[tokio::test]
    async fn the_write_budget_gates_the_whole_connection_not_one_stream() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        // One holder takes the lot: this stands in for the frames a few busy
        // FileWriters already have launched and unacknowledged.
        let whole = conn
            .try_reserve_write_budget(DEFAULT_WRITE_BUDGET_BYTES)
            .expect("an idle connection can afford its whole budget");

        // The NEXT stream is refused, however much room its own pipeline window
        // has left. That refusal is the entire point: before this existed, each
        // stream's window was the only bound and N streams multiplied it.
        assert!(
            conn.try_reserve_write_budget(WRITE_BUDGET_UNIT).is_none(),
            "a spent budget must refuse the next frame, whatever the per-stream window says"
        );

        // A permit is returned by being dropped, which is how the in-flight
        // future gives it back on every path including cancel.
        drop(whole);
        assert!(
            conn.try_reserve_write_budget(WRITE_BUDGET_UNIT).is_some(),
            "dropping the permit must return the budget"
        );
    }

    #[tokio::test]
    async fn a_consumer_can_move_the_write_budget_in_both_directions() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        assert_eq!(conn.write_budget(), DEFAULT_WRITE_BUDGET_BYTES);

        // 32 MiB is a LAN default. On 10 GbE it is ~25 ms of buffer, so a
        // consumer there wants more, and raising it takes effect at once.
        let bigger = DEFAULT_WRITE_BUDGET_BYTES * 8;
        conn.set_write_budget(bigger);
        assert_eq!(conn.write_budget(), bigger);
        let whole = conn
            .try_reserve_write_budget(bigger)
            .expect("a raised budget must be spendable immediately");
        assert!(
            conn.try_reserve_write_budget(WRITE_BUDGET_UNIT).is_none(),
            "and it must still be a bound at the new size"
        );
        drop(whole);

        // A memory-constrained consumer goes the other way.
        conn.set_write_budget(1024 * 1024);
        assert_eq!(conn.write_budget(), 1024 * 1024);
        let small = conn
            .try_reserve_write_budget(1024 * 1024)
            .expect("a lowered budget must still admit its own size");
        assert!(
            conn.try_reserve_write_budget(WRITE_BUDGET_UNIT).is_none(),
            "a lowered budget must bound at the new size, not the old one"
        );
        drop(small);
    }

    #[tokio::test]
    async fn lowering_the_write_budget_lands_as_the_outstanding_frames_finish() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        // Budget already spent by frames on the wire: their bytes are committed,
        // so a reduction cannot claw them back and must not pretend to.
        let outstanding = conn
            .try_reserve_write_budget(DEFAULT_WRITE_BUDGET_BYTES)
            .expect("an idle connection can afford its whole budget");
        conn.set_write_budget(1024 * 1024);
        assert_eq!(conn.write_budget(), 1024 * 1024);
        assert!(
            conn.try_reserve_write_budget(WRITE_BUDGET_UNIT).is_none(),
            "nothing is free while the old budget is still on the wire"
        );

        // Once it does complete, the new bound is what is left standing.
        drop(outstanding);
        let permit = conn
            .try_reserve_write_budget(1024 * 1024)
            .expect("the new budget is available once the old frames finish");
        assert!(
            conn.try_reserve_write_budget(WRITE_BUDGET_UNIT).is_none(),
            "the returned permits must not restore the old, larger budget"
        );
        drop(permit);
    }

    #[tokio::test]
    async fn a_budget_smaller_than_one_frame_still_admits_that_frame() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        // A consumer is free to ask for a budget below a single `MaxWriteSize`
        // frame. Refusing the frame would wedge the writer forever, so the frame
        // is clamped to the whole budget and gets it to itself.
        conn.set_write_budget(0);
        assert_eq!(
            conn.write_budget(),
            WRITE_BUDGET_UNIT,
            "the budget floor is one 64 KiB granule"
        );
        let permit = tokio::time::timeout(
            Duration::from_secs(5),
            conn.reserve_write_budget(8 * 1024 * 1024),
        )
        .await
        .expect("a frame larger than the whole budget must not wait forever");
        assert!(permit.is_some());
    }

    #[tokio::test]
    async fn a_frame_bigger_than_the_whole_budget_still_gets_sent() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        // A server may negotiate a `MaxWriteSize` above the budget. Asking a
        // semaphore for more permits than it will ever hold waits FOREVER, so
        // without clamping this writer's first frame would wedge and never
        // time out. The timeout is the assertion.
        let huge = DEFAULT_WRITE_BUDGET_BYTES * 4;
        let permit = tokio::time::timeout(Duration::from_secs(5), conn.reserve_write_budget(huge))
            .await
            .expect("an oversized frame must not wait forever for budget it can never get");
        assert!(permit.is_some());
    }

    #[test]
    fn the_write_budget_is_smaller_than_one_multiplied_pipeline() {
        // The field case this exists for: ten concurrent files, each pipelining
        // 32 frames at a 1 MiB `MaxWriteSize`, is 320 MiB of
        // payload queued in this process (a 2026-08-26 Cmdr report). The budget
        // has to be a long way under that or it bounds nothing in practice.
        let multiplied = 10 * 32 * 1024 * 1024u64;
        assert!(
            DEFAULT_WRITE_BUDGET_BYTES * 4 <= multiplied,
            "a budget this close to the unbounded case would not be a bound"
        );
        // And it must still hold several frames of a common `MaxWriteSize`, or
        // it would serialize writers that the pipeline window means to overlap.
        assert!(
            budget_units(DEFAULT_WRITE_BUDGET_BYTES) >= 8 * budget_units(1024 * 1024),
            "the budget must fit several 1 MiB frames, or it becomes the bottleneck"
        );
    }

    #[test]
    fn the_default_deadlines_are_layered() {
        assert!(
            SLOW_SEND_REPORT < SEND_TIMEOUT,
            "a slow send has to be reported before it is cut off"
        );
        assert!(
            STALE_WAITER_AFTER < RESPONSE_TIMEOUT,
            "an outstanding request has to be named before its caller abandons it; \
             at or above the response deadline the sweeper can only ever warn about \
             requests that are already gone"
        );
        assert!(
            STALE_WAITER_AFTER + STALE_WAITER_SWEEP <= RESPONSE_TIMEOUT,
            "the sweeper wakes only every STALE_WAITER_SWEEP, so leave room for at \
             least one sweep to land between 'stale' and 'given up'"
        );
        assert!(
            RESPONSE_TIMEOUT <= Duration::from_secs(45),
            "the deadline is a recovery, not a formality: somebody watching a frozen \
             transfer will not wait minutes for the client to notice"
        );
        assert!(
            SEND_TIMEOUT <= Duration::from_secs(30),
            "same on the send side, and it must stay under the response deadline so a \
             wedged socket is blamed on the send rather than on the server"
        );
        assert!(
            SEND_TIMEOUT < RESPONSE_TIMEOUT,
            "a stuck socket has to surface as SendTimeout, not as an innocent server \
             timing out"
        );
        assert!(
            stall_tolerance(Some(SEND_TIMEOUT)) >= SEND_TIMEOUT,
            "the sweeper must never call a send side stalled while the send deadline \
             is still willing to wait for the frame in flight: `wire_bytes_sent` only \
             ticks on a WHOLE frame, so a tighter window would warn about every link \
             slower than one frame per sweep — links SEND_TIMEOUT was sized to accept"
        );
        assert!(
            stall_tolerance(None) >= STALE_WAITER_SWEEP * 2,
            "and a verdict always rests on more than one look, even when a consumer \
             has removed the send deadline entirely"
        );
        assert!(
            KEEPALIVE_AFTER + Inner::keepalive_tick(KEEPALIVE_AFTER) < RESPONSE_TIMEOUT,
            "a probe has to go out and be given time to come back before the response \
             deadline fires, or the deadline would always decide with no evidence to \
             hand and the keepalive could never earn anyone an extension"
        );
        assert!(
            KEEPALIVE_AFTER * LIVENESS_WINDOW_PROBES < RESPONSE_TIMEOUT,
            "evidence older than the deadline it overrides is not evidence: liveness \
             must stop looking 'proven' before a request's own budget runs out, or the \
             extension would be granted on a reading nothing has refreshed"
        );
        // Compile-time: a tuning pass that breaks these should fail the build,
        // not merely a test run.
        const {
            assert!(
                LIVENESS_WINDOW_PROBES >= 2,
                "one probe threshold leaves no room for a probe round that ran late on a \
                 loaded client, so a healthy connection would drop out of 'proven alive' \
                 between two answered probes"
            )
        };
        const {
            assert!(
                ALIVE_DEADLINE_FACTOR >= 2,
                "an extension that isn't meaningfully longer than the base buys nothing \
                 for the slow-but-alive case it exists for"
            )
        };
        assert!(
            RESPONSE_TIMEOUT.saturating_mul(ALIVE_DEADLINE_FACTOR) <= Duration::from_secs(300),
            "'alive' is a reason for more patience, not unlimited patience: nobody \
             watching a frozen transfer waits out five minutes"
        );
        assert!(
            LONG_POLL_REFRESH > RESPONSE_TIMEOUT.saturating_mul(ALIVE_DEADLINE_FACTOR),
            "the refresh is the slowest clock on a connection, and has to stay that way: \
             everything faster than it is about detecting a death, and a refresh detects \
             nothing. One that fired inside the window where a request can still be \
             declared dead would replace subscriptions on a session about to be torn down"
        );
        assert!(
            LONG_POLL_REFRESH >= Duration::from_secs(60),
            "a refresh costs three frames per watched directory and heals nothing faster \
             for being frequent — there is nothing to detect, only a bet not to make"
        );
    }

    /// CHANGE_NOTIFY waits for an event that may never come. Applying a
    /// deadline to it would break the watcher rather than protect it.
    #[tokio::test]
    async fn a_long_poll_command_is_exempt_from_the_deadline() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);
        conn.set_response_timeout(Some(std::time::Duration::from_millis(100)));

        let req = crate::msg::change_notify::ChangeNotifyRequest {
            flags: 0,
            output_buffer_length: 4096,
            file_id: crate::types::FileId {
                persistent: 1,
                volatile: 2,
            },
            completion_filter: 0xFF,
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(600),
            conn.execute(Command::ChangeNotify, &req, Some(TreeId(1))),
        )
        .await;
        assert!(
            outcome.is_err(),
            "CHANGE_NOTIFY must keep waiting, not time out"
        );
        assert_eq!(conn.metrics().response_timeouts, 0);
    }

    /// An ECHO response carrying `credits` for `msg_id`.
    fn echo_response_granting(msg_id: u64, credits: u16) -> Vec<u8> {
        let mut h = Header::new_request(Command::Echo);
        h.flags.set_response();
        h.credits = credits;
        h.message_id = MessageId(msg_id);
        pack_message(&h, &crate::msg::echo::EchoResponse)
    }

    /// Send one ECHO and answer it with a grant of `credits`, the way a server
    /// steers the window: more than the charge grows it, the charge alone
    /// holds it where it is.
    async fn echo_answered_with(
        conn: &Connection,
        mock: &Arc<MockTransport>,
        msg_id: u64,
        credits: u16,
    ) {
        let sent_before = mock.sent_count();
        let sender = conn.clone();
        let echo = tokio::spawn(async move {
            sender
                .execute(Command::Echo, &crate::msg::echo::EchoRequest, None)
                .await
        });
        while mock.sent_count() == sent_before {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        mock.queue_response(echo_response_granting(msg_id, credits));
        echo.await.unwrap().expect("the ECHO is answered");
    }

    /// Park a CHANGE_NOTIFY the server holds on to, the way a directory
    /// watcher does. It keeps a request outstanding indefinitely, so "nothing
    /// outstanding" can never say a charge is hopeless while it's there.
    async fn park_a_long_poll(
        conn: &Connection,
        mock: &Arc<MockTransport>,
    ) -> tokio::task::JoinHandle<Result<Frame>> {
        let sent_before = mock.sent_count();
        let poller = conn.clone();
        let poll = tokio::spawn(async move {
            let req = crate::msg::change_notify::ChangeNotifyRequest {
                flags: 0,
                output_buffer_length: 4096,
                file_id: crate::types::FileId {
                    persistent: 1,
                    volatile: 2,
                },
                completion_filter: 0xFF,
            };
            poller
                .execute(Command::ChangeNotify, &req, Some(TreeId(1)))
                .await
        });
        while mock.sent_count() == sent_before {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        poll
    }

    /// A charge wider than the whole window, on a server that has stopped
    /// growing it, can never be funded however long it waits. Waiting would
    /// also stall everything queued behind it on the fair credit pool, so it
    /// fails at once, even while a long poll keeps a request outstanding.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_charge_wider_than_a_window_the_server_stopped_growing_fails_immediately() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(1);
        // Deliberately long: passing means the fast path fired, not the deadline.
        conn.set_credit_wait_timeout(std::time::Duration::from_secs(60));

        // The server grows the window to 64, then declines to grow it further.
        echo_answered_with(&conn, &mock, 0, 64).await;
        echo_answered_with(&conn, &mock, 1, 1).await;
        let poll = park_a_long_poll(&conn, &mock).await;
        let sent_before = mock.sent_count();

        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            conn.execute_with_credits(
                Command::Echo,
                &crate::msg::echo::EchoRequest,
                None,
                CreditCharge(128),
            ),
        )
        .await;
        poll.abort();

        let result = result.expect("a charge the window can never fund must not wait at all");
        assert!(
            matches!(result, Err(Error::CreditStarvation { needed: 128, .. })),
            "expected a typed starvation error, got {result:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "waited {:?} for a window the server had stopped growing",
            started.elapsed()
        );
        assert_eq!(
            mock.sent_count(),
            sent_before,
            "the unfundable request must not reach the wire"
        );
        assert_eq!(conn.credit_ceiling(), Some(64));
    }

    /// The other side of that rule: a server still growing the window (one
    /// that grants in steps, as Windows Server before 2016 did) is on its
    /// way to funding a big charge, so the charge waits for the ramp. Samba
    /// looks like this too: it declines to grow on the first session setup
    /// leg and grows the window on the last.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_charge_wider_than_a_window_still_growing_waits_for_it() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(1);
        conn.set_credit_wait_timeout(std::time::Duration::from_secs(60));

        echo_answered_with(&conn, &mock, 0, 64).await;
        echo_answered_with(&conn, &mock, 1, 1).await;
        // Growth past where it stopped: the window is still ramping.
        echo_answered_with(&conn, &mock, 2, 33).await;
        assert_eq!(
            conn.credit_ceiling(),
            None,
            "growth past the ceiling clears it"
        );
        let poll = park_a_long_poll(&conn, &mock).await;

        let sender = conn.clone();
        let big = tokio::spawn(async move {
            sender
                .execute_with_credits(
                    Command::Echo,
                    &crate::msg::echo::EchoRequest,
                    None,
                    CreditCharge(128),
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        assert!(
            !big.is_finished(),
            "a still-growing window is worth waiting for"
        );

        // The long poll's answer grows the window past the charge.
        let mut h = Header::new_request(Command::ChangeNotify);
        h.flags.set_response();
        h.credits = 40;
        h.message_id = MessageId(3);
        h.status = NtStatus::NOTIFY_ENUM_DIR;
        mock.queue_response(pack_message(&h, &crate::msg::echo::EchoResponse));
        let _ = poll.await;
        while mock.sent_count() < 5 {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        mock.queue_response(echo_response_granting(4, 128));
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), big)
            .await
            .expect("the funded request completes")
            .unwrap();
        assert!(
            result.is_ok(),
            "expected the ramp to fund it, got {result:?}"
        );
    }

    /// With nothing outstanding, no grant can ever arrive — so there is
    /// nothing to wait for and the deadline is the wrong answer.
    #[tokio::test]
    async fn a_charge_no_outstanding_request_can_fund_fails_immediately() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(1);
        // Deliberately long: passing this test means the fast path fired, not
        // that the deadline did.
        conn.set_credit_wait_timeout(std::time::Duration::from_secs(60));

        let body = crate::msg::echo::EchoRequest;
        let started = std::time::Instant::now();
        let result = conn
            .execute_with_credits(Command::Echo, &body, Some(TreeId(1)), CreditCharge(4))
            .await;

        assert!(
            matches!(
                result,
                Err(Error::CreditStarvation {
                    needed: 4,
                    available: 1,
                    ..
                })
            ),
            "expected an immediate starvation error, got {result:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "waited {:?} for credits that could never arrive",
            started.elapsed()
        );
    }

    /// The other half of the gate: parking is temporary. A grant on a
    /// response has to release the request waiting behind it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_releases_a_send_parked_on_credits() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(2);

        let holder = conn.clone();
        let first = tokio::spawn(async move {
            let body = crate::msg::echo::EchoRequest;
            holder
                .execute_with_credits(Command::Echo, &body, Some(TreeId(1)), CreditCharge(2))
                .await
        });
        while mock.sent_count() < 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let waiter = conn.clone();
        let second = tokio::spawn(async move {
            let body = crate::msg::echo::EchoRequest;
            waiter
                .execute_with_credits(Command::Echo, &body, Some(TreeId(1)), CreditCharge(2))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(mock.sent_count(), 1, "the second send is parked on credits");

        // Answer the first request; its grant funds the parked one.
        mock.queue_response(build_echo_response_with_msg_id(MessageId(0)));
        first.await.unwrap().expect("first request completes");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while mock.sent_count() < 2 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        second.abort();
        assert_eq!(
            mock.sent_count(),
            2,
            "the grant on the first response must release the parked send"
        );
        assert_eq!(conn.metrics().credit_waits, 1);
    }

    #[tokio::test]
    async fn negotiate_increments_message_id() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb2_0_2));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        assert_eq!(conn.next_message_id(), 0);
        conn.negotiate().await.unwrap();
        assert_eq!(conn.next_message_id(), 1);
    }

    #[tokio::test]
    async fn negotiate_updates_preauth_hash() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let initial_hash = *conn.preauth_hasher().value();
        conn.negotiate().await.unwrap();
        assert_ne!(conn.preauth_hasher().value(), &initial_hash);
    }

    #[tokio::test]
    async fn negotiate_rejects_invalid_max_read_size() {
        let resp_header = {
            let mut h = Header::new_request(Command::Negotiate);
            h.flags.set_response();
            h.credits = 1;
            h
        };
        let resp_body = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: Dialect::Smb2_0_2,
            server_guid: Guid::ZERO,
            capabilities: Capabilities::default(),
            max_transact_size: 65536,
            max_read_size: 1024, // Too small
            max_write_size: 65536,
            system_time: 0,
            server_start_time: 0,
            security_buffer: vec![],
            negotiate_contexts: vec![],
        };
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(pack_message(&resp_header, &resp_body));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let result = conn.negotiate().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("MaxReadSize"));
    }

    #[tokio::test]
    async fn message_id_increments_on_send_request() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        conn.set_credits(512);

        // Manually set past negotiate.
        conn.set_next_message_id(5);

        use crate::msg::tree_disconnect::TreeDisconnectRequest;
        let body = TreeDisconnectRequest;
        // With execute(), the msg_id is an internal allocation — we peek it
        // via next_message_id() before sending. Use a timeout so the test
        // doesn't wait for a response the mock never produces.
        assert_eq!(conn.next_message_id(), 5);
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            conn.execute(Command::TreeDisconnect, &body, None),
        )
        .await;
        assert_eq!(conn.next_message_id(), 6);
    }

    #[tokio::test]
    async fn signing_applied_to_outgoing_messages() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        conn.set_credits(512);

        // Activate signing.
        let key = vec![0xAA; 16];
        conn.activate_signing(key, SigningAlgorithm::HmacSha256);
        conn.set_session_id(SessionId(0x1234));

        use crate::msg::tree_disconnect::TreeDisconnectRequest;
        let body = TreeDisconnectRequest;
        // execute() awaits the response, but we only care about the sent bytes.
        // Spawn+abort the future so the send runs but the await doesn't block on
        // a response that never comes.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            conn.execute(Command::TreeDisconnect, &body, None),
        )
        .await;

        let msg_bytes = mock.sent_message(0).expect("one send recorded");
        // Verify the signed flag is set in the header.
        let flags = u32::from_le_bytes(msg_bytes[16..20].try_into().unwrap());
        assert!(flags & HeaderFlags::SIGNED != 0, "message should be signed");

        // Verify signature is non-zero.
        let sig = &msg_bytes[48..64];
        assert_ne!(sig, &[0u8; 16], "signature should not be all zeros");
    }

    #[tokio::test]
    async fn negotiate_with_smb2_dialect() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb2_0_2));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();

        let params = conn.params().unwrap();
        assert_eq!(params.dialect, Dialect::Smb2_0_2);
        assert!(!params.gmac_negotiated);
        assert!(params.cipher.is_none());
    }

    #[tokio::test]
    async fn negotiate_sends_all_five_dialects() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();

        // Verify the sent request contains all 5 dialects.
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = NegotiateRequest::unpack(&mut cursor).unwrap();
        assert_eq!(req.dialects.len(), 5);
        assert!(req.dialects.contains(&Dialect::Smb2_0_2));
        assert!(req.dialects.contains(&Dialect::Smb2_1));
        assert!(req.dialects.contains(&Dialect::Smb3_0));
        assert!(req.dialects.contains(&Dialect::Smb3_0_2));
        assert!(req.dialects.contains(&Dialect::Smb3_1_1));
    }

    // (Compound-specific send/receive tests removed — execute_compound tests live below.)

    // ── Compression tests ────────────────────────────────────────────

    use crate::msg::negotiate::COMPRESSION_LZ4;
    use crate::msg::transform::{
        CompressionTransformHeader, COMPRESSION_ALGORITHM_LZ4, COMPRESSION_PROTOCOL_ID,
        SMB2_COMPRESSION_FLAG_NONE,
    };

    /// Build a negotiate response that includes a compression context with LZ4.
    fn build_negotiate_response_with_compression(dialect: Dialect) -> Vec<u8> {
        let resp_header = {
            let mut h = Header::new_request(Command::Negotiate);
            h.flags.set_response();
            h.credits = 32;
            h
        };
        let resp_body = NegotiateResponse {
            security_mode: SecurityMode::new(SecurityMode::SIGNING_ENABLED),
            dialect_revision: dialect,
            server_guid: Guid::ZERO,
            capabilities: Capabilities::new(Capabilities::DFS | Capabilities::LEASING),
            max_transact_size: 65536,
            max_read_size: 65536,
            max_write_size: 65536,
            system_time: 132_000_000_000_000_000,
            server_start_time: 131_000_000_000_000_000,
            security_buffer: vec![0x60, 0x00],
            negotiate_contexts: vec![
                NegotiateContext::PreauthIntegrity {
                    hash_algorithms: vec![HASH_ALGORITHM_SHA512],
                    salt: vec![0xBB; 32],
                },
                NegotiateContext::Compression {
                    flags: 0,
                    algorithms: vec![COMPRESSION_LZ4],
                },
            ],
        };
        pack_message(&resp_header, &resp_body)
    }

    #[tokio::test]
    async fn negotiate_detects_compression_support() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response_with_compression(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();

        let params = conn.params().unwrap();
        assert!(params.compression_supported);
        assert!(conn.compression_enabled());
    }

    #[tokio::test]
    async fn negotiate_without_compression_context_disables_compression() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.negotiate().await.unwrap();

        let params = conn.params().unwrap();
        assert!(!params.compression_supported);
        assert!(!conn.compression_enabled());
    }

    #[tokio::test]
    async fn compression_disabled_when_client_config_says_no() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response_with_compression(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_compression_requested(false);
        conn.negotiate().await.unwrap();

        // Server supports it, but client disabled it.
        let params = conn.params().unwrap();
        assert!(params.compression_supported);
        assert!(!conn.compression_enabled());
    }

    #[tokio::test]
    async fn negotiate_offers_compression_context_when_requested() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response_with_compression(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // compression_requested defaults to true.
        conn.negotiate().await.unwrap();

        // Parse the sent negotiate request and check for compression context.
        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = NegotiateRequest::unpack(&mut cursor).unwrap();

        let has_compression = req.negotiate_contexts.iter().any(|ctx| {
            matches!(ctx, NegotiateContext::Compression { algorithms, .. }
                if algorithms.contains(&COMPRESSION_LZ4))
        });
        assert!(
            has_compression,
            "negotiate request should include compression context with LZ4"
        );
    }

    #[tokio::test]
    async fn negotiate_does_not_offer_compression_when_disabled() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        mock.queue_response(build_negotiate_response(Dialect::Smb3_1_1));

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_compression_requested(false);
        conn.negotiate().await.unwrap();

        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let _header = Header::unpack(&mut cursor).unwrap();
        let req = NegotiateRequest::unpack(&mut cursor).unwrap();

        let has_compression = req
            .negotiate_contexts
            .iter()
            .any(|ctx| matches!(ctx, NegotiateContext::Compression { .. }));
        assert!(
            !has_compression,
            "negotiate request should not include compression context"
        );
    }

    #[test]
    fn build_compressed_frame_roundtrip() {
        // Create a message with a compressible payload.
        let mut message = vec![0xFE; Header::SIZE]; // header-like prefix
        let payload: Vec<u8> = b"COMPRESS_ME_".iter().copied().cycle().take(2048).collect();
        message.extend_from_slice(&payload);

        let compressed = compress_message(&message, Header::SIZE).expect("should compress");
        let framed = build_compressed_frame(&compressed);

        // Verify the frame starts with compression protocol ID.
        assert_eq!(&framed[0..4], &COMPRESSION_PROTOCOL_ID);

        // Decompress and verify roundtrip.
        let decompressed = decompress_response(&framed).expect("should decompress");
        assert_eq!(decompressed, message);
    }

    #[test]
    fn decompress_response_rejects_unsupported_algorithm() {
        // Build a compression transform header with an unsupported algorithm.
        let header = CompressionTransformHeader {
            original_compressed_segment_size: 100,
            compression_algorithm: 0x0001, // LZNT1, not LZ4
            flags: SMB2_COMPRESSION_FLAG_NONE,
            offset_or_length: 0,
        };
        let mut cursor = WriteCursor::new();
        header.pack(&mut cursor);
        let mut frame = cursor.into_inner();
        frame.extend_from_slice(&[0u8; 10]); // bogus data

        let result = decompress_response(&frame);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unsupported compression algorithm"));
    }

    #[test]
    fn decompress_response_rejects_chained_compression() {
        let header = CompressionTransformHeader {
            original_compressed_segment_size: 100,
            compression_algorithm: COMPRESSION_ALGORITHM_LZ4,
            flags: 0x0001, // chained
            offset_or_length: 0,
        };
        let mut cursor = WriteCursor::new();
        header.pack(&mut cursor);
        let mut frame = cursor.into_inner();
        frame.extend_from_slice(&[0u8; 10]);

        let result = decompress_response(&frame);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unchained"));
    }

    #[test]
    fn decompress_response_rejects_too_short_data() {
        let result = decompress_response(&[0xFC, b'S', b'M']);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too short"));
    }

    // ── Unsolicited oplock break tests ─────────────────────────────

    // ── Phase 3 (silent-discard fix) red test ───────────────────────
    //
    // Pins the invariant that an unrecoverable frame-level error
    // (decrypt failure, decompress failure, malformed header after
    // decryption) MUST NOT silently discard the frame and leave the
    // matching waiter hanging forever. The Phase 2 receiver task
    // currently `log-at-WARN + continue`s on decrypt failure — the
    // msg_id isn't recoverable from an unparseable frame, so there's
    // no waiter to notify targeted; the only correct behavior is to
    // tear down the connection and fan `Err(Disconnected)` to all
    // pending waiters.
    //
    // This test uses `tokio::time::timeout` to detect the hang: if
    // the waiter doesn't resolve within 2 seconds, it's hung (bug
    // present, test fails). Post-P3.4 fix, the waiter resolves with
    // an error before the timeout.

    /// A consumer must be able to SEE which request is hung, not just read a
    /// log line about it: a connection still serving small requests while one
    /// large write hangs looks healthy by every other diagnostic.
    #[tokio::test]
    async fn outstanding_requests_names_the_unanswered_request() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        assert!(conn.outstanding_requests().is_empty(), "idle connection");

        let _rx = conn.register_waiter(MessageId(7), Command::Write).unwrap();
        let outstanding = conn.outstanding_requests();

        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].command, Command::Write);
        assert_eq!(outstanding[0].message_id, 7);
    }

    /// The warning is the crate's opinion about the consumer's server, so the
    /// consumer has to be able to retune or silence it.
    #[tokio::test]
    async fn the_stale_request_warning_can_be_retuned_and_silenced() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let _rx = conn.register_waiter(MessageId(1), Command::Read).unwrap();

        // Silenced: the sweeper must not consider anything stale.
        conn.set_stale_request_warning(None);
        assert!(conn.inner.stale_request_after.lock().unwrap().is_none());

        // Retuned: a zero threshold means everything outstanding is stale, which
        // is the boundary a consumer with a fast server would pick.
        conn.set_stale_request_warning(Some(Duration::from_millis(0)));
        assert_eq!(
            *conn.inner.stale_request_after.lock().unwrap(),
            Some(Duration::from_millis(0))
        );
        sweep_connection(&conn.inner); // must not panic, and must consider it
    }

    /// A parked long poll is never reported as a stalled request.
    ///
    /// The sweeper's every line means "this should have come back by now", and
    /// for a CHANGE_NOTIFY that is false by construction — the deadline exempts
    /// it precisely because hours of silence is the healthy case. Warning about
    /// it anyway is the sweeper contradicting the deadline, and at the
    /// sweeper's cadence it buries every genuine line: a file manager watching
    /// two panes logged 5,911 WARNs in six hours that way (2026-08-03).
    #[tokio::test]
    async fn a_parked_long_poll_is_never_reported_as_a_stalled_request() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let _watch = conn
            .register_waiter(MessageId(1), Command::ChangeNotify)
            .unwrap();
        let _write = conn.register_waiter(MessageId(2), Command::Write).unwrap();

        // Zero threshold: everything outstanding is old enough to be called
        // out, so only the classification can keep the long poll out.
        let split = classify_outstanding(&conn.inner, Duration::from_millis(0));

        assert_eq!(
            split.queued.iter().map(|q| q.command).collect::<Vec<_>>(),
            vec![Command::Write],
            "only the request that is genuinely waiting for the wire is called out"
        );
        assert_eq!(
            split.parked.iter().map(|p| p.command).collect::<Vec<_>>(),
            vec![Command::ChangeNotify],
            "the long poll still has to be VISIBLE — it is reported at TRACE, and named \
             in full whenever a real stale request is warned about, because a wedge \
             investigation wants the whole in-flight picture"
        );
    }

    fn waiting_frame(msg_id: u64, command: Command, age: Duration) -> Outstanding {
        Outstanding {
            msg_id: MessageId(msg_id),
            command,
            age,
        }
    }

    /// A reading the pure reporting tests can hand in, with the send side in a
    /// given state and one sweep interval behind it.
    fn reading(state: SendSide) -> SendSideReading {
        SendSideReading {
            state,
            silent_for: match state {
                SendSide::Draining => Duration::from_secs(0),
                SendSide::Stalled => Duration::from_secs(25),
            },
            window: STALE_WAITER_SWEEP,
        }
    }

    /// A deep send queue collapses to ONE line however many frames are in it.
    ///
    /// The incident this pins: 282 × 66 MB copied to a NAS over a 6.7 MB/s
    /// Wi-Fi link kept ~320 one-MiB frames queued at all times, and a line per
    /// frame per sweep made 4,487 of the 4,744 lines in the user's diagnostic
    /// bundle this one message. The bundle covered 2 minutes 30 seconds;
    /// everything that would have explained anything had already rotated out
    /// (2026-08-26).
    #[test]
    fn a_deep_send_queue_collapses_to_one_line_however_many_frames_are_in_it() {
        let split = OutstandingSplit {
            unanswered: vec![Unanswered {
                request: waiting_frame(1, Command::Read, Duration::from_secs(20)),
                sent_age: Duration::from_secs(19),
            }],
            queued: (2..=321)
                .map(|id| waiting_frame(id, Command::Write, Duration::from_secs(70)))
                .collect(),
            parked: Vec::new(),
        };

        let records = sweep_report(
            &split,
            &reading(SendSide::Draining),
            &SendActivity::default(),
            320,
        );

        assert_eq!(
            records.len(),
            2,
            "one line for the request the server owes an answer for, and ONE for the \
             whole send side — never one per frame: {records:#?}"
        );
    }

    /// Every frame in a bulk copy over a slow link is a slow send, so the slow
    /// ones are counted per sweep, never announced one at a time.
    ///
    /// Per frame, this was six or seven `WARN` lines a second for a sixteen
    /// minute transfer, on top of the queue flood above — the same pathology
    /// twice on the same code path.
    #[test]
    fn a_burst_of_slow_sends_collapses_to_one_line_per_sweep() {
        let activity = SendActivity {
            frames: 64,
            bytes: 67_108_864,
            slow_frames: 64,
            worst_queued: Duration::from_secs(9),
            worst_writing: Duration::from_millis(1_200),
        };

        let records = sweep_report(
            &OutstandingSplit::default(),
            &reading(SendSide::Draining),
            &activity,
            0,
        );

        assert_eq!(records.len(), 1, "{records:#?}");
        assert_eq!(records[0].0, Level::Info, "{}", records[0].1);
        let line = &records[0].1;
        assert!(
            line.contains("64 frame(s)"),
            "the rate, not each frame: {line}"
        );
        assert!(
            line.contains("6.7 MB/s"),
            "what the link actually carried: {line}"
        );
        assert!(
            line.contains("9s queued") && line.contains("1.2s writing"),
            "queue time and write time stay split — they blame different things: {line}"
        );
    }

    /// A healthy, quiet connection says nothing at all.
    #[test]
    fn a_sweep_with_nothing_to_report_stays_silent() {
        let records = sweep_report(
            &OutstandingSplit::default(),
            &reading(SendSide::Draining),
            &SendActivity::default(),
            0,
        );

        assert!(records.is_empty(), "{records:#?}");
    }

    /// A deep queue that is MOVING is the link being slow, not a fault, so it
    /// must not be reported as one.
    #[test]
    fn a_draining_send_queue_is_reported_below_warn() {
        let split = OutstandingSplit {
            unanswered: Vec::new(),
            queued: (1..=320)
                .map(|id| {
                    let command = if id <= 2 {
                        Command::Read
                    } else {
                        Command::Write
                    };
                    waiting_frame(
                        id,
                        command,
                        Duration::from_secs(70) - Duration::from_millis(id),
                    )
                })
                .collect(),
            parked: Vec::new(),
        };

        let records = sweep_report(
            &split,
            &reading(SendSide::Draining),
            &SendActivity::default(),
            322,
        );

        assert_eq!(records.len(), 1, "{records:#?}");
        assert_eq!(
            records[0].0,
            Level::Info,
            "frames are reaching the socket at exactly the rate the link can carry, \
             which is the healthy shape: {}",
            records[0].1
        );
        let line = &records[0].1;
        assert!(line.contains("320"), "how many are waiting: {line}");
        assert!(line.contains("322"), "how deep the queue is: {line}");
        assert!(line.contains("69.9"), "the oldest frame's age: {line}");
        assert!(
            line.contains("Write x318") && line.contains("Read x2"),
            "the command mix, busiest first: {line}"
        );
    }

    /// A send queue that is deep and NOT MOVING is the pathology the sweeper
    /// exists for, and it stays loud.
    ///
    /// This is the half the collapse must not swallow: a wedged writer task or
    /// a dead socket looks identical to a slow link from the queue depth
    /// alone, and only "is anything still reaching the socket" tells them apart.
    #[test]
    fn a_send_queue_that_is_not_draining_still_warns() {
        let split = OutstandingSplit {
            unanswered: Vec::new(),
            queued: (1..=320)
                .map(|id| waiting_frame(id, Command::Write, Duration::from_secs(70)))
                .collect(),
            parked: Vec::new(),
        };

        let records = sweep_report(
            &split,
            &reading(SendSide::Stalled),
            &SendActivity::default(),
            320,
        );

        assert_eq!(records.len(), 1, "{records:#?}");
        assert_eq!(
            records[0].0,
            Level::Warn,
            "nothing has reached the socket for longer than a whole frame is allowed \
             to take, while 320 frames waited: {}",
            records[0].1
        );
    }

    /// "Draining" means bytes actually reached the socket, which is the one
    /// reading that separates a slow link from a wedged send side.
    ///
    /// Deliberately the same signal `CreditInfo::send_queue_depth` documents
    /// for consumers ("steadily non-zero while `wire_bytes_sent` stands still
    /// means the send side is stuck"), so the crate's own warning and a
    /// consumer's dashboard can never disagree.
    #[tokio::test]
    async fn draining_is_read_from_bytes_actually_reaching_the_socket() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let tolerance = Duration::from_secs(20);
        let t0 = Instant::now();

        conn.inner
            .metrics
            .wire_bytes_sent
            .fetch_add(1024, Ordering::Relaxed);
        assert_eq!(
            conn.inner.observe_send_side(t0, tolerance).state,
            SendSide::Draining,
            "a frame reached the socket"
        );

        assert_eq!(
            conn.inner
                .observe_send_side(t0 + tolerance + Duration::from_secs(1), tolerance)
                .state,
            SendSide::Stalled,
            "and then nothing did, for longer than one frame may take"
        );

        conn.inner
            .metrics
            .wire_bytes_sent
            .fetch_add(1024, Ordering::Relaxed);
        assert_eq!(
            conn.inner
                .observe_send_side(t0 + tolerance + Duration::from_secs(2), tolerance)
                .state,
            SendSide::Draining,
            "one frame getting out clears the verdict"
        );
    }

    /// A link too slow to finish a frame between two sweeps is NOT a wedge.
    ///
    /// `wire_bytes_sent` only ticks on a WHOLE frame, so between completions it
    /// stands still by construction, and judging a single sweep interval would
    /// call any link slower than one frame per `STALE_WAITER_SWEEP` stalled:
    /// ~105 KB/s at a 1 MiB `MaxWriteSize`, ~840 KB/s at the 8 MiB one Windows
    /// and several NAS boxes negotiate. `SEND_TIMEOUT` is sized for 50 KB/s and
    /// 400 KB/s, so that window would have warned across the whole band between
    /// them — ordinary Wi-Fi, and exactly the false positive the collapse
    /// exists to remove. `stall_tolerance` closes it by deferring to the send
    /// deadline.
    #[tokio::test]
    async fn a_link_too_slow_to_finish_a_frame_between_sweeps_is_not_a_wedge() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let tolerance = stall_tolerance(Some(SEND_TIMEOUT));
        let start = Instant::now();

        // A 1 MiB frame crawling out at 60 KB/s: ~17 s each, so most sweeps see
        // no completion at all, and the send deadline tolerates every one of
        // them.
        let per_frame = Duration::from_millis(17_000);
        assert!(
            per_frame < SEND_TIMEOUT,
            "the premise: this link is inside what the send deadline accepts"
        );
        assert!(
            per_frame > STALE_WAITER_SWEEP,
            "the premise: and slower than one frame per sweep"
        );

        let mut completed = Duration::ZERO;
        let mut now = start;
        for _ in 0..12 {
            now += STALE_WAITER_SWEEP;
            while completed + per_frame <= now.saturating_duration_since(start) {
                completed += per_frame;
                conn.inner
                    .metrics
                    .wire_bytes_sent
                    .fetch_add(1_048_576, Ordering::Relaxed);
            }
            assert_eq!(
                conn.inner.observe_send_side(now, tolerance).state,
                SendSide::Draining,
                "a link the send deadline accepts must never read as a wedge \
                 (at {:?} into the transfer)",
                now.saturating_duration_since(start)
            );
        }
    }

    /// The writer counts slow sends rather than announcing them.
    ///
    /// Pins the split too: a frame that sat in the queue behind a slow one is a
    /// different diagnosis from a frame this socket was slow to accept, so the
    /// tally keeps both worst cases apart.
    #[test]
    fn the_writer_tallies_slow_sends_instead_of_logging_each_one() {
        let tally = SendTally::default();
        tally.record(
            1_048_576,
            Duration::from_secs(9),
            Duration::from_millis(200),
        );
        tally.record(1_048_576, Duration::from_millis(10), Duration::from_secs(6));
        tally.record(4_096, Duration::from_millis(1), Duration::from_millis(2));

        let activity = tally.drain();

        assert_eq!(activity.frames, 3);
        assert_eq!(activity.bytes, 2_101_248);
        assert_eq!(activity.slow_frames, 2, "the quick one is not slow");
        assert_eq!(activity.worst_queued, Duration::from_secs(9));
        assert_eq!(activity.worst_writing, Duration::from_secs(6));

        let drained_again = tally.drain();
        assert_eq!(
            drained_again.frames, 0,
            "each sweep reports its own interval, not a running total"
        );
    }

    /// A CANCEL has to carry the `AsyncId` the server assigned, so the client
    /// has to remember it from the one frame that ever states it.
    ///
    /// Without this a retired long poll cannot be retired: MS-SMB2 § 3.2.4.24
    /// says a cancel for a request that has been given an `AsyncId` must carry
    /// it, and one sent against the `MessageId` alone matches nothing — so the
    /// server would keep every subscription the refresh cycle walks away from.
    #[tokio::test]
    async fn an_interim_pending_response_records_the_async_id_a_cancel_needs() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let guard = conn
            .register_waiter(MessageId(5), Command::ChangeNotify)
            .unwrap();
        assert_eq!(guard.async_id(), None, "nothing has been told to us yet");

        let mut h = Header::new_request(Command::ChangeNotify);
        h.flags.set_response();
        h.flags.set_async();
        h.message_id = MessageId(5);
        h.status = NtStatus::PENDING;
        h.async_id = Some(0xFEED_FACE_DEAD_BEEF);
        let interim = pack_message(&h, &crate::msg::echo::EchoResponse);
        let action = prepare_sub_frame(&interim, false, &conn.inner)
            .expect("the interim response should be handled");
        assert!(
            matches!(action, SubFrameAction::Skip),
            "an interim response is not the answer, so nothing is routed"
        );

        assert_eq!(
            guard.async_id(),
            Some(0xFEED_FACE_DEAD_BEEF),
            "the interim STATUS_PENDING is the only frame that ever states the AsyncId"
        );
        assert_eq!(
            conn.outstanding_requests()[0].async_id,
            Some(0xFEED_FACE_DEAD_BEEF),
            "and a consumer driving send_cancel itself has to be able to read it"
        );
    }

    /// A caller the receiver task wakes runs before the receiver routes the
    /// rest of a download. On a multi-thread tokio runtime the woken task lands
    /// in the receiver's worker's LIFO slot, which no other worker may steal,
    /// so it runs only once the receiver yields. A socket that never drains
    /// (the mock always has the next frame ready, like a loaded client behind a
    /// fast link) never makes the receiver yield on its own: a side `stat` on a
    /// QNAP waited 333 ms after its answer was routed that way. Counted in
    /// frames, not milliseconds, so a slow CI box can't make it flaky.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_woken_caller_runs_before_the_receiver_routes_the_rest_of_a_download() {
        const BIG_FRAMES: usize = 200;
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        let mut stat = conn.register_waiter(MessageId(1), Command::Echo).unwrap();
        let counter = mock.clone();
        let caller = tokio::spawn(async move {
            stat.recv().await.expect("the ECHO is answered");
            counter.received_count()
        });
        // Registered but never awaited, the way a download's later chunks sit
        // routed and unclaimed: routing them wakes nobody.
        let _download: Vec<WaiterGuard> = (0..BIG_FRAMES as u64)
            .map(|i| {
                conn.register_waiter(MessageId(2 + i), Command::Read)
                    .unwrap()
            })
            .collect();
        // Let the caller park on its answer before any frame arrives.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut frames = vec![echo_response_granting(1, 1)];
        for i in 0..BIG_FRAMES as u64 {
            let mut read = crate::client::test_helpers::build_read_response(vec![0xAB; 64 * 1024]);
            read[24..32].copy_from_slice(&(2 + i).to_le_bytes());
            frames.push(read);
        }
        mock.queue_responses(frames);

        let routed_when_caller_ran = caller.await.unwrap();
        assert!(
            routed_when_caller_ran <= 3,
            "the caller ran only after the receiver had read {routed_when_caller_ran} of {} frames",
            BIG_FRAMES + 1
        );
    }

    #[tokio::test]
    async fn phase3_decrypt_failure_errors_waiter_not_hangs() {
        use crate::crypto::encryption::Cipher;

        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(10);

        // Activate encryption with a key that WON'T match what the
        // malformed frame was "encrypted" with — decrypt will fail auth.
        let enc_key = vec![0x42; 16];
        let dec_key = vec![0x99; 16]; // deliberately wrong decryption key
        conn.activate_encryption(enc_key, dec_key, Cipher::Aes128Gcm);

        // Register a waiter manually so we can inject a bad frame without
        // racing with a real send.
        let mut rx = conn.register_waiter(MessageId(4), Command::Read).unwrap();

        // Build a frame that starts with TRANSFORM_PROTOCOL_ID so the
        // receiver task takes the decrypt path, but whose ciphertext
        // is garbage that will fail the GCM auth tag check. We craft a
        // "valid-shape" transform header (52 bytes) plus ~64 bytes of
        // garbage ciphertext. The receiver task's decrypt_frame call
        // returns Err; currently it's log+continue (the bug).
        let mut frame = Vec::new();
        frame.extend_from_slice(&TRANSFORM_PROTOCOL_ID); // 0xFD 'S' 'M' 'B'
        frame.extend_from_slice(&[0u8; 16]); // signature
        frame.extend_from_slice(&[0u8; 16]); // nonce
        frame.extend_from_slice(&64u32.to_le_bytes()); // original_message_size
        frame.extend_from_slice(&0u16.to_le_bytes()); // reserved
        frame.extend_from_slice(&1u16.to_le_bytes()); // flags (Encrypted)
        frame.extend_from_slice(&0xDEADu64.to_le_bytes()); // session_id
                                                           // Garbage ciphertext — will fail GCM auth on decrypt.
        frame.extend_from_slice(&[0xAAu8; 64]);
        mock.queue_response(frame);

        // Await the waiter with a short timeout. If Phase 3's fix is in
        // place, the receiver task tears down on decrypt failure and the
        // waiter resolves with Err(Disconnected) quickly. Without the
        // fix, the receiver task `log+continue`s, the waiter hangs, and
        // the timeout fires (test fails).
        let result = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;

        assert!(
            result.is_ok(),
            "waiter hung forever on a decrypt-failed frame — Phase 3's silent-discard \
             fix must tear down the connection on unrecoverable frame errors and propagate \
             Err(Disconnected) to pending waiters. Instead the receiver task silently discards \
             the frame and the waiter never resolves. (P3.4 fixes this.)"
        );
        let waiter_result = result.unwrap();
        assert!(
            waiter_result.is_err(),
            "waiter should return an error on decrypt failure, not Ok"
        );
    }

    // ── CANCEL tests (pitfall #7) ────────────────────────────────────

    #[tokio::test]
    async fn a_cancel_stamped_with_another_generation_never_reaches_the_wire() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_session_id(SessionId(0x1234));

        // The generation the ids came from is not the one on the wire now, so
        // the request they name died with its session.
        let stale = conn.generation() + 1;
        conn.send_cancel(MessageId(7), None, stale).await.unwrap();

        assert_eq!(
            mock.sent_count(),
            0,
            "a CANCEL naming a dead session's MessageId must not be sent: on the \
             live connection that id belongs to somebody else, or to nobody"
        );
        assert_eq!(
            conn.metrics().explicit_cancels_sent,
            0,
            "and a cancel that was never sent must not be counted as one"
        );
    }

    #[tokio::test]
    async fn a_cancel_during_the_handshake_never_reaches_the_wire() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A revival wipes the session id and re-negotiates; `revivals` only
        // ticks once the whole thing has succeeded, so the generation still
        // matches while the handshake is in flight. This is the state a
        // reconnecting connection is in for the whole of NEGOTIATE and
        // SESSION_SETUP.
        conn.set_session_id(SessionId(0));

        let generation = conn.generation();
        conn.send_cancel(MessageId(1), None, generation)
            .await
            .unwrap();

        assert_eq!(
            mock.sent_count(),
            0,
            "there is no session to cancel anything on yet, and the server's \
             credit window here is exactly zero wide: a frame sent into it is \
             discarded in silence"
        );
    }

    #[tokio::test]
    async fn send_cancel_does_not_consume_credit_or_advance_message_id() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_next_message_id(10);
        conn.set_credits(5);

        conn.send_cancel(MessageId(7), None, conn.generation())
            .await
            .unwrap();

        // MessageId should NOT have advanced.
        assert_eq!(conn.next_message_id(), 10);
        // Credits should NOT have been consumed.
        assert_eq!(conn.credits(), 5);
    }

    #[tokio::test]
    async fn send_cancel_sync_uses_original_message_id() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_session_id(SessionId(0xAAAA));

        conn.send_cancel(MessageId(42), None, conn.generation())
            .await
            .unwrap();

        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let header = Header::unpack(&mut cursor).unwrap();

        assert_eq!(header.command, Command::Cancel);
        assert_eq!(header.message_id, MessageId(42));
        assert_eq!(header.credit_charge, CreditCharge(0));
        assert_eq!(header.credits, 0);
        assert_eq!(header.session_id, SessionId(0xAAAA));
        assert!(!header.flags.is_async());

        // Body should be CancelRequest: StructureSize=4, Reserved=0.
        assert_eq!(sent.len(), Header::SIZE + 4);
        let body_structure_size = u16::from_le_bytes(sent[64..66].try_into().unwrap());
        assert_eq!(body_structure_size, 4);
    }

    #[tokio::test]
    async fn send_cancel_async_sets_async_flag_and_async_id() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_session_id(SessionId(0xBBBB));

        let async_id = 0x1234_5678_9ABC_DEF0u64;
        conn.send_cancel(MessageId(99), Some(async_id), conn.generation())
            .await
            .unwrap();

        let sent = mock.sent_message(0).unwrap();
        let mut cursor = ReadCursor::new(&sent);
        let header = Header::unpack(&mut cursor).unwrap();

        assert_eq!(header.command, Command::Cancel);
        assert_eq!(header.message_id, MessageId(99));
        assert!(header.flags.is_async());
        assert_eq!(header.async_id, Some(async_id));
        assert_eq!(header.tree_id, None);
        assert_eq!(header.credit_charge, CreditCharge(0));
        assert_eq!(header.credits, 0);
    }

    #[tokio::test]
    async fn send_cancel_signs_message_when_signing_active() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        let key = vec![0xCC; 16];
        conn.activate_signing(key, SigningAlgorithm::HmacSha256);
        conn.set_session_id(SessionId(0xDDDD));

        conn.send_cancel(MessageId(50), None, conn.generation())
            .await
            .unwrap();

        let sent = mock.sent_message(0).unwrap();

        // Verify the signed flag is set.
        let flags = u32::from_le_bytes(sent[16..20].try_into().unwrap());
        assert!(flags & HeaderFlags::SIGNED != 0, "CANCEL should be signed");

        // Verify the signature is non-zero.
        let sig = &sent[48..64];
        assert_ne!(sig, &[0u8; 16], "signature should not be all zeros");
    }

    /// An outgoing CANCEL carries the CANCEL bit in its AES-GMAC nonce.
    ///
    /// MS-SMB2 § 3.1.4.1 puts a bit for "this message is an SMB2 CANCEL" in
    /// the nonce, so a cancel signed without it has a signature no
    /// GMAC-negotiating server will accept. And a rejected cancel is silent —
    /// a cancel has no success response — so the request simply stays
    /// outstanding while the client believes it let go of it. Verified against
    /// a QNAP TS-464 on 2026-08-03: with the bit missing, every CANCEL for a
    /// parked CHANGE_NOTIFY was refused and the watch stopped delivering
    /// events; with it set, the same watch survived repeated cancels and kept
    /// reporting changes.
    #[tokio::test]
    async fn a_cancel_is_signed_with_the_cancel_bit_in_its_gmac_nonce() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let key = vec![0xCC; 16];
        conn.activate_signing(key.clone(), SigningAlgorithm::AesGmac);
        conn.set_session_id(SessionId(0xDDDD));

        conn.send_cancel(MessageId(50), Some(0x77), conn.generation())
            .await
            .unwrap();
        let sent = mock.sent_message(0).unwrap();

        // Re-sign the very bytes that went out, both ways. Only one of them can
        // reproduce the signature on the wire.
        let mut with_bit = sent.clone();
        signing::sign_message(&mut with_bit, &key, SigningAlgorithm::AesGmac, 50, true).unwrap();
        let mut without_bit = sent.clone();
        signing::sign_message(&mut without_bit, &key, SigningAlgorithm::AesGmac, 50, false)
            .unwrap();

        assert_eq!(
            &sent[48..64],
            &with_bit[48..64],
            "the CANCEL has to be signed with the cancel nonce bit set"
        );
        assert_ne!(
            &sent[48..64],
            &without_bit[48..64],
            "a signature that matches the no-cancel-bit nonce is the bug this pins"
        );
    }

    /// And the receive side reads the same bit back.
    ///
    /// The one frame this reaches in practice is the error response a server
    /// sends when it rejects a CANCEL — the single frame that says the cancel
    /// did not take. Verifying it with the bit cleared turns that report into
    /// a `signature_failures` tick and a scary log line about a protocol
    /// anomaly, which is the opposite of what it is.
    #[tokio::test]
    async fn a_cancel_response_is_verified_with_the_cancel_bit() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        let key = vec![0xAB; 16];
        conn.activate_signing(key.clone(), SigningAlgorithm::AesGmac);
        let _guard = conn.register_waiter(MessageId(9), Command::Cancel).unwrap();

        let mut h = Header::new_request(Command::Cancel);
        h.flags.set_response();
        h.flags.set_signed();
        h.message_id = MessageId(9);
        h.status = NtStatus::INVALID_PARAMETER;
        let body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };

        let mut good = pack_message(&h, &body);
        signing::sign_message_as_server(&mut good, &key, SigningAlgorithm::AesGmac, 9, true)
            .unwrap();
        let action = prepare_sub_frame(&good, false, &conn.inner).unwrap();
        assert!(
            matches!(action, SubFrameAction::Route(_, Ok(_))),
            "a correctly signed CANCEL response must verify, got {action:?}"
        );
        assert_eq!(conn.metrics().signature_failures, 0);

        // The same frame signed with the bit cleared is what the old verifier
        // was effectively checking against, and it must NOT pass.
        let mut wrong = pack_message(&h, &body);
        signing::sign_message_as_server(&mut wrong, &key, SigningAlgorithm::AesGmac, 9, false)
            .unwrap();
        let action = prepare_sub_frame(&wrong, false, &conn.inner).unwrap();
        assert!(
            matches!(
                action,
                SubFrameAction::Route(
                    _,
                    Err(Error::SignatureInvalid {
                        reason: SignatureFailure::Mismatch,
                        ..
                    })
                )
            ),
            "the cancel bit has to be part of the check, got {action:?}"
        );
        assert_eq!(conn.metrics().signature_failures, 1);
    }

    /// Whether a response is signed is the server's claim, carried in the very
    /// bytes an on-path attacker controls. Clearing `SMB2_FLAGS_SIGNED` and
    /// rewriting the payload must not get a response past verification
    /// (MS-SMB2 § 3.2.5.1.3 verifies every response on a signed session, with
    /// no exception for an unset flag). Reported in #5 with a proxy that did
    /// exactly this to a READ.
    #[tokio::test]
    async fn an_unsigned_response_on_a_signed_session_is_rejected() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.activate_signing(vec![0xAB; 16], SigningAlgorithm::AesCmac);
        let _guard = conn.register_waiter(MessageId(7), Command::Echo).unwrap();

        let mut h = Header::new_request(Command::Echo);
        h.flags.set_response();
        h.message_id = MessageId(7);
        h.status = NtStatus::SUCCESS;
        let stripped = pack_message(&h, &crate::msg::echo::EchoResponse);

        let action = prepare_sub_frame(&stripped, false, &conn.inner).unwrap();
        assert!(
            matches!(
                action,
                SubFrameAction::Route(
                    MessageId(7),
                    Err(Error::SignatureInvalid {
                        command: Command::Echo,
                        message_id: MessageId(7),
                        reason: SignatureFailure::Missing,
                    })
                )
            ),
            "an unsigned response on a signed session must be refused as Missing, got {action:?}"
        );
        assert_eq!(conn.metrics().signature_failures, 1);
    }

    /// A response that carries a signature, just not the session key's, is
    /// refused the same way, and says the signature was wrong rather than
    /// absent. What an on-path rewrite that re-signs with a key of its own
    /// looks like, and what a server with a different session key sends.
    #[tokio::test]
    async fn a_wrongly_signed_response_on_a_signed_session_is_rejected() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.activate_signing(vec![0xAB; 16], SigningAlgorithm::AesCmac);
        let _guard = conn.register_waiter(MessageId(7), Command::Echo).unwrap();

        let mut h = Header::new_request(Command::Echo);
        h.flags.set_response();
        h.flags.set_signed();
        h.message_id = MessageId(7);
        h.status = NtStatus::SUCCESS;
        let mut forged = pack_message(&h, &crate::msg::echo::EchoResponse);
        signing::sign_message_as_server(
            &mut forged,
            &[0xCD; 16],
            SigningAlgorithm::AesCmac,
            7,
            false,
        )
        .unwrap();
        assert_ne!(
            &forged[48..64],
            &[0u8; 16],
            "the forgery carries a real signature"
        );

        let action = prepare_sub_frame(&forged, false, &conn.inner).unwrap();
        match action {
            SubFrameAction::Route(MessageId(7), Err(e)) => {
                assert!(
                    matches!(
                        e,
                        Error::SignatureInvalid {
                            command: Command::Echo,
                            message_id: MessageId(7),
                            reason: SignatureFailure::Mismatch,
                        }
                    ),
                    "got {e:?}"
                );
                assert_eq!(e.kind(), crate::ErrorKind::SignatureInvalid);
            }
            other => panic!("a wrongly signed response must be refused, got {other:?}"),
        }
        assert_eq!(conn.metrics().signature_failures, 1);
    }

    /// The two frames the spec exempts from verification still pass unsigned:
    /// an interim STATUS_PENDING (the final response carries the signature)
    /// and an unsolicited oplock break (MessageId 0xFFFF_FFFF_FFFF_FFFF).
    #[tokio::test]
    async fn pending_and_oplock_breaks_stay_exempt_from_signing() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.activate_signing(vec![0xAB; 16], SigningAlgorithm::AesCmac);
        let _guard = conn.register_waiter(MessageId(7), Command::Read).unwrap();

        let mut pending = Header::new_request(Command::Read);
        pending.flags.set_response();
        pending.message_id = MessageId(7);
        pending.status = NtStatus::PENDING;
        let body = crate::msg::header::ErrorResponse {
            error_context_count: 0,
            error_data: vec![],
        };
        let action = prepare_sub_frame(&pack_message(&pending, &body), false, &conn.inner).unwrap();
        assert!(matches!(action, SubFrameAction::Skip), "got {action:?}");

        let mut brk = Header::new_request(Command::OplockBreak);
        brk.flags.set_response();
        brk.message_id = MessageId::UNSOLICITED;
        brk.status = NtStatus::SUCCESS;
        let action = prepare_sub_frame(&pack_message(&brk, &body), false, &conn.inner).unwrap();
        assert!(matches!(action, SubFrameAction::Skip), "got {action:?}");

        assert_eq!(conn.metrics().signature_failures, 0);
    }

    // ── Encryption tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn no_encryption_when_not_activated() {
        use crate::msg::echo::EchoRequest;

        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_test_params(NegotiatedParams {
            dialect: Dialect::Smb3_1_1,
            max_read_size: 65536,
            max_write_size: 65536,
            max_transact_size: 65536,
            server_guid: Guid::ZERO,
            signing_required: false,
            capabilities: Capabilities::default(),
            gmac_negotiated: false,
            cipher: Some(Cipher::Aes128Gcm),
            compression_supported: false,
        });
        conn.set_session_id(SessionId(1));
        conn.set_credits(5);

        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            conn.execute(Command::Echo, &EchoRequest, None),
        )
        .await;

        // Without encryption activated, the sent bytes should start with
        // the normal SMB2 protocol ID (0xFE).
        let sent = mock.sent_message(0).unwrap();
        assert_eq!(
            sent[0], 0xFE,
            "without encryption, message must start with 0xFE"
        );
    }

    #[tokio::test]
    async fn activate_encryption_sets_state() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        assert!(!conn.should_encrypt());

        conn.activate_encryption(vec![0x42; 16], vec![0x42; 16], Cipher::Aes128Gcm);

        assert!(conn.should_encrypt());
    }

    // ── DFS flag tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn dfs_flag_set_for_registered_tree() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(256);

        let tree_id = TreeId(7);
        conn.register_dfs_tree(tree_id);

        use crate::msg::echo::EchoRequest;
        let body = EchoRequest;
        // Fire execute with a short timeout so the test doesn't block on a
        // response that never comes — we only care about the sent bytes.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            conn.execute_with_credits(Command::Echo, &body, Some(tree_id), CreditCharge(1)),
        )
        .await;
        let msg_bytes = mock.sent_message(0).expect("one send recorded");

        // Header flags are at bytes 16..20 (little-endian u32).
        let flags_raw = u32::from_le_bytes(msg_bytes[16..20].try_into().unwrap());
        assert_ne!(
            flags_raw & HeaderFlags::DFS_OPERATIONS,
            0,
            "DFS_OPERATIONS flag must be set for registered tree"
        );
    }

    #[tokio::test]
    async fn dfs_flag_not_set_for_unregistered_tree() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(256);

        use crate::msg::echo::EchoRequest;
        let body = EchoRequest;
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            conn.execute_with_credits(Command::Echo, &body, Some(TreeId(7)), CreditCharge(1)),
        )
        .await;
        let msg_bytes = mock.sent_message(0).expect("one send recorded");

        let flags_raw = u32::from_le_bytes(msg_bytes[16..20].try_into().unwrap());
        assert_eq!(
            flags_raw & HeaderFlags::DFS_OPERATIONS,
            0,
            "DFS_OPERATIONS flag must NOT be set for unregistered tree"
        );
    }

    #[tokio::test]
    async fn dfs_flag_cleared_after_deregister() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(256);

        let tree_id = TreeId(7);
        conn.register_dfs_tree(tree_id);
        conn.deregister_dfs_tree(tree_id);

        use crate::msg::echo::EchoRequest;
        let body = EchoRequest;
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            conn.execute_with_credits(Command::Echo, &body, Some(tree_id), CreditCharge(1)),
        )
        .await;
        let msg_bytes = mock.sent_message(0).expect("one send recorded");

        let flags_raw = u32::from_le_bytes(msg_bytes[16..20].try_into().unwrap());
        assert_eq!(
            flags_raw & HeaderFlags::DFS_OPERATIONS,
            0,
            "DFS_OPERATIONS flag must NOT be set after deregister"
        );
    }

    // ── Phase 3 A.1: Connection: Clone ───────────────────────────────

    /// Confirms clones share the same connection-wide state via `Arc<Inner>`.
    ///
    /// A clone is a fresh handle to the same actor, not a snapshot: in-flight
    /// waiters belong to the task that sent the request (`oneshot::Receiver`
    /// isn't `Clone`), while credits, session id, negotiated params, and
    /// crypto state are shared.
    #[tokio::test]
    async fn connection_is_cloneable_and_clones_share_state() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut original = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        // Mutate shared state on the original.
        original.set_credits(42);
        original.set_session_id(SessionId(0x1234_5678_9ABC_DEF0));
        original.set_next_message_id(100);

        // Clone and verify the clone sees the same shared state.
        let cloned = original.clone();
        assert_eq!(cloned.credits(), 42);
        assert_eq!(cloned.session_id(), SessionId(0x1234_5678_9ABC_DEF0));
        assert_eq!(cloned.next_message_id(), 100);
        assert_eq!(cloned.server_name(), "test-server");

        // Mutate via the clone and verify the original observes it too.
        cloned.inner.credits.set_available(7);
        assert_eq!(original.credits(), 7);

        // Phase 3 A.3 removed the caller-local `pending_fifo`; there is no
        // per-clone state anymore. Clones share `Arc<Inner>` exclusively.
    }

    // ── Phase 3 A.2: `execute` / `execute_with_credits` / `execute_compound` ──
    //
    // These tests exercise the additive concurrent-op API. All callers take
    // `&self`, so the orphan filter stays ENABLED (production behavior). Mock
    // responses hardcode the MessageIds that `execute` allocates, starting at 0
    // by default (or a specific `set_next_message_id` for multi-op tests).

    /// Build an ECHO response with a specific MessageId.
    fn build_echo_response_with_msg_id(msg_id: MessageId) -> Vec<u8> {
        let mut h = Header::new_request(Command::Echo);
        h.flags.set_response();
        h.credits = 10;
        h.message_id = msg_id;
        pack_message(&h, &crate::msg::echo::EchoResponse)
    }

    /// Queue a response AFTER the spawned task has sent its request (and
    /// thus registered its waiter). Using `multi_thread` so the receiver
    /// task can race the test task — catching any regression where the
    /// orphan filter silently drops the response.
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_returns_correct_frame_for_sent_request() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        conn.set_credits(512);

        // Spawn the execute first. `execute` allocates msg_id=0.
        let c = conn.clone();
        let handle = tokio::spawn(async move {
            c.execute(Command::Echo, &crate::msg::echo::EchoRequest, None)
                .await
        });

        // Wait for the send to land, then queue the response.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < 1 {
            if std::time::Instant::now() > deadline {
                panic!("execute task did not send its request in 5s");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        mock.queue_response(build_echo_response_with_msg_id(MessageId(0)));

        let frame = handle.await.unwrap().unwrap();

        assert_eq!(frame.header.command, Command::Echo);
        assert_eq!(frame.header.message_id, MessageId(0));
        assert!(frame.header.is_response());
        // Body should unpack as EchoResponse.
        let mut cursor = ReadCursor::new(&frame.body);
        crate::msg::echo::EchoResponse::unpack(&mut cursor).unwrap();

        mock.assert_fully_consumed();
    }

    /// N concurrent `execute` calls on clones of the same `Connection` all
    /// succeed — the receiver task's per-MessageId routing delivers each
    /// response to its own waiter. Needs a multi-threaded runtime so the
    /// receiver task can make progress while the task-under-test runs.
    ///
    /// Gotcha/Why: we MUST spawn the tasks first and wait for all N sends
    /// to register waiters before queuing responses. The receiver task
    /// starts reading `mock` immediately after `from_transport`. If we
    /// pre-queue all N responses, the receiver races the spawned tasks —
    /// any response whose msg_id hasn't had its waiter registered yet is
    /// dropped by the orphan filter (enabled by default in production
    /// mode), and the task hangs forever waiting for a response that's
    /// already been discarded. This ordering reflects the production
    /// reality: responses always arrive AFTER the client sent them.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_execute_on_one_connection_all_succeed() {
        const N: u64 = 20;

        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);

        // Spawn into a JoinSet so a timeout-side panic can introspect
        // which tasks haven't returned yet (`set.len()`). Plain
        // `Vec<JoinHandle>` moves into the await loop and we lose that.
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..N {
            let c = conn.clone();
            set.spawn(async move {
                c.execute(Command::Echo, &crate::msg::echo::EchoRequest, None)
                    .await
            });
        }

        // Wait until all N requests have been sent AND all waiters are
        // registered. Poll `sent_count` rather than hardcode a sleep.
        // `execute` registers the waiter BEFORE calling `sender.send`,
        // so `sent_count >= N` implies all N waiters are live.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < N as usize {
            if std::time::Instant::now() > deadline {
                panic!(
                    "tasks did not send all {} requests in 5s (got {})",
                    N,
                    mock.sent_count()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Queue N responses with msg_id=0 so auto-rewrite pairs each one
        // with the FIFO head of `pending_sent_msg_ids` — i.e. with whatever
        // msg_id was actually sent on the wire in that position. Hard-coding
        // 0..N here was the original bug: the multi_thread runtime can send
        // requests in any order, so the spawned tasks' allocated msg_ids
        // don't line up with 0..N in send order. Auto-rewrite would then
        // overwrite the i=0 response with FIFO[0] (say msg_id=5) and route
        // it to waiter 5, then keep i=5's hard-coded msg_id=5 and re-route
        // to the now-removed waiter 5 — the second arrival counted as
        // stray, and the original waiter-5's task hung forever waiting for
        // a response that was redirected to waiter 0 (which doesn't exist
        // in the FIFO ordering).
        for _ in 0..N {
            mock.queue_response(build_echo_response_with_msg_id(MessageId(0)));
        }

        // Drain the set with a hard timeout. The test has hung on multiple
        // CI runners (Ubuntu stable, Windows-2025 rust 1.85, macos-1.85
        // historically). Until the root cause is understood, a hang turns
        // into a 30 s clean failure instead of a multi-hour CI stall — and
        // the panic dumps the smoking-gun state so the next failure has
        // diagnostic value.
        let mut got_ids: Vec<u64> = Vec::with_capacity(N as usize);
        let drain = tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(joined) = set.join_next().await {
                let frame = joined.unwrap().unwrap();
                got_ids.push(frame.header.message_id.0);
            }
        })
        .await;

        if drain.is_err() {
            let pending = set.len();
            let m = conn.metrics();
            let waiters: Vec<u64> = {
                let g = conn.inner.waiters.lock().unwrap();
                g.keys().map(|mid| mid.0).collect()
            };
            let receiver_alive = !conn.inner.disconnected.load(Ordering::Acquire);
            set.abort_all();
            panic!(
                "concurrent_execute_on_one_connection_all_succeed exceeded 30 s.\n\
                 {pending} of {N} execute() futures still pending.\n\
                 receiver_alive={receiver_alive}\n\
                 mock.sent_count={sent}, mock.pending_responses={pending_resps}\n\
                 still-registered waiters (msg_ids): {waiters:?}\n\
                 counters: requests_sent={req_sent} \
                 responses_routed_ok={ok} responses_routed_err={err} \
                 responses_late_after_drop={late} responses_stray={stray} \
                 status_pending_loops={pl} unsolicited_notifications_received={uns}",
                sent = mock.sent_count(),
                pending_resps = mock.pending_responses(),
                req_sent = m.requests_sent,
                ok = m.responses_routed_ok,
                err = m.responses_routed_err,
                late = m.responses_late_after_drop,
                stray = m.responses_stray,
                pl = m.status_pending_loops,
                uns = m.unsolicited_notifications_received,
            );
        }

        got_ids.sort_unstable();
        assert_eq!(got_ids, (0..N).collect::<Vec<_>>());

        mock.assert_fully_consumed();
    }

    /// Dropping 2 of 5 execute futures before their responses arrive does
    /// NOT corrupt the other 3: the receiver task silently discards the
    /// frames routed to dropped oneshots, and the 3 surviving tasks see
    /// their own responses.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropped_execute_future_does_not_affect_others() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);

        // Spawn 5 tasks. Each allocates its own MessageId in submission
        // order: 0, 1, 2, 3, 4. To make allocation deterministic on the
        // multi_thread runtime, wait for each task's send to land before
        // spawning the next. `yield_now` alone isn't enough — on a
        // multi-worker runtime, the next spawn can race the previous
        // task's send and reorder msg_id allocation.
        let mut handles = Vec::new();
        for idx in 0..5 {
            let c = conn.clone();
            let h = tokio::spawn(async move {
                c.execute(Command::Echo, &crate::msg::echo::EchoRequest, None)
                    .await
            });
            handles.push(h);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while mock.sent_count() < idx + 1 {
                if std::time::Instant::now() > deadline {
                    panic!(
                        "task {} did not send its request in 5s (sent_count={})",
                        idx,
                        mock.sent_count()
                    );
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        // All 5 tasks have sent; waiters registered; msg_ids = 0..5.
        assert_eq!(mock.sent_count(), 5);

        // Abort tasks at indices 1 and 3 (msg_ids 1 and 3).
        handles[1].abort();
        handles[3].abort();

        // Now queue responses for all 5 msg_ids. The 2 aborted-task
        // responses route to closed oneshots and get silently discarded;
        // the 3 live tasks get their responses.
        for i in 0..5u64 {
            mock.queue_response(build_echo_response_with_msg_id(MessageId(i)));
        }

        // Collect results: tasks 0, 2, 4 should complete OK; tasks 1, 3
        // return JoinError (they were aborted).
        for (idx, h) in handles.into_iter().enumerate() {
            let res = h.await;
            if idx == 1 || idx == 3 {
                assert!(res.is_err(), "task {} should have been aborted", idx);
            } else {
                let frame = res.unwrap().unwrap();
                assert_eq!(frame.header.command, Command::Echo);
                assert_eq!(frame.header.message_id, MessageId(idx as u64));
            }
        }

        // All 5 responses were consumed by the receiver task (even the 2
        // whose waiters were dropped — the task reads every frame off the
        // mock regardless of waiter state).
        mock.assert_fully_consumed();
    }

    /// Compound partial failure: op 1 succeeds, op 2 returns an error
    /// status, op 3 succeeds. Outer result is `Ok(vec)`; inner is
    /// `[Ok, Ok(with-error-status), Ok]` — the per-sub-op error is
    /// encoded in `frame.header.status`, not in the inner `Result`,
    /// because the server returned a well-formed frame for every op.
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_compound_partial_failure_routes_correctly() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        // 3-op compound. `execute_compound` allocates msg_ids 0, 1, 2.
        let echo_ok_0 = build_echo_response_with_msg_id(MessageId(0));
        let mut err_hdr = Header::new_request(Command::Echo);
        err_hdr.flags.set_response();
        err_hdr.credits = 10;
        err_hdr.message_id = MessageId(1);
        err_hdr.status = NtStatus::OBJECT_NAME_NOT_FOUND;
        let err_body = pack_message(
            &err_hdr,
            &crate::msg::header::ErrorResponse {
                error_context_count: 0,
                error_data: vec![],
            },
        );
        let echo_ok_2 = build_echo_response_with_msg_id(MessageId(2));

        let compound_response = build_compound_response_frame(&[echo_ok_0, err_body, echo_ok_2]);

        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);

        let c = conn.clone();
        let handle = tokio::spawn(async move {
            let ops = [
                CompoundOp::new(Command::Echo, &crate::msg::echo::EchoRequest, None),
                CompoundOp::new(Command::Echo, &crate::msg::echo::EchoRequest, None),
                CompoundOp::new(Command::Echo, &crate::msg::echo::EchoRequest, None),
            ];
            c.execute_compound(&ops).await
        });

        // Wait for the compound request to land on the wire — one send
        // for all 3 sub-ops — then queue the response. All 3 waiters
        // are registered before the send, so the single compound-reply
        // frame routes to all of them.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < 1 {
            if std::time::Instant::now() > deadline {
                panic!("execute_compound did not send in 5s");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        mock.queue_response(compound_response);

        let results = handle.await.unwrap().unwrap();

        assert_eq!(results.len(), 3);
        let f0 = results[0].as_ref().expect("op 0 should be Ok");
        assert_eq!(f0.header.status, NtStatus::SUCCESS);
        assert_eq!(f0.header.message_id, MessageId(0));

        let f1 = results[1]
            .as_ref()
            .expect("op 1 still carries a Frame — error status in header");
        assert_eq!(f1.header.status, NtStatus::OBJECT_NAME_NOT_FOUND);
        assert_eq!(f1.header.message_id, MessageId(1));

        let f2 = results[2].as_ref().expect("op 2 should be Ok");
        assert_eq!(f2.header.status, NtStatus::SUCCESS);
        assert_eq!(f2.header.message_id, MessageId(2));

        mock.assert_fully_consumed();
    }

    /// Using a clone after the original is dropped: the `Arc<Inner>` keeps
    /// the receiver task alive. Specifically for `execute` (the A.1 test
    /// only exercised direct `sender.send`).
    #[tokio::test(flavor = "multi_thread")]
    async fn execute_on_clone_works_after_original_dropped() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();

        let original = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        original.set_credits(512);
        let cloned = original.clone();
        drop(original);

        let c = cloned.clone();
        let handle = tokio::spawn(async move {
            c.execute(Command::Echo, &crate::msg::echo::EchoRequest, None)
                .await
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < 1 {
            if std::time::Instant::now() > deadline {
                panic!("execute on clone did not send in 5s");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        mock.queue_response(build_echo_response_with_msg_id(MessageId(0)));

        let frame = handle.await.unwrap().unwrap();
        assert_eq!(frame.header.command, Command::Echo);
        assert_eq!(frame.header.message_id, MessageId(0));

        mock.assert_fully_consumed();
    }

    /// A clone'd `Connection` survives the original being dropped: the
    /// receiver task and transport sender are behind `Arc<Inner>`, so
    /// dropping the last Arc (not the first) is what aborts the task.
    #[tokio::test]
    async fn connection_is_cloneable_clone_outlives_original() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let original = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        original.set_credits(9);

        let cloned = original.clone();
        drop(original);

        // Shared state still accessible — the receiver task is still live
        // because the clone holds an `Arc<Inner>`.
        assert_eq!(cloned.credits(), 9);
        assert_eq!(cloned.server_name(), "test-server");

        // Send should still work: the writer task lives on Inner, so it
        // outlives the original handle along with everything else.
        cloned
            .inner
            .send_and_count(b"\x00\x00\x00\x10ignore-me", Command::Echo)
            .await
            .unwrap();
        assert_eq!(mock.sent_count(), 1);
    }
}

// ── The wedge: getting onto the wire must be bounded ─────────────────────
//
// A 2026-08-01 Cmdr wedge sat frozen for 40 minutes with ~700 requests
// registered as waiters and ZERO bytes on the socket: one send held the
// transport's write half forever and every later request queued behind it.
// Nothing fired, because every deadline the crate had bounds the wait for a
// RESPONSE, and these requests never reached the wire to be answered.
#[cfg(test)]
mod inbound_tests {
    use super::*;
    use crate::transport::MockTransport;

    /// A revival swaps the transport, and with it the counter the bytes were
    /// counted in. A rate computed across that must not see the total drop.
    #[tokio::test]
    async fn the_inbound_total_never_runs_backwards_across_a_new_transport() {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let conn = Connection::from_transport(
            Box::new(Arc::clone(&mock)),
            Box::new(Arc::clone(&mock)),
            "inbound-test",
        );
        conn.set_credits(64);
        let mut h = Header::new_request(Command::Echo);
        h.flags.set_response();
        h.credits = 1;
        mock.queue_response(pack_message(&h, &crate::msg::echo::EchoResponse));
        conn.execute(Command::Echo, &EchoRequest, None)
            .await
            .expect("echo");

        let before = conn.inbound().bytes_received;
        assert!(before > 0, "the answer was counted");
        assert_eq!(before, conn.metrics().wire_bytes_received);

        // What `spawn_plumbing` does with a revived transport's counter.
        conn.inner
            .adopt_receive_progress(Some(Arc::new(ReceiveProgress::new())));
        assert_eq!(
            conn.inbound().bytes_received,
            before,
            "the replaced transport's bytes are retired, not forgotten"
        );
    }
}

#[cfg(test)]
mod send_path_liveness_tests {
    use super::*;
    use crate::msg::echo::EchoRequest;
    use crate::transport::mock::MockTransport;
    use std::sync::atomic::AtomicUsize;

    /// A send half that accepts the first `stall_after` frames and then parks
    /// forever, exactly like the wedged socket: no error, no EOF, no progress.
    struct StallingSend {
        inner: Arc<MockTransport>,
        stall_after: usize,
        seen: AtomicUsize,
    }

    impl StallingSend {
        fn new(inner: Arc<MockTransport>, stall_after: usize) -> Self {
            Self {
                inner,
                stall_after,
                seen: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl TransportSend for StallingSend {
        async fn send(&self, data: &[u8]) -> Result<()> {
            if self.seen.fetch_add(1, Ordering::SeqCst) >= self.stall_after {
                std::future::pending::<()>().await;
            }
            self.inner.send(data).await
        }
    }

    /// A caller must not park forever because the wire is stuck. Without a
    /// bound on the send this test hangs — which is the production bug.
    #[tokio::test]
    async fn a_send_that_never_completes_fails_the_caller_instead_of_hanging() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(StallingSend::new(Arc::clone(&mock), 0)),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(64);
        conn.set_send_timeout(Some(Duration::from_millis(150)));

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            conn.execute(Command::Echo, &EchoRequest, None),
        )
        .await
        .expect("the caller must give up on its own, not wait out the test");

        assert!(
            matches!(result, Err(Error::SendTimeout { .. })),
            "expected SendTimeout, got {result:?}"
        );
    }

    /// The wedge's actual shape: one stuck frame, then unrelated traffic
    /// piling up behind it. Every queued caller has to come back with an
    /// error rather than joining the pile.
    #[tokio::test]
    async fn requests_queued_behind_a_stuck_send_all_come_back() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(StallingSend::new(Arc::clone(&mock), 1)),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(512);
        conn.set_send_timeout(Some(Duration::from_millis(150)));

        // One send gets through and parks the wire; nine queue behind it.
        let mut tasks = Vec::new();
        for _ in 0..10 {
            let c = conn.clone();
            tasks.push(tokio::spawn(async move {
                c.execute(Command::Echo, &EchoRequest, None).await
            }));
        }

        for task in tasks {
            let result = tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("no caller may outlive the send deadline")
                .expect("task panicked");
            assert!(
                result.is_err(),
                "a request behind a dead wire cannot report success"
            );
        }

        assert!(
            conn.outstanding_requests().is_empty(),
            "a connection torn down by a send timeout leaves no waiters behind"
        );
    }

    /// `outstanding_requests()` has to say which side of the wire a request is
    /// on. Reporting a never-sent request as "sent and unanswered" is what
    /// pointed three investigations at the server.
    #[tokio::test]
    async fn a_request_still_queued_is_not_reported_as_sent() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(StallingSend::new(Arc::clone(&mock), 0)),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(64);
        conn.set_send_timeout(None); // park; we want to observe the state

        let c = conn.clone();
        let task = tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        let outstanding = wait_for(|| {
            let o = conn.outstanding_requests();
            (!o.is_empty()).then_some(o)
        })
        .await;

        assert_eq!(outstanding.len(), 1);
        assert!(
            outstanding[0].sent_age.is_none(),
            "a request the transport has not accepted yet is not on the wire"
        );
        task.abort();
    }

    /// An aborted caller must deregister. Left behind, its waiter inflates the
    /// diagnostic AND makes `reserve_credits`' "is anything outstanding?"
    /// starvation check permanently true.
    #[tokio::test]
    async fn aborted_callers_do_not_leak_waiters() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(Arc::clone(&mock)),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(512);

        // Nothing is ever queued as a response, so every one of these is
        // outstanding until its caller goes away.
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let c = conn.clone();
            tasks.push(tokio::spawn(async move {
                c.execute(Command::Echo, &EchoRequest, None).await
            }));
        }
        wait_for(|| (conn.outstanding_requests().len() == 12).then_some(())).await;

        for task in &tasks {
            task.abort();
        }

        wait_for(|| conn.outstanding_requests().is_empty().then_some(())).await;
    }

    /// Same for a compound: every sub-op registers a waiter, so every sub-op
    /// has to deregister when the caller goes away.
    #[tokio::test]
    async fn an_aborted_compound_deregisters_every_sub_op() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(Arc::clone(&mock)),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(512);

        let c = conn.clone();
        let task = tokio::spawn(async move {
            c.execute_compound(&[
                CompoundOp::new(Command::Create, &EchoRequest, None),
                CompoundOp::new(Command::Close, &EchoRequest, None),
                CompoundOp::new(Command::Echo, &EchoRequest, None),
            ])
            .await
        });
        wait_for(|| (conn.outstanding_requests().len() == 3).then_some(())).await;

        task.abort();
        wait_for(|| conn.outstanding_requests().is_empty().then_some(())).await;
    }

    /// A link that stalls and recovers must NOT be cut off.
    ///
    /// The send bound exists to catch a socket that has stopped accepting
    /// writes for good. A lossy link that pauses and resumes is the far more
    /// common condition, and turning that into a failed transfer would trade
    /// a rare wedge for frequent spurious errors — the same trap the response
    /// deadline was designed around.
    #[tokio::test]
    async fn a_link_that_stalls_and_recovers_does_not_fail_the_transfer() {
        let mock = Arc::new(MockTransport::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let conn = Connection::from_transport(
            Box::new(GatedSend {
                inner: Arc::clone(&mock),
                gate: Arc::clone(&gate),
                stall_first: AtomicUsize::new(1),
            }),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(64);
        conn.set_send_timeout(Some(Duration::from_secs(30)));

        let c = conn.clone();
        let task = tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        // The first send is parked. Let it through after a pause well short
        // of the deadline, the way a recovering link would.
        tokio::time::sleep(Duration::from_millis(120)).await;
        gate.notify_waiters();

        wait_for(|| (mock.sent_count() == 1).then_some(())).await;
        assert!(
            !task.is_finished(),
            "a send that resumed inside its deadline must not have failed"
        );
        task.abort();
    }

    /// A send half that passes the first `break_after` frames through and then
    /// fails every write with `kind`, the way a socket the server reset
    /// mid-frame does (`EPIPE` on the next write, or `ECONNRESET`).
    struct BreakingSend {
        inner: Arc<MockTransport>,
        break_after: usize,
        kind: std::io::ErrorKind,
        seen: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TransportSend for BreakingSend {
        async fn send(&self, data: &[u8]) -> Result<()> {
            if self.seen.fetch_add(1, Ordering::SeqCst) >= self.break_after {
                return Err(Error::Io(std::io::Error::new(self.kind, "socket gone")));
            }
            self.inner.send(data).await
        }
    }

    /// The request whose own send broke the connection learns the same thing
    /// every other request on it does: the connection is gone. It used to get
    /// the raw `Error::Io`, which classifies as `ErrorKind::Io` ("maybe the
    /// connection is still fine") and isn't retryable, so a consumer treated
    /// the one file in flight as a terminal failure while every other waiter
    /// got `Disconnected` and recovered. The OS error stays as the source.
    #[tokio::test]
    async fn the_request_whose_send_broke_the_connection_reports_it_lost() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
        ] {
            let mock = Arc::new(MockTransport::new());
            let conn = Connection::from_transport(
                Box::new(BreakingSend {
                    inner: Arc::clone(&mock),
                    break_after: 1,
                    kind,
                    seen: AtomicUsize::new(0),
                }),
                Box::new(Arc::clone(&mock)),
                "test-server",
            );
            conn.set_credits(64);

            // One request reaches the wire and waits for an answer that never
            // comes; the next one's send hits the dead socket.
            let c = conn.clone();
            let bystander =
                tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });
            wait_for(|| (mock.sent_count() == 1).then_some(())).await;

            let err = conn
                .execute(Command::Echo, &EchoRequest, None)
                .await
                .expect_err("a send into a dead socket can't succeed");
            assert!(
                conn.is_disconnected(),
                "{kind:?}: the connection is already marked dead when the failing caller hears"
            );
            assert_eq!(
                err.kind(),
                crate::ErrorKind::ConnectionLost,
                "{kind:?}: got {err:?}"
            );
            assert!(err.is_retryable(), "{kind:?}: got {err:?}");
            let source = std::error::Error::source(&err)
                .and_then(|s| s.downcast_ref::<std::io::Error>())
                .unwrap_or_else(|| panic!("{kind:?}: the OS error is kept as the source: {err:?}"));
            assert_eq!(source.kind(), kind);

            let other = tokio::time::timeout(Duration::from_secs(5), bystander)
                .await
                .expect("the other waiter hears at once")
                .expect("task panicked")
                .expect_err("its connection died under it");
            assert_eq!(
                other.kind(),
                crate::ErrorKind::ConnectionLost,
                "got {other:?}"
            );
        }
    }

    /// A request queued from `Drop` that finds the writer's queue full sends
    /// nothing and spends no message id. An id allocated and never sent is a
    /// hole in the server's sequence window, which Samba can't advance past,
    /// so on a live connection it would stall every request after it.
    #[tokio::test]
    async fn queueing_without_waiting_on_a_full_queue_spends_no_message_id() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(StallingSend::new(Arc::clone(&mock), 0)),
            Box::new(Arc::clone(&mock)),
            "test-server",
        );
        conn.set_credits(1024);
        conn.set_send_timeout(None);

        // One frame parked in the writer, the queue full behind it, and a few
        // callers waiting for room.
        let callers = WRITE_QUEUE_DEPTH + 8;
        let tasks: Vec<_> = (0..callers)
            .map(|_| {
                let c = conn.clone();
                tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await })
            })
            .collect();
        wait_for(|| (conn.send_queue_depth() == callers).then_some(())).await;

        let next_id = conn.inner.next_message_id.load(Ordering::SeqCst);
        let queued = conn
            .dispatch_without_waiting(Command::Echo, &EchoRequest, None)
            .expect("nothing about the connection is wrong");
        assert!(queued.is_none(), "a full queue declines");
        assert_eq!(
            conn.inner.next_message_id.load(Ordering::SeqCst),
            next_id,
            "and allocates no message id"
        );
        for task in tasks {
            task.abort();
        }
    }

    /// A send half that parks the first `stall_first` frames until released.
    struct GatedSend {
        inner: Arc<MockTransport>,
        gate: Arc<tokio::sync::Notify>,
        stall_first: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TransportSend for GatedSend {
        // Rust 1.99 renamed `fetch_update` to `try_update`, which the 1.87 MSRV lacks.
        #[allow(deprecated)]
        async fn send(&self, data: &[u8]) -> Result<()> {
            if self
                .stall_first
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                self.gate.notified().await;
            }
            self.inner.send(data).await
        }
    }

    /// Poll `f` until it yields a value, panicking rather than hanging if it
    /// never does. (House rule: no bare sleeps or open-ended poll loops.)
    async fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "condition never became true within 5s"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Answer an oplock break so the client that triggered it isn't left waiting.
///
/// A batch oplock is the price of a durable handle (MS-SMB2 § 3.3.5.9.10): the
/// server will not promise to keep an open alive across a disconnect without
/// one. The cost lands on *other* clients — while we hold it, anyone else
/// opening the file waits for the server's break timeout (~35 s on Samba and
/// Windows) unless we answer. So we answer immediately and give the oplock up.
///
/// Giving it up costs the handle its durability (MS-SMB2 § 3.3.4.6), which is
/// the right trade: a resumable transfer is worth less than not freezing
/// somebody else's file operation for half a minute. The loss surfaces
/// honestly at reclaim time as [`crate::error::DurableLoss::Expired`], and the
/// transfer restarts.
///
/// Sent on its own task and never awaited: the receiver loop must get back to
/// reading, and nothing downstream depends on the acknowledgment's response.
fn acknowledge_oplock_break(
    inner: &Arc<Inner>,
    brk: crate::msg::oplock_break::OplockBreak,
    tree_id: TreeId,
) {
    use crate::msg::oplock_break::OplockBreak;

    debug!(
        "recv: oplock break to {:?} on file_id={:?}; acknowledging so the other \
         client isn't left waiting out the server's break timeout",
        brk.oplock_level, brk.file_id
    );
    // The handle keeps working; it just stops being resumable.
    inner.oplock_trees.lock().unwrap().remove(&brk.file_id);

    let conn = Connection {
        inner: Arc::clone(inner),
        pin: None,
    };
    rt::spawn(async move {
        let ack = OplockBreak {
            // Down to none: we only ever took the oplock to get durability,
            // and durability is already gone the moment a break arrives.
            oplock_level: OplockLevel::None,
            file_id: brk.file_id,
        };
        match conn
            .execute(Command::OplockBreak, &ack, Some(tree_id))
            .await
        {
            Ok(_) => trace!("oplock break acknowledged for file_id={:?}", brk.file_id),
            // Nothing to recover: the break is the server's business and the
            // handle still works. Worth a line because a server that rejects
            // the acknowledgment will hold the other client anyway.
            Err(e) => debug!(
                "oplock break acknowledgment for file_id={:?} did not land: {e}",
                brk.file_id
            ),
        }
    })
    .detach();
}
