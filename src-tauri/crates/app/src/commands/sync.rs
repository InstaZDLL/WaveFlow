//! Diagnostic Tauri commands for the sync infrastructure shipped in
//! Phase 1.f.desktop.2. The Settings → Diagnostics panel will use
//! [`sync_get_queue_state`] to show the user how many ops are
//! waiting to be sent and what the local Lamport floor + device id
//! are; [`sync_clear_pending`] is the nuclear option for when the
//! queue is wedged.
//!
//! No CRUD enqueue hooks are wired in this PR — see the
//! [`crate::sync`] module docstring for the scope split.

use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, AppResult},
    server_client::WaveflowServerClient,
    state::AppState,
    sync::{backfill, device, digest, drain, lamport, mode, queue},
};

#[derive(Debug, Serialize)]
pub struct SyncQueueState {
    /// Stable per-install device id the server pins its UNIQUEs
    /// against. `None` only on a fresh install before the first call
    /// — production code always goes through [`device::ensure`] so
    /// the diagnostic value mirrors what the future drain task will
    /// send.
    pub device_id: Option<String>,
    /// Per-profile Lamport floor. `0` on a fresh profile, otherwise
    /// the value the next outbound op would slot at (= last issued
    /// `+ 1`).
    pub lamport_local_max: i64,
    /// Number of rows currently in the local queue.
    pub pending_count: i64,
    /// Current per-profile sync mode (`"local"` | `"hybrid"`). Falls
    /// back to `"hybrid"` (the default) on a fresh profile with no
    /// stored row.
    pub mode: &'static str,
}

/// Snapshot of the desktop's sync infrastructure for the Settings
/// panel. Does NOT generate a device id if the row hasn't been
/// planted yet — reading-without-side-effects is safer for a
/// diagnostic surface, and the CRUD enqueue hook (1.f.desktop.2b)
/// is the right place to lazy-create on first write.
#[tauri::command]
pub async fn sync_get_queue_state(state: tauri::State<'_, AppState>) -> AppResult<SyncQueueState> {
    let device_id = device::read(&state.app_db).await?;

    let (lamport_local_max, pending_count, sync_mode) = match state.require_profile_pool().await {
        Ok(pool) => (
            lamport::read(&pool).await?,
            queue::count_pending(&pool).await?,
            mode::read(&pool).await?,
        ),
        Err(err) => {
            // No active profile is the only legitimate path here
            // post-bootstrap (we render defaults so the Settings card
            // can still mount). Anything else — a pool init failure,
            // a closed RwLock, etc. — should at minimum land in the
            // tracing sink so an operator can correlate the "0 / 0"
            // surface with the actual cause instead of staring at a
            // silently-empty card.
            tracing::warn!(
                error = %err,
                "sync_get_queue_state: require_profile_pool failed, returning defaults",
            );
            (0, 0, mode::SyncMode::Hybrid)
        }
    };

    Ok(SyncQueueState {
        device_id,
        lamport_local_max,
        pending_count,
        mode: sync_mode.as_str(),
    })
}

/// Drop every queued op. Used by the Settings diagnostic panel when
/// the user wants a clean slate (e.g. after switching servers).
/// Returns the number of rows that were removed so the UI can
/// surface a confirmation toast.
#[tauri::command]
pub async fn sync_clear_pending(state: tauri::State<'_, AppState>) -> AppResult<u64> {
    let pool = state.require_profile_pool().await?;
    queue::clear(&pool).await
}

#[derive(Debug, Deserialize)]
pub struct SetSyncModeRequest {
    /// Canonical lowercase string — must match
    /// [`mode::SyncMode::as_str`] (currently `"local"` or
    /// `"hybrid"`). Anything else fails 400-style with a clear
    /// error so a typoed JSON payload can't silently land an
    /// unknown mode in storage.
    pub mode: String,
}

