//! Backfill LWW divergent resolver (RFC-003 Phase B.2 / §2).
//!
//! `divergent` diff bucket = same canonical_id, different
//! payload_hash. Both sides have the row, they just disagree on
//! its contents. Resolve by fetching the server's full state +
//! comparing §2 total-order tuples `(hlc_wall, hlc_logical,
//! origin_device_id)`:
//!
//! - Remote tuple > Local tuple → remote wins → [`super::pull::pull_one`]
//! - Remote tuple < Local tuple → local wins → re-push from local
//! - Equal tuples → hashes differ but timestamps tied. Either a
//!   corruption (one side dropped a field) or a hash-function
//!   bug. Logged + skipped; the user can trigger a manual
//!   `sync_backfill_now` to retry once the underlying issue is
//!   addressed.

use sqlx::SqlitePool;
use uuid::Uuid;
use waveflow_core::sync::payload_hash::HlcTriple;
use waveflow_core::sync::Hlc;

use crate::error::{AppError, AppResult};
use crate::server_client::WaveflowServerClient;
use crate::state::AppState;
use crate::sync::digest::diff::DivergentMember;
use crate::sync::digest::entity_client::{self, RemoteEntityRow};

use super::pull;
use super::push;

/// Counters returned to the orchestrator.
#[derive(Debug, Default)]
pub struct LwwStats {
    pub local_wins: u32,
    pub remote_wins: u32,
    pub failed: u32,
}

