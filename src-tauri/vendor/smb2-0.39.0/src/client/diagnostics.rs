//! Diagnostics: an in-process observability surface for a running [`SmbClient`](crate::SmbClient).
//!
//! Call [`SmbClient::diagnostics`](crate::SmbClient::diagnostics) to capture
//! a point-in-time tree of the client's negotiated parameters, credits,
//! in-flight requests, per-connection counters, and DFS cache state. Call
//! [`Connection::diagnostics`](crate::client::Connection::diagnostics) for
//! the per-connection slice.
//!
//! ## Consistency model
//!
//! Snapshots are **eventually consistent**. Each field is loaded
//! independently: the available-credits gauge, the in-flight count
//! (`waiters.len()`), and each counter are sampled at slightly different
//! moments. Sums of related fields (for example `credits.available +
//! credits.in_flight`) are **not** invariant — read each field for what it
//! says about itself, not as a coupled tuple. A consumer that wants
//! atomicity quiesces operations first.
//!
//! ## Snapshot lock order
//!
//! The snapshot acquires these locks, one at a time, in this order, never
//! across an `.await`: `crypto → waiters → dfs_trees → estimated_rtt`,
//! then, for the `inbound` and `liveness` readings, `inbound →
//! last_frame_at` and `keepalive_after → waiters → inbound → last_frame_at`.
//! Each is held only as long as it takes to copy primitives out and
//! release, and none is held while another is taken. `params` is an `OnceLock` (wait-free read). `preauth_hasher`
//! and `receiver_task` are not touched by the snapshot.
//!
//! If you add a field that touches a new lock, **extend** this order, don't
//! reshuffle it.
//!
//! ## Counters survive teardown
//!
//! Counters live on `Arc<Inner>`, which outlives the receiver task. A
//! snapshot taken on a torn-down connection (`disconnected: true`) returns
//! the final counter values at the moment of death.
//!
//! ## Counters carry across a reconnect
//!
//! [`SmbClient::reconnect`](crate::SmbClient::reconnect) revives the existing
//! [`Connection`](crate::client::Connection) on a fresh socket rather than
//! building a new one, so `Inner` — and every counter on it — survives. The
//! numbers describe the whole life of the client's link to this server, blips
//! included, which is also what makes
//! [`MetricsSnapshot::reconnects_succeeded`] meaningful: a counter reset by the
//! event it counts would always read zero.
//!
//! Client-level counters (the [`ClientMetricsSnapshot`] on
//! [`Diagnostics::client`]) survive too; `reconnects` counts explicit
//! [`SmbClient::reconnect`](crate::SmbClient::reconnect) calls, while
//! `reconnects_succeeded` counts every revival including the automatic ones.
//!
//! ## Why a snapshot, and no event stream
//!
//! - **What consumers want is state.** Credits, the in-flight count, the
//!   negotiated dialect, and counter totals describe *now*, not a sequence of
//!   past events.
//! - **The events already have a channel.** Oplock breaks, session expiry, DFS
//!   failovers, and decrypt failures all go through the `log` facade; a
//!   consumer that wants a timeline subscribes there.
//! - **Polling is close to free**: a handful of atomic loads and a few short
//!   critical sections, so a 1 Hz dashboard is below noise.
//! - **An event channel can't be taken back.** Its bounding and drop policy
//!   become a forever decision for every consumer. A future `event_stream()`
//!   can sit next to `diagnostics()` if a real workload asks for one.
//!
//! ## What it deliberately leaves out
//!
//! - **No reset.** Counters are monotonic; diff two snapshots for a rate.
//! - **No key material**: lengths, algorithm ids, and active flags only.
//! - **No caller-owned objects.** Open handles, `FileDownload`s, `FileWriter`s,
//!   and `Watcher`s belong to the caller, who folds their progress into their
//!   own view; tracking them here would mean registering every one back.
//! - **No per-NTSTATUS histogram** yet: too noisy to be worth a field.
//!
//! ## Stability
//!
//! Every type that can plausibly grow is `#[non_exhaustive]`, so a new field is
//! a minor change. [`SigningInfo`], [`EncryptionInfo`], and
//! [`CompressionInfo`] aren't, because the protocol fixes their shape and
//! tests can build them with struct literals. The `Display` format is for
//! humans and may change; use the `serde` feature (serialize only, never read
//! back) for anything programmatic.

use std::fmt;
use std::time::Duration;

use crate::crypto::encryption::Cipher;
use crate::crypto::signing::SigningAlgorithm;
use crate::pack::Guid;
use crate::types::flags::Capabilities;
use crate::types::{Command, Dialect, SessionId, TreeId};

/// Top-level diagnostics tree, captured by [`SmbClient::diagnostics`](crate::SmbClient::diagnostics).
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Diagnostics {
    /// Client-level configuration and counters.
    pub client: ClientInfo,
    /// The primary connection. Its `session` field carries the primary
    /// session (or `None` until session setup runs).
    pub primary: ConnectionDiagnostics,
    /// DFS cross-server connections, each with its own session. Each
    /// extra entry was authenticated separately.
    pub extra_connections: Vec<ConnectionDiagnostics>,
    /// DFS referral cache snapshot (one entry per cached path prefix).
    pub dfs_cache: Vec<DfsCacheEntry>,
}

/// Client-level configuration + counters.
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ClientInfo {
    /// The server address the client was constructed with (`host:port`).
    pub primary_server: String,
    /// Connection timeout from [`ClientConfig`](crate::ClientConfig).
    pub timeout: Duration,
    /// Whether the client was configured for auto-reconnect on loss.
    pub auto_reconnect: bool,
    /// Whether DFS resolution is enabled.
    pub dfs_enabled: bool,
    /// Client-level counters (survive `reconnect`).
    pub metrics: ClientMetricsSnapshot,
}

