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
  return (
    value
      .normalize("NFD")
      .replace(/[\p{Diacritic}\u0653-\u0655]/gu, "")
      // Not toLocaleLowerCase: under a Turkish locale it writes I as ı, so
      // "International" stopped matching "international". A dotted İ is
      // already I by now, its dot stripped with the diacritics.
      .toLowerCase()
      // Lowercasing a whole string writes a word-final Greek sigma as ς, so
      // "ΚΟΣ" typed in capitals became "κος" and missed "κοσμος".
      .replace(/ς/g, "σ")
  );
}

/** A query split into folded words; every word must appear somewhere in
 *  the searched text, in any order. Empty for a blank query. */
export function searchTerms(query: string): string[] {
  return foldForSearch(query).trim().split(/\s+/).filter(Boolean);
}
