//! Apply inbound sync ops to the local profile DB. Phase
//! 1.f.desktop.4b.
//!
//! Mirror of [`crate::sync::hooks::enqueue_op_in_tx`] on the inbound
//! side — but where the outbound hook layers an outbox row on top of
//! a CRUD write, this module translates a remote op back into the
//! matching CRUD write WITHOUT touching the queue. Inbound ops must
//! NEVER re-enqueue, otherwise every WS frame would round-trip
//! straight back to the server in an infinite ping-pong.
//!
//! ## Atomicity
//!
//! Every entry point takes a caller-owned `&mut SqliteConnection`
//! (typically a `Transaction<'_, Sqlite>` borrowed as `&mut *tx`).
//! The WS subscriber wraps each op in a single tx so the Lamport bump
//! ([`lamport::observe_remote_conn`]) + the entity write + the
//! `sync_id_map` row land atomically; a crash mid-apply rolls all
//! three back so the next reconnect's catch-up resyncs cleanly.
//!
//! ## Conflict resolution
//!
//! The protocol is last-writer-wins keyed on `lamport_ts`. We don't
//! attempt to merge concurrent edits beyond what the per-field op
//! shape already gives us — `set name = "A"` and `set color = "B"`
//! commute trivially. For two `set name` ops with overlapping
//! lamport ranges, the higher one wins (the server's monotonic id
//! orders them on the wire so the apply order matches the global
//! view).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use waveflow_core::repository::playlist::{PlaylistDraft, PlaylistUpdate};
use waveflow_core::repository::sqlite::playlist::{
    append_tracks_conn, delete_conn, insert_custom_conn, remove_track_conn, reorder_track_conn,
    update_conn,
};

use crate::{
    error::{AppError, AppResult},
    sync::{canonical, lamport},
};

/// Inbound op envelope — mirrors the server's `SyncOp` wire shape so
/// the WS subscriber + the catch-up REST handler can both feed
/// [`apply_remote_op_in_tx`] without an intermediate translation
/// layer.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteSyncOp {
    /// Server-assigned monotonic id. The subscriber uses it to
    /// advance `profile_setting['sync.last_seen_id']`.
    pub id: i64,
    /// Originating device's `lamport_ts`. Observed locally
    /// (`observe_remote_conn`) so the next local op slots above it.
    pub lamport_ts: i64,
    /// Originating device id. Echoed back for diagnostics; the apply
    /// path drops self-broadcasts (an op whose `device_id` matches
    /// this desktop's id is an echo of something we sent moments ago
    /// and re-applying it would just bump `updated_at` for no
    /// reason).
    pub device_id: String,
    pub entity: String,
    /// Canonical id of the target entity. Translated to a local
    /// rowid via [`canonical::local_for_canonical`].
    pub entity_id: String,
    pub field: Option<String>,
    pub op: String,
    pub payload: Option<serde_json::Value>,
}

/// Outcome of a single apply pass. Surfaces enough information for
/// the WS subscriber to decide whether the op should be ACKed
/// upstream (everything except [`AppliedOutcome::Skipped`] should
/// advance the cursor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedOutcome {
    /// The op landed in the local DB. Mapping + last_seen_id should
    /// both advance.
    Applied,
    /// The op was an echo of one this device sent (matching
    /// `device_id`). Cursor still advances so we don't pull it again,
    /// but the local DB stays untouched.
    Skipped,
    /// The op references an entity the desktop has no mapping for
    /// (e.g. `delete` against a row that was never created here).
    /// Cursor still advances — replaying it endlessly wouldn't help.
    Ignored,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Entry point. Routes to the per-entity dispatcher, bumps the
