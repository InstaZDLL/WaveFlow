//! Glue between CRUD command handlers and the local sync queue.
//!
//! Every command in `commands/playlist.rs` that mutates a syncable
//! entity ends with a call to [`enqueue_op`] so the change lands in
//! the local `sync_pending_op` log alongside the SQLite write. A
//! future drain task (1.f.desktop.4) will POST these ops to the
//! waveflow-server's `/api/v1/sync/ops` endpoint.
//!
//! ## Failure model
//!
//! [`enqueue_op`] returns nothing — every failure is logged via
//! `tracing` and swallowed. Two reasons:
//!
//! 1. The user's CRUD operation already succeeded by the time we
//!    hit the queue. Aborting here would surface a confusing error
//!    ("your playlist was created but…") for what is internally a
//!    sync-pipeline hiccup.
//! 2. The future drain task (1.f.desktop.4) is the right place to
//!    detect server-view drift — it can compare the local state to
//!    the server's known set and prompt for a full re-sync if the
//!    operation_ids don't line up.
//!
//! ## Skip path
//!
//! When no `waveflow_server` JWT is stored for the active profile,
//! [`enqueue_op`] short-circuits without writing. A local-only user
//! never accumulates pending ops they'll never sync — the queue
//! stays empty until the first sign-in.

use crate::{
    server_client,
    state::AppState,
    sync::{lamport, queue},
};

pub use crate::sync::queue::PendingOpDraft;

/// Enqueue an op against the active profile's local sync queue.
///
/// Logs + swallows every failure mode (see the module docstring on
/// the failure model). The caller never has to `?` or `let _ =` —
/// the function shape is "fire and forget".
pub async fn enqueue_op(state: &AppState, draft: PendingOpDraft) {
    if let Err(err) = enqueue_op_inner(state, &draft).await {
        // The structured fields make a `tracing::error!` correlation
        // easy without dumping the (potentially large) payload into
        // the log — a future operator can re-fetch the row by
        // entity / op if needed.
        tracing::error!(
            error = %err,
            entity = %draft.entity,
            entity_id = %draft.entity_id,
            op = %draft.op,
            field = ?draft.field,
            "sync enqueue failed; server view will diverge until reconciled",
        );
    }
}

async fn enqueue_op_inner(state: &AppState, draft: &PendingOpDraft) -> crate::error::AppResult<()> {
    // Skip when no JWT is stored. Once 1.f.desktop.1 ships the
    // OAuth-loopback handshake and the user signs into a waveflow-
    // server, the very next CRUD operation flips this branch on and
    // the queue starts accumulating.
    if server_client::read_token(state).await?.is_none() {
        return Ok(());
    }
    let pool = state.require_profile_pool().await?;
    let lamport_ts = lamport::next(&pool).await?;
    queue::enqueue(&pool, draft, lamport_ts).await?;
    Ok(())
}
