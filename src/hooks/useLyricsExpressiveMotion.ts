import { useProfileBooleanSetting } from "./useProfileSetting";
import { SETTINGS } from "../lib/settingsRegistry";

/** Broadcast after a write, so every mounted lyrics view re-reads. */
export const LYRICS_EXPRESSIVE_MOTION_EVENT =
  "waveflow:lyrics-expressive-motion-changed";

/**
 * Whether the lyrics move expressively, per profile in
 * `profile_setting['lyrics.expressive_motion']`: the list moves on line by
 * line, the lines below the sung one a beat later (`useLyricsFollow`), and
 * each word rises as it is sung (`lyricsWordTransform`). Default off: every
 * line in view glides on a layer of its own, which a webview that
 * composites poorly may not keep smooth, and the rising words shake their
 * line on WebKit (`LYRICS_WORD_LIFT_TRANSITION`).
 */
export function useLyricsExpressiveMotionSetting() {
  return useProfileBooleanSetting({
    key: "lyrics.expressive_motion",
    defaultValue: SETTINGS["lyrics.expressive_motion"].defaultValue,
    event: LYRICS_EXPRESSIVE_MOTION_EVENT,
    label: "useLyricsExpressiveMotionSetting",
  });
}

/** Just the value, for the lyrics views. */
export function useLyricsExpressiveMotion(): boolean {
  return useLyricsExpressiveMotionSetting().value;
}
