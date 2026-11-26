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
 * The one exception, behind `lyrics.expressive_motion` (off by default):
 * a word rises as it is sung and stays up until its line ends, its
 * transform carried by a transition. On WebKit that is the shake above,
 * which the setting's description warns of; Chromium shows none.
 */
/**
 * On a word while it is up, put on as it rises. A line's words are created
 * as it becomes the sung one, its first word already up, so no transition
 * could carry that first rise: the class plays it from the baseline
 * (`app.css`), and does the same for each later word at the moment it is
 * reached, never ahead of it. Taken off as the line ends, so the words
 * come down through the transition.
 */
export const LYRICS_WORD_LIFT_CLASS = "wf-word-lift";

export const LYRICS_WORD_LIFT_TRANSITION =
  "transform 420ms cubic-bezier(0.22, 1, 0.36, 1), color 150ms ease";

/**
 * A word's transform: its zoom while it is the sung one, and, when the
 * words lift, how far it has risen. Without the lift it is the zoom
 * alone, as it always was.
 */
export function lyricsWordTransform(
  active: boolean,
  lift: "off" | "down" | "up",
): string {
  const zoom = active ? "scale(1.04)" : "scale(1)";
  if (lift === "off") return zoom;
  return `translateY(${lift === "up" ? "-0.08em" : "0"}) ${zoom}`;
}

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