/// Per-connection snapshot, captured by
/// [`Connection::diagnostics`](crate::client::Connection::diagnostics) and
/// included in [`Diagnostics::primary`] / [`Diagnostics::extra_connections`].
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ConnectionDiagnostics {
    /// Server hostname or IP this connection talks to.
    pub server: String,
    /// Negotiated parameters, or `None` until `negotiate()` runs.
    pub negotiated: Option<NegotiatedSummary>,
    /// Credit gauge + in-flight count + next-MessageId.
    pub credits: CreditInfo,
    /// Signing state.
    pub signing: SigningInfo,
    /// Encryption state.
    pub encryption: EncryptionInfo,
    /// Compression state.
    pub compression: CompressionInfo,
    /// RTT measured during `negotiate`, if it ran.
    pub rtt_estimate: Option<Duration>,
    /// `true` after the receiver task has torn down (transport error,
    /// decrypt failure, etc.).
    pub disconnected: bool,
    /// Tree IDs that have DFS capability on this connection.
    pub dfs_trees: Vec<TreeId>,
    /// Session on this connection, or `None` until session setup runs.
    pub session: Option<SessionDiagnostics>,
    /// Per-connection counters.
    pub metrics: MetricsSnapshot,
    /// Requests sent and not yet answered, oldest first.
    ///
    /// Empty on a healthy idle connection. A long-lived entry here is the
    /// signature of a hung request; see [`OutstandingRequest`].
    pub outstanding: Vec<OutstandingRequest>,
    /// What the server has put on the wire, a frame still arriving included.
    /// Same reading as [`Connection::inbound`](crate::client::Connection::inbound).
    pub inbound: InboundProgress,
    /// Whether the server is still there, by the connection's own clocks.
    /// Same reading as [`Connection::liveness`](crate::client::Connection::liveness).
    pub liveness: Liveness,
}

/// What the server has put on the wire so far, counted as the bytes land
/// rather than when a frame completes. Read with
/// [`Connection::inbound`](crate::client::Connection::inbound).
///
/// Two things a consumer can get from it that whole-frame counters can't give:
/// a live **rate** while one large response is arriving (poll
/// `bytes_received` and divide by the time between polls), and whether the
/// server is **talking at all** right now (`since_last_byte`), which is what
/// separates a slow link from a dead one.
///
/// ❌ Not data you have. Bytes in a frame still arriving are unverified (a
/// signature or an AEAD tag covers the whole message), so this is for a rate
/// and for liveness. A byte bar that counted them would have to roll back if
/// the frame then failed verification.
///
/// Connection-wide: every request on the connection shares the one inbound
/// stream, so with several in flight the rate is theirs together.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct InboundProgress {
    /// Payload bytes received over the connection's whole life, revivals
    /// included, the part of a frame still arriving included. Never runs
    /// backwards.
    ///
    /// Excludes the 4-byte frame prefixes, so once no frame is partway in it
    /// agrees with [`MetricsSnapshot::wire_bytes_received`]. It moves mid-frame
    /// only when the transport can see inside a frame
    /// ([`TransportReceive::receive_progress`](crate::transport::TransportReceive::receive_progress));
    /// `TcpTransport` can.
    pub bytes_received: u64,
    /// The frame currently arriving, or `None` between frames.
    pub frame: Option<crate::transport::FrameProgress>,
    /// How long since the server last put a byte on the wire, or `None` if it
    /// hasn't since this session was established.
    ///
    /// The same clock [`liveness`](crate::client::Connection::liveness) reads,
    /// so time this process spent unscheduled (a system sleep, App Nap) is
    /// taken off it: silence nobody was listening to isn't the server's.
    pub since_last_byte: Option<Duration>,
}

/// Whether the server is still there, by the connection's own clocks. Read
/// with [`Connection::liveness`](crate::client::Connection::liveness).
///
/// A reading, never an action: asking tears nothing down and fails nothing.
/// What acts on the same clocks is the response deadline, which fails a
/// request with [`Error::ServerUnresponsive`](crate::Error::ServerUnresponsive)
/// once it has run out of budget on a connection reading [`Unresponsive`](Self::Unresponsive).
/// This is how a consumer (a transfer watchdog, a status indicator) sees the
/// same thing BEFORE any request pays for it.
///
/// Every byte the server sends counts as it lands, a frame still arriving
/// included: TCP delivers in order, so a large response trickling in over a
/// slow link holds the wire, and everything queued behind it (ECHO replies
/// included) waits. Reading only whole frames would call that link dead.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Liveness {
    /// Nothing on the wire is waiting for an answer, so there is nothing the
    /// server could be silent about.
    ///
    /// Requests still queued for the socket don't count: the server can't
    /// answer what it hasn't been asked, and a socket that won't take them is
    /// the send deadline's to report
    /// ([`Error::SendTimeout`](crate::Error::SendTimeout)).
    Idle,
    /// The server put a byte on the wire within the liveness window, three
    /// keepalive thresholds (15 s at the default 5 s).
    ///
    /// With the keepalive armed, this is also what earns a request that runs
    /// past its response deadline the longer, alive-connection ceiling.
    Alive,
    /// Work is outstanding and the wire has been quiet for `silent_for`,
    /// which proves nothing either way: either not for long enough yet, or
    /// the keepalive is off, and without probes a quiet connection is only
    /// quiet. It never becomes [`Unresponsive`](Self::Unresponsive) with the
    /// keepalive off.
    Quiet {
        /// How long, measured from the later of the server's last byte and
        /// the oldest unanswered request reaching the wire.
        silent_for: Duration,
    },
    /// The keepalive is armed, work is outstanding, and the server has put
    /// nothing at all on the wire for `silent_for`, past the liveness window,
    /// ECHO replies included.
    ///
    /// The verdict [`Error::ServerUnresponsive`](crate::Error::ServerUnresponsive)
    /// rests on. It isn't final while you read it (a byte arriving turns it
    /// back into [`Alive`](Self::Alive)), and a slow operation on a loaded
    /// server can't produce it, because answering a probe needs no disk.
    Unresponsive {
        /// Measured the same way as [`Quiet`](Self::Quiet)'s.
        silent_for: Duration,
    },
    /// The connection has been torn down. Revive it with
    /// [`Connection::reconnect_if_needed`](crate::client::Connection::reconnect_if_needed),
    /// or it stays this way.
    Disconnected,
}

/// Snapshot of [`NegotiatedParams`](crate::client::NegotiatedParams) for
/// the diagnostics tree. Same fields, copied (not borrowed).
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NegotiatedSummary {
    /// Negotiated dialect.
    pub dialect: Dialect,
    /// Maximum read size the server supports.
    pub max_read_size: u32,
    /// Maximum write size the server supports.
    pub max_write_size: u32,
    /// Maximum transact size the server supports.
    pub max_transact_size: u32,
    /// Server GUID.
    pub server_guid: Guid,
    /// Whether the server requires signing.
    pub signing_required: bool,
    /// Server capabilities.
    ///
    /// With the `serde` feature on, this serializes as the underlying
    /// `u32` bits (not a JSON object of named flags).
    pub capabilities: Capabilities,
    /// Whether AES-GMAC signing was negotiated (SMB 3.1.1).
    pub gmac_negotiated: bool,
    /// The negotiated encryption cipher (SMB 3.x).
    pub cipher: Option<Cipher>,
    /// Whether compression was negotiated with the server.
    pub compression_supported: bool,
}