/// Read the active profile's current sync mode. Returns the canonical
/// string form so the frontend doesn't have to enumerate the variants
/// in two places.
#[tauri::command]
pub async fn sync_get_mode(state: tauri::State<'_, AppState>) -> AppResult<&'static str> {
    let pool = state.require_profile_pool().await?;
    Ok(mode::read(&pool).await?.as_str())
}

/// Persist the active profile's sync mode.
#[tauri::command]
pub async fn sync_set_mode(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    req: SetSyncModeRequest,
) -> AppResult<&'static str> {
    let parsed = match req.mode.trim() {
        "local" => mode::SyncMode::Local,
        "hybrid" => mode::SyncMode::Hybrid,
        other => {
            return Err(AppError::Other(format!(
                "unknown sync mode '{other}', expected 'local' or 'hybrid'",
            )));
        }
    };
    let pool = state.require_profile_pool().await?;
    // Read the previous mode so the post-write side-effects only
    // fire on a genuine Local → Hybrid transition. Without this,
    // a user who clicks the same "Hybrid" radio repeatedly would
    // spawn a fresh drain notify + WS wake + auto-backfill on
    // every click.
    let previous = mode::read(&pool).await?;
    mode::write(&pool, parsed).await?;
    let became_hybrid = parsed == mode::SyncMode::Hybrid && previous != mode::SyncMode::Hybrid;
    // Flipping to Hybrid likely means the user wants their pending
    // ops to fly upstream right away — wake the drain task so the
    // first push doesn't wait for the 30 s tick, and wake the WS
    // subscriber so the catch-up pull + live socket connect without
    // the 30 s idle gate.
    if became_hybrid {
        state.drain.notify();
        state.ws.wake();
        // Fire an auto-backfill pass too if the user opted in.
        // Best-effort, fire-and-forget — the pass logs internally
        // and the mode-flip response shouldn't wait on a multi-
        // second network round-trip.
        let app_handle = app.clone();
        tokio::spawn(async move {
            use tauri::Manager;
            let inner_state = app_handle.state::<AppState>();
            if let Err(err) = backfill::maybe_auto_backfill(inner_state.inner()).await {
                tracing::warn!(error = %err, "auto-backfill after mode flip failed");
            }
        });
    }
    Ok(parsed.as_str())
}

/// Force an immediate drain pass — used by the Settings diagnostic
/// "Push now" button so the operator doesn't have to wait for the
/// periodic tick to verify the wiring.
///
/// Serialised against the background drain task via
/// [`AppState::drain_lock`] so a manual click while the periodic
/// pass is in flight waits for it to finish instead of racing it
/// onto the same batch.
#[tauri::command]
pub async fn sync_drain_now(state: tauri::State<'_, AppState>) -> AppResult<drain::DrainOutcome> {
    let _guard = state.drain_lock.lock().await;
    drain::drain_once(&state).await
}

/// Per-entity outcome of a digest check pass. Mirrors
/// [`digest::diff::DigestDiff`] minus the bulk member lists so the
/// IPC payload stays bounded — the full diff is kept server-side
/// internally for the future B.2 backfill orchestrator. Counts are
/// `u32` because the wire never carries more than the row counts
/// the user actually owns.
#[derive(Debug, Serialize)]
pub struct SyncDigestReport {
    pub entity: String,
    /// `true` when the server's `set_hash` matches the locally
    /// recomputed one. Equivalent to `missing_*` + `divergent` all
    /// being zero, but cheaper for the UI to render.
    pub in_sync: bool,
    pub local_version: i64,
    pub remote_version: i64,
    pub missing_locally: u32,
    pub missing_remotely: u32,
    pub divergent: u32,
}

/// Status returned to the frontend describing why
/// [`sync_digest_check`] couldn't talk to the server. Mirrors the
/// drain task's gating shape so the Settings card can render a
/// single "All synced / Syncing / Offline" affordance.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncDigestOutcome {
    /// The active profile is sync-enabled and the digest pass ran.
    /// `reports` holds one entry per checked entity.
    Ran { reports: Vec<SyncDigestReport> },
    /// The active profile isn't sync-enabled (no URL, no JWT, or
    /// `SyncMode::Local`). Same shape the drain task surfaces as
    /// `DrainOutcome::Skipped`.
    Skipped { reason: &'static str },
}

