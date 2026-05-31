//! Multi-device sync infrastructure for the desktop side of
//! [RFC-001 §6.6](https://github.com/InstaZDLL/WaveFlow/blob/main/docs/rfcs/RFC-001-waveflow-server.md#66-sync).
//!
//! Phase 1.f.desktop.2 ships the foundational helpers every later
//! sub-PR builds on:
//!
//! - [`device`] — the stable per-install UUID the server pins the
//!   `(user_id, device_id, …)` UNIQUEs against. Lazily generated on
//!   first read, persisted app-wide in `app_setting['sync.device_id']`.
//!
//! - [`lamport`] — per-profile monotonic clock. [`lamport::next`]
//!   atomically increments and returns the new value; the desktop's
//!   CRUD commands will pair it with [`queue::enqueue`] to stamp
//!   every outgoing op. [`lamport::observe_remote`] is the rejoin
//!   path — when a future WS subscriber (1.f.desktop.4) sees a higher
//!   remote `lamport_ts`, it bumps the local clock past it so the
//!   next local op stays globally monotonic.
//!
//! - [`queue`] — the local write-ahead log. Rows here are ops the
//!   user produced while signed into a `waveflow-server`; a future
//!   drain task (1.f.desktop.4) posts them to
//!   `/api/v1/sync/ops` and removes the rows the server accepts.
//!
//! ## What this PR does NOT ship
//!
//! - **CRUD enqueue hooks** in `commands/playlist`, `commands/library`,
//!   `commands/edit`. Wiring each command site is a 1.f.desktop.2b
//!   concern so the infrastructure here can be validated in isolation
//!   and the CRUD changes review separately.
//! - **Canonical-id mapping** for cross-device entity identity (see
//!   [`queue`] docstring for the open design question).
//! - **The drain task itself** — that's 1.f.desktop.4 alongside the
//!   WebSocket subscriber.

pub mod device;
pub mod hooks;
pub mod lamport;
pub mod queue;
