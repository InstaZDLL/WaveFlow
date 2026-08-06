import { useCallback, useMemo } from "react";
import { useProfileSetting } from "./useProfileSetting";

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
 * Window event broadcast after a **mode** write so the Home banner
 * re-reads without remounting. Same pattern as `usePlayerBarLayout`.
 */
export const WRAPPED_BANNER_EVENT = "waveflow:wrapped-banner-changed";

/**
 * Separate channel for the dismissed-year key. The two keys deliberately
 * do NOT share one event: a broadcast makes every listening instance
 * re-read, so a `mode` write would kick off a read of the dismissed year
 * that can land before an in-flight write of that key commits — and
 * overwrite its optimistic value with the pre-write one. Transient (the
 * write's own broadcast re-reads afterwards) but visible, and there's no
 * reason for one key's write to make the other re-read at all.
 */
export const WRAPPED_DISMISSED_YEAR_EVENT =
  "waveflow:wrapped-dismissed-year-changed";

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
  /** Resolves whether the banner should render for a given recap year.
   *  Returns `false` until both per-profile keys have been read — their
   *  defaults would otherwise show the banner to users who opted out. */
  shouldShow: (recapYear: number) => boolean;
  setMode: (next: WrappedBannerMode) => Promise<void>;
  dismissYear: (recapYear: number) => Promise<void>;
}

/**
 * React hook resolving Wrapped banner visibility from per-profile
 * settings + the current date. Re-reads on the broadcast event so a
 * Settings change flips the Home banner without a remount.
 *
 * Two keys, so two [`useProfileSetting`](./useProfileSetting.ts)
 * instances — each on its **own** broadcast channel, so a write to one
 * key never makes the other re-read (see
 * {@link WRAPPED_DISMISSED_YEAR_EVENT}). Concurrency, profile isolation
 * and rollback come from there; this hook used to carry none of it (no
 * serialized writes, no profile guard on the write at all).
 */
export function useWrappedBannerVisibility(): WrappedBannerVisibility {
  const inSeason = useMemo(() => isInWrappedSeason(), []);

  const {
    value: mode,
    ready: modeReady,
    setValue: setModeValue,
  } = useProfileSetting<WrappedBannerMode>({
    key: MODE_KEY,
    defaultValue: DEFAULT_MODE,
    parse: parseMode,
    serialize: (value) => value,
    valueType: "string",
    event: WRAPPED_BANNER_EVENT,
    label: "useWrappedBannerVisibility.mode",
  });

  const {
    value: dismissedYear,
    ready: dismissedYearReady,
    setValue: setDismissedYearValue,
  } = useProfileSetting<number | null>({
    key: DISMISSED_YEAR_KEY,
    defaultValue: null,
    parse: parseDismissedYear,
    // Never serialized as null: `dismissYear` only ever stores a year.
    serialize: (value) => String(value ?? ""),
    valueType: "int",
    event: WRAPPED_DISMISSED_YEAR_EVENT,
    label: "useWrappedBannerVisibility.dismissedYear",
  });

  const setMode = useCallback(
    async (next: WrappedBannerMode) => {
      await setModeValue(next);
    },
    [setModeValue],
  );

  const dismissYear = useCallback(
    async (recapYear: number) => {
      await setDismissedYearValue(recapYear);
    },
    [setDismissedYearValue],
  );

  const shouldShow = useCallback(
    (recapYear: number): boolean => {
      // Both defaults ("auto", nothing dismissed) are *permissive*, so
      // answering before the reads land shows the banner for one frame
      // to the very users who turned it off or dismissed this year —
      // during the season, when it's on screen. Withhold until both keys
      // have been read for the active profile.
      if (!modeReady || !dismissedYearReady) return false;
      if (mode === "never") return false;
      if (dismissedYear === recapYear) return false;
      if (mode === "always") return true;
      return inSeason;
    },
    [mode, dismissedYear, inSeason, modeReady, dismissedYearReady],
  );

  return { mode, inSeason, dismissedYear, shouldShow, setMode, dismissYear };
}