/// Compute the local digest, fetch the server's digest, and diff
/// per entity. Used by the Settings UI's "Sync status" card +
/// future B.2 backfill orchestrator.
///
/// `entity` is optional — when omitted the check runs across every
/// supported entity (`library`, `playlist`, `track`, `liked_track`,
/// `track_rating`). When supplied, only that one is checked; an
/// unknown name surfaces as `Err`.
#[tauri::command]
pub async fn sync_digest_check(
    state: tauri::State<'_, AppState>,
    entity: Option<String>,
) -> AppResult<SyncDigestOutcome> {
    // Gate 0 — process-wide offline mode. Same short-circuit every
    // other outbound HTTP path applies (CLAUDE.md cross-cutting
    // rule); no point building the HTTP client or hitting SQLite
    // when the user explicitly set `network.offline_mode`.
    if crate::offline::is_offline() {
        return Ok(SyncDigestOutcome::Skipped { reason: "offline" });
    }

    // Gate 1 — local-only profile means the digest comparison has
    // no remote to compare against. Surface the same `Skipped`
    // shape the drain task uses so the UI can treat them
    // uniformly.
    let pool = state.require_profile_pool().await?;
    if mode::read(&pool).await? == mode::SyncMode::Local {
        return Ok(SyncDigestOutcome::Skipped {
            reason: "sync_mode_local",
        });
    }

    // Gate 2 — server URL + JWT present.
    let Some(client) = WaveflowServerClient::try_build(&state).await? else {
        return Ok(SyncDigestOutcome::Skipped {
            reason: "not_configured",
        });
    };

    // Gate 3 — the profile carries a canonical id; profile-scoped
    // digest queries require it.
    let profile_id = state.require_profile_id().await?;
    let profile_canonical_id =
        crate::db::profile_meta::canonical_id_for(&state.app_db, profile_id).await?;

    let entities: Vec<&str> = match entity.as_deref() {
        Some(e) => {
            if !digest::SUPPORTED_ENTITIES.contains(&e) {
                return Err(AppError::Other(format!(
                    "sync_digest_check: unknown entity '{e}'"
                )));
            }
            vec![e]
        }
        None => digest::SUPPORTED_ENTITIES.to_vec(),
    };

    let mut reports = Vec::with_capacity(entities.len());
    let mut skipped_canonical = 0usize;
    for e in entities {
        let canonical_arg = match e {
            "library" | "playlist" | "track" => {
                let Some(canon) = profile_canonical_id.as_deref() else {
                    // Same defer-don't-fail semantic as the drain
                    // task — without a canonical id the
                    // profile-scoped query would 400 on the server.
                    tracing::warn!(
                        profile_id,
                        entity = e,
                        "digest: profile.canonical_id is NULL — skipping entity",
                    );
                    skipped_canonical += 1;
                    continue;
                };
                Some(canon)
            }
            "liked_track" | "track_rating" => None,
            _ => continue,
        };

        let local = digest::read_local_digest(&pool, e).await?;
        let remote = digest::client::fetch_remote_digest(&client, e, canonical_arg).await?;
        let d = digest::diff::diff(&local, &remote);
        reports.push(SyncDigestReport {
            entity: d.entity,
            in_sync: d.in_sync,
            local_version: d.local_version,
            remote_version: d.remote_version,
            missing_locally: d.missing_locally.len() as u32,
            missing_remotely: d.missing_remotely.len() as u32,
            divergent: d.divergent.len() as u32,
        });
    }

    // If every targeted entity was skipped (caller asked for
    // profile-scoped entities only, and `profile.canonical_id` is
    // NULL pending the drain's backfill), `Ran { reports: [] }`
    // would falsely render as "everything in sync" in the UI.
    // Promote to `Skipped` so the surface matches reality.
    if reports.is_empty() && skipped_canonical > 0 {
        return Ok(SyncDigestOutcome::Skipped {
            reason: "profile_canonical_id_missing",
        });
    }

    Ok(SyncDigestOutcome::Ran { reports })
}

