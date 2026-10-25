/**
 * Case- and accent-insensitive form of a string, so "beyonce" finds
 * "Beyoncé" and "metadonnees" finds "Métadonnées".
 *
 * What goes is the diacritics (`\p{Diacritic}`), not every combining
 * mark: a Devanagari vowel sign is a mark but a letter of the word, and
 * dropping it made a Hindi query match words it does not spell. The
 * virama and the nukta are diacritics and do go, so a conjunct also
 * matches its letters typed apart — looser, never a word with other
 * letters. The Arabic madda and hamza above / below (U+0653–U+0655) are
 * marks but not diacritics; they go too, since "اداء" is how most type
 * "أداء".
 */
export function foldForSearch(value: string): string {
  return value
    .normalize("NFD")
    .replace(/[\p{Diacritic}ٓ-ٕ]/gu, "")
    .toLocaleLowerCase();
}

/** A query split into folded words; every word must appear somewhere in
 *  the searched text, in any order. Empty for a blank query. */
export function searchTerms(query: string): string[] {
  return foldForSearch(query).trim().split(/\s+/).filter(Boolean);
}
