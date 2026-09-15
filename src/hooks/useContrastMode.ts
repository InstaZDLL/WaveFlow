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
   *  profile. Until then the attribute belongs to the bootstrap, which
   *  already stamped the cached choice — see the effect below. */
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
  const { value, ready, revision, setValue } = useProfileSetting<ContrastMode>({
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
  //
  // Gated on `ready`, and that gate is the whole point: before the read
  // lands, `value` is the *default*, `auto`. Painting it would resolve
  // against the OS and write `normal` over the `high` the bootstrap
  // just stamped — a flash of ordinary contrast on every launch, for
  // the one user who asked for the opposite, which is precisely the
  // flicker the bootstrap exists to prevent.
  //
  // The cache is written from here rather than from `setMode`, so it
  // only ever holds a value React is actually showing: a write that
  // fails is rolled back by `useProfileSetting`, this effect re-runs
  // with the restored value, and the cache follows it back.
  //
  // `revision` is in the deps for the case `value` cannot cover: the
  // Settings card paints optimistically and is unmounted before a
  // failed write rolls back, so the rollback broadcast arrives here at
  // an instance that never held the optimistic value. Its `value` is
  // already right, the document is not, and without a dep that moves
  // anyway this effect would not run.
  useEffect(() => {
    if (!ready) return;
    applyContrast(value);
    writeCachedContrast(value);
  }, [ready, value, revision]);

  // In `auto`, the OS is the input and it can change while the app is
  // running. Bound only in that mode, so an explicit choice carries no
  // listener at all.
  useEffect(() => {
    if (!ready || value !== "auto") return;
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
  }, [ready, value]);

  const setMode = useCallback(
    async (next: ContrastMode) => {
      // Painted before the write: the visual answer to a click on an
      // accessibility control should not wait on a database round trip.
      // The cache is left to the effect above, which runs on the same
      // optimistic state change and, on a failed write, runs again on
      // the rollback — so what is persisted for the next launch can
      // never outlive what the user is looking at.
      applyContrast(next);
      await setValue(next);
    },
    [setValue],
  );

  return { mode: value, ready, setMode };
}
