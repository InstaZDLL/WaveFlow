// Pure edits of a lyrics document, for the lyrics editor. Every function
// returns a new document and leaves the one it was given untouched, so the
// editor's undo history can keep both. Times are whole milliseconds: the
// backend reads them as unsigned integers.

import type {
  LyricsAgent,
  LyricsDocument,
  LyricsDocumentLine,
  LyricsUnit,
} from "./tauri/lyricsDocument";

/** Free text split into untimed units, one per word. */
export function splitUnits(text: string): LyricsUnit[] {
  return text
    .split(/\s+/)
    .filter(Boolean)
    .map((t) => ({ text: t }));
}

/** Units joined back into display text, honouring `joined`. */
export function unitsText(units: readonly LyricsUnit[]): string {
  let out = "";
  units.forEach((u, i) => {
    out += u.text;
    if (!u.joined && i + 1 < units.length) out += " ";
  });
  return out;
}

export const lineText = (line: LyricsDocumentLine) => unitsText(line.words);

/** When the line starts: its own time, else its first unit's. */
export const lineBegin = (line: LyricsDocumentLine): number | undefined =>
  line.beginMs ?? line.words[0]?.beginMs;

const ms = (value: number) => Math.max(0, Math.round(value));

function withLines(
  doc: LyricsDocument,
  lines: LyricsDocumentLine[],
): LyricsDocument {
  return { ...doc, lines };
}

function mapLine(
  doc: LyricsDocument,
  i: number,
  f: (line: LyricsDocumentLine) => LyricsDocumentLine,
): LyricsDocument {
  if (!doc.lines[i]) return doc;
  return withLines(
    doc,
    doc.lines.map((line, k) => (k === i ? f(line) : line)),
  );
}

function mapUnit(
  line: LyricsDocumentLine,
  j: number,
  f: (unit: LyricsUnit) => LyricsUnit,
): LyricsDocumentLine {
  return { ...line, words: line.words.map((u, k) => (k === j ? f(u) : u)) };
}

// ----- text ------------------------------------------------------------

/** Units grouped into the words they spell: syllables joined to the
 *  next unit belong to the same word. */
function wordsOf(units: readonly LyricsUnit[]): LyricsUnit[][] {
  const words: LyricsUnit[][] = [];
  let current: LyricsUnit[] = [];
  for (const unit of units) {
    current.push(unit);
    if (!unit.joined) {
      words.push(current);
      current = [];
    }
  }
  if (current.length) words.push(current);
  return words;
}

/**
 * Replace a line's text. A fix that keeps the number of words (a typo)
 * keeps their times: a word whose text did not change keeps its units,
 * syllables included, and a word that did becomes one unit spanning
 * what its syllables did. Otherwise the units start over untimed and
 * only the line's own start survives.
 */
export function setLineText(
  doc: LyricsDocument,
  i: number,
  text: string,
): LyricsDocument {
  return mapLine(doc, i, (line) => {
    const fresh = splitUnits(text);
    const words = wordsOf(line.words);
    if (fresh.length === words.length) {
      return {
        ...line,
        words: words.flatMap((units, k) => {
          const last = units[units.length - 1];
          const unchanged = unitsText(units) === fresh[k].text;
          const kept = unchanged
            ? units
            : [
                {
                  text: fresh[k].text,
                  beginMs: units[0].beginMs,
                  endMs: last.endMs,
                },
              ];
          // The line's last word is followed by nothing.
          return kept.map((u, j) =>
            j === kept.length - 1 ? { ...u, joined: false } : u,
          );
        }),
      };
    }
    return { ...line, beginMs: lineBegin(line), words: fresh };
  });
}

/**
 * Rebuild the lines from a block of text, one per non-blank row; a run of
 * blank rows between two is a stanza break, kept as one line without
 * words. A line whose text is still there keeps everything (times,
 * singer, translation): rows are matched in order, so a line inserted or
 * removed above does not cost the ones after it their sync.
 */
