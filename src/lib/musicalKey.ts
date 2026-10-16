/** The slice of `t` this needs, so a component handed a narrower
 *  translator than i18next's own can still call it. */
type Translate = (key: string, options?: Record<string, unknown>) => string;

/**
 * A musical key, as a tonic pitch class (0 = C) and a mode.
 */
export interface ParsedKey {
  tonic: number;
  minor: boolean;
}

const LETTERS: Record<string, number> = {
  c: 0,
  d: 2,
  e: 4,
  f: 5,
  g: 7,
  a: 9,
  b: 11,
};

/**
 * Read a key the way taggers and DJ tools write one, or the analysis
 * stores one: `Am`, `A minor`, `Bbm`, `F#`, `E♭ major`, Camelot `8A`,
 * Open Key `1m`. `null` for anything else, which the caller then shows
 * as it was written.
 */
export function parseMusicalKey(
  raw: string | null | undefined,
): ParsedKey | null {
  const text = raw?.trim();
  if (!text) return null;

  // Camelot: 1-12 then A (minor) or B (major). 8B is C major, each step
  // a fifth up; the A ring is each B key's relative minor.
  const camelot = /^0?(1[0-2]|[1-9])\s*([ab])$/i.exec(text);
  if (camelot) {
    const n = Number(camelot[1]);
    const majorTonic = (n * 7 + 4) % 12;
    const minor = camelot[2].toLowerCase() === "a";
    return { tonic: minor ? (majorTonic + 9) % 12 : majorTonic, minor };
  }
  // Open Key: 1d is C major (Camelot 8B), d major and m minor.
  const openKey = /^(1[0-2]|[1-9])\s*([dm])$/i.exec(text);
  if (openKey) {
    const n = ((Number(openKey[1]) + 6) % 12) + 1;
    return parseMusicalKey(
      `${n}${openKey[2].toLowerCase() === "m" ? "A" : "B"}`,
    );
  }

  const letter = /^([a-g])\s*([#♯b♭]?)\s*(.*)$/i.exec(text);
  if (!letter) return null;
  let tonic = LETTERS[letter[1].toLowerCase()];
  if (letter[2] === "#" || letter[2] === "♯") tonic += 1;
  if (letter[2] === "b" || letter[2] === "♭") tonic -= 1;
  const mode = letter[3].trim().toLowerCase().replace(/[-_.]/g, "");
  let minor: boolean;
  if (mode === "" || mode === "maj" || mode === "major" || mode === "dur") {
    minor = false;
  } else if (
    mode === "m" ||
    mode === "min" ||
    mode === "minor" ||
    mode === "moll"
  ) {
    minor = true;
  } else {
    return null;
  }
  return { tonic: (tonic + 12) % 12, minor };
}

/**
 * A key in the reader's language — "La mineur", "A minor", "イ短調" —
 * or the text as written when it is not one this can read.
 */
export function formatMusicalKey(
  raw: string | null | undefined,
  t: Translate,
): string {
  const key = parseMusicalKey(raw);
  if (!key) return raw?.trim() ?? "";
  const notes = t(key.minor ? "keyNames.minorNotes" : "keyNames.majorNotes")
    .split(",")
    .map((n) => n.trim());
  const note = notes[key.tonic];
  if (!note) return raw?.trim() ?? "";
  return t(key.minor ? "keyNames.minor" : "keyNames.major", { note });
}
