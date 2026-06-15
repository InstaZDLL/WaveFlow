//! Drain task — pushes pending `sync_pending_op` rows to the
//! `waveflow-server`'s `POST /api/v1/sync/ops` endpoint and removes
//! the rows the server accepts. Phase 1.f.desktop.4a.
//!
//! ## Lifecycle
//!
//! Spawned once at boot via [`spawn`]. The returned [`DrainHandle`]
//! lives on [`AppState`] so any CRUD command can wake the task on
//! demand via [`DrainHandle::notify`] — typically the playlist
//! commands fire it after a successful `tx.commit()` so a chatty
//! user doesn't wait the full poll interval to see their changes
//! reach the server.
//!
//! ## Gates (cheap, evaluated per pass)
//!
//! 1. [`WaveflowServerClient::try_build`] — both the server URL and
//!    the active profile's JWT must be configured. A local-only or
//!    half-configured profile no-ops without any HTTP.
//! 2. [`mode::SyncMode::Hybrid`] — a profile explicitly set to
//!    `Local` keeps its queue local even when signed in.
//!
//! Either gate short-circuits to [`DrainOutcome::Skipped`].
//!
//! ## Failure semantics
//!
//! - **HTTP 200** — server accepted the batch. `drop_acked` removes
//!   the rows from the local queue; the loop runs again to see if
//!   more pending rows surfaced while we were posting.
//! - **HTTP 409** — `lamport_regression`. [`lamport::observe_remote`]
//!   bumps the local floor past the server's view; the offending row
//!   is `mark_failed`d so it surfaces in diagnostics; the pass
//!   breaks. The next iteration retries with the bumped clock.
//! - **Other HTTP statuses + network errors** — the batch is
//!   `mark_failed`d with the server reply for operator triage; the
//!   pass breaks. The periodic poll re-attempts later.
//!
//! ## What this PR does NOT ship
//!
//! - **WebSocket subscriber + apply remote ops** — 1.f.desktop.4b.
//!   This module's drain is one-way push only; the desktop's local
//!   SQLite stays the source of truth until the WS path lands.
//! - **Canonical-id mapping** — also 1.f.desktop.4b. Today's
//!   `entity_id` is the local i64 coerced to TEXT, which is fine for
//!   the push direction (server keys ops on `(user_id, device_id,
//!   entity, entity_id)`, so different devices' ops live in disjoint
//!   namespaces). Cross-device replay needs a separate `local_id ↔
//!   canonical_id` table the WS subscriber will introduce.

use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

use crate::{
    error::AppResult,
    server_client::WaveflowServerClient,
    state::AppState,
    sync::{device, lamport, mode, queue},
};

/// How often the task wakes on its own. A user-driven push notifies
/// the task immediately, so this is really just the floor for "we
/// went a while without any activity, are there any retries to
/// attempt?". 30 s is comfortable for a typical edit cadence.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Max ops per HTTP request. Mirrors waveflow-server's
/// `MAX_BATCH_SIZE` (1024) but a smaller floor keeps a flaky server
/// from holding a huge batch in flight.
const BATCH_SIZE: i64 = 100;

/// Wake-up signal the rest of the app uses to nudge the drain task
/// after a successful enqueue. Internally a `tokio::sync::Notify`,
/// which means `notify_one` is a cheap atomic flag set — never
/// blocks the caller, never drops a permit even if no waiter is
/// currently parked (the next `notified().await` resolves
/// immediately).
#[derive(Default)]
pub struct DrainHandle {
    notifier: Notify,
}

impl DrainHandle {
    /// Wake the drain task on the next iteration.
    pub fn notify(&self) {
        self.notifier.notify_one();
    }
}

/// Result of a single drain pass. Surface for the diagnostic
/// `sync_drain_now` Tauri command.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DrainOutcome {
    /// The gates short-circuited the pass. No HTTP round-trip.
    Skipped,
    /// The pass ran; `sent` is the number of ops the server
    /// accepted, `remaining` is the queue depth observed after the
    /// pass completed.
    Drained { sent: usize, remaining: i64 },
}

