import { useEffect, useState } from "react";
import { getProfileSetting } from "../lib/tauri/profile";

/**
 * What clicking the small cover thumbnail on the left side of the
 * player bar does. Mirrors common patterns from Spotify (opens the
 * Now Playing right panel) and Apple Music (opens immersive full
 * screen). `none` opts out entirely for users who keep clicking it
 * by accident.
 */
export type CoverAction = "none" | "now_playing" | "immersive";

/**
 * Resolved per-button visibility for the player bar plus the
 * cover-click behaviour. Backed by `profile_setting` rows so the
 * choice is per-listener (a kid's profile may want a calmer bar).
 *
 * Defaults are picked to match the pre-customization behaviour so
 * existing users see zero change after the upgrade.
 */
export interface PlayerBarLayout {
  showLyrics: boolean;
  showQueue: boolean;
  showDevice: boolean;
  showMiniPlayer: boolean;
  showImmersive: boolean;
  showEqPreset: boolean;
  showSleepTimer: boolean;
  showAbLoop: boolean;
  showAudioQualityFooter: boolean;
  coverAction: CoverAction;
}

type BoolKey = Exclude<keyof PlayerBarLayout, "coverAction">;

const DEFAULTS: PlayerBarLayout = {
  showLyrics: true,
  showQueue: true,
  showDevice: true,
  showMiniPlayer: true,
  showImmersive: true,
  showEqPreset: false,
  showSleepTimer: false,
  showAbLoop: false,
  showAudioQualityFooter: false,
  coverAction: "immersive",
};

/**
 * Setting keys. Names re-use the existing `ui.show_*` pattern
 * established for sleep-timer / A-B loop pins so legacy code (and
 * any in-flight migrations) keeps working. The cover action lives
 * under its own non-`show_` key because it isn't a boolean.
 */
export const PLAYER_BAR_LAYOUT_KEYS: Record<BoolKey, string> = {
  showLyrics: "ui.show_lyrics",
  showQueue: "ui.show_queue",
  showDevice: "ui.show_device",
  showMiniPlayer: "ui.show_mini_player",
  showImmersive: "ui.show_immersive",
  showEqPreset: "ui.show_eq_preset",
  showSleepTimer: "ui.show_sleep_timer",
  showAbLoop: "ui.show_ab_loop",
  showAudioQualityFooter: "ui.show_audio_quality_footer",
};

export const COVER_ACTION_KEY = "ui.cover_action";

/**
 * Unified window event the Settings panel dispatches after any
 * write. Replaces the three feature-specific events used previously
 * (`waveflow:sleep-timer-visibility`, etc.) by re-fetching the full
 * payload. The legacy events are still observed below so any other
 * code that dispatches them keeps triggering a refresh.
 */
export const PLAYER_BAR_LAYOUT_EVENT = "waveflow:playerbar-layout-changed";

function parseBool(raw: string | null, fallback: boolean): boolean {
  if (raw == null) return fallback;
  return raw === "1" || raw === "true";
}

function parseCoverAction(raw: string | null): CoverAction {
  if (raw === "none" || raw === "now_playing" || raw === "immersive") {
    return raw;
  }
  return DEFAULTS.coverAction;
}

/**
 * React hook returning the resolved player-bar layout for the
 * active profile. Re-reads on the unified
 * `waveflow:playerbar-layout-changed` event and on the legacy
 * per-feature events so old Settings code that still dispatches
 * them isn't silently ignored.
 *
 * Failures are swallowed (with a console error) so a backend hiccup
 * never blanks the player bar — the previous value stays in state.
 */
export function usePlayerBarLayout(): PlayerBarLayout {
  const [layout, setLayout] = useState<PlayerBarLayout>(DEFAULTS);

  useEffect(() => {
    let cancelled = false;
    const refresh = async () => {
      try {
        const boolKeys = Object.entries(PLAYER_BAR_LAYOUT_KEYS) as Array<
          [BoolKey, string]
        >;
        const boolResults = await Promise.all(
          boolKeys.map(async ([prop, key]) => {
            const v = await getProfileSetting(key);
            return [prop, parseBool(v, DEFAULTS[prop])] as const;
          }),
        );
        const coverRaw = await getProfileSetting(COVER_ACTION_KEY);
        if (cancelled) return;
        const next: PlayerBarLayout = { ...DEFAULTS };
        for (const [prop, value] of boolResults) {
          next[prop] = value;
        }
        next.coverAction = parseCoverAction(coverRaw);
        setLayout(next);
      } catch (err) {
        console.error("[usePlayerBarLayout] read failed", err);
      }
    };
    void refresh();

    const legacyEvents = [
      "waveflow:sleep-timer-visibility",
      "waveflow:ab-loop-visibility",
      "waveflow:audio-quality-footer-visibility",
    ];
    window.addEventListener(PLAYER_BAR_LAYOUT_EVENT, refresh);
    for (const evt of legacyEvents) {
      window.addEventListener(evt, refresh);
    }
    return () => {
      cancelled = true;
      window.removeEventListener(PLAYER_BAR_LAYOUT_EVENT, refresh);
      for (const evt of legacyEvents) {
        window.removeEventListener(evt, refresh);
      }
    };
  }, []);

  return layout;
}
