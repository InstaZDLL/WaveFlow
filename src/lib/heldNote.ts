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
  const letters = heldNoteLetters(word.text.trim()).length;
  if (letters === 0 || letters > HELD_MAX_LETTERS) return false;
  const heldMs = word.endMs - word.timeMs;
  return heldMs >= HELD_MIN_MS && heldMs >= letters * HELD_MS_PER_LETTER;
}

/**
 * Scripts whose letters change shape with their neighbours (Arabic's
 * joined forms, the Indic conjuncts, Thai's stacked vowels…). Cut into
 * separate boxes they fall apart, so a held word in one of them moves as
 * a whole.
 */
const SHAPED_SCRIPT =
  /[\p{Script=Arabic}\p{Script=Syriac}\p{Script=Nko}\p{Script=Mongolian}\p{Script=Hebrew}\p{Script=Devanagari}\p{Script=Bengali}\p{Script=Gurmukhi}\p{Script=Gujarati}\p{Script=Oriya}\p{Script=Tamil}\p{Script=Telugu}\p{Script=Kannada}\p{Script=Malayalam}\p{Script=Sinhala}\p{Script=Thai}\p{Script=Lao}\p{Script=Tibetan}\p{Script=Myanmar}\p{Script=Khmer}]/u;

const EMOJI_SEQUENCE = /\p{Extended_Pictographic}|\p{Regional_Indicator}|\u200D/u;

/**
 * The letters a held word ripples through: each character with the
 * combining marks that follow it (an accent typed as a separate mark
 * stays on its letter), or the whole word in a script that shapes its
 * letters together, or when it holds an emoji sequence.
 */
export function heldNoteLetters(text: string): string[] {
  // An emoji sequence (skin tone, joiner, flag) is one picture made of
  // several code points; cut apart it falls to pieces too.
  if (SHAPED_SCRIPT.test(text) || EMOJI_SEQUENCE.test(text)) {
    return text ? [text] : [];
  }
  return text.match(/\P{M}\p{M}*/gu) ?? [];
}
