/**
 * The last of `count` items, sorted by when they start, that has started by `ms`:
 * the chapter under the position, or the line of a transcript. `-1` before the
 * first. In a search and not a walk, since a transcript may hold 50 000 lines and
 * this runs four times a second.
 */
export function indexAtPosition(
  count: number,
  startOf: (index: number) => number | null,
  ms: number,
): number {
  let low = 0;
  let high = count - 1;
  let found = -1;
  while (low <= high) {
    const middle = (low + high) >> 1;
    if ((startOf(middle) ?? Number.POSITIVE_INFINITY) <= ms) {
      found = middle;
      low = middle + 1;
    } else {
      high = middle - 1;
    }
  }
  return found;
}

/**
 * How far past a chapter's start "previous" still means the start of that one, as
 * a previous-track button does. Under it, it means the one before.
 */
export const CHAPTER_RESTART_MS = 3_000;

/** Where "previous chapter" leads from `ms`: its own start, or the one before. */
export function previousChapterStart(
  chapters: readonly { start_ms: number }[],
  ms: number,
): number | null {
  const current = indexAtPosition(
    chapters.length,
    (index) => chapters[index].start_ms,
    ms,
  );
  if (current < 0) return null;
  if (ms - chapters[current].start_ms > CHAPTER_RESTART_MS) {
    return chapters[current].start_ms;
  }
  return current > 0 ? chapters[current - 1].start_ms : null;
}

/** Where "next chapter" leads from `ms`, if anywhere. */
export function nextChapterStart(
  chapters: readonly { start_ms: number }[],
  ms: number,
): number | null {
  const current = indexAtPosition(
    chapters.length,
    (index) => chapters[index].start_ms,
    ms,
  );
  return chapters[current + 1]?.start_ms ?? null;
}