/// Resolve every divergent member by fetching the server's row +
/// comparing §2 tuples per RFC-003. Reuses [`super::pull`] /
/// [`super::push`] helpers for the actual mutations so behaviour
/// stays consistent with the missing-row paths.
pub async fn resolve_divergent(
    state: &AppState,
    client: &WaveflowServerClient,
    pool: &SqlitePool,
    entity: &str,
    profile_canonical_id: Option<&str>,
    divergent: &[DivergentMember],
) -> AppResult<LwwStats> {
    let mut stats = LwwStats::default();
    for member in divergent {
        let outcome = resolve_one(
            state,
            client,
            pool,
            entity,
            profile_canonical_id,
            &member.canonical_id,
        )
        .await;
        match outcome {
            Ok(Decision::RemoteWins) => stats.remote_wins += 1,
            Ok(Decision::LocalWins) => stats.local_wins += 1,
            Ok(Decision::Tied) => {
                tracing::warn!(
                    entity,
                    canonical_id = %member.canonical_id,
                    "backfill lww: tied §2 tuples but hash mismatch — skipping (manual intervention)",
                );
                stats.failed += 1;
            }
            Ok(Decision::Absent) => {
                // Server dropped the row between digest + entity
                // fetch. The local copy is now the source of
                // truth — re-push so the server's set matches
                // local. Treats as local_wins for the counter.
                if push_one(state, pool, entity, &member.canonical_id).await? {
                    stats.local_wins += 1;
                }
            }
            Err(err) => {
                tracing::warn!(
                    entity,
                    canonical_id = %member.canonical_id,
                    error = %err,
                    "backfill lww failed for row"
                );
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

#[derive(Debug)]
enum Decision {
    RemoteWins,
    LocalWins,
    Tied,
    /// Server returned 404 — row vanished after the digest read.
    Absent,
}

async fn resolve_one(
    state: &AppState,
    client: &WaveflowServerClient,
    pool: &SqlitePool,
    entity: &str,
    profile_canonical_id: Option<&str>,
    canonical_id: &str,
) -> AppResult<Decision> {
    let Some(remote) =
        entity_client::fetch_remote_entity(client, entity, canonical_id, profile_canonical_id)
            .await?
    else {
        return Ok(Decision::Absent);
    };

    let local = read_local_tuple(pool, entity, canonical_id).await?;
    let Some(local) = local else {
        // Local row vanished after the digest read — treat as
        // missing_locally now.
        pull::pull_one(
            state,
            client,
            pool,
            entity,
            profile_canonical_id,
            canonical_id,
        )
        .await?;
        return Ok(Decision::RemoteWins);
    };

    let remote_triple = HlcTriple::new(
        Hlc {
            wall: remote.hlc.wall,
            logical: remote.hlc.logical,
        },
        remote.origin_device_id,
    );
    let local_triple = HlcTriple::new(
        Hlc {
            wall: local.hlc_wall,
            logical: local.hlc_logical,
        },
        local.origin_device_id,
    );

    use std::cmp::Ordering::*;
    match remote_triple.cmp(&local_triple) {
        Greater => {
            // Remote newer — apply the row state locally.
            apply_remote_inline(pool, &remote).await?;
            Ok(Decision::RemoteWins)
        }
        Less => {
            // Local newer — re-push so the server picks our state up.
            if push_one(state, pool, entity, canonical_id).await? {
                Ok(Decision::LocalWins)
            } else {
                Ok(Decision::Tied)
            }
        }
        Equal => Ok(Decision::Tied),
    }
}

struct LocalTuple {
    hlc_wall: i64,
    hlc_logical: i32,
    origin_device_id: Option<Uuid>,
}

async fn read_local_tuple(
    pool: &SqlitePool,
    entity: &str,
    canonical_id: &str,
) -> AppResult<Option<LocalTuple>> {
    let row: Option<(i64, i32, Option<String>)> = match entity {
        "library" => {
            sqlx::query_as(
                "SELECT hlc_wall, hlc_logical, origin_device_id
                   FROM library WHERE canonical_id = ?",
            )
            .bind(canonical_id)
            .fetch_optional(pool)
            .await?
        }
        "track" => {
            // Composite canonical: `<library.canonical_id>\u{1F}<file_path>`.
            // Mirror the server's `entity_read::fetch_track` split.
            let Some((lib_canonical, file_path)) = canonical_id.split_once('\u{001F}') else {
                return Err(AppError::Other(format!(
                    "lww track: composite canonical missing `\\u{{1F}}`, got `{canonical_id}`",
                )));
            };
            sqlx::query_as(
                "SELECT t.hlc_wall, t.hlc_logical, t.origin_device_id
                   FROM track t
                   JOIN library l ON l.id = t.library_id
                  WHERE l.canonical_id = ? AND t.file_path = ?",
            )
            .bind(lib_canonical)
            .bind(file_path)
            .fetch_optional(pool)
            .await?
        }
        "playlist" => {
            sqlx::query_as(
                "SELECT hlc_wall, hlc_logical, origin_device_id
                   FROM playlist WHERE canonical_id = ?",
            )
            .bind(canonical_id)
            .fetch_optional(pool)
            .await?
        }
        "liked_track" => {
            sqlx::query_as(
                "SELECT lt.hlc_wall, lt.hlc_logical, lt.origin_device_id
                   FROM liked_track lt
                   JOIN track t ON t.id = lt.track_id
                  WHERE t.file_hash = ?",
            )
            .bind(canonical_id)
            .fetch_optional(pool)
            .await?
        }
        "track_rating" => {
            sqlx::query_as(
                "SELECT rating_hlc_wall, rating_hlc_logical, rating_origin_device_id
                   FROM track WHERE file_hash = ?",
            )
            .bind(canonical_id)
            .fetch_optional(pool)
            .await?
        }
        other => {
            return Err(AppError::Other(format!(
                "lww: unsupported entity '{other}'",
            )))
        }
    };
    let Some((hlc_wall, hlc_logical, origin)) = row else {
        return Ok(None);
    };
    let origin_device_id = match origin {
        Some(s) => match Uuid::parse_str(&s) {
            Ok(u) => Some(u),
            Err(_) => None,
        },
        None => None,
    };
    Ok(Some(LocalTuple {
        hlc_wall,
        hlc_logical,
        origin_device_id,
    }))
}

/// Apply a remote row directly inside a transaction. Mirror of
/// the dispatch in [`super::pull::apply_remote_row`] but the
/// orchestrator already opened a tx for this caller, so we just
/// dispatch by entity. Re-uses the per-entity helpers via the
/// pull module's pub fn surface.
async fn apply_remote_inline(pool: &SqlitePool, row: &RemoteEntityRow) -> AppResult<()> {
    let mut tx = pool.begin().await?;
    pull::apply_remote_row(&mut tx, row).await?;
    tx.commit().await?;
    Ok(())
}

/// Re-push a single canonical via the push module. Returns
/// `Ok(true)` if the op was enqueued, `Ok(false)` if the local
/// row vanished. Drain notify is left to the orchestrator-level
/// aggregator.
async fn push_one(
    state: &AppState,
    pool: &SqlitePool,
    entity: &str,
    canonical_id: &str,
) -> AppResult<bool> {
    push::push_one_by_canonical(state, pool, entity, canonical_id).await
}