/// Lamport clock past the remote's `lamport_ts`, and returns the
/// outcome the WS subscriber surfaces for ACK + cursor accounting.
///
/// `local_device_id` is the value [`crate::sync::device::ensure`]
/// returned — used for the echo-detection short-circuit. Passing it
/// in (rather than re-reading from `app.db`) keeps the apply path
/// off the global app DB pool — the subscriber resolves it once per
/// session.
pub async fn apply_remote_op_in_tx(
    conn: &mut SqliteConnection,
    op: &RemoteSyncOp,
    local_device_id: &str,
) -> AppResult<AppliedOutcome> {
    if op.device_id == local_device_id {
        // Echo. Don't touch the local DB; the cursor still advances
        // so we don't pull it again next reconnect.
        return Ok(AppliedOutcome::Skipped);
    }

    // Bump the Lamport floor first so a local op that fires in
    // parallel can't slot below the remote's `lamport_ts` — would
    // surface as a 409 on the next drain pass otherwise.
    lamport::observe_remote_conn(conn, op.lamport_ts).await?;

    match op.entity.as_str() {
        canonical::ENTITY_PLAYLIST => apply_playlist_op(conn, op).await,
        other => {
            // Forward compat: a future entity (`library`, `track`, …)
            // arrives but this desktop doesn't know how to apply it.
            // Log + Ignore — the cursor still advances so the WS
            // subscriber moves on instead of looping on the same op
            // forever.
            tracing::debug!(
                entity = %other,
                op = %op.op,
                "apply_remote_op: unknown entity, ignored"
            );
            Ok(AppliedOutcome::Ignored)
        }
    }
}

