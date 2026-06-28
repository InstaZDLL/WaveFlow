import { useCallback, useEffect, useRef, useState } from "react";
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
 */
export function useHiddenKpis(): HiddenKpis {
  const { activeProfile } = useProfile();
  const [hidden, setHiddenState] = useState<Set<StatsKpiId>>(new Set());
  const [ready, setReady] = useState(false);

  // Authoritative copy read synchronously by `toggle` — React state
  // lags a render behind, so two rapid clicks would otherwise both
  // branch off the same stale snapshot and the second would clobber
  // the first. `setHidden` keeps the ref and the render state in lockstep.
  const hiddenRef = useRef<Set<StatsKpiId>>(hidden);
  const setHidden = useCallback((next: Set<StatsKpiId>) => {
    hiddenRef.current = next;
    setHiddenState(next);
  }, []);

  // Serializes persistence: each write runs only after the previous
  // one settles, so two rapid toggles can't complete out of order and
  // the last user action always wins the final DB state.
  const writeChainRef = useRef<Promise<unknown>>(Promise.resolve());
  // Monotonic token: only the most recently enqueued write broadcasts
  // the refresh event, so a stale queued write can't trigger a re-read
  // that reverts a newer optimistic state.
  const seqRef = useRef(0);

  // Latest active profile id, mirrored into a ref so a queued write can
  // check — at the moment it actually runs — whether the profile is
  // still the one the user toggled, and skip persisting otherwise
  // (`set_profile_setting` is scoped to whatever profile is active when
  // it runs, so a mid-flight switch would write to the wrong profile).
  const activeProfileId = activeProfile?.id;
  const activeProfileIdRef = useRef(activeProfileId);
  useEffect(() => {
    activeProfileIdRef.current = activeProfileId;
  }, [activeProfileId]);

  useEffect(() => {
    let cancelled = false;
    // Start each profile from a clean slate: clear the previous
    // profile's hidden set and readiness immediately, so a stale set
    // can never leak into the Settings checkboxes (which read
    // `isHidden` ungated) before — or if — the new read lands.
    /* eslint-disable react-hooks/set-state-in-effect */
    setReady(false);
    setHidden(new Set<StatsKpiId>());
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
  }, [activeProfile?.id, setHidden]);

  const toggle = useCallback(
    (id: StatsKpiId) => {
      // Read the authoritative ref (not React state) so back-to-back
      // toggles each build on the previous one's result.
      const previous = hiddenRef.current;
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      setHidden(next); // optimistic; advances `hiddenRef` too
      // Persist in declaration order for a stable, diff-friendly blob.
      const nextArray = STATS_KPI_IDS.filter((k) => next.has(k));
      const profileAtToggle = activeProfileIdRef.current;
      const seq = ++seqRef.current;
      // Queue behind any in-flight write. A leading no-op catch keeps a
      // prior failure from breaking the chain for later toggles.
      writeChainRef.current = writeChainRef.current
        .catch(() => {})
        .then(async () => {
          // Profile switched out from under this queued write — skip so
          // a toggle never lands in another profile's settings.
          if (activeProfileIdRef.current !== profileAtToggle) return;
          await setProfileSetting(KEY, JSON.stringify(nextArray), "json");
          // Only the latest enqueued toggle broadcasts, so an older
          // write's completion can't refresh over a newer state.
          if (seq === seqRef.current) {
            window.dispatchEvent(new CustomEvent(HIDDEN_KPIS_EVENT));
          }
        })
        .catch((err: unknown) => {
          console.error("[useHiddenKpis] write failed", err);
          // Only roll back if no later toggle superseded this one —
          // otherwise we'd clobber a newer, still-unpersisted state.
          // `next` is the exact Set we installed; a later toggle would
          // have replaced `hiddenRef.current` with a different object.
          if (hiddenRef.current === next) setHidden(previous);
        });
    },
    [setHidden],
  );

  const isHidden = useCallback((id: StatsKpiId) => hidden.has(id), [hidden]);

  return { hidden, isHidden, toggle, ready };
}
