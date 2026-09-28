import type { LyricsWord } from "./tauri/lyrics";

/** Shortest hold that reads as a held note, whatever the word. */
const HELD_MIN_MS = 1000;
/** A long word sung at an ordinary pace is not held: this much per letter. */
const HELD_MS_PER_LETTER = 220;
/** Longer words are sung, not held — and a wave through them reads as noise. */
const HELD_MAX_LETTERS = 8;

/**
 * Whether `word` is a held note: sung long enough for its length. A
 * two-letter word held for a second is one; a seven-letter word sung over
 * the same second is ordinary pace.
 *
 * Only real word timing counts. An estimated word (it carries `fillEndMs`)
 * has a length made up from the line's, and would light up at random.
 */
export function isHeldNote(word: LyricsWord | undefined): boolean {
  if (!word || word.fillEndMs !== undefined || word.endMs < 0) return false;
  const letters = Array.from(word.text.trim()).length;
  if (letters === 0 || letters > HELD_MAX_LETTERS) return false;
  const heldMs = word.endMs - word.timeMs;
  return heldMs >= HELD_MIN_MS && heldMs >= letters * HELD_MS_PER_LETTER;
}
