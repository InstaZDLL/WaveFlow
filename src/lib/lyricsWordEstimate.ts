import type { LyricsLine, LyricsWord } from "./tauri/lyrics";

/**
 * Estimated word timing for a line-synced lyric (#716).
 *
 * Line-synced lyrics say when a line starts and nothing about the words
 * inside it, so the karaoke fill never ran on them. This spreads the
 * line's time over its words — **purely for display**: nothing here is
 * stored, the estimate is re-derived every time a line becomes active,
 * and only when the user opted in.
 *
 * Better than an equal split, which lets a one-syllable word hold the
 * highlight as long as a four-syllable one:
 *
 * - **Weighted by syllables**, counted as vowel groups for alphabetic
 *   scripts. Rough, and wrong for silent vowels, but it tracks how long a
 *   word is sung far better than its letter count does.
 * - **A pause after punctuation.** A comma or a full stop is where a
 *   singer breathes; the pause is dead time between two words, not time
 *   one of them holds.
 * - **Whole phrases for scripts without spaces.** Every lyric view puts a
 *   space between two words, so splitting a Chinese or Japanese line per
 *   character would write spaces into it. Those lines step through their
 *   space-separated phrases instead, weighted one syllable per character;
 *   Korean keeps its spaces, one syllable per block.
 * - **The sung span is capped.** A line followed by a long instrumental
 *   is not sung across the whole gap until the next line; the estimate
 *   ends where a plausible delivery would.
 * - **The words finish early, and the last one is held.** Even within
 *   the span, a singer is through the words before the next line starts;
 *   see `DELIVERY_SHARE`.
 */

/** Seconds a syllable takes at most, before the span is capped. */
const MAX_MS_PER_SYLLABLE = 550;
/** Floor for a guessed span, so a one-word last line still animates. */
const MIN_SPAN_MS = 400;
/** Assumed span when the line's end is unknown (the last line). */
const FALLBACK_MS_PER_SYLLABLE = 300;
/**
 * Share of a known span the words are delivered in. A singer gets through
 * a line faster than the gap to the next one, then holds the last vowel or
 * breathes: spreading the words over the whole gap put every word after
 * the first a little further behind the voice — 0.2 to 0.4 s by mid-line
 * on the tracks reported (#750). The last word stays active to the end of
 * the span, so the line still hands over on time, but its fill finishes
 * with the delivery (`fillEndMs`): a sweep crawling through the rest would
 * trail a singer who stopped to breathe. Not applied to a guessed
 * span, which is already paced per syllable rather than stretched to fit.
 */
const DELIVERY_SHARE = 0.8;

const PAUSE_SHORT = 0.5; // , ; : and dashes, in syllables
const PAUSE_LONG = 0.9; // . ! ? …

const HAN_OR_KANA =
  /[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff66-\uff9f]/u;
const HAN_OR_KANA_ALL = new RegExp(HAN_OR_KANA.source, "gu");
const HANGUL = /[\uac00-\ud7af]/gu;
const VOWEL_GROUPS =
  /[aeiouyàáâãäåæèéêëìíîïòóôõöøœùúûüýÿаеёиоуыэюяіїєαεηιουωάέήίόύώ]+/giu;

/** Split a line into the units the highlight steps through. */
function tokenize(text: string): string[] {
  return text.split(/\s+/u).filter(Boolean);
}

/** How many syllables a unit is sung over — at least one. */
export function syllables(unit: string): number {
  const han = unit.match(HAN_OR_KANA_ALL)?.length ?? 0;
  if (han > 0) return han;
  const hangul = unit.match(HANGUL)?.length ?? 0;
  if (hangul > 0) return hangul;
  const groups = unit.match(VOWEL_GROUPS)?.length ?? 0;
  if (groups > 0) return groups;
  // A script with no vowel table here, or a bare number: go by length.
  const letters = Array.from(unit).filter((c) => /[\p{L}\p{N}]/u.test(c));
  return Math.max(1, Math.round(letters.length / 3));
}

/** Silence after a unit, in syllables, from its trailing punctuation. */
function pauseAfter(unit: string): number {
  if (/[.!?…。！？]["'’”)\]]*$/u.test(unit)) return PAUSE_LONG;
  if (/[,;:—–、，；：]["'’”)\]]*$/u.test(unit)) return PAUSE_SHORT;
  return 0;
}

/**
 * Words for `line`, spread over the time until `nextStartMs` (the next
 * line's start, when there is one). Empty when the line has no text.
 */
export function estimateLineWords(
  line: LyricsLine,
  nextStartMs: number | undefined,
): LyricsWord[] {
  const units = tokenize(line.text);
  if (units.length === 0) return [];

  const weights = units.map(syllables);
  const pauses = units.map((u, i) =>
    i < units.length - 1 ? pauseAfter(u) : 0,
  );
  const total =
    weights.reduce((a, b) => a + b, 0) + pauses.reduce((a, b) => a + b, 0);

  // The earlier of the two ends the line can have: its own, and the next
  // line's start. A line whose stamped end overlaps the next must still
  // hand over on time.
  const ends = [line.endMs, nextStartMs].filter(
    (end): end is number => end != null && end > line.timeMs,
  );
  const knownEnd = ends.length > 0 ? Math.min(...ends) : null;
  const span =
    knownEnd != null
      ? Math.min(knownEnd - line.timeMs, total * MAX_MS_PER_SYLLABLE)
      : total * FALLBACK_MS_PER_SYLLABLE;
  // The floor only where the end is a guess: with a known end, a short
  // line is short, and stretching it would run into the next one.
  const fullSpan = knownEnd != null ? span : Math.max(span, MIN_SPAN_MS);
  const delivered = knownEnd != null ? fullSpan * DELIVERY_SHARE : fullSpan;
  const perUnit = delivered / total;

  const words: LyricsWord[] = [];
  let cursor = line.timeMs;
  units.forEach((text, i) => {
    const last = i === units.length - 1;
    const sungEnd = cursor + weights[i] * perUnit;
    // The last word stays active through the rest of the span, but its
    // fill completes where its delivery does.
    const end = last ? line.timeMs + fullSpan : sungEnd;
    const word: LyricsWord = {
      timeMs: Math.round(cursor),
      endMs: Math.round(end),
      text,
    };
    if (last && sungEnd < end) word.fillEndMs = Math.round(sungEnd);
    words.push(word);
    cursor = end + pauses[i] * perUnit;
  });
  return words;
}