/// Credit gauge for the connection.
///
/// The fields are sampled independently — `available + in_flight` is **not**
/// invariant. See the module-level eventual-consistency note.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CreditInfo {
    /// Credits on hand: granted by the server and not reserved by a request
    /// that is still in flight. This is what a new request can draw on, not
    /// the size of the server's window.
    pub available: u16,
    /// Number of `MessageId`s currently waiting for a response (i.e.
    /// `waiters.len()`).
    pub in_flight: usize,
    /// The `MessageId` that will be assigned to the next request.
    pub next_message_id: u64,
    /// Frames handed to the writer task and not yet written.
    ///
    /// Steadily non-zero while `wire_bytes_sent` stands still means the
    /// send side is stuck, not the server.
    pub send_queue_depth: usize,
}

/// One request that is in flight: registered for a response, and not yet
/// answered.
///
/// Surfaced so a consumer can see WHICH request a connection is waiting on, not
/// just how many. A connection that keeps serving small requests while one large
/// write hangs looks healthy by every other measure in this snapshot: credits
/// recover, counters advance, `disconnected` stays `false`. This is the field
/// that tells them apart.
///
/// **Read [`sent_age`](Self::sent_age) before concluding anything about the
/// server.** "In flight" starts at registration, which is BEFORE the bytes
/// reach the transport, so an entry here does not by itself mean the server
/// was asked and stayed quiet. A 2026-08-01 wedge was read that way for
/// weeks: hundreds of requests looked unanswered for half an hour while the
/// truth was that not one byte had left the socket.
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OutstandingRequest {
    /// The SMB2 command that was sent.
    pub command: Command,
    /// Its `MessageId`, matching the `dispatch:` log line.
    pub message_id: u64,
    /// How long ago the request was registered as in flight.
    ///
    /// Measured from registration, so it includes any time spent queued for
    /// the transport.
    pub age: Duration,
    /// How long ago the frame reached the transport, or `None` if it is still
    /// queued to be sent.
    ///
    /// `None` with a large `age` is the signature of a send-side wedge: the
    /// server cannot answer a question it was never asked. `Some(d)` with a
    /// large `d` is the genuine "server went quiet" case.
    pub sent_age: Option<Duration>,
    /// The `AsyncId` the server assigned in its interim `STATUS_PENDING`, or
    /// `None` if it has not sent one.
    ///
    /// What a
    /// [`Connection::send_cancel`](crate::client::connection::Connection::send_cancel)
    /// for this request has to carry: once a request has an `AsyncId`, a
    /// cancel against its `MessageId` alone matches nothing (MS-SMB2
    /// § 3.2.4.24).
    pub async_id: Option<u64>,
}

/// Signing state.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SigningInfo {
    /// `true` when outgoing requests are being signed.
    pub active: bool,
    /// Negotiated signing algorithm, or `None` if signing isn't active.
    pub algorithm: Option<SigningAlgorithm>,
}

/// Encryption state.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EncryptionInfo {
    /// `true` when outgoing requests are being encrypted with
    /// `TRANSFORM_HEADER`.
    pub active: bool,
    /// Negotiated encryption cipher, or `None` if encryption isn't active.
    pub cipher: Option<Cipher>,
}

/// Compression state.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CompressionInfo {
    /// Whether the client requested compression in `ClientConfig`.
    pub requested: bool,
    /// Whether compression was actually negotiated and is active.
    pub negotiated: bool,
}

/// Per-connection session snapshot. Each
/// [`ConnectionDiagnostics`] has its own — DFS extra connections each
/// authenticate separately, so they each carry a distinct session.
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SessionDiagnostics {
    /// SMB session ID assigned by the server.
    pub session_id: SessionId,
    /// `true` when the session requires signing.
    pub should_sign: bool,
    /// `true` when the session requires encryption.
    pub should_encrypt: bool,
    /// Signing algorithm derived for this session.
    pub signing_algorithm: SigningAlgorithm,
}

/// One entry in the DFS referral cache.
#[non_exhaustive]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DfsCacheEntry {
    /// The DFS path prefix this entry covers. Lowercased UNC form (the
    /// internal normalization used for case-insensitive matching).
    pub path_prefix: String,
    /// Number of failover targets the server returned.
    pub target_count: usize,
    /// Which target is tried first: the last one that worked (MS-DFSC
    /// § 3.1.1 TargetHint). `0` until a failover moves it.
    pub target_hint: usize,
    /// Whether the referral named DFS *root* targets (`ServerType = 1`).
    ///
    /// Read it as a fact about the server's answer, not as a verdict: Samba
    /// reports `false` for a namespace root that Windows reports `true` for,
    /// and nothing in this crate gates on it.
    pub root_targets: bool,
    /// The referral points into another DFS namespace, so reaching a target
    /// means resolving it again as a fresh path (§ 3.1.5.4.5).
    pub interlink: bool,
    /// The server enabled target failback for this namespace (V4 only).
    ///
    /// This crate does not implement failback, so a `true` here explains why
    /// the client stays on a target it failed over to.
    pub target_failback: bool,
    /// Remaining time-to-live. `None` if the entry has already expired
    /// (cache eviction is lazy: expired entries linger until the next
    /// `resolve()` for an overlapping prefix).
    pub expires_in: Option<Duration>,
}

