import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  CheckCircle2,
  CloudOff,
  Loader2,
  RefreshCw,
  Server,
  WifiOff,
} from "lucide-react";
import {
  syncBackfillGetEnabled,
  syncBackfillNow,
  syncBackfillSetEnabled,
  syncDigestCheck,
  type BackfillOutcome,
  type SyncDigestOutcome,
  type SyncDigestReport,
} from "../../../lib/tauri/sync";

/**
 * Settings → Diagnostics → "Sync status" card (RFC-003 Phase B.3).
 *
 * Surface the digest_check + backfill commands the B.1/B.2 PRs
 * shipped so users can see whether their local state matches the
 * server's + trigger reconciliation when it doesn't.
 *
 * The card is intentionally read-only beyond two action buttons —
 * no continuous polling, no background heartbeat. Triggers:
 *
 * - On mount: one digest_check pass (cheap, read-only).
 * - "Refresh" button: another digest_check.
 * - "Resync now" button: a backfill pass + automatic digest_check
 *   re-run after to surface the new state.
 *
 * Gating reasons surfaced (matches the backend's `Skipped { reason }`):
 *
 * - `offline` — user enabled offline mode.
 * - `sync_mode_local` — profile is set to Local mode.
 * - `not_configured` — no server URL or no JWT.
 * - `profile_canonical_id_missing` — profile hasn't been backfilled
 *   with its canonical id yet (drain task handles it on next pass).
 */

type CardState =
  | { kind: "loading" }
  | { kind: "ran"; reports: SyncDigestReport[] }
  | { kind: "skipped"; reason: string }
  | { kind: "error"; message: string };

