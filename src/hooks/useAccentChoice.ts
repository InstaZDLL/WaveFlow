import { useProfileSetting, type ProfileSetting } from "./useProfileSetting";
import {
  ACCENT_SETTING_KEY,
  DEFAULT_ACCENT_CHOICE,
  parseAccentChoice,
  serializeAccentChoice,
  type AccentChoice,
} from "../lib/accent";

/** Broadcast after a successful write, so the Settings card and every
 *  window's `AccentSync` re-read together. */
export const ACCENT_EVENT = "waveflow:accent";

/**
 * Per-profile accent choice: the theme's own, a picked colour, or the
 * playing track's cover. Persistence, profile isolation and cross-window
 * sync come from [`useProfileSetting`](./useProfileSetting.ts).
 */
export function useAccentChoice(): ProfileSetting<AccentChoice> {
  return useProfileSetting<AccentChoice>({
    key: ACCENT_SETTING_KEY,
    defaultValue: DEFAULT_ACCENT_CHOICE,
    parse: parseAccentChoice,
    serialize: serializeAccentChoice,
    valueType: "json",
    event: ACCENT_EVENT,
    label: "useAccentChoice",
  });
}