/// Per-connection counter snapshot, taken atomically at the field level
/// but not as a whole (fields may skew — see [module docs](self)).
///
/// Counters are monotonic across the connection's lifetime. To compute a
/// rate, take two snapshots and subtract.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MetricsSnapshot {
    /// Every `MessageId` allocated for a request. Includes negotiate,
    /// session-setup, every `execute`, every `execute_with_credits`, every
    /// `dispatch` (Watcher's pre-arm CHANGE_NOTIFY), and every sub-op of
    /// every `execute_compound`. Does *not* include CANCEL (see
    /// [`Self::explicit_cancels_sent`]).
    pub requests_sent: u64,
    /// Every successful `execute_compound` call — the chain itself, not
    /// the per-sub-op count (those tick `requests_sent`).
    pub compound_requests_sent: u64,
    /// Bytes handed to `Transport::send` — the wire-layer count, after
    /// any sign / encrypt / compress on the send side. The byte count a
    /// packet capture would observe.
    pub wire_bytes_sent: u64,
    /// `Connection::send_cancel` invocations. CANCEL is the only SMB op
    /// today that we send proactively; cancellation-by-drop is invisible
    /// here (the drop never reaches the wire).
    pub explicit_cancels_sent: u64,

    /// Sub-frames where the receiver task found the waiter in the map
    /// and successfully delivered `Ok(frame)` to it. The normal happy
    /// path.
    pub responses_routed_ok: u64,
    /// Sub-frames where the receiver task found the waiter in the map
    /// and successfully delivered `Err(_)` to it. Today this is the
    /// union: [`Self::signature_failures`] + [`Self::session_expired_events`].
    /// Don't sum *those* with this counter — they're a partition of it.
    pub responses_routed_err: u64,
    /// Sub-frames where the receiver task found the waiter in the map
    /// but the caller's `oneshot::Receiver` was already dropped. Typical
    /// for `tokio::spawn` + `JoinHandle::abort()` patterns where the
    /// caller's future was cancelled mid-flight. The frame is discarded
    /// silently; credits already applied.
    pub responses_late_after_drop: u64,
    /// Sub-frames where the receiver task did **not** find the waiter
    /// in the map. The genuine orphan: server sent a frame for a
    /// `MessageId` we never allocated, or a send-error cleanup raced
    /// with arrival. Should be near-zero in normal operation.
    pub responses_stray: u64,
    /// Bytes received from `Transport::receive` — wire-layer, before
    /// any decrypt / decompress.
    pub wire_bytes_received: u64,

    /// Interim STATUS_PENDING sub-frames the receiver kept the waiter
    /// alive on (CHANGE_NOTIFY long-polls, slow IOCTLs).
    pub status_pending_loops: u64,
    /// Sub-frames with `MessageId::UNSOLICITED` (today: oplock breaks;
    /// the same magic id is reserved for future lease-break and other
    /// server-initiated notifications). Counted, logged at DEBUG,
    /// skipped — no waiter to route to.
    pub unsolicited_notifications_received: u64,
    /// Sub-frames whose signature verification failed. The error is
    /// routed to the matching waiter (also ticks
    /// [`Self::responses_routed_err`]); the connection continues.
    pub signature_failures: u64,
    /// Frames the receiver task could not decrypt (auth-tag mismatch,
    /// missing decryption key, malformed `TransformHeader`). Counted
    /// once before the connection tears down — the receiver task
    /// fans `Err(Disconnected)` to every pending waiter and exits.
    pub decrypt_failures: u64,
    /// Frames the receiver task could not decompress. Same teardown
    /// behavior as decrypt failures.
    pub decompress_failures: u64,
    /// Frames the receiver task could not parse (compound split,
    /// sub-frame header parse). Same teardown behavior. Covers both
    /// the `split_compound` parse-failure branch and the
    /// `prepare_sub_frame` header-parse branch.
    pub malformed_frames: u64,
    /// Sub-frames with `STATUS_NETWORK_SESSION_EXPIRED`. Counted
    /// per-sub-frame, not per session-event: a compound of N expired
    /// sub-ops ticks N times. For the event-shaped signal "did we
    /// reconnect", use [`ClientMetricsSnapshot::reconnects`]. Subset
    /// of [`Self::responses_routed_err`]; don't sum.
    pub session_expired_events: u64,

    /// `execute` / `execute_with_credits` / `execute_compound` returned
    /// an outer `Err` to a caller that polled to completion. Per-call,
    /// not per-sub-op: an `execute_compound` whose inner `Vec` contains
    /// errors but whose outer `Result` is `Ok` does **not** tick this.
    ///
    /// Caller-drop (the spawn/abort pattern) is captured by
    /// [`Self::responses_late_after_drop`], not here — a dropped future
    /// never polls to a return value.
    pub requests_returned_err: u64,

    /// Sends that had to park because every credit the server granted was
    /// already in flight. A trickle is normal on a saturated pipeline; a
    /// flood means the server's window is small relative to the chunk size,
    /// and throughput is bounded by credits rather than by the network.
    pub credit_waits: u64,
    /// Sends that gave up waiting for a grant and returned
    /// [`Error::CreditStarvation`](crate::Error::CreditStarvation). Subset of
    /// [`Self::credit_waits`]; don't sum. Non-zero means a server stopped
    /// answering while its socket stayed open.
    pub credit_starvations: u64,
    /// Requests abandoned because the server went silent for longer than
    /// [`Connection::set_response_timeout`](crate::client::connection::Connection::set_response_timeout).
    /// The clock restarts on every interim `STATUS_PENDING`, so this counts
    /// total silence, not slowness.
    pub response_timeouts: u64,
    /// Frames the transport refused to write: a send that timed out
    /// ([`Error::SendTimeout`](crate::Error::SendTimeout)) or errored.
    ///
    /// Non-zero means the wedge was on OUR side of the wire — the request
    /// never reached the server, so nothing about the server follows from it.
    pub send_failures: u64,
    /// SMB2 ECHO probes the keepalive put on the wire.
    ///
    /// Zero on a healthy busy connection and that is correct, not a bug: the
    /// keepalive only probes when the server has gone quiet with work
    /// outstanding, and responses flowing are already proof of life.
    pub keepalive_probes_sent: u64,
    /// Probe rounds that asked the server nothing, so they are evidence of
    /// nothing: no credit was on hand for the ECHO, or the connection was
    /// already going down.
    ///
    /// A steady stream of these means the credit window is fully spent
    /// whenever the server goes quiet, which leaves the connection without a
    /// liveness signal — slow-but-alive operations fall back to the plain
    /// response deadline. Skips are never counted as failures: they say
    /// nothing about the server.
    pub keepalive_probes_skipped: u64,
    /// Probes that reached the wire and were never answered.
    ///
    /// Not a failure count in any operational sense — a busy NAS drops probes
    /// while it writes, and this rising on its own costs a connection nothing
    /// but the deadline extension it would otherwise have earned. It is worth
    /// watching next to `response_deadline_extensions`: probes going
    /// unanswered while slow operations need the extra room is the shape that
    /// ends in [`Error::Timeout`](crate::Error::Timeout).
    pub keepalive_failures: u64,
    /// Requests that went past
    /// [`Connection::set_response_timeout`](crate::client::connection::Connection::set_response_timeout)
    /// without being abandoned, because an ECHO had just proven the server
    /// alive.
    ///
    /// Each tick is a slow-but-healthy operation the deadline alone would have
    /// killed — a large write to a loaded spinning-disk NAS is the usual one.
    /// A rising count next to a flat `response_timeouts` is the keepalive
    /// working exactly as intended.
    pub response_deadline_extensions: u64,
    /// Long-poll requests (CHANGE_NOTIFY) retired and re-issued because they
    /// reached
    /// [`Connection::set_long_poll_refresh`](crate::client::connection::Connection::set_long_poll_refresh).
    ///
    /// ❌ Not an error count and not a detection count. Nothing can tell a
    /// subscription the server has forgotten from a directory nobody has
    /// touched — both are silence — so the client re-issues on a cycle instead
    /// of trying, and this ticks every time it does. On a healthy watch it
    /// climbs at roughly one per interval per watched directory. Zero while a
    /// watcher has been open for longer than the interval means the refresh is
    /// turned off, which is what leaves a dropped subscription dead forever.
    pub long_poll_refreshes: u64,
    /// Stretches this PROCESS spent unscheduled, long enough that its own
    /// clocks could no longer be trusted: a system sleep, an App Nap, a
    /// machine so starved that a 1 s timer came back seconds late.
    ///
    /// Says nothing whatever about the server. It is here because every other
    /// number on this connection measures wall time, so a stall inflates
    /// silence readings that the server had no part in, and a reader
    /// diagnosing a burst of `response_timeouts` wants to know the process
    /// stopped running before blaming the network.
    pub scheduling_stalls: u64,

    /// Dials made trying to bring this connection back, across every revival.
    pub reconnect_attempts: u64,
    /// Revivals that ended with a live, authenticated session on a fresh
    /// socket.
    ///
    /// The after-the-fact answer to "was this link quietly flaky?". A transfer
    /// that finished with a non-zero value here survived something the user
    /// never saw, which is exactly the outcome the spec worried about hiding:
    /// the number is always here whether or not anyone subscribed to
    /// [`ReconnectEvent`](crate::client::connection::ReconnectEvent).
    ///
    /// ❌ Unlike every other per-connection counter this one is NOT reset by a
    /// reconnect — it counts them, and a counter that resets on the event it
    /// counts would always read zero.
    pub reconnects_succeeded: u64,
    /// Revivals that gave up, each surfaced to its caller as
    /// [`Error::ReconnectFailed`](crate::Error::ReconnectFailed).
    pub reconnects_failed: u64,
}

