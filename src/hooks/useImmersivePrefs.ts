import { useEffect, useState } from "react";
import { getProfileSetting } from "../lib/tauri/profile";

/**
 * Per-profile preferences for the immersive view (issue #328).
 *
 * - `mergedLyrics` — show the lyrics as a side column next to the
 *   now-playing cover/transport (Spotify / Apple-Music-TV style) so the
 *   user can switch tracks while reading. OFF falls back to the legacy
 *   single-view-with-toggle behaviour. (The merged layout also collapses
 *   to single-column automatically on narrow windows regardless of this
 *   flag — see `ImmersiveView`.)
 * - `useNativeFullscreen` — drive the OS window into real fullscreen
 *   (`setFullscreen(true)`) on open, restoring the prior window state on
 *   close. OFF keeps the in-window overlay so multi-monitor users can
 *   still see the rest of their desktop.
 *
 * Lives in `profile_setting` (not `app_setting`) to sit next to the
 * sibling appearance prefs — player-bar layout, cover action,
 * fullscreen-lyrics centering, Wrapped banner — which are all
 * per-profile and read through the same generic wrapper.
 */
export interface ImmersivePrefs {
  mergedLyrics: boolean;
  useNativeFullscreen: boolean;
  /** False until the first read resolves. Consumers that take an
   *  OS-level action on a pref (native fullscreen) MUST wait for this —
   *  acting on the optimistic defaults would briefly flash a user who
   *  disabled the pref into fullscreen before the real value lands. */
  loaded: boolean;
}

const DEFAULTS: ImmersivePrefs = {
  // Both OFF by default → the immersive view stays exactly the
  // pre-#328 experience out of the box (classic single-view with a Mic2
  // toggle to lyrics, in-window overlay). Both are opt-in via Settings →
  // Appearance: `mergedLyrics` = the two-column control panel,
  // `useNativeFullscreen` = real OS fullscreen.
  mergedLyrics: false,
  useNativeFullscreen: false,
  loaded: false,
};

type StoredPrefs = Omit<ImmersivePrefs, "loaded">;

export const IMMERSIVE_PREF_KEYS: Record<keyof StoredPrefs, string> = {
  mergedLyrics: "immersive.merged_lyrics",
  useNativeFullscreen: "immersive.use_native_fullscreen",
};

const STORED_DEFAULTS: StoredPrefs = {
  mergedLyrics: DEFAULTS.mergedLyrics,
  useNativeFullscreen: DEFAULTS.useNativeFullscreen,
};

/** Window event the Settings card dispatches after a write so every
 *  mounted consumer re-reads in one go. */
export const IMMERSIVE_PREFS_EVENT = "waveflow:immersive-prefs-changed";

function parseBool(raw: string | null, fallback: boolean): boolean {
  if (raw == null) return fallback;
  return raw === "1" || raw === "true";
}

/**
 * React hook returning the resolved immersive prefs for the active
 * profile. Re-reads on `waveflow:immersive-prefs-changed`. Read
 * failures are swallowed (console error) so a backend hiccup keeps the
 * last good value instead of forcing defaults mid-session.
 */
export function useImmersivePrefs(): ImmersivePrefs {
  const [prefs, setPrefs] = useState<ImmersivePrefs>(DEFAULTS);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const entries = Object.entries(IMMERSIVE_PREF_KEYS) as Array<
          [keyof StoredPrefs, string]
        >;
        const results = await Promise.all(
          entries.map(async ([prop, key]) => {
            const v = await getProfileSetting(key);
            return [prop, parseBool(v, STORED_DEFAULTS[prop])] as const;
          }),
        );
        if (cancelled) return;
        const next: ImmersivePrefs = { ...DEFAULTS, loaded: true };
        for (const [prop, value] of results) {
          next[prop] = value;
        }
        setPrefs(next);
      } catch (err) {
        console.error("[useImmersivePrefs] read failed", err);
      }
    };
    void refresh();

    window.addEventListener(IMMERSIVE_PREFS_EVENT, refresh);
    return () => {
      cancelled = true;
      window.removeEventListener(IMMERSIVE_PREFS_EVENT, refresh);
    };
  }, []);

  return prefs;
}
