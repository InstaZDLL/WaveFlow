/**
 * The genres a `;`-separated genre field names, as the backend reads it
 * (`split_genres` in the scanner, #729): split on `;` and `\` (a tag
 * editor's "Alternative\\Rock", pasted as text), trimmed, a name
 * the library would file under one already listed dropped, the order
 * kept. `/` and `,` stay inside a name ("Hip-Hop/Rap").
 */
export function splitGenres(value: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const part of value.split(/[;\\]/)) {
    const name = part.trim();
    // The scanner's canonical key: lowercased, anything that is not a
    // letter or a digit made a single space.
    const key = name
      .toLowerCase()
      // `Alphabetic` and `N`, as Rust's `is_alphanumeric`: some marks
      // (Indic vowel signs) are letters there, and `\p{L}` would split
      // a name on them that the backend keeps whole.
      .replace(/[^\p{Alphabetic}\p{N}]+/gu, " ")
      .trim();
    if (key !== "" && !seen.has(key)) {
      seen.add(key);
      out.push(name);
    }
  }
  return out;
}