export function setLinesFromText(
  doc: LyricsDocument,
  text: string,
): LyricsDocument {
  const rows: string[] = [];
  for (const raw of text.split(/\r?\n/)) {
    const row = raw.trim();
    if (row || (rows.length > 0 && rows[rows.length - 1] !== "")) {
      rows.push(row);
    }
  }
  while (rows.length > 0 && rows[rows.length - 1] === "") rows.pop();
  let from = 0;
  return withLines(
    doc,
    rows.map((row) => {
      if (!row) return { words: [] };
      for (let k = from; k < doc.lines.length; k += 1) {
        if (doc.lines[k].words.length > 0 && lineText(doc.lines[k]) === row) {
          from = k + 1;
          return doc.lines[k];
        }
      }
      return { words: splitUnits(row) };
    }),
  );
}

export function patchLine(
  doc: LyricsDocument,
  i: number,
  patch: Partial<LyricsDocumentLine>,
): LyricsDocument {
  return mapLine(doc, i, (line) => ({ ...line, ...patch }));
}

export function insertLine(
  doc: LyricsDocument,
  at: number,
  line: LyricsDocumentLine = { words: [] },
): LyricsDocument {
  const lines = doc.lines.slice();
  lines.splice(at, 0, line);
  return withLines(doc, lines);
}

export function deleteLine(doc: LyricsDocument, i: number): LyricsDocument {
  return withLines(
    doc,
    doc.lines.filter((_, k) => k !== i),
  );
}

/** Line `i` and the next one become one line, the second one's end kept. */
export function mergeWithNext(doc: LyricsDocument, i: number): LyricsDocument {
  const a = doc.lines[i];
  const b = doc.lines[i + 1];
  if (!a || !b) return doc;
  const background = [...(a.background ?? []), ...(b.background ?? [])];
  const merged: LyricsDocumentLine = {
    ...a,
    endMs: b.endMs,
    words: [
      ...a.words.map((u, k) =>
        k === a.words.length - 1 ? { ...u, joined: false } : u,
      ),
      ...b.words,
    ],
    background: background.length ? background : undefined,
  };
  return withLines(doc, [
    ...doc.lines.slice(0, i),
    merged,
    ...doc.lines.slice(i + 2),
  ]);
}

/** Set one translation of a line, the only one the editor shows. */
export function setTranslation(
  doc: LyricsDocument,
  i: number,
  lang: string | undefined,
  text: string,
): LyricsDocument {
  return mapLine(doc, i, (line) => {
    const rest = line.translations?.slice(1) ?? [];
    const first = text || lang ? [{ lang, text }] : [];
    const translations = [...first, ...rest];
    return {
      ...line,
      translations: translations.length ? translations : undefined,
    };
  });
}

/** Strip what a file should not carry: translations with no text, a
 *  romanization of only spaces. */
export function cleanedDocument(doc: LyricsDocument): LyricsDocument {
  return withLines(
    doc,
    doc.lines.map((line) => {
      const translations = line.translations?.filter((t) => t.text.trim());
      return {
        ...line,
        translations: translations?.length ? translations : undefined,
        romanization: line.romanization?.trim() || undefined,
      };
    }),
  );
}

// ----- singers ---------------------------------------------------------

/** The first `v<n>` id no singer has yet. */
export function nextAgentId(agents: readonly LyricsAgent[]): string {
  let n = 1;
  while (agents.some((a) => a.id === `v${n}`)) n += 1;
  return `v${n}`;
}

export function addAgent(doc: LyricsDocument): LyricsDocument {
  return {
    ...doc,
    agents: [...doc.agents, { id: nextAgentId(doc.agents), kind: "person" }],
  };
}

export function patchAgent(
  doc: LyricsDocument,
  id: string,
  patch: Partial<LyricsAgent>,
): LyricsDocument {
  return {
    ...doc,
    agents: doc.agents.map((a) => (a.id === id ? { ...a, ...patch } : a)),
  };
}