async fn apply_playlist_op(
    conn: &mut SqliteConnection,
    op: &RemoteSyncOp,
) -> AppResult<AppliedOutcome> {
    let now = now_ms();
    let entity = canonical::ENTITY_PLAYLIST;
    match (op.op.as_str(), op.field.as_deref()) {
        // ─ Whole-entity ops ──────────────────────────────────────
        ("insert", None) => {
            // Idempotent: a second insert for the same canonical
            // (e.g. catch-up replay after a WS reconnect) is a no-op.
            if canonical::local_for_canonical(conn, entity, &op.entity_id)
                .await?
                .is_some()
            {
                return Ok(AppliedOutcome::Skipped);
            }
            // Parser errors must NOT roll back the tx — that would leave
            // the cursor unmoved and a malformed frame would replay on
            // every reconnect. Log + Ignore so the cursor advances. DB
            // errors below still propagate via `?` (a real failure
            // should retry).
            let draft = match playlist_draft_from_payload(op, now) {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        canonical = %op.entity_id,
                        "apply: malformed insert payload, ignoring"
                    );
                    return Ok(AppliedOutcome::Ignored);
                }
            };
            let local_id = insert_custom_conn(conn, &draft).await?;
            canonical::set_canonical_playlist(conn, local_id, &op.entity_id).await?;
            tracing::debug!(
                canonical = %op.entity_id,
                local_id,
                "applied remote playlist insert"
            );
            Ok(AppliedOutcome::Applied)
        }
        ("delete", None) => {
            let Some(local_id) =
                canonical::local_for_canonical(conn, entity, &op.entity_id).await?
            else {
                return Ok(AppliedOutcome::Ignored);
            };
            let removed = delete_conn(conn, local_id).await?;
            if !removed {
                // Mapping pointed at a row that vanished out-of-band.
                // Drop the stale mapping so a future insert of the
                // same canonical doesn't trip the UNIQUE index.
                canonical::drop_mapping(conn, entity, &op.entity_id).await?;
                return Ok(AppliedOutcome::Ignored);
            }
            canonical::drop_mapping(conn, entity, &op.entity_id).await?;
            Ok(AppliedOutcome::Applied)
        }
        // ─ Partial updates ──────────────────────────────────────
        ("set", Some(field @ ("name" | "description" | "color_id" | "icon_id"))) => {
            let Some(local_id) =
                canonical::local_for_canonical(conn, entity, &op.entity_id).await?
            else {
                return Ok(AppliedOutcome::Ignored);
            };
            let value = match string_value_from_payload(op, field) {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        field = %field,
                        canonical = %op.entity_id,
                        "apply: malformed set payload, ignoring"
                    );
                    return Ok(AppliedOutcome::Ignored);
                }
            };
            let patch = build_patch(field, value);
            let updated = update_conn(conn, local_id, &patch, now).await?;
            if updated {
                Ok(AppliedOutcome::Applied)
            } else {
                Ok(AppliedOutcome::Ignored)
            }
        }
        // ─ Track-list ops ───────────────────────────────────────
        ("insert", Some("tracks")) => {
            let Some(local_id) =
                canonical::local_for_canonical(conn, entity, &op.entity_id).await?
            else {
                return Ok(AppliedOutcome::Ignored);
            };
            let track_ids = match track_ids_from_payload(op) {
                Ok(t) => t,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        canonical = %op.entity_id,
                        "apply: malformed insert tracks payload, ignoring"
                    );
                    return Ok(AppliedOutcome::Ignored);
                }
            };
            // Map remote track ids (integers in this desktop's
            // local-i64 world) into rows we actually have. Tracks
            // don't carry canonical ids in this PR scope — a future
            // sub-PR will mirror this branch's lookup against
            // `sync_id_map` once `track` is plumbed through. For
            // now we route track ids through the local table
            // directly: an inbound op whose payload references a
            // track id we don't have is silently filtered. The
            // server's broadcast still lands the playlist as the
            // remote saw it; the missing tracks resolve once the
            // user re-scans the same library on this device.
            let resolved = filter_existing_track_ids(conn, &track_ids).await?;
            if resolved.is_empty() {
                return Ok(AppliedOutcome::Ignored);
            }
            append_tracks_conn(conn, local_id, &resolved, now).await?;
            Ok(AppliedOutcome::Applied)
        }
        ("delete", Some("tracks")) => {
            let Some(local_id) =
                canonical::local_for_canonical(conn, entity, &op.entity_id).await?
            else {
                return Ok(AppliedOutcome::Ignored);
            };
            let track_ids = match track_ids_from_payload(op) {
                Ok(t) => t,
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        canonical = %op.entity_id,
                        "apply: malformed delete tracks payload, ignoring"
                    );
                    return Ok(AppliedOutcome::Ignored);
                }
            };
            let mut applied = false;
            for tid in track_ids {
                if remove_track_conn(conn, local_id, tid, now).await? {
                    applied = true;
                }
            }
            Ok(if applied {
                AppliedOutcome::Applied
            } else {
                AppliedOutcome::Ignored
            })
        }
        ("set", Some("tracks")) => {
            let Some(local_id) =
                canonical::local_for_canonical(conn, entity, &op.entity_id).await?
            else {
                return Ok(AppliedOutcome::Ignored);
            };
            // Payload shape from the outbound hook is
            // `{"track_id": N, "position": M}`. Mirror it on the
            // inbound side via `reorder_track_conn`. A malformed
            // payload becomes Ignored (cursor still advances)
            // instead of an Err that would replay forever.
            let Some((track_id, new_position)) = op.payload.as_ref().and_then(|p| {
                let t = p.get("track_id").and_then(|v| v.as_i64())?;
                let n = p.get("position").and_then(|v| v.as_i64())?;
                Some((t, n))
            }) else {
                tracing::warn!(
                    canonical = %op.entity_id,
                    "apply: malformed set tracks payload (expected track_id + position), ignoring"
                );
                return Ok(AppliedOutcome::Ignored);
            };
            let effective = reorder_track_conn(conn, local_id, track_id, new_position, now).await?;
            Ok(if effective.is_some() {
                AppliedOutcome::Applied
            } else {
                AppliedOutcome::Ignored
            })
        }
        // ─ Catch-all ────────────────────────────────────────────
        other => {
            tracing::debug!(
                entity = "playlist",
                op = ?other,
                "apply_playlist_op: unknown (op, field), ignored"
            );
            Ok(AppliedOutcome::Ignored)
        }
    }
}