// ── Wire shape mirroring waveflow-server's sync types ────────────

/// Mirrors `waveflow_server::sync::Hlc`. RFC-003 v2 pair shipped
/// under `SyncOpIn.hlc: Option<Hlc>` (waveflow-server #52). The
/// server-side dual-shape ingest derives `(0, lamport_ts)` for v1
/// (legacy) rows that omit this key, so the desktop can ship either
/// form without coordinating a release.
#[derive(Debug, Serialize)]
struct WireHlc {
    wall: i64,
    logical: i32,
}

/// Mirrors `waveflow_server::sync::SyncOpIn`.
#[derive(Debug, Serialize)]
struct SyncOpInBody {
    operation_id: String,
    lamport_ts: i64,
    entity: String,
    entity_id: String,
    field: Option<String>,
    op: String,
    payload: Option<serde_json::Value>,
    /// Active profile's canonical UUID (Phase 1.g.3-desktop). The
    /// server's apply pipeline (PR #26) routes each op to a
    /// materialised server profile via this field. Always `Some` on
    /// outbound pushes — the drain gates on it before serialising
    /// the batch — but kept `Option` so the JSON omits the key when
    /// a hypothetical future caller wants the legacy shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_canonical_id: Option<String>,
    /// RFC-003 Phase A.4.2 — v2 wire shape. `Some` when the row in
    /// `sync_pending_op` was enqueued with a non-zero pair (any
    /// post-A.4.2 enqueue); `None` for legacy (0, 0) rows where the
    /// server's dual-shape ingest will derive `(0, lamport_ts)`
    /// itself. The key is omitted from the JSON when `None` so the
    /// wire stays bit-identical to the v1 form when there's nothing
    /// to send.
    #[serde(skip_serializing_if = "Option::is_none")]
    hlc: Option<WireHlc>,
}

/// Lift a pending row's `(hlc_wall, hlc_logical)` pair onto the
/// outbound wire field. Pre-A.4.2 rows backfill to `(0, 0)` via the
/// migration DEFAULT — `None` rather than `Some(0, 0)` keeps the
/// JSON identical to the legacy form, so the server picks the v1
/// dual-shape branch and derives `(0, lamport_ts)` itself.
///
/// The gate is `wall == 0` rather than `wall == 0 && logical == 0`
/// because `hlc::next_conn` ALWAYS produces a `wall = max(now_ms,
/// last_wall)` that's strictly greater than zero (epoch-ms in 2026
/// is ~1.7e12). A row carrying `(0, n > 0)` can only arise from
/// corruption or a manual edit — emitting it on the wire as a v2
/// HLC with `wall = 0` (= January 1970) would feed the server's
/// LWW comparator an ancient pair that loses against every legit
/// op. Falling back to the v1 shape lets the server derive a
/// usable `(0, lamport_ts)` from the row's Lamport instead.
fn wire_hlc_from_row(wall: i64, logical: i32) -> Option<WireHlc> {
    if wall == 0 {
        None
    } else {
        Some(WireHlc { wall, logical })
    }
}

/// Mirrors `waveflow_server::api::sync::PushBatchRequest`.
#[derive(Debug, Serialize)]
struct PushBatchRequest<'a> {
    device_id: &'a str,
    ops: Vec<SyncOpInBody>,
}

/// Subset of `waveflow_server::api::sync::LamportRegression` we
/// actually consume. `error` + `device_id` are echoed for diagnostic
/// log lines; `stored_max` drives [`lamport::observe_remote`] and
/// `offending_lamport_ts` drives the per-row mark_failed.
#[derive(Debug, Deserialize)]
struct LamportRegression {
    #[allow(dead_code)]
    error: String,
    device_id: String,
    stored_max: i64,
    offending_lamport_ts: i64,
}

// ── Public surface ───────────────────────────────────────────────

