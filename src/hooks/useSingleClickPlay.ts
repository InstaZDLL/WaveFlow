import type { MouseEvent } from "react";
import { useProfileBooleanSetting } from "./useProfileSetting";

/** Broadcast after the preference is written, so every list re-reads it. */
export const SINGLE_CLICK_PLAY_EVENT = "waveflow:single-click-play-changed";

/**
 * Settings → "Single-click play": one click on a track starts it, instead
 * of a double-click. Every list of tracks reads it through this hook, so a
 * change made in Settings reaches the lists already on screen (#932: it was
 * read by the library's song list alone, and every other list still asked
 * for a double-click).
 */
export function useSingleClickPlay() {
  return useProfileBooleanSetting({
    key: "ui.single_click_play",
    defaultValue: false,
    event: SINGLE_CLICK_PLAY_EVENT,
    label: "useSingleClickPlay",
  });
}

/** What a click on a row may land on without being a click on the row. */
const NESTED_CONTROLS =
  "button, a, input, select, textarea, label, [role='checkbox'], [role='menuitem']";

/**
 * Whether a click on a track row should play it: single-click play is on,
 * and this is a plain click on the row itself. Not the second click of a
 * double-click (the first already started the track), not a click with a
 * modifier (those select), and not a click on a control inside the row (a
 * like button, an artist link, a drag handle), which does its own thing.
 */
export function isPlayClick(
  event: MouseEvent<HTMLElement>,
  singleClickPlay: boolean,
): boolean {
  if (!singleClickPlay) return false;
  if (event.detail > 1) return false;
  if (event.shiftKey || event.ctrlKey || event.metaKey || event.altKey) {
    return false;
  }
  const target = event.target;
  if (!(target instanceof Element)) return true;
  const control = target.closest(NESTED_CONTROLS);
  return (
    control == null ||
    control === event.currentTarget ||
    !event.currentTarget.contains(control)
  );
}