/** Remove a singer; the lines they sang go back to the lead. */
export function removeAgent(doc: LyricsDocument, id: string): LyricsDocument {
  return {
    ...doc,
    agents: doc.agents.filter((a) => a.id !== id),
    lines: doc.lines.map((l) =>
      l.agent === id ? { ...l, agent: undefined } : l,
    ),
  };
}

// ----- timing ----------------------------------------------------------

export function stampLine(
  doc: LyricsDocument,
  i: number,
  at: number,
): LyricsDocument {
  const t = ms(at);
  return mapLine(doc, i, (line) => {
    // Words already stamped move with the line: stamping its start again
    // corrects the whole line, as a nudge does.
    const before = lineBegin(line);
    const moved = before === undefined ? line : shiftLine(line, t - before);
    return {
      ...moved,
      beginMs: t,
      // An end stamped before is no end of a line starting later.
      endMs:
        moved.endMs !== undefined && moved.endMs > t ? moved.endMs : undefined,
    };
  });
}

/** Where line `i` stops: a silence before the next line starts. */
export function endLine(
  doc: LyricsDocument,
  i: number,
  at: number,
): LyricsDocument {
  return mapLine(doc, i, (line) => {
    const begin = lineBegin(line);
    return { ...line, endMs: Math.max(ms(at), begin ?? 0) };
  });
}

/** Every time of line `i` dropped, its units' included. */
export function clearLine(doc: LyricsDocument, i: number): LyricsDocument {
  return mapLine(doc, i, (line) => ({
    ...line,
    beginMs: undefined,
    endMs: undefined,
    words: line.words.map((u) => ({
      ...u,
      beginMs: undefined,
      endMs: undefined,
    })),
  }));
}

/**
 * Stamp the start of unit `j`. The first unit also starts the line, and a
 * stamp earlier than the previous unit's stated end shortens that unit.
 */
export function stampUnit(
  doc: LyricsDocument,
  i: number,
  j: number,
  at: number,
): LyricsDocument {
  const t = ms(at);
  return mapLine(doc, i, (line) => {
    let next = mapUnit(line, j, (u) => ({
      ...u,
      beginMs: t,
      endMs: u.endMs !== undefined && u.endMs > t ? u.endMs : undefined,
    }));
    if (j === 0) next = { ...next, beginMs: t };
    // The line's stated end came from its last unit (`endUnit`): one
    // earlier than this stamp is no longer where the line stops.
    if (next.endMs !== undefined && next.endMs < t) {
      next = { ...next, endMs: undefined };
    }
    if (j > 0) {
      const prev = next.words[j - 1];
      if (prev.endMs !== undefined && prev.endMs > t) {
        next = mapUnit(next, j - 1, (u) => ({ ...u, endMs: undefined }));
      }
    }
    return next;
  });
}

/** Stamp where unit `j` stops; the last unit also ends the line. */
export function endUnit(
  doc: LyricsDocument,
  i: number,
  j: number,
  at: number,
): LyricsDocument {
  const t = ms(at);
  return mapLine(doc, i, (line) => {
    const next = mapUnit(line, j, (u) => ({
      ...u,
      endMs: Math.max(t, u.beginMs ?? t),
    }));
    return j === line.words.length - 1 ? { ...next, endMs: t } : next;
  });
}

/** Unit `j`'s times dropped; the first unit takes the line's start along,
 *  the last one the line's end it set (`endUnit`). */
export function clearUnit(
  doc: LyricsDocument,
  i: number,
  j: number,
): LyricsDocument {
  return mapLine(doc, i, (line) => {
    const next = mapUnit(line, j, (u) => ({
      ...u,
      beginMs: undefined,
      endMs: undefined,
    }));
    const last = j === line.words.length - 1;
    return {
      ...next,
      beginMs: j === 0 ? undefined : next.beginMs,
      endMs: last ? undefined : next.endMs,
    };
  });
}

