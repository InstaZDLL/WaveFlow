//! Sync protocol primitives shared between the WaveFlow desktop app
//! and `waveflow-server` (RFC-003).
//!
//! Everything in this module is pure protocol — no Tauri runtime, no
//! axum types, no database I/O. The server's `Hlc` (which today
//! carries a `utoipa::ToSchema` derive for its OpenAPI surface) maps
//! onto the version exposed here by structural equality, and Phase
//! A.4.3 will collapse the two through a thin newtype wrapper or a
//! cargo feature. Until then desktop + server keep parallel `Hlc`
//! definitions and consume the canonical-serialisation + total-order
//! helpers from one source of truth.

use serde::{Deserialize, Serialize};

pub mod payload_hash;

/// Hybrid Logical Clock pair carried by RFC-003 v2 ops on the wire.
///
/// `wall` is epoch-millis (Postgres `BIGINT`, SQLite `INTEGER`).
/// `logical` is the per-tick counter the HLC paper defines as `u32`;
/// every storage layer narrows it to `i32` on bind so the value stays
/// representable in `INTEGER`. Both sides of the protocol agree on
/// `0..=i32::MAX` as the legal range — the desktop guard ships in
/// A.4.2, the server's guard landed with A.1.1.
///
/// Paired with `origin_device_id` to form the §2 total-order triple
/// the apply pipeline LWW rule runs on.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct Hlc {
    pub wall: i64,
    pub logical: i32,
}
