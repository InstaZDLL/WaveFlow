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
