/**
 * Case- and accent-insensitive form of a string, so "beyonce" finds
 * "Beyoncé" and "metadonnees" finds "Métadonnées".
 *
 * Only diacritics are stripped (`\p{Diacritic}`), not every combining
 * mark: a Devanagari vowel sign is a mark but part of the letter, and
 * dropping it would let a Hindi query match words it does not spell.
 */
export function foldForSearch(value: string): string {
  return value
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLocaleLowerCase();
}

/** A query split into folded words; every word must appear somewhere in
 *  the searched text, in any order. Empty for a blank query. */
export function searchTerms(query: string): string[] {
  return foldForSearch(query).trim().split(/\s+/).filter(Boolean);
}
