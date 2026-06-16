//! Backfill orchestrator (RFC-003 Phase B.2).
//!
//! Closes the loop the B.1 digest client read-only laid:
//!
//! - Compute local digest (B.1 [`crate::sync::digest::read_local_digest`])
//! - Fetch server digest (B.1
//!   [`crate::sync::digest::client::fetch_remote_digest`])
//! - Diff (B.1 [`crate::sync::digest::diff::diff`])
//! - Dispatch each diff bucket:
//!   - `missing_remotely` → [`push::push_missing_remotely`]: read
//!     the local row's canonical fields, re-emit the insert op
//!     via the outbound queue (drain delivers).
//!   - `missing_locally` → [`pull::pull_missing_locally`]: fetch
//!     each row via `GET /api/v1/sync/entity` (server PR #66) and
//!     write the local row directly, stamping the exact HLC +
//!     origin_device_id + payload_hash the server returned so the
//!     next digest sweep matches byte-exact.
//!   - `divergent` → [`lww::resolve_divergent`]: fetch each row,
//!     compare §2 (hlc, origin_device_id) tuples per RFC-003 §2;
//!     remote winner → pull, local winner → push.
//!
//! ## Track entity is excluded in B.2 v1
//!
//! The composite canonical (`<lib_canonical>\u{1F}<file_path>`) +
//! the album / artist / track_artist relations make track pull
//! non-trivial — needs a dedicated upsert path that mirrors the
//! scanner's. Deferred to a follow-up PR. Until then the
//! orchestrator runs digest+diff for `track` but logs each diff
//! bucket without acting on it.
//!
//! ## Gating
//!
//! The Tauri command (`commands::sync::sync_backfill_now`) holds
//! [`crate::state::AppState::backfill_lock`] so a manual click
//! while a backfill is already in flight surfaces
//! `BackfillOutcome::AlreadyRunning` rather than racing. Offline
//! / no-JWT / `SyncMode::Local` are short-circuited UPSTREAM (the
//! command checks the same gates as the digest_check), so this
//! module assumes a configured client.

pub mod lww;
pub mod pull;
pub mod push;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::error::AppResult;
use crate::server_client::WaveflowServerClient;
use crate::state::AppState;
use crate::sync::digest::{self, diff::DigestDiff};

/// `profile_setting` key for the auto-poll toggle. When `true`,
/// [`maybe_auto_backfill`] fires a pass on boot + every sync-mode
/// flip to Hybrid. Default `false` — opt-in only.
pub const AUTO_BACKFILL_KEY: &str = "sync.v2.backfill_enabled";

/// Read the per-profile auto-backfill enabled flag. Returns
/// `false` when the row is absent (matches the opt-in default).
pub async fn read_auto_enabled(pool: &SqlitePool) -> AppResult<bool> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM profile_setting WHERE key = ?")
            .bind(AUTO_BACKFILL_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(value.as_deref() == Some("1"))
}