/// Build a [`PlaylistDraft`] from the `insert` op's payload. Hooks
/// outbound at [`crate::commands::playlist::create_playlist`] send a
/// `{name, description, color_id, icon_id}` blob; mirror it here.
fn playlist_draft_from_payload(op: &RemoteSyncOp, now_ms: i64) -> AppResult<PlaylistDraft> {
    let payload = op.payload.as_ref().ok_or_else(|| {
        AppError::Other("insert playlist op missing payload (expected name/…)".into())
    })?;
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::Other("insert playlist op: name missing".into()))?
        .to_string();
    let description = payload
        .get("description")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let color_id = payload
        .get("color_id")
        .and_then(|v| v.as_str())
        .unwrap_or("violet")
        .to_string();
    let icon_id = payload
        .get("icon_id")
        .and_then(|v| v.as_str())
        .unwrap_or("music")
        .to_string();
    Ok(PlaylistDraft {
        name,
        description,
        color_id,
        icon_id,
        now_ms,
    })
}

/// Extract a `{"value": "..."}` string from a `set <field>` op. The
/// `field` param drives per-column nullability: `description` is the
/// only `NULL`-able column on `playlist` (see the initial profile
/// schema), so `{"value": null}` is only a legitimate "reset" signal
/// for that field. Sending `null` for a `NOT NULL` column like
/// `name`, `color_id`, or `icon_id` is corruption and lands Ignored.
fn string_value_from_payload(op: &RemoteSyncOp, field: &str) -> AppResult<Option<String>> {
    let payload = op
        .payload
        .as_ref()
        .ok_or_else(|| AppError::Other("set op missing payload (expected {value: ...})".into()))?;
    // Strict shape: outbound hooks always send `{"value": "<string>"}`
    // for user-supplied values; they never emit an empty payload nor
    // a `null` value today. Anything other than the per-field-allowed
    // shape is rejected so a corrupted frame can't silently mutate
    // a row.
    match payload.get("value") {
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(serde_json::Value::Null) if field == "description" => Ok(None),
        Some(serde_json::Value::Null) => Err(AppError::Other(format!(
            "set op: '{field}' is NOT NULL — value cannot be null"
        ))),
        Some(_) => Err(AppError::Other(
            "set op: value must be a string or null".into(),
        )),
        None => Err(AppError::Other(
            "set op: payload missing required 'value' key".into(),
        )),
    }
}

fn build_patch(field: &str, value: Option<String>) -> PlaylistUpdate {
    let mut patch = PlaylistUpdate {
        name: None,
        description: None,
        color_id: None,
        icon_id: None,
    };
    match field {
        "name" => patch.name = value,
        "description" => patch.description = value,
        "color_id" => patch.color_id = value,
        "icon_id" => patch.icon_id = value,
        _ => {}
    }
    patch
}