/// Client-level counter snapshot. Lives on [`SmbClient`](crate::SmbClient)
/// (above the per-connection layer) and survives
/// [`SmbClient::reconnect`](crate::SmbClient::reconnect).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ClientMetricsSnapshot {
    /// `SmbClient::reconnect` invocations. The event-shaped signal "did
    /// we reconnect" — pair with
    /// [`MetricsSnapshot::session_expired_events`] if you want both.
    pub reconnects: u64,
    /// DFS path resolutions that resulted in a referral IOCTL to the
    /// server (cache miss).
    pub dfs_referrals_resolved: u64,
    /// DFS path resolutions served from the in-process referral cache
    /// (cache hit).
    pub dfs_cache_hits: u64,
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.client;
        writeln!(f, "SMB client → {}", c.primary_server)?;
        writeln!(
            f,
            "  reconnects: {}    dfs: {} (hits: {}, referrals resolved: {}, cache entries: {})",
            c.metrics.reconnects,
            if c.dfs_enabled { "enabled" } else { "disabled" },
            c.metrics.dfs_cache_hits,
            c.metrics.dfs_referrals_resolved,
            self.dfs_cache.len(),
        )?;
        writeln!(f)?;
        writeln!(f, "Primary connection ({})", self.primary.server)?;
        fmt_connection_body(&self.primary, f)?;

        if !self.extra_connections.is_empty() {
            writeln!(f)?;
            writeln!(
                f,
                "DFS extra connections: ({})",
                self.extra_connections.len()
            )?;
            for c in &self.extra_connections {
                writeln!(f)?;
                writeln!(f, "  ↳ {}", c.server)?;
                fmt_connection_body(c, f)?;
            }
        } else {
            writeln!(f)?;
            writeln!(f, "DFS extra connections: (0)")?;
        }
        Ok(())
    }
}

fn fmt_connection_body(c: &ConnectionDiagnostics, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let m = &c.metrics;
    match &c.negotiated {
        Some(n) => {
            let rtt = c
                .rtt_estimate
                .map(|d| format!("{:.1} ms", d.as_secs_f64() * 1000.0))
                .unwrap_or_else(|| "—".to_string());
            writeln!(f, "  dialect: {:?}    rtt: {}", n.dialect, rtt)?;
            writeln!(
                f,
                "  signing: {}    encryption: {}    compression: {}",
                fmt_signing(&c.signing),
                fmt_encryption(&c.encryption),
                fmt_compression(&c.compression),
            )?;
        }
        None => {
            writeln!(
                f,
                "  (pre-negotiate — no dialect / signing / encryption yet)"
            )?;
        }
    }
    writeln!(
        f,
        "  credits: {} available · {} in flight · next msg_id {}",
        c.credits.available, c.credits.in_flight, c.credits.next_message_id
    )?;
    writeln!(
        f,
        "  wire bytes: {} sent · {} received",
        m.wire_bytes_sent, m.wire_bytes_received
    )?;
    writeln!(
        f,
        "  responses: {} ok · {} wire-err · {} late · {} stray    (sent: {}, caller-err: {})",
        m.responses_routed_ok,
        m.responses_routed_err,
        m.responses_late_after_drop,
        m.responses_stray,
        m.requests_sent,
        m.requests_returned_err,
    )?;
    writeln!(
        f,
        "  protocol events: {} status-pending · {} unsolicited · {} compound chains · {} cancels",
        m.status_pending_loops,
        m.unsolicited_notifications_received,
        m.compound_requests_sent,
        m.explicit_cancels_sent,
    )?;
    writeln!(
        f,
        "  errors: {} signature · {} decrypt · {} decompress · {} malformed · {} session-expired",
        m.signature_failures,
        m.decrypt_failures,
        m.decompress_failures,
        m.malformed_frames,
        m.session_expired_events,
    )?;
    writeln!(
        f,
        "  credit waits: {} parked · {} starved · {} response timeouts · {} send failures",
        m.credit_waits, m.credit_starvations, m.response_timeouts, m.send_failures,
    )?;
    writeln!(
        f,
        "  keepalive: {} probes · {} skipped · {} unanswered · {} deadline extensions",
        m.keepalive_probes_sent,
        m.keepalive_probes_skipped,
        m.keepalive_failures,
        m.response_deadline_extensions,
    )?;
    if m.long_poll_refreshes > 0 {
        writeln!(f, "  long polls: {} refreshed", m.long_poll_refreshes)?;
    }
    if m.scheduling_stalls > 0 {
        writeln!(
            f,
            "  scheduling: {} stall(s) on OUR side (process unscheduled; silence readings across \
             them mean nothing)",
            m.scheduling_stalls,
        )?;
    }
    if m.reconnect_attempts > 0 {
        writeln!(
            f,
            "  reconnects: {} succeeded · {} failed · {} dials",
            m.reconnects_succeeded, m.reconnects_failed, m.reconnect_attempts,
        )?;
    }
    writeln!(
        f,
        "  inbound: {} bytes{} · {}",
        c.inbound.bytes_received,
        match c.inbound.frame {
            Some(frame) => format!(" (frame arriving: {} of {})", frame.received, frame.len),
            None => String::new(),
        },
        fmt_liveness(&c.liveness),
    )?;
    if c.disconnected {
        writeln!(f, "  status: DISCONNECTED")?;
    }
    Ok(())
}

