import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  CloudOff,
  Loader2,
  RefreshCw,
  Server,
  WifiOff,
} from "lucide-react";
import {
  syncBackfillGetEnabled,
  syncBackfillGetHeartbeatInterval,
  syncBackfillGetStatus,
  syncBackfillNow,
  syncBackfillSetEnabled,
  syncBackfillSetHeartbeatInterval,
  syncDigestCheck,
  syncDigestCheckDetailed,
  type BackfillOutcome,
  type SyncDigestDetailed,
  type SyncDigestOutcome,
  type SyncDigestReport,
} from "../../../lib/tauri/sync";

/**
 * Settings → Diagnostics → "Sync status" card (RFC-003 Phase B
 * polish).
 *
 * Surface the digest_check + backfill commands the B.1/B.2/B.3 PRs
 * shipped, plus the polish round:
 *
 * - "Last sync: X ago" subtitle hydrated from `sync_backfill_get_status`.
 * - Per-entity drill-down with the divergent / missing-locally /
 *   missing-remotely canonical_id + hash preview lists.
 * - Configurable heartbeat cadence (15 min – 24 h) for the
 *   background poll wired in `lib.rs::run`.
 *
 * The card stays read-only beyond the action buttons + the two
 * toggles — no continuous polling on this surface (the heartbeat
 * task lives in the backend and fires `maybe_auto_backfill`
 * independently); the timestamp self-refreshes every 30 s while
 * the card is mounted so "5 min ago" doesn't go stale.
 */

type CardState =
  | { kind: "loading" }
  | { kind: "ran"; reports: SyncDigestReport[] }
  | { kind: "skipped"; reason: string }
  | { kind: "error"; message: string };

type DetailedEntry =
  | { kind: "loading" }
  | { kind: "ran"; diff: SyncDigestDetailed }
  | { kind: "skipped"; reason: string }
  | { kind: "error"; message: string };

/**
 * Heartbeat cadence presets surfaced in the dropdown. Values
 * match the `[15, 1440]` clamp the backend applies — picking from
 * the list is always safe, the freeform path is reserved for
 * future power-user tooling.
 */
