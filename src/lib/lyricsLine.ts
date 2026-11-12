/**
 * The only transition a lyrics word may carry as the sung word moves on:
 * its colour. Its opacity and its transform change in one step.
 *
 * WebKit (macOS, WebKitGTK) runs an opacity or transform transition on a
 * layer of its own, created when the transition starts and dropped when
 * it ends, and the text around it is redrawn snapped to the device
 * pixel each time: every word of the line shifts by a fraction of a
 * pixel at each word change. A colour transition is a plain repaint.
 * Chromium keeps the sub-pixel offset, so Windows never shows it.
 * See `docs/architecture/invariants.md`.
 */
export const LYRICS_WORD_TRANSITION = "color 150ms ease";

/**
 * How a timed line reads as the position passes it: the lyrics panel's, and the
 * transcript panel's, which is the same line with different words in it.
 */
export function lyricsLineClass(isActive: boolean, isPast: boolean): string {
  return `text-base leading-relaxed cursor-pointer transition-all select-none focus:outline-none focus-visible:ring-2 focus-visible:ring-emerald-500 rounded ${
    isActive
      ? "text-zinc-900 dark:text-white font-semibold scale-[1.02]"
      : isPast
        ? "text-zinc-300 dark:text-zinc-600"
        : "text-zinc-500 dark:text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
  }`;
}