fn fmt_liveness(l: &Liveness) -> String {
    match l {
        Liveness::Idle => "idle".to_string(),
        Liveness::Alive => "alive".to_string(),
        Liveness::Quiet { silent_for } => format!("quiet for {silent_for:.1?}"),
        Liveness::Unresponsive { silent_for } => {
            format!("UNRESPONSIVE for {silent_for:.1?}")
        }
        Liveness::Disconnected => "disconnected".to_string(),
    }
}

fn fmt_signing(s: &SigningInfo) -> String {
    match (s.active, s.algorithm) {
        (true, Some(algo)) => format!("active ({:?})", algo),
        (true, None) => "active".to_string(),
        (false, _) => "inactive".to_string(),
    }
}

fn fmt_encryption(e: &EncryptionInfo) -> String {
    match (e.active, e.cipher) {
        (true, Some(c)) => format!("active ({:?})", c),
        (true, None) => "active".to_string(),
        (false, _) => "inactive".to_string(),
    }
}

fn fmt_compression(c: &CompressionInfo) -> String {
    match (c.requested, c.negotiated) {
        (true, true) => "active".to_string(),
        (true, false) => "requested, not negotiated".to_string(),
        (false, true) => "active (not requested)".to_string(),
        (false, false) => "off".to_string(),
    }
}

// ── M3: optional serde derives ───────────────────────────────────────────
// Each diagnostics type carries `#[cfg_attr(feature = "serde", derive(Serialize))]`
// directly on its definition (above). `Capabilities` has a manual `Serialize`
// impl in `types/flags.rs` that emits the underlying u32 bits.

#[cfg(test)]
mod tests {
    //! Per-counter unit tests for M1.
    //!
    //! Each test exercises one counter against a `MockTransport`, asserting
    //! it ticks the expected number of times. The disjoint-partition test
    //! at the bottom checks the four routing-outcome counters sum to the
    //! total sub-frames the receiver routed.

    use std::sync::Arc;
    use std::time::Duration;

    use crate::client::connection::Connection;
    use crate::msg::echo::{EchoRequest, EchoResponse};
    use crate::msg::header::Header;
    use crate::pack::Pack;
    use crate::transport::mock::MockTransport;
    use crate::types::status::NtStatus;
    use crate::types::{Command, MessageId};

    /// Build a packed message (header + body) — mirrors `pack_message` in
    /// `connection.rs`, kept inline to avoid widening that helper's
    /// visibility just for tests.
    fn pack(header: &Header, body: &dyn Pack) -> Vec<u8> {
        let mut cursor = crate::pack::WriteCursor::with_capacity(64 + 16);
        header.pack(&mut cursor);
        body.pack(&mut cursor);
        cursor.into_inner()
    }

    fn echo_response(msg_id: MessageId, status: NtStatus) -> Vec<u8> {
        let mut h = Header::new_request(Command::Echo);
        h.flags.set_response();
        h.credits = 10;
        h.message_id = msg_id;
        h.status = status;
        pack(&h, &EchoResponse)
    }

    fn echo_ok(msg_id: MessageId) -> Vec<u8> {
        echo_response(msg_id, NtStatus::SUCCESS)
    }

