import { invoke } from "@tauri-apps/api/core";

/**
 * Settings → Diagnostics → "Sync status" card surface.
 *
 * RFC-003 Phase B.3 — wraps the desktop's B.1 (digest_check) +
 * B.2 (backfill) Tauri commands so the Settings card can poll
 * the local-vs-server state and trigger reconciliation without
 * the user having to grok the protocol.
 */

/**
 * Per-entity report returned by `sync_digest_check`. Counts
 * only — the full diff member lists stay backend-side for the
 * backfill orchestrator (B.2).
 */
export interface SyncDigestReport {
  entity: string;
  in_sync: boolean;
  local_version: number;
  remote_version: number;
  missing_locally: number;
  missing_remotely: number;
  divergent: number;
}

/**
 * Outcome of `sync_digest_check`. `Skipped { reason }` covers the
 * gating paths (offline / SyncMode::Local / no JWT / no profile
 * canonical id) so the UI can render an explicit "blocked" state
 * instead of a misleading empty "all in sync" surface.
 */
export type SyncDigestOutcome =
  | { status: "ran"; reports: SyncDigestReport[] }
  | { status: "skipped"; reason: string };

/**
 * Per-entity report returned by `sync_backfill_now`. Mirrors the
 * Rust shape verbatim — `entity` + per-bucket counters +
 * `error` / `skipped_reason` for the failure / deferred paths
 * (Phase B.2 deferred `track` via `track_backfill_not_implemented`).
 */
export interface EntityBackfillReport {
  entity: string;
  error?: string | null;
  pushed: number;
  push_skipped_out_of_date: number;
  push_failed: number;
  pulled: number;
  pull_failed: number;
  lww_local_wins: number;
  lww_remote_wins: number;
  lww_failed: number;
  skipped_reason?: string | null;
}

/**
 * Outcome of `sync_backfill_now`. `AlreadyRunning` covers the
 * concurrent-call guard (a second user click while a pass is
 * still in flight returns immediately without firing a parallel
 * sweep).
 */
export type BackfillOutcome =
  | { status: "ran"; reports: EntityBackfillReport[] }
  | { status: "skipped"; reason: string }
  | { status: "already_running" };

/**
 * Trigger a digest check pass. `entity` narrows the check to a
 * single entity name (one of `library` / `playlist` / `track` /
 * `liked_track` / `track_rating`); omit it to sweep all five.
 *
 * Cheap by design — read-only, no SQLite writes, no outbox ops.
 * The Settings card calls it on mount + on every "Refresh" click.
 */
export function syncDigestCheck(entity?: string): Promise<SyncDigestOutcome> {
  return invoke<SyncDigestOutcome>("sync_digest_check", { entity });
}

/**
 * Trigger a backfill pass. Holds the per-state `backfill_lock`
 * for the duration of the pass — concurrent calls (e.g. the
 * user double-clicks) get `AlreadyRunning` back rather than
 * racing the same diff buckets.
 *
 * Not cheap: per row, either an outbox enqueue (push direction)
 * or an HTTP fetch + direct-SQL UPSERT (pull / LWW remote-wins).
 * The Settings card surfaces a spinner while the call is in flight.
 */
export function syncBackfillNow(): Promise<BackfillOutcome> {
  return invoke<BackfillOutcome>("sync_backfill_now");
}

/**
 * Read the per-profile auto-backfill enabled flag. The Settings
 * card renders this as a toggle alongside the manual "Resync now"
 * button.
 */
export function syncBackfillGetEnabled(): Promise<boolean> {
  return invoke<boolean>("sync_backfill_get_enabled");
}

/**
 * Persist the per-profile auto-backfill enabled flag. When `true`,
 * `maybe_auto_backfill` fires a pass on boot + every sync-mode
 * flip to Hybrid. The toggle itself doesn't fire an immediate
 * pass — the user can click the manual button right after if
 * they want one now.
 */
export function syncBackfillSetEnabled(enabled: boolean): Promise<boolean> {
  return invoke<boolean>("sync_backfill_set_enabled", { enabled });
}

