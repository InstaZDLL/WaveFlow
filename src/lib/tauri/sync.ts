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