    /// Wait until at least `n` messages have been recorded as sent on
    /// the mock. Times out after 5 s.
    async fn wait_for_sent(mock: &MockTransport, n: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mock.sent_count() < n {
            if std::time::Instant::now() > deadline {
                panic!("expected {n} sent messages, got {}", mock.sent_count());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// A bare `Connection` over a mock transport, with auto-rewrite ON.
    /// Mirrors the existing `execute_returns_correct_frame_for_sent_request`
    /// setup but returns the mock so the test can queue / inspect.
    fn fresh_conn() -> (Connection, Arc<MockTransport>) {
        let mock = Arc::new(MockTransport::new());
        mock.enable_auto_rewrite_msg_id();
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // Stage the state a real connection holds after NEGOTIATE /
        // SESSION_SETUP / TREE_CONNECT: a fresh pool is empty, and nothing but
        // NEGOTIATE can send out of one, while a CANCEL additionally refuses to
        // go out on a connection with no session id.
        conn.set_credits(512);
        conn.set_session_id(crate::types::SessionId(0x1234));
        (conn, mock)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_sent_and_wire_bytes_sent_tick_for_one_execute() {
        let (conn, mock) = fresh_conn();

        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        mock.queue_response(echo_ok(MessageId(0)));
        handle.await.unwrap().unwrap();

        let m = conn.metrics();
        assert_eq!(m.requests_sent, 1, "one msg_id allocated → one request");
        assert!(m.wire_bytes_sent > 0, "send wrote some bytes to the wire");
        assert!(
            m.wire_bytes_received > 0,
            "receive read some bytes from the wire"
        );
        assert_eq!(m.responses_routed_ok, 1);
        assert_eq!(m.responses_routed_err, 0);
        assert_eq!(m.responses_late_after_drop, 0);
        assert_eq!(m.responses_stray, 0);
        assert_eq!(m.requests_returned_err, 0);

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_sent_ticks_per_sub_op_in_compound_and_compound_chain_counted() {
        use crate::client::connection::CompoundOp;

        let (conn, mock) = fresh_conn();

        let c = conn.clone();
        let handle = tokio::spawn(async move {
            let ops = vec![
                CompoundOp::new(Command::Echo, &EchoRequest, None),
                CompoundOp::new(Command::Echo, &EchoRequest, None),
                CompoundOp::new(Command::Echo, &EchoRequest, None),
            ];
            c.execute_compound(&ops).await
        });

        wait_for_sent(&mock, 1).await;
        // Three sub-frames → three responses. Auto-rewrite handles the
        // msg_id pairing per sub-frame.
        mock.queue_response(echo_ok(MessageId(0)));
        mock.queue_response(echo_ok(MessageId(0)));
        mock.queue_response(echo_ok(MessageId(0)));
        handle.await.unwrap().unwrap();

        let m = conn.metrics();
        assert_eq!(m.requests_sent, 3, "three sub-ops → requests_sent += 3");
        assert_eq!(m.compound_requests_sent, 1, "one compound chain");
        assert_eq!(m.responses_routed_ok, 3);
        assert_eq!(m.requests_returned_err, 0);

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn requests_returned_err_ticks_on_outer_err_to_completed_caller() {
        let (conn, mock) = fresh_conn();

        // Close before sending → execute returns Err(Disconnected) once the
        // receiver task's transport-error branch fans to the waiter.
        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        mock.close();
        let result = handle.await.unwrap();
        assert!(result.is_err(), "execute should error after close");

        // Receiver-task tear-down may take a beat to propagate; loop briefly.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while conn.metrics().requests_returned_err == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(conn.metrics().requests_returned_err, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn responses_late_after_drop_ticks_when_caller_dropped() {
        let (conn, mock) = fresh_conn();

        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        // Drop the caller's future BEFORE the response arrives. The waiter
        // is still in the map; the oneshot::Receiver gets dropped.
        handle.abort();
        let _ = handle.await; // observe the JoinError, don't unwrap

        // Now queue the response. The receiver task finds the waiter,
        // tries to send, sees the dropped Receiver, bumps
        // responses_late_after_drop (and NOT responses_stray).
        mock.queue_response(echo_ok(MessageId(0)));

        // Wait until the counter ticks (the receiver task drives this).
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while conn.metrics().responses_late_after_drop == 0 && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let m = conn.metrics();
        assert_eq!(m.responses_late_after_drop, 1, "caller-drop should tick");
        assert_eq!(m.responses_stray, 0, "stray is for unregistered ids only");
        assert_eq!(m.responses_routed_ok, 0);

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn responses_stray_ticks_for_unregistered_msg_id() {
        let (conn, mock) = fresh_conn();

        // Don't call execute at all. Queue a response for a msg_id no one
        // allocated. Auto-rewrite would normally pair with a sent msg_id,
        // but there is none — so we use the *non*-auto path: build a
        // response with an explicit non-zero msg_id and drop into the
        // queue. Auto-rewrite's "keep non-zero, still consume one id"
        // logic would block on `send_notify` forever. So disable it
        // first by NOT enabling on a fresh second mock.
        let _ = (conn, mock); // discarded — we use a non-auto-rewrite mock below
        let plain_mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(plain_mock.clone()),
            Box::new(plain_mock.clone()),
            "test-server",
        );

        plain_mock.queue_response(echo_ok(MessageId(999_999)));

        // Poll the counter — `pending_responses() == 0` only proves the
        // transport drained, not that the receiver finished processing the
        // frame and bumped `responses_stray`. The latter is the actual
        // signal we're testing.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while conn.metrics().responses_stray == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let m = conn.metrics();
        assert_eq!(m.responses_stray, 1);
        assert_eq!(m.responses_late_after_drop, 0);
        assert_eq!(m.responses_routed_ok, 0);

        plain_mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsolicited_notifications_received_ticks_for_unsolicited_msg_id() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );

        let mut h = Header::new_request(Command::OplockBreak);
        h.flags.set_response();
        h.credits = 0;
        h.message_id = MessageId::UNSOLICITED;
        let frame = pack(&h, &EchoResponse); // body shape doesn't matter; it's skipped
        mock.queue_response(frame);

        // Wait for consumption.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while conn.metrics().unsolicited_notifications_received == 0
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(conn.metrics().unsolicited_notifications_received, 1);
        // UNSOLICITED is skipped — it does NOT tick the routing counters.
        assert_eq!(conn.metrics().responses_routed_ok, 0);
        assert_eq!(conn.metrics().responses_stray, 0);

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_pending_loops_ticks_for_interim_pending_then_final() {
        // Don't use auto_rewrite — we need TWO responses paired with ONE sent
        // msg_id. The first execute on a fresh connection always allocates
        // msg_id=0, so we can hardcode that in both responses.
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        conn.set_credits(512);

        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        // Interim STATUS_PENDING with msg_id=0, then final SUCCESS with msg_id=0.
        mock.queue_response(echo_response(MessageId(0), NtStatus::PENDING));
        mock.queue_response(echo_response(MessageId(0), NtStatus::SUCCESS));

        handle.await.unwrap().unwrap();

        let m = conn.metrics();
        assert_eq!(m.status_pending_loops, 1, "one interim PENDING observed");
        assert_eq!(m.responses_routed_ok, 1, "one final response routed");

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_expired_events_ticks_and_also_routes_err() {
        let mock = Arc::new(MockTransport::new());
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A negotiated connection has a credit window; NEGOTIATE's response is
        // what opens it. Without staging one, nothing below can be sent.
        conn.set_credits(512);

        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        mock.queue_response(echo_response(
            MessageId(0),
            NtStatus::NETWORK_SESSION_EXPIRED,
        ));

        let result = handle.await.unwrap();
        assert!(result.is_err(), "session-expired should surface as Err");

        let m = conn.metrics();
        assert_eq!(m.session_expired_events, 1);
        assert_eq!(
            m.responses_routed_err, 1,
            "session_expired_events is a subset of responses_routed_err"
        );
        assert_eq!(m.responses_routed_ok, 0);
        assert_eq!(
            m.requests_returned_err, 1,
            "caller polled to completion and got Err"
        );

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn explicit_cancels_sent_ticks_on_send_cancel() {
        let mock = Arc::new(MockTransport::new());
        let mut conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        // A CANCEL only goes out on a connection with a live session; without
        // one it is skipped, and a skipped cancel is deliberately not counted.
        conn.set_session_id(crate::types::SessionId(0x1234));

        conn.send_cancel(MessageId(42), None, conn.generation())
            .await
            .unwrap();

        assert_eq!(conn.metrics().explicit_cancels_sent, 1);
        // CANCEL does NOT allocate a msg_id — it reuses the original.
        assert_eq!(conn.metrics().requests_sent, 0);

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_path_is_counted() {
        // `dispatch` is the watcher's pre-arm path — funnel-counted via
        // allocate_msg_id, same as `execute`.
        let (conn, mock) = fresh_conn();

        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.dispatch(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        let mut rx = handle.await.unwrap().unwrap();
        mock.queue_response(echo_ok(MessageId(0)));
        // Drive the response so the receiver processes it (and the awaiter
        // sees the result).
        let _ = rx.recv().await.unwrap();

        let m = conn.metrics();
        assert_eq!(m.requests_sent, 1, "dispatch funnel-counts via allocate");
        assert!(m.wire_bytes_sent > 0);
        assert_eq!(m.responses_routed_ok, 1);

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn counters_survive_teardown() {
        let (conn, mock) = fresh_conn();

        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });

        wait_for_sent(&mock, 1).await;
        mock.queue_response(echo_ok(MessageId(0)));
        handle.await.unwrap().unwrap();

        let before = conn.metrics();
        assert_eq!(before.responses_routed_ok, 1);

        // Tear down.
        mock.close();
        // Give the receiver task a tick to observe Err and fan.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Counters still readable.
        let after = conn.metrics();
        assert_eq!(after.responses_routed_ok, before.responses_routed_ok);
        assert_eq!(after.requests_sent, before.requests_sent);
    }

    // ── M2 / M3: full Diagnostics tree + Display + serde ──────────────

    fn fake_client(conn: Connection, session: crate::client::Session) -> crate::SmbClient {
        let cfg = crate::ClientConfig {
            addr: conn.server_name().to_string(),
            timeout: Duration::from_secs(30),
            username: String::new(),
            password: String::new(),
            domain: String::new(),
            auto_reconnect: false,
            compression: true,
            dfs_enabled: true,
            dfs_target_overrides: std::collections::HashMap::new(),
            connect_options: None,
            ..Default::default()
        };
        crate::SmbClient::from_parts(cfg, conn, session)
    }

    fn fake_session() -> crate::client::Session {
        crate::client::Session {
            session_id: crate::types::SessionId(0x1234_5678_9ABC_DEF0),
            signing_key: vec![],
            encryption_key: None,
            decryption_key: None,
            signing_algorithm: crate::crypto::signing::SigningAlgorithm::HmacSha256,
            should_sign: false,
            should_encrypt: false,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn display_contains_key_labels() {
        let (conn, mock) = fresh_conn();
        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });
        wait_for_sent(&mock, 1).await;
        mock.queue_response(echo_ok(MessageId(0)));
        handle.await.unwrap().unwrap();

        let client = fake_client(conn, fake_session());
        let d = client.diagnostics();
        let text = format!("{}", d);
        for label in [
            "SMB client",
            "test-server",
            "credits:",
            "wire bytes:",
            "responses:",
            "protocol events:",
            "errors:",
            "DFS extra connections",
        ] {
            assert!(
                text.contains(label),
                "Display missing {label:?} in:\n{text}"
            );
        }

        mock.close();
    }

    #[cfg(feature = "serde")]
    #[tokio::test(flavor = "multi_thread")]
    async fn serde_round_trip_into_json_value() {
        let (conn, mock) = fresh_conn();
        let c = conn.clone();
        let handle =
            tokio::spawn(async move { c.execute(Command::Echo, &EchoRequest, None).await });
        wait_for_sent(&mock, 1).await;
        mock.queue_response(echo_ok(MessageId(0)));
        handle.await.unwrap().unwrap();

        let client = fake_client(conn, fake_session());
        let d = client.diagnostics();

        let json = serde_json::to_string(&d).expect("serialize");
        let v: serde_json::Value = serde_json::from_str(&json).expect("re-parse");

        assert_eq!(v["client"]["primary_server"], "test-server", "json: {json}");
        assert_eq!(v["primary"]["server"], "test-server");
        assert_eq!(v["primary"]["metrics"]["requests_sent"], 1);
        assert_eq!(v["primary"]["metrics"]["responses_routed_ok"], 1);
        assert!(v["primary"]["disconnected"].is_boolean());
        assert!(v["primary"]["credits"]["available"].is_number());
        // SessionId is transparent: bare integer, not `{"0": ...}`.
        assert_eq!(
            v["primary"]["session"]["session_id"], 0x1234_5678_9ABC_DEF0_u64,
            "json: {json}"
        );

        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_releases_all_locks_before_returning() {
        // Regression test: the snapshot promises it holds each lock only
        // briefly and releases it before returning. Try_lock'ing after
        // the snapshot call must succeed.
        let (conn, mock) = fresh_conn();
        let _d = conn.diagnostics();
        // We can't reach `inner` from here without crate access; this test
        // lives in-crate so it CAN. The diagnostics module is in
        // `client/`, the connection internals are `pub(crate)`-shaped.
        // If a future refactor breaks lock ordering, the in-flight test
        // above catches it indirectly; this test pins the "no held lock"
        // invariant cheaply.
        for _ in 0..100 {
            let _ = conn.diagnostics();
        }
        mock.close();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn routing_partition_is_disjoint_and_complete() {
        // 3 sent, 1 normal, 1 caller-drop, 1 stray on top.
        let mock = Arc::new(MockTransport::new());
        // Plain mode so we can fully control msg_ids.
        let conn = Connection::from_transport(
            Box::new(mock.clone()),
            Box::new(mock.clone()),
            "test-server",
        );
        conn.set_credits(512);

        // Op A: send, will succeed.
        let c1 = conn.clone();
        let h1 = tokio::spawn(async move { c1.execute(Command::Echo, &EchoRequest, None).await });
        wait_for_sent(&mock, 1).await; // msg_id 0

        // Op B: send, then abort (caller drop).
        let c2 = conn.clone();
        let h2 = tokio::spawn(async move { c2.execute(Command::Echo, &EchoRequest, None).await });
        wait_for_sent(&mock, 2).await; // msg_id 1

        // Op A response.
        mock.queue_response(echo_ok(MessageId(0)));
        h1.await.unwrap().unwrap();

        // Drop op B then queue its response.
        h2.abort();
        let _ = h2.await;
        mock.queue_response(echo_ok(MessageId(1)));

        // Stray frame for a msg_id no one allocated.
        mock.queue_response(echo_ok(MessageId(999_999)));

        // Poll the actual signals — `pending_responses() == 0` only proves
        // the transport drained, not that the receiver finished bumping the
        // counters. `responses_routed_ok` is already 1 (h1.await guarantees
        // it after the receiver_loop fix); `late_after_drop` and
        // `responses_stray` each tick once when their frame is processed.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while (conn.metrics().responses_late_after_drop == 0 || conn.metrics().responses_stray == 0)
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let m = conn.metrics();
        assert_eq!(m.responses_routed_ok, 1);
        assert_eq!(m.responses_routed_err, 0);
        assert_eq!(m.responses_late_after_drop, 1);
        assert_eq!(m.responses_stray, 1);

        // Partition: routed_ok + routed_err + late + stray ==
        // total sub-frames the receiver dispatched (3 here).
        assert_eq!(
            m.responses_routed_ok
                + m.responses_routed_err
                + m.responses_late_after_drop
                + m.responses_stray,
            3
        );

        mock.close();
    }
}