/// Spawn the drain task. The wake handle is already in
/// [`AppState::drain`] — both sides of the notification (CRUD
/// command sites + the task itself) clone the same `Arc<DrainHandle>`
/// so a single `notify_one` wakes the task regardless of which
/// caller fires it.
pub fn spawn(app: AppHandle) {
    let task_handle = app.state::<AppState>().drain.clone();
    // Tauri 2's `setup` callback runs without an ambient tokio
    // runtime, so a bare `tokio::spawn` panics with "no reactor
    // running". `tauri::async_runtime::spawn` resolves to the
    // runtime Tauri configures internally (tokio under the hood)
    // and is safe to call from the setup hook.
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // First tick fires immediately — burn it so we don't spam
        // the server on startup before the user has done anything.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = task_handle.notifier.notified() => {}
            }
            let state = app.state::<AppState>();
            // Serialise against the `sync_drain_now` Tauri command
            // — a manual user-driven push racing this background
            // tick would otherwise read the same `sync_pending_op`
            // rows and POST them twice (server absorbs the
            // duplicates via the `operation_id` UNIQUE, but the
            // wasted round-trip + duplicated accounting is
            // avoidable). The lock is held across the whole pass;
            // dropped automatically when `_guard` falls out of
            // scope at the end of the match.
            let _guard = state.drain_lock.lock().await;
            match drain_once(&state).await {
                Ok(DrainOutcome::Skipped) => {
                    // Common path when not signed in / not Hybrid —
                    // silent, no log spam.
                }
                Ok(DrainOutcome::Drained { sent, remaining }) => {
                    if sent > 0 || remaining > 0 {
                        tracing::debug!(sent, remaining, "sync drain pass completed",);
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "sync drain pass failed");
                }
            }
        }
    });
}

