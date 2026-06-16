//! Backfill push direction — re-emit local rows the server's
//! digest doesn't see (Phase B.2 / RFC-003 §4).
//!
//! For each `canonical_id` flagged `missing_remotely`, read the
//! local row's canonical fields and enqueue an `insert` op via
//! the regular outbox path. The drain task picks it up on the
//! next pass (the orchestrator wakes the drain after a non-zero
//! push count so the user doesn't wait the 30 s tick).
//!
//! ## HLC clobber guard
//!
//! Re-emitting via [`crate::sync::hooks::enqueue_op_in_tx`] draws
//! a fresh HLC from [`crate::sync::hlc::next_conn`], so the
//! server side sees the row as a "now" write rather than a
//! replay of when the user actually edited. That's intentional —
//! the original HLC is lost on the queue side anyway; what
//! matters for §2 LWW resolution at the server is that the
//! re-emit doesn't go backwards. Since we always draw a fresh
//! monotonic HLC, this property holds by construction.
//!
//! ## Track is deferred
//!
//! The orchestrator in [`super::run_backfill`] short-circuits
//! `entity = "track"` before reaching this module — track's
//! composite canonical + album/artist plumbing needs its own
//! sub-PR.

use serde_json::{Map, Value};
use sqlx::{SqliteConnection, SqlitePool};

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::sync::digest::client::RemoteMaxHlc;
use crate::sync::digest::diff::DivergentMember;
use crate::sync::hooks::{self, EnqueuedStamp, PendingOpDraft};
use crate::sync::payload;

/// Counters returned to the orchestrator.
#[derive(Debug, Default)]
pub struct PushStats {
    pub pushed: u32,
    /// Reserved for a future HLC-guard implementation. Stays 0
    /// in the current always-push policy (see module banner).
    pub skipped_out_of_date: u32,
    pub failed: u32,
}

