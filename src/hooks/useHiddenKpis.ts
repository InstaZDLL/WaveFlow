import { useCallback, useEffect, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";
import { useProfile } from "./useProfile";

/**
 * Stable identifiers for the KPI cards on the Statistics view. Adding
 * a new KPI = append an id here, render it behind `isHidden(id)`, and
 * add a checkbox row in the Settings card — visibility comes for free.
 */
export type StatsKpiId =
  | "total_plays"
  | "total_time"
  | "unique_tracks"
  | "completion_rate";

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
 * Same pattern as `useWrappedBannerVisibility`.
 */
export const HIDDEN_KPIS_EVENT = "waveflow:stats-hidden-kpis-changed";

/**
 * Parse the persisted JSON array, tolerating junk. Unknown ids are
 * dropped so a stale setting from a future build never hides a card
 * that no longer maps to it.
 */
function parseHidden(raw: string | null): StatsKpiId[] {
  if (raw == null) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((v): v is StatsKpiId =>
      STATS_KPI_IDS.includes(v as StatsKpiId),
    );
  } catch {
    return [];
  }
}

export interface HiddenKpis {
  hidden: Set<StatsKpiId>;
  isHidden: (id: StatsKpiId) => boolean;
  toggle: (id: StatsKpiId) => Promise<void>;
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
 */
export function useHiddenKpis(): HiddenKpis {
  const { activeProfile } = useProfile();
  const [hidden, setHidden] = useState<Set<StatsKpiId>>(new Set());
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    // Start each profile from a clean slate: clear the previous
    // profile's hidden set and readiness immediately, so a stale set
    // can never leak into the Settings checkboxes (which read
    // `isHidden` ungated) before — or if — the new read lands.
    /* eslint-disable react-hooks/set-state-in-effect */
    setReady(false);
    setHidden(new Set());
    /* eslint-enable react-hooks/set-state-in-effect */
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        setHidden(new Set(parseHidden(raw)));
      } catch (err) {
        console.error("[useHiddenKpis] read failed", err);
      } finally {
        if (!cancelled) setReady(true);
      }
    };
    void refresh();
    window.addEventListener(HIDDEN_KPIS_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(HIDDEN_KPIS_EVENT, refresh);
    };
  }, [activeProfile?.id]);

  const toggle = useCallback(
    async (id: StatsKpiId) => {
      // Snapshot the pre-toggle state so we can roll back if the write
      // fails. `[hidden]` in the deps keeps this closure fresh.
      const previous = hidden;
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      setHidden(next); // optimistic
      // Persist in declaration order for a stable, diff-friendly blob.
      const nextArray = STATS_KPI_IDS.filter((k) => next.has(k));
      try {
        await setProfileSetting(KEY, JSON.stringify(nextArray), "json");
        window.dispatchEvent(new CustomEvent(HIDDEN_KPIS_EVENT));
      } catch (err) {
        console.error("[useHiddenKpis] write failed", err);
        // Roll back so the UI stays consistent with what's persisted;
        // skip the broadcast since nothing actually changed.
        setHidden(previous);
      }
    },
    [hidden],
  );

  const isHidden = useCallback((id: StatsKpiId) => hidden.has(id), [hidden]);

  return { hidden, isHidden, toggle, ready };
}