/// Persist the per-profile auto-backfill enabled flag.
pub async fn write_auto_enabled(pool: &SqlitePool, enabled: bool) -> AppResult<()> {
    sqlx::query(
        "INSERT INTO profile_setting (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(AUTO_BACKFILL_KEY)
    .bind(if enabled { "1" } else { "0" })
    .execute(pool)
    .await?;
    Ok(())
}

/// Best-effort auto-backfill trigger. Reads every gate (auto
/// enabled flag, offline mode, Local sync mode, JWT presence,
/// profile canonical id) and short-circuits silently when any
/// gate refuses. Fires nothing when the lock is already held by
/// a concurrent pass.
///
/// Designed for fire-and-forget call sites: boot-time check in
/// `lib.rs::run`, sync-mode flip Local→Hybrid in
/// `commands::sync::sync_set_mode`. Failures log + swallow so a
/// transient network blip doesn't blow up the wrapping command.
pub async fn maybe_auto_backfill(state: &AppState) -> AppResult<()> {
    // Gate 0 — auto flag must be on.
    let pool = state.require_profile_pool().await?;
    if !read_auto_enabled(&pool).await? {
        return Ok(());
    }
    // Gate 1 — process-wide offline.
    if crate::offline::is_offline() {
        return Ok(());
    }
    // Gate 2 — Local sync mode.
    if crate::sync::mode::read(&pool).await? == crate::sync::mode::SyncMode::Local {
        return Ok(());
    }
    // Gate 3 — server URL + JWT.
    let Some(client) = WaveflowServerClient::try_build(state).await? else {
        return Ok(());
    };
    // Lock — silent skip if a concurrent caller is already
    // mid-pass.
    let Ok(_guard) = state.backfill_lock.try_lock() else {
        return Ok(());
    };

    let profile_id = state.require_profile_id().await?;
    let profile_canonical_id =
        crate::db::profile_meta::canonical_id_for(&state.app_db, profile_id).await?;

    match run_backfill(state, &client, profile_canonical_id.as_deref()).await {
        Ok(report) => {
            let entities_with_action = report
                .reports
                .iter()
                .filter(|r| r.pushed + r.pulled + r.lww_local_wins + r.lww_remote_wins > 0)
                .count();
            tracing::info!(
                entities_with_action,
                "auto-backfill pass completed",
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, "auto-backfill pass failed");
        }
    }
    Ok(())
}

/// Per-entity outcome of a single backfill pass. Counts only —
/// the orchestrator logs the per-row decisions; the user-facing
/// surface (Settings UI in B.3) just renders the totals.
#[derive(Debug, Default, Serialize)]
pub struct EntityBackfillReport {
    pub entity: String,
    /// Set when the entity-level dispatch produced an
    /// uncatchable error (network / JSON / SQLite). Per-row
    /// errors are tallied inside `push_failed` / `pull_failed`
    /// instead so a single bad row doesn't take the whole
    /// entity down.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub pushed: u32,
    pub push_skipped_out_of_date: u32,
    pub push_failed: u32,
    pub pulled: u32,
    pub pull_failed: u32,
    pub lww_local_wins: u32,
    pub lww_remote_wins: u32,
    pub lww_failed: u32,
    /// Set when the entity is skipped entirely (e.g. `track` in
    /// B.2 v1). Differs from `error` semantically: this isn't a
    /// failure, just deferred coverage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<&'static str>,
}

/// Aggregate over the per-entity passes.
#[derive(Debug, Serialize)]
pub struct BackfillReport {
    pub reports: Vec<EntityBackfillReport>,
}

/// Backfill execution outcome surfaced to the Tauri command.
/// Mirrors the digest-check outcome shape so the Settings UI can
/// render them with a single `match`.
#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BackfillOutcome {
    /// The pass ran end-to-end. `reports` carries per-entity
    /// counts.
    Ran { reports: Vec<EntityBackfillReport> },
    /// A gate short-circuited the pass before any HTTP / SQLite
    /// work happened. `reason` is one of the static strings the
    /// command paths emit ("offline" / "sync_mode_local" /
    /// "not_configured") — same shape the digest command uses.
    Skipped { reason: &'static str },
    /// A second concurrent caller arrived while the first was
    /// still in flight. The lock holder finishes; this caller
    /// returns immediately.
    AlreadyRunning,
}

/// Run a single backfill pass. Assumes the caller already
/// validated gates (offline / sync mode / JWT presence) and
/// acquired [`AppState::backfill_lock`].
///
/// `client` is taken as a parameter (not built inside) so the
/// caller can hand the same builder it used for the digest pass —
/// reuses the underlying `reqwest::Client` connection pool.
pub async fn run_backfill(
    state: &AppState,
    client: &WaveflowServerClient,
    profile_canonical_id: Option<&str>,
) -> AppResult<BackfillReport> {
    let pool = state.require_profile_pool().await?;
    let mut reports = Vec::with_capacity(digest::SUPPORTED_ENTITIES.len());

    for entity in digest::SUPPORTED_ENTITIES {
        let entity = *entity;
        let mut report = EntityBackfillReport {
            entity: entity.to_string(),
            ..Default::default()
        };

        // Per-entity scope check mirrors the digest_check
        // command (commands/sync.rs). User-scoped entities skip
        // the canonical id; profile-scoped need it.
        let canonical_arg = match entity {
            "library" | "playlist" | "track" => {
                let Some(c) = profile_canonical_id else {
                    report.error = Some("profile_canonical_id missing".into());
                    reports.push(report);
                    continue;
                };
                Some(c)
            }
            "liked_track" | "track_rating" => None,
            _ => {
                report.skipped_reason = Some("unsupported_entity");
                reports.push(report);
                continue;
            }
        };

        let local = match digest::read_local_digest(&pool, entity).await {
            Ok(d) => d,
            Err(err) => {
                report.error = Some(format!("local digest: {err}"));
                reports.push(report);
                continue;
            }
        };
        let remote = match digest::client::fetch_remote_digest(client, entity, canonical_arg).await
        {
            Ok(d) => d,
            Err(err) => {
                report.error = Some(format!("remote digest: {err}"));
                reports.push(report);
                continue;
            }
        };
        let d: DigestDiff = digest::diff::diff(&local, &remote);
        if d.in_sync {
            // Fast path — nothing to do.
            reports.push(report);
            continue;
        }

        let remote_max_hlc = remote.max_hlc;

        // ── push direction ───────────────────────────────────
        let push_res = push::push_missing_remotely(
            state,
            &pool,
            entity,
            &d.missing_remotely,
            remote_max_hlc,
        )
        .await;
        match push_res {
            Ok(stats) => {
                report.pushed = stats.pushed;
                report.push_skipped_out_of_date = stats.skipped_out_of_date;
                report.push_failed = stats.failed;
            }
            Err(err) => {
                report.error = Some(format!("push: {err}"));
                reports.push(report);
                continue;
            }
        }

        // ── pull direction ───────────────────────────────────
        let pull_res = pull::pull_missing_locally(
            state,
            client,
            &pool,
            entity,
            canonical_arg,
            &d.missing_locally,
        )
        .await;
        match pull_res {
            Ok(stats) => {
                report.pulled = stats.pulled;
                report.pull_failed = stats.failed;
            }
            Err(err) => {
                report.error = Some(format!("pull: {err}"));
                reports.push(report);
                continue;
            }
        }

        // ── LWW resolution ───────────────────────────────────
        let lww_res = lww::resolve_divergent(
            state,
            client,
            &pool,
            entity,
            canonical_arg,
            &d.divergent,
        )
        .await;
        match lww_res {
            Ok(stats) => {
                report.lww_local_wins = stats.local_wins;
                report.lww_remote_wins = stats.remote_wins;
                report.lww_failed = stats.failed;
            }
            Err(err) => {
                report.error = Some(format!("lww: {err}"));
            }
        }

        // After a successful pass for `liked_track` / `track_rating` /
        // `library` / `playlist`, drain the outbox so the re-emitted
        // ops fly to the server without waiting the 30 s tick.
        if report.pushed > 0 {
            state.drain.notify();
        }

        reports.push(report);
    }

    Ok(BackfillReport { reports })
}