/// Push every member of `missing_remotely` for the given entity.
/// `_remote_max_hlc` is plumbed in for a future guard — see the
/// module banner.
pub async fn push_missing_remotely(
    state: &AppState,
    pool: &SqlitePool,
    entity: &str,
    missing: &[DivergentMember],
    _remote_max_hlc: Option<RemoteMaxHlc>,
) -> AppResult<PushStats> {
    let mut stats = PushStats::default();

    for member in missing {
        match push_one_by_canonical(state, pool, entity, &member.canonical_id).await {
            Ok(true) => stats.pushed += 1,
            Ok(false) => {
                tracing::debug!(
                    entity,
                    canonical_id = %member.canonical_id,
                    "backfill push: local row gone, skipping"
                );
            }
            Err(err) => {
                tracing::warn!(
                    entity,
                    canonical_id = %member.canonical_id,
                    error = %err,
                    "backfill push failed for row"
                );
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

/// Push one row by canonical_id. Pub(super) so [`super::lww`]
/// reuses it on the "local wins" branch.
pub(super) async fn push_one_by_canonical(
    state: &AppState,
    pool: &SqlitePool,
    entity: &str,
    canonical_id: &str,
) -> AppResult<bool> {
    match entity {
        "library" => push_library(state, pool, canonical_id).await,
        "playlist" => push_playlist(state, pool, canonical_id).await,
        "liked_track" => push_liked_track(state, pool, canonical_id).await,
        "track_rating" => push_track_rating(state, pool, canonical_id).await,
        other => Err(AppError::Other(format!(
            "push_one_by_canonical: unsupported entity '{other}'",
        ))),
    }
}

// ── Per-entity push implementations ───────────────────────────

/// Push a `library` row. Reads canonical fields via the existing
/// B.0a helper and enqueues an `insert` op against the row's
/// canonical id. Returns `Ok(true)` when the op was enqueued,
/// `Ok(false)` when the local row is gone.
async fn push_library(state: &AppState, pool: &SqlitePool, canonical_id: &str) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let local_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM library WHERE canonical_id = ?")
            .bind(canonical_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(local_id) = local_id else {
        return Ok(false);
    };
    let Some(fields) = payload::library::fields_from_row(&mut tx, local_id).await? else {
        return Ok(false);
    };
    enqueue_insert(
        &mut tx,
        "library",
        canonical_id,
        fields,
        local_id,
        Some(stamp_library),
    )
    .await?;
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

async fn push_playlist(state: &AppState, pool: &SqlitePool, canonical_id: &str) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let local_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM playlist WHERE canonical_id = ?")
            .bind(canonical_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(local_id) = local_id else {
        return Ok(false);
    };
    let Some(fields) = payload::playlist::fields_from_row(&mut tx, local_id).await? else {
        return Ok(false);
    };
    enqueue_insert(
        &mut tx,
        "playlist",
        canonical_id,
        fields,
        local_id,
        Some(stamp_playlist),
    )
    .await?;
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

/// `liked_track` canonical is the file_hash. The wire payload is
/// the empty map `{}`; the canonical_fields hash agrees.
async fn push_liked_track(
    state: &AppState,
    pool: &SqlitePool,
    file_hash: &str,
) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let track_id: Option<i64> = sqlx::query_scalar(
        "SELECT t.id FROM liked_track lt
            JOIN track t ON t.id = lt.track_id
           WHERE t.file_hash = ?",
    )
    .bind(file_hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(track_id) = track_id else {
        return Ok(false);
    };
    let fields = payload::liked_track::canonical_fields();
    let stamp = hooks::enqueue_op_in_tx(
        &mut tx,
        &PendingOpDraft {
            entity: "liked_track".into(),
            entity_id: file_hash.to_owned(),
            field: None,
            op: "insert".into(),
            payload: Some(Value::Object(fields)),
        },
    )
    .await?;
    if let Some(stamp) = stamp {
        payload::liked_track::stamp_in_tx(&mut tx, track_id, stamp).await?;
    }
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

/// `track_rating` canonical is also the file_hash. The wire
/// payload uses `{value: <0..=255>}` to match the apply pipeline's
/// existing shape; the canonical_fields used for the hash use
/// `{rating: <0..=255>}` (server's apply canonical for
/// track_rating).
async fn push_track_rating(
    state: &AppState,
    pool: &SqlitePool,
    file_hash: &str,
) -> AppResult<bool> {
    let mut tx = pool.begin().await?;
    let row: Option<(i64, Option<i64>)> =
        sqlx::query_as("SELECT id, rating FROM track WHERE file_hash = ?")
            .bind(file_hash)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((track_id, rating)) = row else {
        return Ok(false);
    };
    let Some(rating) = rating else {
        // No rating to push — the digest membership shouldn't
        // have flagged this row, but stay defensive.
        return Ok(false);
    };
    let fields = payload::track_rating::canonical_fields(rating);
    let stamp = hooks::enqueue_op_in_tx(
        &mut tx,
        &PendingOpDraft {
            entity: "track_rating".into(),
            entity_id: file_hash.to_owned(),
            field: None,
            op: "set".into(),
            payload: Some(serde_json::json!({ "value": rating })),
        },
    )
    .await?;
    if let Some(stamp) = stamp {
        payload::track_rating::stamp_set_in_tx(&mut tx, track_id, rating, stamp).await?;
    }
    let _ = fields;
    tx.commit().await?;
    let _ = state;
    Ok(true)
}

// ── Shared insert helper for library / playlist ───────────────

type StampFn = fn(
    &mut SqliteConnection,
    i64,
    Map<String, Value>,
    EnqueuedStamp,
) -> futures::future::BoxFuture<'_, AppResult<()>>;

/// Build + enqueue an `insert` op whose payload is the canonical
/// fields map, then stamp the row's HLC + payload_hash if the
/// queue write produced a stamp.
async fn enqueue_insert(
    conn: &mut SqliteConnection,
    entity: &str,
    canonical_id: &str,
    fields: Map<String, Value>,
    local_id: i64,
    stamp_fn: Option<StampFn>,
) -> AppResult<()> {
    let stamp = hooks::enqueue_op_in_tx(
        conn,
        &PendingOpDraft {
            entity: entity.to_owned(),
            entity_id: canonical_id.to_owned(),
            field: None,
            op: "insert".into(),
            payload: Some(Value::Object(fields.clone())),
        },
    )
    .await?;
    if let (Some(stamp), Some(stamp_fn)) = (stamp, stamp_fn) {
        stamp_fn(conn, local_id, fields, stamp).await?;
    }
    Ok(())
}

fn stamp_library(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> futures::future::BoxFuture<'_, AppResult<()>> {
    Box::pin(async move { payload::library::stamp_in_tx(conn, local_id, fields, stamp).await })
}

fn stamp_playlist(
    conn: &mut SqliteConnection,
    local_id: i64,
    fields: Map<String, Value>,
    stamp: EnqueuedStamp,
) -> futures::future::BoxFuture<'_, AppResult<()>> {
    Box::pin(async move { payload::playlist::stamp_in_tx(conn, local_id, fields, stamp).await })
}
