/**
 * The name an artist or album is filed under when the user keeps "Ignore
 * articles when sorting" on: "The Beatles" under B, "L'Impératrice" under I.
 *
 * Mirrors `strip_article` in the backend's `NAME` collation
 * (`waveflow_core::repository::sqlite::collation`), which sorts the
 * listings; this copy only files a name under the same letter of the A–Z
 * rail. Keep the two lists in step.
 *
 * The backend sorts the scanner's canonical key, not the name shown, so
 * this works on the same key: lowercased, every run of punctuation and
 * spaces made one space (`canonical_name` in `scanner/canonical.rs`).
 * "L'Impératrice" becomes `l impératrice`, and "A-ha" `a ha` — which is
 * why it files under H, in the list and on the rail alike.
 */

/** Followed by a space. */
const ARTICLES = [
  "the",
  "a",
  "an",
  "le",
  "la",
  "les",
  "l",
  "der",
  "die",
  "das",
  "het",
  "el",
  "los",
  "las",
  "il",
  "lo",
  "gli",
];

/** The scanner's canonical key for a name, as the listings sort it. */
function canonicalKey(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .trim();
}

/** The canonical key of `name` without a leading article, or the whole
 *  key when it is only an article or the article runs into the next word
 *  ("Theory"). */
export function stripArticle(name: string): string {
  const key = canonicalKey(name);
  for (const article of ARTICLES) {
    if (key.startsWith(`${article} `)) {
      const rest = key.slice(article.length + 1);
      if (rest.length > 0) return rest;
    }
  }
  return key;
}
