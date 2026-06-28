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

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const raw = await getProfileSetting(KEY);
        if (cancelled) return;
        setHidden(new Set(parseHidden(raw)));
      } catch (err) {
        console.error("[useHiddenKpis] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(HIDDEN_KPIS_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(HIDDEN_KPIS_EVENT, refresh);
    };
  }, [activeProfile?.id]);

  const toggle = useCallback(async (id: StatsKpiId) => {
    let nextArray: StatsKpiId[] = [];
    setHidden((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      // Persist in declaration order for a stable, diff-friendly blob.
      nextArray = STATS_KPI_IDS.filter((k) => next.has(k));
      return next;
    });
    try {
      await setProfileSetting(KEY, JSON.stringify(nextArray), "string");
      window.dispatchEvent(new CustomEvent(HIDDEN_KPIS_EVENT));
    } catch (err) {
      console.error("[useHiddenKpis] write failed", err);
    }
  }, []);

  const isHidden = useCallback((id: StatsKpiId) => hidden.has(id), [hidden]);

  return { hidden, isHidden, toggle };
}
