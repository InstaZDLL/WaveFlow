//! Multi-device sync infrastructure for the desktop side of
//! [RFC-001 §6.6](https://github.com/InstaZDLL/WaveFlow/blob/main/docs/rfcs/RFC-001-waveflow-server.md#66-sync).
//!
//! ## Module map
//!
//! - [`device`] — the stable per-install UUID the server pins the
//!   `(user_id, device_id, …)` UNIQUEs against. Lazily generated on
//!   first read, persisted app-wide in `app_setting['sync.device_id']`.
//!
//! - [`lamport`] — per-profile monotonic clock. [`lamport::next`]
//!   atomically increments and returns the new value; the desktop's
//!   CRUD commands pair it with [`queue::enqueue`] to stamp every
//!   outgoing op. [`lamport::observe_remote_conn`] is the rejoin
//!   path — every inbound op the WS subscriber applies bumps the
//!   local clock past it so the next local op stays globally
//!   monotonic.
//!
//! - [`queue`] — the local write-ahead log. Rows here are ops the
//!   user produced while signed into a `waveflow-server`; the
//!   [`drain`] task posts them to `/api/v1/sync/ops` and removes the
//!   rows the server accepts.
//!
//! - [`mode`] — the per-profile sync toggle (`Local` vs `Hybrid`).
//!   Both outbound enqueue + inbound subscriber gate against
//!   `Hybrid`.
//!
//! - [`hooks`] — CRUD command sites' atomic write+enqueue glue.
//!   [`hooks::enqueue_op_in_tx`] keeps the playlist write + outbox
//!   row + Lamport bump in a single SQLite tx.
//!
//! - [`drain`] — outbound push. Periodic + on-demand task that
//!   batches `sync_pending_op` rows to the server and drops accepted
//!   ones (Phase 1.f.desktop.4a).
//!
//! - [`canonical`] — local↔canonical-id mapping (Phase
//!   1.f.desktop.4b). Outbound ops carry the canonical UUID instead
//!   of the local rowid so peer devices can route them through
//!   `sync_id_map` back to their own rowid.
//!
//! - [`apply`] — inbound application. Translates a `RemoteSyncOp`
//!   from the server back into a CRUD write on the active profile,
//!   WITHOUT touching the outbox (no ping-pong).
//!
//! - [`cursor`] — per-profile `last_seen_id` tracker. Resumes the
//!   catch-up REST pull after every reconnect.
//!
//! - [`ws`] — WebSocket subscriber + catch-up REST puller (Phase
//!   1.f.desktop.4b). Closes the loop opened by [`drain`].

pub mod apply;
pub mod canonical;
pub mod cursor;
pub mod device;
pub mod drain;
pub mod hooks;
pub mod lamport;
pub mod mode;
pub mod queue;
pub mod track_emit;
pub mod track_snapshots;
pub mod ws;