/// Read the per-profile auto-backfill enabled flag. Settings UI
/// reads this to render the toggle state on mount.
#[tauri::command]
pub async fn sync_backfill_get_enabled(
    state: tauri::State<'_, AppState>,
) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;
    backfill::read_auto_enabled(&pool).await
}

/// Persist the per-profile auto-backfill enabled flag. The user
/// can click the manual "Resync now" button immediately after to
/// trigger a pass; the next boot / sync-mode flip to Hybrid
/// fires it automatically.
#[tauri::command]
pub async fn sync_backfill_set_enabled(
    state: tauri::State<'_, AppState>,
    enabled: bool,
) -> AppResult<bool> {
    let pool = state.require_profile_pool().await?;
    backfill::write_auto_enabled(&pool, enabled).await?;
    Ok(enabled)
}

/// Trigger a backfill pass (RFC-003 Phase B.2). Same gates as
/// [`sync_digest_check`] plus a process-wide mutual-exclusion
/// lock — a second concurrent caller returns
/// `BackfillOutcome::AlreadyRunning` without firing a parallel
/// sweep.
#[tauri::command]
pub async fn sync_backfill_now(
    state: tauri::State<'_, AppState>,
) -> AppResult<backfill::BackfillOutcome> {
    // Gate 0 — offline mode short-circuits before any HTTP /
    // SQLite work. Same `Skipped { reason }` shape the digest
    // command uses so the UI renders both surfaces uniformly.
    if crate::offline::is_offline() {
        return Ok(backfill::BackfillOutcome::Skipped { reason: "offline" });
    }
    // Pin the active profile's pool AND profile_id under a SINGLE
    // RwLock read guard so the gate read, the canonical lookup,
    // the inner backfill orchestration, and the post-pass stamp
    // all see the same per-profile `(pool, profile_id)` pair.
    // Two separate `require_profile_pool` + `require_profile_id`
    // calls would each re-acquire the lock and leave an interleave
    // window where `activate_profile` swaps the inner
    // `ActiveProfile` — pairing the wrong pool with the wrong
    // canonical id silently corrupts the per-profile timestamp
    // stamp. The pool itself is `Arc<…>` internally (sqlx) so
    // holding the clone is cheap, and the underlying connections
    // survive the swap that `activate_profile` performs.
    let (pool, profile_id) = {
        let guard = state.profile.read().await;
        let active = guard.as_ref().ok_or(AppError::NoActiveProfile)?;
        (active.pool.clone(), active.profile_id)
    };
    // Gate 1 — Local mode.
    if mode::read(&pool).await? == mode::SyncMode::Local {
        return Ok(backfill::BackfillOutcome::Skipped {
            reason: "sync_mode_local",
        });
    }

    // Gate 2 — server URL + JWT present.
    let Some(client) = WaveflowServerClient::try_build(&state).await? else {
        return Ok(backfill::BackfillOutcome::Skipped {
            reason: "not_configured",
        });
    };

    // Mutex lock — a parallel call surfaces AlreadyRunning.
    let guard = state.backfill_lock.try_lock();
    let Ok(_guard) = guard else {
        return Ok(backfill::BackfillOutcome::AlreadyRunning);
    };

    // `canonical_id_for` hits the global `app.db`, not the
    // per-profile pool, so it's safe to call with the pinned
    // `profile_id` even after concurrent profile activity.
    let profile_canonical_id =
        crate::db::profile_meta::canonical_id_for(&state.app_db, profile_id).await?;

    let report =
        backfill::run_backfill(&state, &client, profile_canonical_id.as_deref()).await?;

    // Stamp the per-profile last-successful-backfill timestamp now
    // that the top-level pass returned `Ok`. Reuses the same pool
    // pinned at the top of the function so a concurrent profile
    // switch can't land the stamp in the wrong profile's
    // `profile_setting`. Per-entity row failures don't gate the
    // stamp; the Ran outcome reports them independently.
    backfill::stamp_last_run_at(&pool).await;

    Ok(backfill::BackfillOutcome::Ran {
        reports: report.reports,
    })
}