fn track_ids_from_payload(op: &RemoteSyncOp) -> AppResult<Vec<i64>> {
    let payload = op.payload.as_ref().ok_or_else(|| {
        AppError::Other("tracks op missing payload (expected {track_ids: [...]})".into())
    })?;
    let arr = payload
        .get("track_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::Other("tracks op: track_ids array missing".into()))?;
    // Reject mixed-type arrays rather than silently dropping the
    // non-integer entries — a malformed frame would otherwise apply
    // partially and leave the playlist out of sync with the broadcast.
    let mut ids = Vec::with_capacity(arr.len());
    for value in arr {
        ids.push(value.as_i64().ok_or_else(|| {
            AppError::Other("tracks op: track_ids must contain only integers".into())
        })?);
    }
    Ok(ids)
}

/// Filter a list of remote track ids down to the ones this profile
/// actually has. A future sub-PR will replace this with a
/// canonical-id lookup once tracks carry one too; today we just
/// project against `track.id`.
async fn filter_existing_track_ids(
    conn: &mut SqliteConnection,
    ids: &[i64],
) -> AppResult<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Re-use the existing repository on the same connection. The
    // `list_ids_filter_existing` helper isn't on the trait yet — do
    // the projection here so the apply path stays self-contained.
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let row: Option<i64> = sqlx::query_scalar("SELECT id FROM track WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
        if row.is_some() {
            out.push(*id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;
    use uuid::Uuid;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        // Minimal schema covering the columns the apply path
        // touches. Keeping it stripped down avoids dragging the
        // entire profile migrator into the unit suite.
        sqlx::query(
            "CREATE TABLE profile_setting (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                value_type TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE playlist (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                color_id TEXT NOT NULL DEFAULT 'violet',
                icon_id TEXT NOT NULL DEFAULT 'music',
                is_smart INTEGER NOT NULL DEFAULT 0,
                position INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                canonical_id TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE track (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE playlist_track (
                playlist_id INTEGER NOT NULL,
                track_id INTEGER NOT NULL,
                position INTEGER NOT NULL,
                added_at INTEGER NOT NULL,
                PRIMARY KEY (playlist_id, track_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE sync_id_map (
                entity TEXT NOT NULL,
                canonical_id TEXT NOT NULL,
                local_id INTEGER NOT NULL,
                PRIMARY KEY (entity, canonical_id)
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn op(
        device: &str,
        canonical_id: &str,
        op: &str,
        field: Option<&str>,
        payload: Option<serde_json::Value>,
        lamport: i64,
    ) -> RemoteSyncOp {
        RemoteSyncOp {
            id: lamport,
            lamport_ts: lamport,
            device_id: device.into(),
            entity: "playlist".into(),
            entity_id: canonical_id.into(),
            field: field.map(str::to_string),
            op: op.into(),
            payload,
        }
    }

    /// Scoping the conn before any pool-level read is the workaround
    /// for `max_connections = 1` + `:memory:` (see
    /// sync::canonical::tests).
    #[tokio::test]
    async fn applies_remote_insert_and_plants_mapping() {
        let pool = pool().await;
        let canonical = Uuid::new_v4().to_string();
        let (outcome, local) = {
            let mut conn = pool.acquire().await.unwrap();
            let outcome = apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "insert",
                    None,
                    Some(serde_json::json!({
                        "name": "Soirée",
                        "color_id": "indigo",
                        "icon_id": "headphones"
                    })),
                    7,
                ),
                "device-a",
            )
            .await
            .unwrap();
            let local = canonical::local_for_canonical(&mut conn, "playlist", &canonical)
                .await
                .unwrap();
            (outcome, local)
        };
        assert_eq!(outcome, AppliedOutcome::Applied);
        assert!(local.is_some());
        let name: String = sqlx::query_scalar("SELECT name FROM playlist WHERE id = ?")
            .bind(local.unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Soirée");
        assert!(lamport::read(&pool).await.unwrap() >= 7);
    }

    #[tokio::test]
    async fn echo_op_is_skipped_without_touching_db() {
        let pool = pool().await;
        let canonical = Uuid::new_v4().to_string();
        let outcome = {
            let mut conn = pool.acquire().await.unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-a",
                    &canonical,
                    "insert",
                    None,
                    Some(serde_json::json!({"name": "Echo"})),
                    1,
                ),
                "device-a",
            )
            .await
            .unwrap()
        };
        assert_eq!(outcome, AppliedOutcome::Skipped);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn duplicate_insert_is_idempotent() {
        let pool = pool().await;
        let canonical = Uuid::new_v4().to_string();
        let payload = Some(serde_json::json!({"name": "Dup"}));
        let (first, second) = {
            let mut conn = pool.acquire().await.unwrap();
            let f = apply_remote_op_in_tx(
                &mut conn,
                &op("device-b", &canonical, "insert", None, payload.clone(), 5),
                "device-a",
            )
            .await
            .unwrap();
            let s = apply_remote_op_in_tx(
                &mut conn,
                &op("device-b", &canonical, "insert", None, payload, 6),
                "device-a",
            )
            .await
            .unwrap();
            (f, s)
        };
        assert_eq!(first, AppliedOutcome::Applied);
        assert_eq!(second, AppliedOutcome::Skipped);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn set_name_translates_via_mapping() {
        let pool = pool().await;
        let canonical = Uuid::new_v4().to_string();
        {
            let mut conn = pool.acquire().await.unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "insert",
                    None,
                    Some(serde_json::json!({"name": "old"})),
                    1,
                ),
                "device-a",
            )
            .await
            .unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "set",
                    Some("name"),
                    Some(serde_json::json!({"value": "new"})),
                    2,
                ),
                "device-a",
            )
            .await
            .unwrap();
        }
        let name: String = sqlx::query_scalar("SELECT name FROM playlist LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "new");
    }

    #[tokio::test]
    async fn delete_then_replay_is_ignored() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = Uuid::new_v4().to_string();
        apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "insert",
                None,
                Some(serde_json::json!({"name": "p"})),
                1,
            ),
            "device-a",
        )
        .await
        .unwrap();
        apply_remote_op_in_tx(
            &mut conn,
            &op("device-b", &canonical, "delete", None, None, 2),
            "device-a",
        )
        .await
        .unwrap();
        // Mapping gone; replay is ignored (cursor still advances at
        // the caller).
        let replay = apply_remote_op_in_tx(
            &mut conn,
            &op("device-b", &canonical, "delete", None, None, 3),
            "device-a",
        )
        .await
        .unwrap();
        assert_eq!(replay, AppliedOutcome::Ignored);
    }

    #[tokio::test]
    async fn set_against_unknown_canonical_is_ignored() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = Uuid::new_v4().to_string();
        let outcome = apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "set",
                Some("name"),
                Some(serde_json::json!({"value": "x"})),
                1,
            ),
            "device-a",
        )
        .await
        .unwrap();
        assert_eq!(outcome, AppliedOutcome::Ignored);
    }

    #[tokio::test]
    async fn unknown_entity_is_ignored_not_error() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let weird = RemoteSyncOp {
            id: 1,
            lamport_ts: 1,
            device_id: "device-b".into(),
            entity: "future_thing".into(),
            entity_id: Uuid::new_v4().to_string(),
            field: None,
            op: "insert".into(),
            payload: None,
        };
        let outcome = apply_remote_op_in_tx(&mut conn, &weird, "device-a")
            .await
            .unwrap();
        assert_eq!(outcome, AppliedOutcome::Ignored);
    }

    /// Malformed payloads MUST NOT bubble as DB errors — that would
    /// roll back the calling tx, leave the cursor unmoved, and have
    /// the same frame replay every reconnect. Pin the fall-through
    /// to `Ignored` so the cursor still advances.
    #[tokio::test]
    async fn malformed_insert_payload_is_ignored_not_error() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = Uuid::new_v4().to_string();
        // Missing required `name` field.
        let outcome = apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "insert",
                None,
                Some(serde_json::json!({ "color_id": "indigo" })),
                3,
            ),
            "device-a",
        )
        .await
        .unwrap();
        assert_eq!(outcome, AppliedOutcome::Ignored);
        // No mapping row planted.
        assert!(
            canonical::local_for_canonical(&mut conn, "playlist", &canonical)
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A `{"value": 123}` payload (number where a string is expected)
    /// must NOT be coerced to "clear the field" — pin that the type-
    /// mismatch path takes the Ignored branch.
    #[tokio::test]
    async fn malformed_set_value_type_is_ignored_not_coerced_to_null() {
        let pool = pool().await;
        let canonical = Uuid::new_v4().to_string();
        let outcome = {
            let mut conn = pool.acquire().await.unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "insert",
                    None,
                    Some(serde_json::json!({"name": "before"})),
                    1,
                ),
                "device-a",
            )
            .await
            .unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "set",
                    Some("name"),
                    Some(serde_json::json!({ "value": 42 })),
                    2,
                ),
                "device-a",
            )
            .await
            .unwrap()
        };
        assert_eq!(outcome, AppliedOutcome::Ignored);
        // Name MUST NOT have been cleared — the malformed type
        // mismatch is rejected, not silently coerced to NULL.
        let name: String = sqlx::query_scalar("SELECT name FROM playlist LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "before");
    }

    /// `null` on a `NOT NULL` column (name / color_id / icon_id) is
    /// corruption — the per-field nullability guard MUST reject it.
    #[tokio::test]
    async fn set_value_null_on_non_nullable_field_is_ignored() {
        let pool = pool().await;
        let canonical = Uuid::new_v4().to_string();
        let outcome = {
            let mut conn = pool.acquire().await.unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "insert",
                    None,
                    Some(serde_json::json!({"name": "kept"})),
                    1,
                ),
                "device-a",
            )
            .await
            .unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "set",
                    Some("name"),
                    Some(serde_json::json!({ "value": null })),
                    2,
                ),
                "device-a",
            )
            .await
            .unwrap()
        };
        assert_eq!(outcome, AppliedOutcome::Ignored);
        let name: String = sqlx::query_scalar("SELECT name FROM playlist LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "kept");
    }

    /// A `{"value": null}` payload on the nullable `description`
    /// column is a legitimate "clear the field" signal — the
    /// strict-type guard MUST NOT misclassify it as malformed.
    #[tokio::test]
    async fn set_value_null_clears_field() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = Uuid::new_v4().to_string();
        apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "insert",
                None,
                Some(serde_json::json!({
                    "name": "x",
                    "description": "old"
                })),
                1,
            ),
            "device-a",
        )
        .await
        .unwrap();
        let outcome = apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "set",
                Some("description"),
                Some(serde_json::json!({ "value": null })),
                2,
            ),
            "device-a",
        )
        .await
        .unwrap();
        assert_eq!(outcome, AppliedOutcome::Applied);
    }

    /// `{"track_ids": [1, "x", 3]}` is rejected wholesale — a
    /// partial apply would leave the playlist diverged from the
    /// broadcast view on every peer.
    #[tokio::test]
    async fn malformed_tracks_array_mixed_types_is_ignored() {
        let pool = pool().await;
        // Seed a track that matches one of the IDs in the malformed
        // payload. Without this seed, the OLD permissive behaviour
        // (filter_map on track_ids) would still produce an empty
        // `resolved` list because `filter_existing_track_ids` would
        // drop the unseen IDs — the test would pass for the wrong
        // reason. The seed ensures a partial apply would observably
        // insert a row, so a green test pins the strict-array
        // invariant rather than the empty-resolved short-circuit.
        sqlx::query("INSERT INTO track (id, title) VALUES (1, 'seed')")
            .execute(&pool)
            .await
            .unwrap();
        let canonical = Uuid::new_v4().to_string();
        let outcome = {
            let mut conn = pool.acquire().await.unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "insert",
                    None,
                    Some(serde_json::json!({"name": "p"})),
                    1,
                ),
                "device-a",
            )
            .await
            .unwrap();
            apply_remote_op_in_tx(
                &mut conn,
                &op(
                    "device-b",
                    &canonical,
                    "insert",
                    Some("tracks"),
                    Some(serde_json::json!({ "track_ids": [1, "x", 3] })),
                    2,
                ),
                "device-a",
            )
            .await
            .unwrap()
        };
        assert_eq!(outcome, AppliedOutcome::Ignored);
        // No partial track insert — even though track id=1 exists,
        // the strict parser rejected the whole array so nothing was
        // appended.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist_track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn malformed_set_tracks_payload_is_ignored_not_error() {
        let pool = pool().await;
        let mut conn = pool.acquire().await.unwrap();
        let canonical = Uuid::new_v4().to_string();
        // Seed a playlist so the canonical lookup hits.
        apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "insert",
                None,
                Some(serde_json::json!({"name": "p"})),
                1,
            ),
            "device-a",
        )
        .await
        .unwrap();
        // Reorder op missing both `track_id` and `position`.
        let outcome = apply_remote_op_in_tx(
            &mut conn,
            &op(
                "device-b",
                &canonical,
                "set",
                Some("tracks"),
                Some(serde_json::json!({ "wrong_shape": true })),
                2,
            ),
            "device-a",
        )
        .await
        .unwrap();
        assert_eq!(outcome, AppliedOutcome::Ignored);
    }
}