/** Every time of a line moved by `delta` ms: its own, its units', its
 *  backing vocals'. */
function shiftLine(
  line: LyricsDocumentLine,
  delta: number,
): LyricsDocumentLine {
  const shift = (v: number | undefined) =>
    v === undefined ? v : ms(v + delta);
  const move = (u: LyricsUnit): LyricsUnit => ({
    ...u,
    beginMs: shift(u.beginMs),
    endMs: shift(u.endMs),
  });
  return {
    ...line,
    beginMs: shift(line.beginMs),
    endMs: shift(line.endMs),
    words: line.words.map(move),
    background: line.background?.map(move),
  };
}

/** Move a whole line (`j === null`), or one unit's start, by `delta` ms. */
export function nudge(
  doc: LyricsDocument,
  i: number,
  j: number | null,
  delta: number,
): LyricsDocument {
  return mapLine(doc, i, (line) => {
    if (j === null) return shiftLine(line, delta);
    const next = mapUnit(line, j, (u) => ({
      ...u,
      beginMs: u.beginMs === undefined ? undefined : ms(u.beginMs + delta),
    }));
    return j === 0 ? { ...next, beginMs: next.words[0].beginMs } : next;
  });
}

/** Every time in the document moved by `delta` ms: a file stamped
 *  consistently early or late, or a reaction offset found afterwards. */
export function shiftDocument(
  doc: LyricsDocument,
  delta: number,
): LyricsDocument {
  return withLines(
    doc,
    doc.lines.map((line) => shiftLine(line, delta)),
  );
}

// ----- syllables -------------------------------------------------------

/** Split unit `j` at character `at` into two syllables of one word. */
export function splitUnit(
  doc: LyricsDocument,
  i: number,
  j: number,
  at: number,
): LyricsDocument {
  return mapLine(doc, i, (line) => {
    const u = line.words[j];
    if (!u || at <= 0 || at >= u.text.length) return line;
    const a: LyricsUnit = {
      text: u.text.slice(0, at),
      beginMs: u.beginMs,
      joined: true,
    };
    const b: LyricsUnit = {
      text: u.text.slice(at),
      endMs: u.endMs,
      joined: u.joined,
    };
    return {
      ...line,
      words: [...line.words.slice(0, j), a, b, ...line.words.slice(j + 1)],
    };
  });
}

/** Glue unit `j` to the next one, or unglue it. */
export function toggleJoined(
  doc: LyricsDocument,
  i: number,
  j: number,
): LyricsDocument {
  return mapLine(doc, i, (line) =>
    j >= line.words.length - 1
      ? line
      : mapUnit(line, j, (u) => ({ ...u, joined: !u.joined })),
  );
}

// ----- lookups ---------------------------------------------------------

/** The last line that has started at `at`, or -1. */
export function activeLineIndex(doc: LyricsDocument, at: number): number {
  let found = -1;
  doc.lines.forEach((line, k) => {
    const b = lineBegin(line);
    if (b !== undefined && b <= at) found = k;
  });
  return found;
}

/** How many lines, and how many units, carry a start. A stanza break
 *  (a line without words) is no line to sync and is not counted. */
export function syncProgress(doc: LyricsDocument) {
  let lines = 0;
  let totalLines = 0;
  let units = 0;
  let totalUnits = 0;
  for (const line of doc.lines) {
    if (line.words.length === 0) continue;
    totalLines += 1;
    if (lineBegin(line) !== undefined) lines += 1;
    totalUnits += line.words.length;
    units += line.words.filter((u) => u.beginMs !== undefined).length;
  }
  return { lines, totalLines, units, totalUnits };
}

/** Whether a later unit than the first is timed: a word-by-word sync. */
export function isWordSynced(line: LyricsDocumentLine): boolean {
  return line.words.some((u, j) => j > 0 && u.beginMs !== undefined);
}