/// Run one drain pass synchronously. Exposed so the
/// `sync_drain_now` Tauri command (Settings diagnostics) can force
/// an immediate attempt without waiting for the periodic tick.
pub async fn drain_once(state: &AppState) -> AppResult<DrainOutcome> {
    // Gate 1: client buildable. Both the server URL and the active
    // profile's JWT must be configured.
    let Some(client) = WaveflowServerClient::try_build(state).await? else {
        return Ok(DrainOutcome::Skipped);
    };
    let pool = state.require_profile_pool().await?;
    // Gate 2: per-profile mode = Hybrid.
    if mode::read(&pool).await? != mode::SyncMode::Hybrid {
        return Ok(DrainOutcome::Skipped);
    }
    let device_id = device::ensure(&state.app_db).await?;

    // Gate 3 (Phase 1.g.3) — the server's apply pipeline (PR #26)
    // routes each op to a materialised profile via
    // `profile_canonical_id`. Without it, the durable log still
    // accepts the op, but apply silently skips — i.e. the desktop
    // would "succeed" in pushing while the server's entity tables
    // stay empty. Better to defer the push until the canonical id
    // is backfilled (background job, at most one boot away) than
    // to drop ops on the floor.
    let profile_id = state.require_profile_id().await?;
    let Some(profile_canonical_id) =
        crate::db::profile_meta::canonical_id_for(&state.app_db, profile_id).await?
    else {
        tracing::warn!(
            profile_id,
            "drain: profile.canonical_id is NULL — backfill pending, skipping push",
        );
        return Ok(DrainOutcome::Skipped);
    };

    let mut total_sent = 0usize;
    loop {
        let pending = queue::list_pending(&pool, BATCH_SIZE).await?;
        if pending.is_empty() {
            break;
        }

        let ops: Vec<SyncOpInBody> = pending
            .iter()
            .map(|p| SyncOpInBody {
                operation_id: p.operation_id.clone(),
                lamport_ts: p.lamport_ts,
                entity: p.entity.clone(),
                entity_id: p.entity_id.clone(),
                field: p.field.clone(),
                op: p.op.clone(),
                payload: p.payload.clone(),
                profile_canonical_id: Some(profile_canonical_id.clone()),
                hlc: wire_hlc_from_row(p.hlc_wall, p.hlc_logical),
            })
            .collect();
        let body = PushBatchRequest {
            device_id: &device_id,
            ops,
        };

        let resp = match client
            .request(reqwest::Method::POST, "/api/v1/sync/ops")
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(err) => {
                // Network-level failure — mark the batch failed for
                // diagnostics and break the loop. The periodic pass
                // will retry; we don't `?` because a transient DNS
                // hiccup shouldn't surface as a hard error from the
                // drain task.
                let summary = format!("network: {err}");
                for p in &pending {
                    let _ = queue::mark_failed(&pool, p.id, &summary).await;
                }
                tracing::warn!(error = %err, "sync push: network failure");
                break;
            }
        };

        match resp.status() {
            StatusCode::OK => {
                let ids: Vec<i64> = pending.iter().map(|p| p.id).collect();
                queue::drop_acked(&pool, &ids).await?;
                total_sent += pending.len();
                // Continue the loop to drain any rows that surfaced
                // while we were posting.
            }
            StatusCode::CONFLICT => {
                let regression: LamportRegression = match resp.json().await {
                    Ok(b) => b,
                    Err(err) => {
                        // A 409 we can't parse is still a server
                        // rejection — leaving the rows untouched
                        // would just have us hit the same 409 next
                        // pass forever. Mark the batch failed so it
                        // surfaces in diagnostics + the
                        // `attempt_count` climbs, then break.
                        let summary = format!("409 body parse failed: {err}");
                        tracing::warn!(
                            error = %err,
                            "sync push: 409 body did not parse as LamportRegression",
                        );
                        for p in &pending {
                            let _ = queue::mark_failed(&pool, p.id, &summary).await;
                        }
                        break;
                    }
                };
                lamport::observe_remote(&pool, regression.stored_max).await?;
                if let Some(off) = pending
                    .iter()
                    .find(|p| p.lamport_ts == regression.offending_lamport_ts)
                {
                    let _ = queue::mark_failed(
                        &pool,
                        off.id,
                        &format!("lamport_regression stored_max={}", regression.stored_max,),
                    )
                    .await;
                }
                tracing::warn!(
                    device_id = %regression.device_id,
                    stored_max = regression.stored_max,
                    offending = regression.offending_lamport_ts,
                    "sync push: lamport regression; bumped local clock",
                );
                break;
            }
            other => {
                let body_text = resp.text().await.unwrap_or_default();
                tracing::warn!(
                    status = %other,
                    body = %body_text,
                    "sync push: server rejected batch",
                );
                let summary = format!("HTTP {other}: {body_text}");
                for p in &pending {
                    let _ = queue::mark_failed(&pool, p.id, &summary).await;
                }
                break;
            }
        }
    }

    let remaining = queue::count_pending(&pool).await?;
    Ok(DrainOutcome::Drained {
        sent: total_sent,
        remaining,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_outcome_serialises_with_snake_case_tag() {
        let skipped = DrainOutcome::Skipped;
        let drained = DrainOutcome::Drained {
            sent: 3,
            remaining: 7,
        };
        // Tag is `snake_case`-renamed so the frontend's
        // discriminated-union matcher reads `outcome === "drained"`
        // rather than `"Drained"`.
        let s = serde_json::to_value(skipped).unwrap();
        assert_eq!(s, serde_json::json!({"outcome": "skipped"}));
        let d = serde_json::to_value(drained).unwrap();
        assert_eq!(
            d,
            serde_json::json!({
                "outcome": "drained",
                "sent": 3,
                "remaining": 7,
            }),
        );
    }

    #[test]
    fn push_batch_request_body_matches_server_wire_shape_v1_legacy() {
        // Legacy (v1) wire shape — `hlc` field stays absent so the
        // server's dual-shape ingest derives `(0, lamport_ts)`. The
        // desktop falls into this branch for rows enqueued before
        // Phase A.4.2 (DEFAULT 0/0 columns mean
        // `wire_hlc_from_row` returns `None`).
        let body = PushBatchRequest {
            device_id: "device-a",
            ops: vec![SyncOpInBody {
                operation_id: "00000000-0000-0000-0000-000000000001".into(),
                lamport_ts: 42,
                entity: "playlist".into(),
                entity_id: "7".into(),
                field: Some("name".into()),
                op: "set".into(),
                payload: Some(serde_json::json!({ "value": "Soirée" })),
                profile_canonical_id: Some("11111111-2222-4333-8444-555555555555".into()),
                hlc: None,
            }],
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "device_id": "device-a",
                "ops": [{
                    "operation_id": "00000000-0000-0000-0000-000000000001",
                    "lamport_ts": 42,
                    "entity": "playlist",
                    "entity_id": "7",
                    "field": "name",
                    "op": "set",
                    "payload": { "value": "Soirée" },
                    "profile_canonical_id": "11111111-2222-4333-8444-555555555555",
                }],
            }),
        );
    }

    #[test]
    fn push_batch_request_body_carries_hlc_on_v2_wire_shape() {
        // Phase A.4.2 v2 shape — `hlc: { wall, logical }` rides on
        // the op. `device_id` doubles as the §2 tiebreaker
        // `origin_device_id` (UUID-shaped TEXT round-trip per the
        // A.1.1 server header), so no separate wire key is needed.
        let body = PushBatchRequest {
            device_id: "11111111-1111-1111-1111-111111111111",
            ops: vec![SyncOpInBody {
                operation_id: "00000000-0000-0000-0000-000000000002".into(),
                lamport_ts: 5,
                entity: "playlist".into(),
                entity_id: "9".into(),
                field: None,
                op: "insert".into(),
                payload: Some(serde_json::json!({ "name": "Mix" })),
                profile_canonical_id: Some("22222222-3333-4444-8555-666666666666".into()),
                hlc: Some(WireHlc {
                    wall: 1_700_000_000_001,
                    logical: 3,
                }),
            }],
        };
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "device_id": "11111111-1111-1111-1111-111111111111",
                "ops": [{
                    "operation_id": "00000000-0000-0000-0000-000000000002",
                    "lamport_ts": 5,
                    "entity": "playlist",
                    "entity_id": "9",
                    "field": null,
                    "op": "insert",
                    "payload": { "name": "Mix" },
                    "profile_canonical_id": "22222222-3333-4444-8555-666666666666",
                    "hlc": { "wall": 1_700_000_000_001_i64, "logical": 3 },
                }],
            }),
        );
    }

    #[test]
    fn wire_hlc_from_row_gates_on_wall_alone() {
        // wall == 0 → None: covers both the (0, 0) backfill DEFAULT
        // and the (0, n > 0) corruption case where emitting a v2
        // hlc with wall=0 would feed the server an ancient
        // 1970-epoch pair that loses every LWW comparison.
        assert!(wire_hlc_from_row(0, 0).is_none());
        assert!(wire_hlc_from_row(0, 1).is_none());
        assert!(wire_hlc_from_row(0, i32::MAX).is_none());
        // wall > 0 → Some: the production path (hlc::next_conn
        // produces wall = max(now_ms, last_wall) so wall is always
        // ~1.7e12 in 2026).
        let hlc = wire_hlc_from_row(1, 0).unwrap();
        assert_eq!((hlc.wall, hlc.logical), (1, 0));
        let hlc = wire_hlc_from_row(123, 456).unwrap();
        assert_eq!((hlc.wall, hlc.logical), (123, 456));
    }

    #[test]
    fn lamport_regression_parses_server_409_body() {
        // Mirrors the JSON waveflow-server's `LamportRegression`
        // serialises. Renaming a field on either side trips this
        // test before a regression lands in tracked code paths.
        let body = serde_json::json!({
            "error": "lamport_regression",
            "device_id": "device-a",
            "stored_max": 11,
            "offending_lamport_ts": 10,
        });
        let parsed: LamportRegression = serde_json::from_value(body).unwrap();
        assert_eq!(parsed.device_id, "device-a");
        assert_eq!(parsed.stored_max, 11);
        assert_eq!(parsed.offending_lamport_ts, 10);
    }
}
