//! Glue between CRUD command handlers and the local sync queue.
//!
//! Every command in `commands/playlist.rs` that mutates a syncable
//! entity wraps its SQLite write AND the matching
//! [`enqueue_op_in_tx`] call in a single transaction. The two writes
//! either both commit or both roll back — closing the drift window
//! issue #193 documented (where the playlist write committed but a
//! subsequent enqueue could fail without touching the entity row).
//!
//! ## Skip path
//!
//! [`enqueue_op_in_tx`] returns `Ok(false)` without writing when
//! either:
//!
//! - no `waveflow_server` JWT is stored for the active profile
//!   (local-only user, never accumulates ops they won't sync), OR
//! - the active profile's [`mode::SyncMode`] is
//!   [`mode::SyncMode::Local`] — an explicit user opt-out from
//!   1.f.desktop.3 even when signed in.
//!
//! Both checks run on the same connection as the write so they see
//! whatever the caller's transaction has already committed, without
//! an extra pool acquire.
//!
//! ## Failure model
//!
//! Errors from [`enqueue_op_in_tx`] propagate to the caller. The
//! caller is expected to `?` them so the transaction rolls back the
//! entity write too — the whole point of the atomic path is that a
//! failure in the outbox aborts the user's CRUD operation rather
//! than leaving the local state ahead of the queue.

use sqlx::SqliteConnection;
use uuid::Uuid;

use crate::{
    error::AppResult,
    server_client,
    sync::{device, hlc, lamport, mode, queue},
};

pub use crate::sync::queue::PendingOpDraft;

/// Stamp returned by [`enqueue_op_in_tx`] when the enqueue actually
/// ran. Carries the values the apply-side payload-hash + entity-row
/// stamp need: the HLC pair the queue row carries on the wire, plus
/// the `origin_device_id` (UUID) the device's stable identity ride
/// in `PushBatchRequest.device_id` (UUID-shaped TEXT). v1 emitters
/// don't need these — they read `enqueue_op_in_tx` for the boolean
/// shape via the [`Option`] wrapper.
#[derive(Debug, Clone, Copy)]
pub struct EnqueuedStamp {
    /// HLC wall (epoch-millis) drawn by [`hlc::next_conn`].
    pub hlc_wall: i64,
    /// HLC logical (i32) drawn by [`hlc::next_conn`].
    pub hlc_logical: i32,
    /// Parsed device id. `None` when the persisted device id isn't
    /// UUID-shaped (legacy installs); the apply-side payload-hash
    /// builder treats `None` as "no triple tiebreaker" per RFC-003.
    pub origin_device_id: Option<Uuid>,
}

/// Atomically enqueue an op against a caller-owned connection
/// (typically an open `Transaction<'_, Sqlite>` borrowed as
/// `&mut *tx`). Composes with the entity mutation in the same
/// transaction so the playlist write + the outbox row + the Lamport
/// bump either ALL land or ALL roll back.
///
/// Returns `Some(stamp)` when an op was actually inserted, `None`
/// when the skip conditions kicked in (no JWT for the active
/// profile, or `SyncMode::Local`). Phase B.0 consumers use the
/// stamp to also write `(hlc_wall, hlc_logical, origin_device_id,
/// payload_hash)` onto the entity row inside the same transaction —
/// see `sync::payload::*::stamp_in_tx` per entity. v1 consumers can
/// ignore the inner shape and read the result through
/// [`Option::is_some`].
pub async fn enqueue_op_in_tx(
    conn: &mut SqliteConnection,
    draft: &PendingOpDraft,
) -> AppResult<Option<EnqueuedStamp>> {
    if server_client::read_token_conn(conn).await?.is_none() {
        return Ok(None);
    }
    if mode::read_conn(conn).await? == mode::SyncMode::Local {
        return Ok(None);
    }
    let lamport_ts = lamport::next_conn(conn).await?;
    // Phase A.4.2 — draw the HLC pair on the same connection so the
    // entity write + outbox row + Lamport bump + HLC bump are one
    // atomic SQLite commit. The pair rides on the row for the drain
    // to lift onto `SyncOpIn.hlc` (v2 wire shape — waveflow-server
    // #52). v1 server reads still work because the dual-shape ingest
    // there derives `(0, lamport_ts)` when `hlc` is absent.
    let hlc_pair = hlc::next_conn(conn).await?;
    queue::enqueue_conn(conn, draft, lamport_ts, hlc_pair.wall, hlc_pair.logical).await?;
    // `device::read_conn` reads from `app_setting` via the
    // connection's ATTACHed `app` schema — same path the drain
    // uses via `device::ensure(&state.app_db)`, except pure-read so
    // we don't mint a UUID mid-tx. The desktop's main loop calls
    // `device::ensure` at boot, so by the time enqueue runs the row
    // always exists; the `None` branch only fires on the very first
    // CRUD before `ensure` has had a chance to seed. UUID parse
    // failure → `None`; the apply-side payload-hash builder treats
    // it as "no tiebreaker", matching the server's
    // `apply::parse_origin_device_id` contract.
    let origin_device_id = device::read_conn(conn)
        .await?
        .and_then(|id| Uuid::parse_str(&id).ok());
    Ok(Some(EnqueuedStamp {
        hlc_wall: hlc_pair.wall,
        hlc_logical: hlc_pair.logical,
        origin_device_id,
    }))
}
