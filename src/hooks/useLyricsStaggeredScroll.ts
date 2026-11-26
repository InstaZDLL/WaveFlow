import { useProfileBooleanSetting } from "./useProfileSetting";
import { SETTINGS } from "../lib/settingsRegistry";

/** Broadcast after a write, so every mounted lyrics view re-reads. */
export const LYRICS_STAGGERED_SCROLL_EVENT =
  "waveflow:lyrics-staggered-scroll-changed";

/**
 * Whether the lyrics move on line by line, the lines below the sung one a
 * beat later (`useLyricsFollow`), rather than in one block, per profile in
 * `profile_setting['lyrics.staggered_scroll']`. Default off: it moves every
 * line in view on its own layer, which a webview that composites poorly may
 * not keep smooth.
 */
export function useLyricsStaggeredScrollSetting() {
  return useProfileBooleanSetting({
    key: "lyrics.staggered_scroll",
    defaultValue: SETTINGS["lyrics.staggered_scroll"].defaultValue,
    event: LYRICS_STAGGERED_SCROLL_EVENT,
    label: "useLyricsStaggeredScrollSetting",
  });
}

/** Just the value, for the lyrics views. */
export function useLyricsStaggeredScroll(): boolean {
  return useLyricsStaggeredScrollSetting().value;
}