/// Snapshot of the backfill task surfaced to the Settings card.
/// Used by the "Last sync: X ago" timestamp + the "in progress"
/// disabled state on the Resync button.
#[derive(Debug, Serialize)]
pub struct SyncBackfillStatus {
    /// Epoch-millisecond timestamp of the last completed backfill
    /// pass (automatic or manual). `None` when the user has never
    /// run one — fresh profile, opted-out, or first launch.
    pub last_run_at: Option<i64>,
    /// `true` when [`crate::state::AppState::backfill_lock`] is
    /// held by another caller — manual click or heartbeat tick
    /// currently mid-flight.
    pub in_progress: bool,
}

/// Read the persisted last-run timestamp + the live in-flight
/// state. Cheap — one SELECT + one `try_lock`. Used by the Settings
/// card on mount + every time the manual button completes.
#[tauri::command]
pub async fn sync_backfill_get_status(
    state: tauri::State<'_, AppState>,
) -> AppResult<SyncBackfillStatus> {
    let pool = state.require_profile_pool().await?;
    let last_run_at = backfill::read_last_run_at(&pool).await?;
    // `try_lock` is non-blocking; we hold the guard only for the
    // duration of the `is_ok` test (drops at end of statement).
    // The race window where a concurrent pass starts between the
    // check and the UI rendering is bounded by IPC latency — the
    // user's next status poll resolves it.
    let in_progress = state.backfill_lock.try_lock().is_err();
    Ok(SyncBackfillStatus {
        last_run_at,
        in_progress,
    })
}

/// Read the per-profile heartbeat cadence (minutes). Clamped to
/// the documented range so a malformed stored value can't surface
/// the heartbeat task with an out-of-range interval.
#[tauri::command]
pub async fn sync_backfill_get_heartbeat_interval(
    state: tauri::State<'_, AppState>,
) -> AppResult<i64> {
    let pool = state.require_profile_pool().await?;
    backfill::read_heartbeat_interval_min(&pool).await
}

/// Persist the per-profile heartbeat cadence (minutes). Returns
/// the clamped value so the caller can hydrate its UI even when
/// the input was out of range.
#[tauri::command]
pub async fn sync_backfill_set_heartbeat_interval(
    state: tauri::State<'_, AppState>,
    minutes: i64,
) -> AppResult<i64> {
    let clamped = backfill::clamp_heartbeat_interval_min(minutes);
    let pool = state.require_profile_pool().await?;
    backfill::write_heartbeat_interval_min(&pool, clamped).await?;
    Ok(clamped)
}