/**
 * Snapshot of the backfill task surfaced to the Settings card.
 * Powers the "Last sync: X ago" affordance and the disabled state
 * on the manual button while a pass is mid-flight.
 */
export interface SyncBackfillStatus {
  /** Epoch milliseconds of the last successful backfill pass,
   * or `null` when none has ever run on this profile. */
  last_run_at: number | null;
  /** `true` when a pass is currently holding the per-state lock. */
  in_progress: boolean;
}

/**
 * Read the persisted "last successful backfill" timestamp + the
 * live in-progress state. Cheap (one SELECT + one non-blocking
 * `try_lock`), called on mount + after the manual button completes.
 */
export function syncBackfillGetStatus(): Promise<SyncBackfillStatus> {
  return invoke<SyncBackfillStatus>("sync_backfill_get_status");
}

/**
 * Read the per-profile heartbeat cadence (minutes between automatic
 * background passes). Clamped server-side to the documented
 * 15-1440 range so a malformed stored value can't crash the UI.
 */
export function syncBackfillGetHeartbeatInterval(): Promise<number> {
  return invoke<number>("sync_backfill_get_heartbeat_interval");
}

/**
 * Persist the per-profile heartbeat cadence (minutes). The backend
 * clamps to the [15, 1440] range and echoes the stored value so the
 * UI hydrates with whatever actually landed in the row.
 */
export function syncBackfillSetHeartbeatInterval(
  minutes: number,
): Promise<number> {
  return invoke<number>("sync_backfill_set_heartbeat_interval", { minutes });
}

/**
 * Hex-encoded `(canonical_id, payload_hash)` pair returned by the
 * detailed digest endpoint. Used by the drill-down panel to show
 * which specific rows the two replicas disagree on.
 *
 * For the `missing_remotely` and `divergent` buckets, the
 * desktop's `local_payload_hash` is populated. For `divergent`,
 * the server's `remote_payload_hash` is also present so the UI
 * can render a side-by-side hash preview.
 */
export interface SyncDigestDivergentMember {
  canonical_id: string;
  /** Hex-encoded local payload_hash. Empty string when the member
   * is absent locally (i.e. `missing_locally` direction handled by
   * the sibling `SyncDigestRemoteMember`). */
  local_payload_hash: string;
  /** Hex-encoded server payload_hash. Empty string when the member
   * is absent remotely (`missing_remotely` direction). */
  remote_payload_hash: string;
}

/**
 * Hex-encoded `(canonical_id, payload_hash)` pair from the server's
 * digest response — used to surface the `missing_locally` bucket,
 * where only the remote side has the row.
 */
export interface SyncDigestRemoteMember {
  canonical_id: string;
  /** Hex-encoded BLAKE3-256 payload_hash from the server. */
  payload_hash: string;
}

/**
 * Per-entity detailed digest diff — full member-level lists for
 * the drill-down panel. Each bucket is truncated server-side to
 * 100 entries so the IPC stays bounded; the `*_total` fields carry
 * the un-truncated count so the UI can render "+N more".
 */
export interface SyncDigestDetailed {
  entity: string;
  in_sync: boolean;
  local_version: number;
  remote_version: number;
  missing_locally: SyncDigestRemoteMember[];
  missing_locally_total: number;
  missing_remotely: SyncDigestDivergentMember[];
  missing_remotely_total: number;
  divergent: SyncDigestDivergentMember[];
  divergent_total: number;
}

/**
 * Outcome of [`syncDigestCheckDetailed`]. Mirrors the summary
 * variant's shape so the UI can render gating reasons (offline /
 * local mode / unconfigured) with the same code path.
 */
export type SyncDigestDetailedOutcome =
  | { status: "ran"; diff: SyncDigestDetailed }
  | { status: "skipped"; reason: string };

/**
 * Run a detailed digest check for a single entity. Returns the
 * full member lists for the three diff buckets, truncated to 100
 * entries per bucket. The Settings card calls this when the user
 * expands a row in the summary table.
 */
export function syncDigestCheckDetailed(
  entity: string,
): Promise<SyncDigestDetailedOutcome> {
  return invoke<SyncDigestDetailedOutcome>("sync_digest_check_detailed", {
    entity,
  });
}
