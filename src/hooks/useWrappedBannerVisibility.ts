import { useCallback, useEffect, useMemo, useState } from "react";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";

/**
 * How the Wrapped banner on the Home view decides to show itself.
 *
 *  - `auto` (default): hidden the rest of the year, surfaces
 *    automatically inside the Wrapped season (Dec 1 → Jan 31).
 *    Matches the Spotify Wrapped / Apple Replay cadence — the recap
 *    is an event, not a permanent fixture.
 *  - `always`: render the banner whenever the profile has at least
 *    one play_event year, regardless of the date. Power-user opt-in
 *    from Settings → Appearance.
 *  - `never`: hide the banner unconditionally. The WrappedView is
 *    still reachable directly (via URL / sidebar entry), this just
 *    suppresses the Home promotion.
 */
export type WrappedBannerMode = "auto" | "always" | "never";

const MODE_KEY = "wrapped.banner_visibility";
const DISMISSED_YEAR_KEY = "wrapped.dismissed_year";

/**
 * Window event broadcast after any write so the Home banner re-reads
 * without remounting. Same pattern as `usePlayerBarLayout`.
 */
export const WRAPPED_BANNER_EVENT = "waveflow:wrapped-banner-changed";

const DEFAULT_MODE: WrappedBannerMode = "auto";

/**
 * Auto-show window in local time — December 1 through January 31
 * inclusive. Picked to mirror Spotify Wrapped (drops Dec 1) and give
 * January installs / late openers a fair window to catch their recap.
 *
 * `today` is injected for unit testing; production callers omit it.
 */
export function isInWrappedSeason(today: Date = new Date()): boolean {
  const month = today.getMonth(); // 0-indexed
  return month === 11 || month === 0; // December or January
}

function parseMode(raw: string | null): WrappedBannerMode {
  if (raw === "always" || raw === "never" || raw === "auto") return raw;
  return DEFAULT_MODE;
}

function parseDismissedYear(raw: string | null): number | null {
  if (raw == null) return null;
  const n = Number.parseInt(raw, 10);
  return Number.isFinite(n) ? n : null;
}

export interface WrappedBannerVisibility {
  mode: WrappedBannerMode;
  inSeason: boolean;
  dismissedYear: number | null;
  /** Resolves whether the banner should render for a given recap year. */
  shouldShow: (recapYear: number) => boolean;
  setMode: (next: WrappedBannerMode) => Promise<void>;
  dismissYear: (recapYear: number) => Promise<void>;
}

/**
 * React hook resolving Wrapped banner visibility from per-profile
 * settings + the current date. Re-reads on the broadcast event so a
 * Settings change flips the Home banner without a remount.
 */
export function useWrappedBannerVisibility(): WrappedBannerVisibility {
  const [mode, setModeState] = useState<WrappedBannerMode>(DEFAULT_MODE);
  const [dismissedYear, setDismissedYearState] = useState<number | null>(null);
  const inSeason = useMemo(() => isInWrappedSeason(), []);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const [rawMode, rawDismissed] = await Promise.all([
          getProfileSetting(MODE_KEY),
          getProfileSetting(DISMISSED_YEAR_KEY),
        ]);
        if (cancelled) return;
        setModeState(parseMode(rawMode));
        setDismissedYearState(parseDismissedYear(rawDismissed));
      } catch (err) {
        console.error("[useWrappedBannerVisibility] read failed", err);
      }
    };
    void refresh();
    window.addEventListener(WRAPPED_BANNER_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(WRAPPED_BANNER_EVENT, refresh);
    };
  }, []);

  const setMode = useCallback(async (next: WrappedBannerMode) => {
    setModeState(next);
    try {
      await setProfileSetting(MODE_KEY, next, "string");
      window.dispatchEvent(new CustomEvent(WRAPPED_BANNER_EVENT));
    } catch (err) {
      console.error("[useWrappedBannerVisibility] write mode failed", err);
    }
  }, []);

  const dismissYear = useCallback(async (recapYear: number) => {
    setDismissedYearState(recapYear);
    try {
      await setProfileSetting(DISMISSED_YEAR_KEY, String(recapYear), "int");
      window.dispatchEvent(new CustomEvent(WRAPPED_BANNER_EVENT));
    } catch (err) {
      console.error("[useWrappedBannerVisibility] dismiss failed", err);
    }
  }, []);

  const shouldShow = useCallback(
    (recapYear: number): boolean => {
      if (mode === "never") return false;
      if (dismissedYear === recapYear) return false;
      if (mode === "always") return true;
      return inSeason;
    },
    [mode, dismissedYear, inSeason],
  );

  return { mode, inSeason, dismissedYear, shouldShow, setMode, dismissYear };
}