const CADENCE_OPTIONS: ReadonlyArray<{ value: number; labelKey: string }> = [
  { value: 15, labelKey: "settings.syncStatus.cadence.every15Min" },
  { value: 30, labelKey: "settings.syncStatus.cadence.every30Min" },
  { value: 60, labelKey: "settings.syncStatus.cadence.every1Hour" },
  { value: 360, labelKey: "settings.syncStatus.cadence.every6Hours" },
  { value: 720, labelKey: "settings.syncStatus.cadence.every12Hours" },
  { value: 1440, labelKey: "settings.syncStatus.cadence.every24Hours" },
] as const;

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
  const [lastRunAt, setLastRunAt] = useState<number | null>(null);
  const [cadence, setCadence] = useState<number | null>(null);
  const [cadenceBusy, setCadenceBusy] = useState(false);
  // Mirrors the backend's `state.backfill_lock` snapshot —
  // surfaced by `sync_backfill_get_status` and refreshed on
  // mount + after every `refresh()`. Used to disable the Resync
  // button while a heartbeat / mode-flip pass is mid-flight so
  // the user can't queue a redundant click that would surface
  // `AlreadyRunning` on return.
  const [serverInProgress, setServerInProgress] = useState(false);
  // Re-render every 30 s so the "X min ago" subtitle stays in
  // sync with wall-clock without polling the backend. Each tick
  // bumps a counter `nowTick`; we don't store the actual
  // timestamp because `formatRelativeTime` derives it from
  // `Date.now()` at render time.
  // `nowTick` is the dep that drags the "X min ago" suffix's
  // memo invalidation. We don't read the value directly — only
  // its identity matters — but stripping it with `[, setNowTick]`
  // would prevent passing it to `useOverallStatus` below, and
  // the memo there would never see a change to re-run
  // `formatLastSyncSuffix(lastRunAt)` (which samples `Date.now()`
  // at call time).
  const [nowTick, setNowTick] = useState(0);
  const [expandedEntity, setExpandedEntity] = useState<string | null>(null);
  const [detailed, setDetailed] = useState<Record<string, DetailedEntry>>({});
  // Generation counter bumped on every `refresh()` so in-flight
  // `syncDigestCheckDetailed` requests started before the user
  // clicked Refresh can't repopulate the freshly-cleared cache
  // with stale data. Each request captures the current value
  // and drops its result if a newer generation is active by
  // the time it resolves.
  const detailedGenRef = useRef(0);

  const refresh = useCallback(async () => {
    // Invalidate any in-flight detailed fetches BEFORE wiping the
    // cache so a request started pre-refresh can't repopulate it
    // with stale member lists. The bump is observed by the
    // closures captured in `handleToggleExpand`.
    detailedGenRef.current += 1;
    setState({ kind: "loading" });
    // Detailed cache is now stale — drop it. Next expand triggers
    // a fresh fetch (gated on the new generation).
    setDetailed({});
    setExpandedEntity(null);
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
    // Re-hydrate the last-run timestamp + in-progress flag too —
    // a heartbeat tick could have fired between the user clicking
    // Refresh and the status command returning.
    try {
      const status = await syncBackfillGetStatus();
      setLastRunAt(status.last_run_at);
      setServerInProgress(status.in_progress);
    } catch {
      // Best-effort; the subtitle just stays on its previous value.
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      // `Promise.allSettled` so a digest_check failure doesn't
      // strand `autoEnabled` / `cadence` / `lastRunAt` at their
      // initial null state — each of the four reads is
      // independent backend-side; we surface each outcome on its
      // own slice.
      const [digestRes, enabledRes, statusRes, cadenceRes] =
        await Promise.allSettled([
          syncDigestCheck(),
          syncBackfillGetEnabled(),
          syncBackfillGetStatus(),
          syncBackfillGetHeartbeatInterval(),
        ]);
      if (cancelled) return;

      setAutoEnabled(
        enabledRes.status === "fulfilled" ? enabledRes.value : false,
      );
      if (statusRes.status === "fulfilled") {
        setLastRunAt(statusRes.value.last_run_at);
        setServerInProgress(statusRes.value.in_progress);
      }
      if (cadenceRes.status === "fulfilled") {
        setCadence(cadenceRes.value);
      }

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

  // 30 s relative-time tick. Cheap — one re-render every 30 s
  // while the card is mounted; the rest of the tree is unaffected
  // because `nowTick` only lives here. `setNowTick` is stable
  // across renders so the empty dependency array is honest.
  useEffect(() => {
    const id = setInterval(() => setNowTick((n) => n + 1), 30_000);
    return () => clearInterval(id);
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

  const handleCadenceChange = useCallback(async (next: number) => {
    setCadenceBusy(true);
    try {
      const stored = await syncBackfillSetHeartbeatInterval(next);
      setCadence(stored);
      setBackfillError(null);
    } catch (err) {
      setBackfillError(err instanceof Error ? err.message : String(err));
    } finally {
      setCadenceBusy(false);
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

  const handleToggleExpand = useCallback(
    (entity: string, inSync: boolean) => {
      if (inSync) return;
      setExpandedEntity((prev) => (prev === entity ? null : entity));
      // Lazy-load the detailed diff on first expand. Don't re-fetch
      // if we already have one in cache — `refresh` clears the
      // cache so a manual Refresh re-hydrates.
      if (detailed[entity] !== undefined) return;
      // Capture the current generation so a `refresh()` mid-fetch
      // can invalidate this result on return rather than letting
      // it land into a freshly-cleared cache.
      const gen = detailedGenRef.current;
      setDetailed((prev) => ({ ...prev, [entity]: { kind: "loading" } }));
      void (async () => {
        try {
          const outcome = await syncDigestCheckDetailed(entity);
          if (gen !== detailedGenRef.current) return;
          setDetailed((prev) => ({
            ...prev,
            [entity]:
              outcome.status === "ran"
                ? { kind: "ran", diff: outcome.diff }
                : { kind: "skipped", reason: outcome.reason },
          }));
        } catch (err) {
          if (gen !== detailedGenRef.current) return;
          setDetailed((prev) => ({
            ...prev,
            [entity]: {
              kind: "error",
              message: err instanceof Error ? err.message : String(err),
            },
          }));
        }
      })();
    },
    [detailed],
  );

  const overall = useOverallStatus(state, lastRunAt, nowTick);

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
              backfilling ||
              serverInProgress
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
          <EntityReportTable
            reports={state.reports}
            expandedEntity={expandedEntity}
            detailed={detailed}
            onToggle={handleToggleExpand}
          />
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
          <span className="relative w-10 h-6 bg-zinc-200 dark:bg-zinc-700 rounded-full peer-checked:bg-emerald-600 transition-colors peer-disabled:opacity-50 peer-focus-visible:ring-2 peer-focus-visible:ring-emerald-500 peer-focus-visible:ring-offset-2 peer-focus-visible:ring-offset-white dark:peer-focus-visible:ring-offset-zinc-900">
            <span className="absolute top-0.5 left-0.5 w-5 h-5 bg-white rounded-full transition-transform peer-checked:translate-x-4" />
          </span>
        </label>
      </div>

      <div className="mt-3 ml-9 flex items-center justify-between gap-3 text-xs">
        <div className="min-w-0">
          <label
            htmlFor="sync-cadence-select"
            className="font-medium text-zinc-700 dark:text-zinc-200"
          >
            {t("settings.syncStatus.cadenceTitle")}
          </label>
          <div className="text-zinc-400">
            {t("settings.syncStatus.cadenceSubtitle")}
          </div>
        </div>
        <select
          id="sync-cadence-select"
          value={cadence ?? 60}
          disabled={
            cadence === null || cadenceBusy || autoEnabled !== true
          }
          onChange={(e) => void handleCadenceChange(Number(e.target.value))}
          className="shrink-0 px-3 py-1.5 rounded-lg border border-zinc-200 bg-white text-xs text-zinc-700 dark:border-zinc-700 dark:bg-zinc-800 dark:text-zinc-300 focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed"
        >
          {CADENCE_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {t(opt.labelKey)}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}

interface OverallStatus {
  icon: ReactNode;
  subtitle: (t: ReturnType<typeof useTranslation>["t"]) => string;
}

function useOverallStatus(
  state: CardState,
  lastRunAt: number | null,
  nowTick: number,
): OverallStatus {
  return useMemo<OverallStatus>(() => {
    // Touch `nowTick` so `react-hooks/exhaustive-deps` doesn't
    // strip it from the dep list. The value itself is unused —
    // its identity is the signal that the 30 s `setInterval` in
    // the parent has bumped wall-clock and the suffix needs to
    // re-sample `Date.now()`.
    void nowTick;
    const lastSyncSuffix = formatLastSyncSuffix(lastRunAt);
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
                className="text-emerald-600 dark:text-emerald-400 shrink-0"
                aria-hidden="true"
              />
            ),
            subtitle: (t) =>
              joinSubtitle(t("settings.syncStatus.subtitleAllInSync"), lastSyncSuffix(t)),
          };
        }
        return {
          icon: (
            <AlertCircle
              size={20}
              className="text-amber-600 dark:text-amber-400 shrink-0"
              aria-hidden="true"
            />
          ),
          subtitle: (t) =>
            joinSubtitle(
              t("settings.syncStatus.subtitleOutOfSync", {
                outOfSync: total - inSync,
                total,
              }),
              lastSyncSuffix(t),
            ),
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
              className="text-rose-600 dark:text-rose-400 shrink-0"
              aria-hidden="true"
            />
          ),
          subtitle: (t) =>
            t("settings.syncStatus.error", { message: state.message }),
        };
    }
    // `nowTick` enters the dep list so the 30 s `setInterval`
    // in the parent invalidates this memo and `formatLastSyncSuffix`
    // re-samples `Date.now()` — without it, the suffix stays
    // pinned to whatever wall-clock the previous render saw.
  }, [state, lastRunAt, nowTick]);
}

interface EntityReportTableProps {
  reports: SyncDigestReport[];
  expandedEntity: string | null;
  detailed: Record<string, DetailedEntry>;
  onToggle: (entity: string, inSync: boolean) => void;
}

function EntityReportTable({
  reports,
  expandedEntity,
  detailed,
  onToggle,
}: EntityReportTableProps) {
  const { t } = useTranslation();
  return (
    <div className="overflow-hidden rounded-lg border border-zinc-200 dark:border-zinc-700">
      <table className="w-full text-xs">
        <thead className="bg-zinc-50 dark:bg-zinc-800/50 text-zinc-500 dark:text-zinc-400">
          <tr>
            <th scope="col" className="w-6 px-2 py-2" aria-hidden="true" />
            <th scope="col" className="px-3 py-2 text-left font-medium">
              {t("settings.syncStatus.col.entity")}
            </th>
            <th scope="col" className="px-3 py-2 text-center font-medium">
              {t("settings.syncStatus.col.state")}
            </th>
            <th scope="col" className="px-3 py-2 text-right font-medium">
              {t("settings.syncStatus.col.missingLocally")}
            </th>
            <th scope="col" className="px-3 py-2 text-right font-medium">
              {t("settings.syncStatus.col.missingRemotely")}
            </th>
            <th scope="col" className="px-3 py-2 text-right font-medium">
              {t("settings.syncStatus.col.divergent")}
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-zinc-200 dark:divide-zinc-700">
          {reports.map((r) => {
            const expanded = expandedEntity === r.entity;
            const expandable = !r.in_sync;
            return (
              <ExpandableRow
                key={r.entity}
                report={r}
                expanded={expanded}
                expandable={expandable}
                detailed={detailed[r.entity]}
                onToggle={() => onToggle(r.entity, r.in_sync)}
              />
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

interface ExpandableRowProps {
  report: SyncDigestReport;
  expanded: boolean;
  expandable: boolean;
  detailed: DetailedEntry | undefined;
  onToggle: () => void;
}

function ExpandableRow({
  report: r,
  expanded,
  expandable,
  detailed,
  onToggle,
}: ExpandableRowProps) {
  const { t } = useTranslation();
  const interactive = expandable
    ? "cursor-pointer hover:bg-zinc-50 dark:hover:bg-zinc-800/40 focus-visible:outline-none focus-visible:bg-zinc-100 dark:focus-visible:bg-zinc-800/60"
    : "";
  // Keyboard handler so the chevron-row is reachable + activatable
  // without a mouse. We deliberately leave `role="row"` on the `<tr>`
  // (the implicit ARIA role of a `tr`) rather than swap to
  // `role="button"`: overriding it would break the table's
  // accessible tree (every row should expose a `row` role for
  // assistive tech to grok the grid structure). `aria-expanded`
  // + the visible chevron carry the "expandable button" affordance
  // for screen readers.
  const handleKeyDown = (e: KeyboardEvent<HTMLTableRowElement>) => {
    if (!expandable) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onToggle();
    }
  };
  return (
    <>
      <tr
        className={`group ${interactive}`}
        onClick={expandable ? onToggle : undefined}
        onKeyDown={expandable ? handleKeyDown : undefined}
        tabIndex={expandable ? 0 : undefined}
        aria-expanded={expandable ? expanded : undefined}
      >
        <td className="px-2 py-2 text-zinc-400">
          {expandable ? (
            expanded ? (
              <ChevronDown size={14} aria-hidden="true" />
            ) : (
              <ChevronRight size={14} aria-hidden="true" />
            )
          ) : null}
        </td>
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
      {expanded ? (
        <tr>
          <td
            colSpan={6}
            className="px-3 py-3 bg-zinc-50/50 dark:bg-zinc-800/30"
          >
            <DetailedPanel entry={detailed} />
          </td>
        </tr>
      ) : null}
    </>
  );
}

/**
 * Maximum number of canonical_ids rendered per bucket in the
 * drill-down panel. The backend already caps at 100; the UI
 * shows the first 20 to keep the panel scannable, with a
 * "+N more" footer surfacing the rest of the cap.
 */
const DRILLDOWN_PER_BUCKET_LIMIT = 20;

function DetailedPanel({ entry }: { entry: DetailedEntry | undefined }) {
  const { t } = useTranslation();
  if (entry === undefined || entry.kind === "loading") {
    return (
      <div className="flex items-center space-x-2 text-zinc-500 dark:text-zinc-400">
        <Loader2 size={12} className="animate-spin" aria-hidden="true" />
        <span>{t("settings.syncStatus.drilldown.loading")}</span>
      </div>
    );
  }
  if (entry.kind === "skipped") {
    return (
      <div className="text-zinc-500 dark:text-zinc-400">
        {t(`settings.syncStatus.reason.${entry.reason}`, {
          defaultValue: t("settings.syncStatus.reasonGeneric", {
            reason: entry.reason,
          }),
        })}
      </div>
    );
  }
  if (entry.kind === "error") {
    return (
      <div className="text-rose-600 dark:text-rose-400">
        {t("settings.syncStatus.drilldown.error", { message: entry.message })}
      </div>
    );
  }
  const d = entry.diff;
  const buckets: Array<{
    titleKey: string;
    rows: Array<{
      canonical_id: string;
      local_hash: string;
      remote_hash: string;
    }>;
    total: number;
  }> = [
    {
      titleKey: "settings.syncStatus.drilldown.missingLocally",
      rows: d.missing_locally.map((m) => ({
        canonical_id: m.canonical_id,
        local_hash: "",
        remote_hash: m.payload_hash,
      })),
      total: d.missing_locally_total,
    },
    {
      titleKey: "settings.syncStatus.drilldown.missingRemotely",
      rows: d.missing_remotely.map((m) => ({
        canonical_id: m.canonical_id,
        local_hash: m.local_payload_hash,
        remote_hash: m.remote_payload_hash,
      })),
      total: d.missing_remotely_total,
    },
    {
      titleKey: "settings.syncStatus.drilldown.divergent",
      rows: d.divergent.map((m) => ({
        canonical_id: m.canonical_id,
        local_hash: m.local_payload_hash,
        remote_hash: m.remote_payload_hash,
      })),
      total: d.divergent_total,
    },
  ];
  const nonEmpty = buckets.filter((b) => b.rows.length > 0);
  if (nonEmpty.length === 0) {
    return (
      <div className="text-zinc-500 dark:text-zinc-400">
        {t("settings.syncStatus.drilldown.empty")}
      </div>
    );
  }
  return (
    <div className="space-y-3">
      {nonEmpty.map((b) => (
        <DrilldownBucket
          key={b.titleKey}
          title={t(b.titleKey)}
          rows={b.rows}
          total={b.total}
        />
      ))}
    </div>
  );
}

function DrilldownBucket({
  title,
  rows,
  total,
}: {
  title: string;
  rows: Array<{ canonical_id: string; local_hash: string; remote_hash: string }>;
  total: number;
}) {
  const { t } = useTranslation();
  const shown = rows.slice(0, DRILLDOWN_PER_BUCKET_LIMIT);
  const remaining = total - shown.length;
  return (
    <div>
      <div className="text-xs font-semibold text-zinc-600 dark:text-zinc-300 mb-1">
        {title}{" "}
        <span className="font-normal text-zinc-400">({total})</span>
      </div>
      <ul className="space-y-0.5 text-[11px] font-mono text-zinc-600 dark:text-zinc-400">
        {shown.map((row, idx) => (
          <li key={`${row.canonical_id}-${idx}`} className="flex gap-2">
            <span className="truncate" title={row.canonical_id}>
              {row.canonical_id}
            </span>
            {row.local_hash || row.remote_hash ? (
              <span className="shrink-0 text-zinc-400 dark:text-zinc-500">
                {row.local_hash ? row.local_hash.slice(0, 8) : "—"}
                {" / "}
                {row.remote_hash ? row.remote_hash.slice(0, 8) : "—"}
              </span>
            ) : null}
          </li>
        ))}
      </ul>
      {remaining > 0 ? (
        <div className="mt-1 text-[11px] text-zinc-400 dark:text-zinc-500">
          {t("settings.syncStatus.drilldown.moreNotShown", {
            count: remaining,
          })}
        </div>
      ) : null}
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

/**
 * Format the "Last sync: X ago" suffix for the subtitle. Returns
 * a function so the i18n `t` is captured at render time (the
 * `useOverallStatus` hook receives a stale `t` otherwise).
 */
function formatLastSyncSuffix(
  lastRunAt: number | null,
): (t: ReturnType<typeof useTranslation>["t"]) => string {
  if (lastRunAt === null) {
    return (t) => t("settings.syncStatus.lastSyncNever");
  }
  // Recompute relative buckets at render time so the 30 s tick
  // can re-render with fresh values.
  const ageMs = Date.now() - lastRunAt;
  if (ageMs < 60_000) {
    return (t) => t("settings.syncStatus.lastSyncJustNow");
  }
  const minutes = Math.floor(ageMs / 60_000);
  if (minutes < 60) {
    return (t) =>
      t("settings.syncStatus.lastSyncMinutesAgo", { count: minutes });
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return (t) => t("settings.syncStatus.lastSyncHoursAgo", { count: hours });
  }
  const days = Math.floor(hours / 24);
  return (t) => t("settings.syncStatus.lastSyncDaysAgo", { count: days });
}

/** Join the in-sync / out-of-sync summary with the optional
 *  "Last sync: X ago" suffix. Skips the separator when the
 *  suffix is empty (no last_run_at). */
function joinSubtitle(base: string, suffix: string): string {
  return suffix ? `${base} · ${suffix}` : base;
}
