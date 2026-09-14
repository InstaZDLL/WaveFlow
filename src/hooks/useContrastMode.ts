import { useCallback, useEffect } from "react";
import { useProfileSetting } from "./useProfileSetting";
import {
  applyContrast,
  CONTRAST_SETTING_KEY,
  DEFAULT_CONTRAST_MODE,
  parseContrastMode,
  writeCachedContrast,
  type ContrastMode,
} from "../lib/contrast";

/** Broadcast after a successful write so the Settings card and the
 *  provider that paints the attribute re-read together. */
export const CONTRAST_EVENT = "waveflow:contrast";

export interface ContrastPreference {
  mode: ContrastMode;
  /** `false` until the stored choice has been read for the active
   *  profile. Nothing gates on it today — the attribute is already
   *  correct from the bootstrap cache — but a consumer that renders
   *  different copy per mode would need it. */
  ready: boolean;
  setMode: (next: ContrastMode) => Promise<void>;
}

const parse = (raw: string | null) => parseContrastMode(raw);
const serialize = (value: ContrastMode) => value;

/**
 * Per-profile preference: lift contrast across the whole interface
 * (#596).
 *
 * Concurrency, profile isolation and rollback live in
 * [`useProfileSetting`](./useProfileSetting.ts), per the settings
 * invariant. What this adds is the two side effects the stored value
 * has outside React state: the `data-contrast` attribute on the
 * document root, and the localStorage cache the `index.html` bootstrap
 * reads on the next launch.
 */
export function useContrastMode(): ContrastPreference {
  const { value, ready, setValue } = useProfileSetting<ContrastMode>({
    key: CONTRAST_SETTING_KEY,
    defaultValue: DEFAULT_CONTRAST_MODE,
    parse,
    serialize,
    valueType: "string",
    event: CONTRAST_EVENT,
    label: "useContrastMode",
  });

  // Paint on every change, including the one that lands when the stored
  // value comes back from the database and the one a profile switch
  // brings. The bootstrap already stamped the previous profile's cached
  // choice, so this is what corrects it.
  useEffect(() => {
    applyContrast(value);
  }, [value]);

  // In `auto`, the OS is the input and it can change while the app is
  // running. Bound only in that mode, so an explicit choice carries no
  // listener at all.
  useEffect(() => {
    if (value !== "auto") return;
    if (typeof window === "undefined" || !window.matchMedia) return;
    let query: MediaQueryList;
    try {
      query = window.matchMedia("(prefers-contrast: more)");
    } catch {
      return;
    }
    const onChange = () => applyContrast("auto");
    query.addEventListener("change", onChange);
    return () => query.removeEventListener("change", onChange);
  }, [value]);

  const setMode = useCallback(
    async (next: ContrastMode) => {
      // Cache and paint before the write: the visual answer to a click
      // on an accessibility control should not wait on a database round
      // trip, and a failed write rolls the value back through
      // `useProfileSetting`, which re-runs the effect above.
      writeCachedContrast(next);
      applyContrast(next);
      await setValue(next);
    },
    [setValue],
  );

  return { mode: value, ready, setMode };
}
