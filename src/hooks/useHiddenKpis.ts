import { useCallback, useMemo } from "react";
import { useProfileSetting } from "./useProfileSetting";

/**
 * Stable identifiers for the KPI cards on the Statistics view. Adding
 * a new KPI = append an id here, render it behind `isHidden(id)`, and
 * add a checkbox row in the Settings card — visibility comes for free.
 */
export type StatsKpiId =
  "total_plays" | "total_time" | "unique_tracks" | "completion_rate";

export const STATS_KPI_IDS: readonly StatsKpiId[] = [
  "total_plays",
  "total_time",
  "unique_tracks",
  "completion_rate",
] as const;

const KEY = "stats.hidden_kpis";

/**
 * Window event broadcast after a write so a mounted Statistics view
 * re-reads when the Settings checkboxes change without remounting.
 */
export const HIDDEN_KPIS_EVENT = "waveflow:stats-hidden-kpis-changed";

/** Nothing hidden. Module-level so its identity is stable — the shared
 *  hook resets to this object on every profile switch. */
const DEFAULT_HIDDEN: StatsKpiId[] = [];

/**
 * Parse the persisted JSON array, tolerating junk. Unknown ids are
 * dropped so a stale setting from a future build never hides a card
 * that no longer maps to it.
 */
function parseHidden(raw: string | null): StatsKpiId[] {
  if (raw == null) return DEFAULT_HIDDEN;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return DEFAULT_HIDDEN;
    return parsed.filter((v): v is StatsKpiId =>
      STATS_KPI_IDS.includes(v as StatsKpiId),
    );
  } catch {
    return DEFAULT_HIDDEN;
  }
}

export interface HiddenKpis {
  hidden: Set<StatsKpiId>;
  isHidden: (id: StatsKpiId) => boolean;
  /** Fire-and-forget: optimistic UI update, persistence is serialized
   *  internally and rolls back on failure. */
  toggle: (id: StatsKpiId) => void;
  /**
   * `false` until the first per-profile read resolves. Consumers that
   * render conditionally on `isHidden` should wait for this so hidden
   * cards never flash visible before the preference loads.
   */
  ready: boolean;
}

/**
 * Per-profile visibility for the Statistics KPI cards, backed by
 * `profile_setting['stats.hidden_kpis']` (JSON array of hidden ids).
 * Default = nothing hidden, so every profile keeps the current
 * layout until the user opts a card out from Settings → Appearance.
 *
 * Concurrency, profile isolation and rollback all live in
 * [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useHiddenKpis(): HiddenKpis {
  const {
    value: hiddenIds,
    ready,
    setValue,
  } = useProfileSetting<StatsKpiId[]>({
    key: KEY,
    defaultValue: DEFAULT_HIDDEN,
    parse: parseHidden,
    // Persist in declaration order for a stable, diff-friendly blob.
    serialize: (ids) =>
      JSON.stringify(STATS_KPI_IDS.filter((k) => ids.includes(k))),
    valueType: "json",
    event: HIDDEN_KPIS_EVENT,
    label: "useHiddenKpis",
  });

  const hidden = useMemo(() => new Set(hiddenIds), [hiddenIds]);

  const toggle = useCallback(
    (id: StatsKpiId) => {
      // Functional update so back-to-back clicks each build on the
      // previous one rather than on a render-lagged snapshot.
      void setValue((previous) =>
        previous.includes(id)
          ? previous.filter((k) => k !== id)
          : [...previous, id],
      );
    },
    [setValue],
  );

  const isHidden = useCallback((id: StatsKpiId) => hidden.has(id), [hidden]);

  return { hidden, isHidden, toggle, ready };
}
