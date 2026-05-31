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

use crate::{
    error::AppResult,
    server_client,
    sync::{lamport, mode, queue},
};

pub use crate::sync::queue::PendingOpDraft;

/// Atomically enqueue an op against a caller-owned connection
/// (typically an open `Transaction<'_, Sqlite>` borrowed as
/// `&mut *tx`). Composes with the entity mutation in the same
/// transaction so the playlist write + the outbox row + the Lamport
/// bump either ALL land or ALL roll back.
///
/// Returns `true` when an op was actually inserted, `false` when
/// the skip conditions kicked in (no JWT for the active profile,
/// or `SyncMode::Local`). Callers can use the boolean to decide
/// whether to log a "synced" trace, but the truth-source for the
/// queue is still the row itself.
pub async fn enqueue_op_in_tx(
    conn: &mut SqliteConnection,
    draft: &PendingOpDraft,
) -> AppResult<bool> {
    if server_client::read_token_conn(conn).await?.is_none() {
        return Ok(false);
    }
    if mode::read_conn(conn).await? == mode::SyncMode::Local {
        return Ok(false);
    }
    let lamport_ts = lamport::next_conn(conn).await?;
    queue::enqueue_conn(conn, draft, lamport_ts).await?;
    Ok(true)
}