/// Detailed digest outcome carrying the full
/// [`digest::diff::DigestDiff`] for a single entity. Used by the
/// Settings card's drill-down panel: a click on a row out-of-sync
/// fetches this and renders the divergent / missing-locally /
/// missing-remotely member lists with their canonical_ids + hash
/// previews.
///
/// Member lists are truncated server-side to
/// [`DETAILED_BUCKET_CAP`] to keep IPC payloads bounded — a 10k-row
/// divergence would otherwise marshal ~1 MB into the frontend for
/// little incremental UX value. The truncated flags let the UI
/// render a "+N more" affordance.
#[derive(Debug, Serialize)]
pub struct SyncDigestDetailed {
    pub entity: String,
    pub in_sync: bool,
    pub local_version: i64,
    pub remote_version: i64,
    pub missing_locally: Vec<digest::client::RemoteMember>,
    pub missing_locally_total: u32,
    pub missing_remotely: Vec<digest::diff::DivergentMember>,
    pub missing_remotely_total: u32,
    pub divergent: Vec<digest::diff::DivergentMember>,
    pub divergent_total: u32,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncDigestDetailedOutcome {
    Ran { diff: SyncDigestDetailed },
    Skipped { reason: &'static str },
}

/// Per-bucket truncation cap surfaced through
/// [`sync_digest_check_detailed`]. The Settings drill-down
/// renders the first ~20 entries per bucket; the cap leaves
/// headroom for a "show more" affordance without unbounded
/// growth.
pub const DETAILED_BUCKET_CAP: usize = 100;

/// Detailed digest check for a single entity. Runs the same
/// gates as [`sync_digest_check`] (offline / Local mode / no
/// JWT / no canonical id) and returns the full member-level
/// diff for the requested entity, truncated to
/// [`DETAILED_BUCKET_CAP`] per bucket.
#[tauri::command]
pub async fn sync_digest_check_detailed(
    state: tauri::State<'_, AppState>,
    entity: String,
) -> AppResult<SyncDigestDetailedOutcome> {
    if crate::offline::is_offline() {
        return Ok(SyncDigestDetailedOutcome::Skipped { reason: "offline" });
    }
    // Pin pool + profile_id under a SINGLE RwLock read guard so a
    // mid-call profile switch can't pair the old pool's local
    // digest with the new profile's canonical_id when querying
    // the server. Two separate `require_profile_pool` +
    // `require_profile_id` calls would each re-acquire the lock,
    // leaving an interleave window where `activate_profile`
    // swaps the inner `ActiveProfile` between them.
    let (pool, profile_id) = {
        let guard = state.profile.read().await;
        let active = guard.as_ref().ok_or(AppError::NoActiveProfile)?;
        (active.pool.clone(), active.profile_id)
    };
    if mode::read(&pool).await? == mode::SyncMode::Local {
        return Ok(SyncDigestDetailedOutcome::Skipped {
            reason: "sync_mode_local",
        });
    }
    let Some(client) = WaveflowServerClient::try_build(&state).await? else {
        return Ok(SyncDigestDetailedOutcome::Skipped {
            reason: "not_configured",
        });
    };

    if !digest::SUPPORTED_ENTITIES.contains(&entity.as_str()) {
        return Err(AppError::Other(format!(
            "sync_digest_check_detailed: unknown entity '{entity}'"
        )));
    }

    // `canonical_id_for` hits the global `app.db`, not the
    // per-profile pool, so a profile switch can't desync the
    // canonical against the pinned profile_id (the row stays
    // queryable by id for both the old and new active profile).
    let profile_canonical_id =
        crate::db::profile_meta::canonical_id_for(&state.app_db, profile_id).await?;
    let canonical_arg = match entity.as_str() {
        "library" | "playlist" | "track" => {
            let Some(canon) = profile_canonical_id.as_deref() else {
                return Ok(SyncDigestDetailedOutcome::Skipped {
                    reason: "profile_canonical_id_missing",
                });
            };
            Some(canon)
        }
        _ => None,
    };

    let local = digest::read_local_digest(&pool, entity.as_str()).await?;
    let remote =
        digest::client::fetch_remote_digest(&client, entity.as_str(), canonical_arg).await?;
    let d = digest::diff::diff(&local, &remote);

    let missing_locally_total = d.missing_locally.len() as u32;
    let missing_remotely_total = d.missing_remotely.len() as u32;
    let divergent_total = d.divergent.len() as u32;
    let mut missing_locally = d.missing_locally;
    missing_locally.truncate(DETAILED_BUCKET_CAP);
    let mut missing_remotely = d.missing_remotely;
    missing_remotely.truncate(DETAILED_BUCKET_CAP);
    let mut divergent = d.divergent;
    divergent.truncate(DETAILED_BUCKET_CAP);

    Ok(SyncDigestDetailedOutcome::Ran {
        diff: SyncDigestDetailed {
            entity: d.entity,
            in_sync: d.in_sync,
            local_version: d.local_version,
            remote_version: d.remote_version,
            missing_locally,
            missing_locally_total,
            missing_remotely,
            missing_remotely_total,
            divergent,
            divergent_total,
        },
    })
}