export function SyncStatusCard() {
  const { t } = useTranslation();
  const [state, setState] = useState<CardState>({ kind: "loading" });
  const [backfilling, setBackfilling] = useState(false);
  const [autoEnabled, setAutoEnabled] = useState<boolean | null>(null);
  const [autoToggleBusy, setAutoToggleBusy] = useState(false);
  const [backfillOutcome, setBackfillOutcome] = useState<BackfillOutcome | null>(
    null,
  );
  // Runtime errors from `syncBackfillNow` (network, deserialize,
  // unexpected status) are tracked separately from the backend's
  // `BackfillOutcome` wire shape: mapping them onto `Skipped { reason }`
  // would lie to the user ("the backend chose not to run") when the
  // truth is "the call itself blew up". Cleared on every fresh
  // attempt so a successful retry hides the stale banner.
  const [backfillError, setBackfillError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setState({ kind: "loading" });
    try {
      const outcome: SyncDigestOutcome = await syncDigestCheck();
      if (outcome.status === "skipped") {
        setState({ kind: "skipped", reason: outcome.reason });
      } else {
        setState({ kind: "ran", reports: outcome.reports });
      }
    } catch (err) {
      setState({
        kind: "error",
        message: err instanceof Error ? err.message : String(err),
      });
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      // `Promise.allSettled` so a digest_check failure doesn't
      // strand `autoEnabled` at `null` for the rest of the
      // session — the toggle would then be disabled because the
      // checkbox gates on `autoEnabled === null` (loading). The
      // two reads are independent backend calls; surface each
      // outcome on its own state.
      const [digestRes, enabledRes] = await Promise.allSettled([
        syncDigestCheck(),
        syncBackfillGetEnabled(),
      ]);
      if (cancelled) return;

      setAutoEnabled(
        enabledRes.status === "fulfilled" ? enabledRes.value : false,
      );

      if (digestRes.status === "fulfilled") {
        const outcome = digestRes.value;
        if (outcome.status === "skipped") {
          setState({ kind: "skipped", reason: outcome.reason });
        } else {
          setState({ kind: "ran", reports: outcome.reports });
        }
      } else {
        const err = digestRes.reason;
        setState({
          kind: "error",
          message: err instanceof Error ? err.message : String(err),
        });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const handleToggleAuto = useCallback(async (next: boolean) => {
    setAutoToggleBusy(true);
    try {
      const stored = await syncBackfillSetEnabled(next);
      setAutoEnabled(stored);
      // Clear any stale banner from a previous failed toggle so a
      // successful retry doesn't keep the rose-600 error visible.
      setBackfillError(null);
    } catch (err) {
      // Surface as the backfill-error banner — same channel a
      // failed manual click uses, so the user sees one consistent
      // error path rather than a separate toast.
      setBackfillError(err instanceof Error ? err.message : String(err));
    } finally {
      setAutoToggleBusy(false);
    }
  }, []);

  const handleBackfill = useCallback(async () => {
    setBackfilling(true);
    setBackfillOutcome(null);
    setBackfillError(null);
    try {
      const outcome = await syncBackfillNow();
      setBackfillOutcome(outcome);
      // Refresh the digest snapshot post-backfill so the user
      // sees the new state without an extra click. Only when the
      // pass actually ran — `skipped` / `already_running` outcomes
      // wouldn't change anything to re-check.
      if (outcome.status === "ran") {
        void refresh();
      }
    } catch (err) {
      setBackfillError(err instanceof Error ? err.message : String(err));
    } finally {
      setBackfilling(false);
    }
  }, [refresh]);

  const overall = useOverallStatus(state);

  return (
    <div className="py-5 px-4 rounded-xl hover:bg-zinc-50 dark:hover:bg-zinc-800/30 transition-colors">
      <div className="flex items-start justify-between gap-4">
        <div className="flex items-start space-x-4 min-w-0">
          {overall.icon}
          <div className="min-w-0">
            <div className="text-sm font-medium text-zinc-900 dark:text-white">
              {t("settings.syncStatus.title")}
            </div>
            <div className="text-xs text-zinc-400">{overall.subtitle(t)}</div>
            {backfillError ? (
              <div className="mt-2 text-xs text-rose-600 dark:text-rose-400">
                {t("settings.syncStatus.backfillError", {
                  message: backfillError,
                })}
              </div>
            ) : backfillOutcome ? (
              <div className="mt-2 text-xs">
                <BackfillOutcomeSummary outcome={backfillOutcome} />
              </div>
            ) : null}
          </div>
        </div>
        <div className="flex items-center space-x-2 shrink-0">
          <button
            type="button"
            onClick={refresh}
            disabled={state.kind === "loading" || backfilling}
            className="flex items-center space-x-2 px-4 py-2 rounded-xl border border-zinc-200 bg-white text-sm font-medium text-zinc-700 hover:bg-zinc-50 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <RefreshCw
              size={14}
              aria-hidden="true"
              className={state.kind === "loading" ? "animate-spin" : undefined}
            />
            <span>{t("settings.syncStatus.refresh")}</span>
          </button>
          <button
            type="button"
            onClick={handleBackfill}
            disabled={
              state.kind !== "ran" ||
              state.reports.every((r) => r.in_sync) ||
              backfilling
            }
            className="flex items-center space-x-2 px-4 py-2 rounded-xl bg-emerald-600 text-sm font-medium text-white hover:bg-emerald-700 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {backfilling ? (
              <Loader2 size={14} aria-hidden="true" className="animate-spin" />
            ) : (
              <Server size={14} aria-hidden="true" />
            )}
            <span>{t("settings.syncStatus.resyncNow")}</span>
          </button>
        </div>
      </div>

      {state.kind === "ran" && state.reports.length > 0 ? (
        <div className="mt-4 ml-9">
          <EntityReportTable reports={state.reports} />
        </div>
      ) : null}

      <div className="mt-4 ml-9 flex items-center justify-between gap-3 text-xs">
        <div className="min-w-0">
          <div className="font-medium text-zinc-700 dark:text-zinc-200">
            {t("settings.syncStatus.autoToggleTitle")}
          </div>
          <div className="text-zinc-400">
            {t("settings.syncStatus.autoToggleSubtitle")}
          </div>
        </div>
        <label className="inline-flex items-center cursor-pointer shrink-0">
          <input
            type="checkbox"
            className="sr-only peer"
            checked={autoEnabled === true}
            disabled={autoEnabled === null || autoToggleBusy}
            onChange={(e) => void handleToggleAuto(e.target.checked)}
            aria-label={t("settings.syncStatus.autoToggleTitle") ?? undefined}
          />
          <span className="relative w-10 h-6 bg-zinc-200 dark:bg-zinc-700 rounded-full peer-checked:bg-emerald-500 transition-colors peer-disabled:opacity-50 peer-focus-visible:ring-2 peer-focus-visible:ring-emerald-500 peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-white dark:peer-focus-visible:ring-offset-zinc-900">
            <span className="absolute top-0.5 left-0.5 w-5 h-5 bg-white rounded-full transition-transform peer-checked:translate-x-4" />
          </span>
        </label>
      </div>
    </div>
  );
}

interface OverallStatus {
  icon: ReactNode;
  subtitle: (t: ReturnType<typeof useTranslation>["t"]) => string;
}

function useOverallStatus(state: CardState): OverallStatus {
  return useMemo<OverallStatus>(() => {
    switch (state.kind) {
      case "loading":
        return {
          icon: (
            <Loader2
              size={20}
              className="text-zinc-400 shrink-0 animate-spin"
              aria-hidden="true"
            />
          ),
          subtitle: (t) => t("settings.syncStatus.loading"),
        };
      case "ran": {
        const total = state.reports.length;
        const inSync = state.reports.filter((r) => r.in_sync).length;
        if (total === 0) {
          return {
            icon: (
              <AlertCircle
                size={20}
                className="text-zinc-400 shrink-0"
                aria-hidden="true"
              />
            ),
            subtitle: (t) => t("settings.syncStatus.subtitleEmpty"),
          };
        }
        if (inSync === total) {
          return {
            icon: (
              <CheckCircle2
                size={20}
                className="text-emerald-500 shrink-0"
                aria-hidden="true"
              />
            ),
            subtitle: (t) => t("settings.syncStatus.subtitleAllInSync"),
          };
        }
        return {
          icon: (
            <AlertCircle
              size={20}
              className="text-amber-500 shrink-0"
              aria-hidden="true"
            />
          ),
          subtitle: (t) =>
            t("settings.syncStatus.subtitleOutOfSync", {
              outOfSync: total - inSync,
              total,
            }),
        };
      }
      case "skipped": {
        const reasonIcon =
          state.reason === "offline" ? (
            <WifiOff
              size={20}
              className="text-zinc-400 shrink-0"
              aria-hidden="true"
            />
          ) : (
            <CloudOff
              size={20}
              className="text-zinc-400 shrink-0"
              aria-hidden="true"
            />
          );
        return {
          icon: reasonIcon,
          subtitle: (t) =>
            t(`settings.syncStatus.reason.${state.reason}`, {
              defaultValue: t("settings.syncStatus.reasonGeneric", {
                reason: state.reason,
              }),
            }),
        };
      }
      case "error":
        return {
          icon: (
            <AlertCircle
              size={20}
              className="text-rose-500 shrink-0"
              aria-hidden="true"
            />
          ),
          subtitle: (t) =>
            t("settings.syncStatus.error", { message: state.message }),
        };
    }
  }, [state]);
}

function EntityReportTable({ reports }: { reports: SyncDigestReport[] }) {
  const { t } = useTranslation();
  return (
    <div className="overflow-hidden rounded-lg border border-zinc-200 dark:border-zinc-700">
      <table className="w-full text-xs">
        <thead className="bg-zinc-50 dark:bg-zinc-800/50 text-zinc-500 dark:text-zinc-400">
          <tr>
            <th className="px-3 py-2 text-left font-medium">
              {t("settings.syncStatus.col.entity")}
            </th>
            <th className="px-3 py-2 text-center font-medium">
              {t("settings.syncStatus.col.state")}
            </th>
            <th className="px-3 py-2 text-right font-medium">
              {t("settings.syncStatus.col.missingLocally")}
            </th>
            <th className="px-3 py-2 text-right font-medium">
              {t("settings.syncStatus.col.missingRemotely")}
            </th>
            <th className="px-3 py-2 text-right font-medium">
              {t("settings.syncStatus.col.divergent")}
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-zinc-200 dark:divide-zinc-700">
          {reports.map((r) => (
            <tr key={r.entity}>
              <td className="px-3 py-2 font-mono text-zinc-700 dark:text-zinc-200">
                {r.entity}
              </td>
              <td className="px-3 py-2 text-center">
                {r.in_sync ? (
                  <span
                    className="inline-flex items-center text-emerald-600 dark:text-emerald-400"
                    title={t("settings.syncStatus.inSync") ?? undefined}
                  >
                    <CheckCircle2 size={14} aria-hidden="true" />
                  </span>
                ) : (
                  <span
                    className="inline-flex items-center text-amber-600 dark:text-amber-400"
                    title={t("settings.syncStatus.outOfSync") ?? undefined}
                  >
                    <AlertCircle size={14} aria-hidden="true" />
                  </span>
                )}
              </td>
              <td className="px-3 py-2 text-right tabular-nums text-zinc-700 dark:text-zinc-200">
                {r.missing_locally}
              </td>
              <td className="px-3 py-2 text-right tabular-nums text-zinc-700 dark:text-zinc-200">
                {r.missing_remotely}
              </td>
              <td className="px-3 py-2 text-right tabular-nums text-zinc-700 dark:text-zinc-200">
                {r.divergent}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function BackfillOutcomeSummary({ outcome }: { outcome: BackfillOutcome }) {
  const { t } = useTranslation();
  if (outcome.status === "already_running") {
    return (
      <span className="text-amber-600 dark:text-amber-400">
        {t("settings.syncStatus.backfillAlreadyRunning")}
      </span>
    );
  }
  if (outcome.status === "skipped") {
    return (
      <span className="text-zinc-500 dark:text-zinc-400">
        {t("settings.syncStatus.backfillSkipped", { reason: outcome.reason })}
      </span>
    );
  }
  const totals = outcome.reports.reduce(
    (acc, r) => {
      acc.pushed += r.pushed;
      acc.pulled += r.pulled;
      acc.lww += r.lww_local_wins + r.lww_remote_wins;
      acc.failed +=
        r.push_failed + r.pull_failed + r.lww_failed + (r.error ? 1 : 0);
      return acc;
    },
    { pushed: 0, pulled: 0, lww: 0, failed: 0 },
  );
  return (
    <span className="text-zinc-600 dark:text-zinc-300">
      {t("settings.syncStatus.backfillSummary", totals)}
    </span>
  );
}
