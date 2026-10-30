import type {
  WorkshopField,
  WorkshopSource,
} from "./tauri/tagWorkshop";

/**
 * Opening the tag workshop from anywhere — a track's menu, a folder's,
 * an album's, the enrichment review — without threading a navigation
 * callback through each of them: the layout listens for this event and
 * pushes the view with its source.
 */
export const OPEN_TAG_WORKSHOP_EVENT = "waveflow:open-tag-workshop";

export interface TagWorkshopRequest {
  source: WorkshopSource;
  /** A release to match against as soon as the files are read: the
   *  proposal the enrichment review was showing. */
  releaseMbid?: string | null;
}

export function openTagWorkshop(request: TagWorkshopRequest) {
  window.dispatchEvent(
    new CustomEvent<TagWorkshopRequest>(OPEN_TAG_WORKSHOP_EVENT, {
      detail: request,
    }),
  );
}

export type CaseMode = "title" | "sentence" | "upper" | "lower";

/** Words a title-cased English title keeps in lower case, except first
 *  and last. Other languages have no such list and get every word
 *  capitalised, which is what their own style guides mostly ask. */
const MINOR_WORDS = new Set([
  "a",
  "an",
  "and",
  "as",
  "at",
  "but",
  "by",
  "for",
  "in",
  "nor",
  "of",
  "on",
  "or",
  "the",
  "to",
  "vs",
  "vs.",
]);

/** `value` in `mode`. Title case keeps a word already in capitals
 *  ("AC/DC", "EP") as it is: it is an acronym, not a shout. */
export function applyCase(value: string, mode: CaseMode): string {
  switch (mode) {
    case "upper":
      return value.toLocaleUpperCase();
    case "lower":
      return value.toLocaleLowerCase();
    case "sentence": {
      const lower = value.toLocaleLowerCase();
      return lower.replace(/^(\s*)(\p{L})/u, (_, space: string, letter: string) =>
        `${space}${letter.toLocaleUpperCase()}`,
      );
    }
    case "title": {
      const words = value.split(/(\s+)/);
      const real = words.filter((w) => w.trim() !== "");
      let seen = 0;
      return words
        .map((word) => {
          if (word.trim() === "") return word;
          seen += 1;
          const isEdge = seen === 1 || seen === real.length;
          if (word.length > 1 && word === word.toLocaleUpperCase() && /\p{L}/u.test(word)) {
            return word;
          }
          const lower = word.toLocaleLowerCase();
          if (!isEdge && MINOR_WORDS.has(lower)) return lower;
          // "(feat." and "rock-n-roll": each part after a bracket or a
          // hyphen starts a word of its own.
          return lower.replace(
            /(^|[-(["'/])(\p{L})/gu,
            (_, lead: string, letter: string) =>
              `${lead}${letter.toLocaleUpperCase()}`,
          );
        })
        .join("");
    }
  }
}

export interface FindReplace {
  find: string;
  replace: string;
  regex: boolean;
  caseSensitive: boolean;
}

/** A compiled search, or the reason it does not compile. */
export function compileFind(
  spec: FindReplace,
): { pattern: RegExp } | { error: string } | null {
  if (spec.find === "") return null;
  const flags = spec.caseSensitive ? "gu" : "giu";
  try {
    const source = spec.regex
      ? spec.find
      : spec.find.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return { pattern: new RegExp(source, flags) };
  } catch (err) {
    return { error: err instanceof Error ? err.message : String(err) };
  }
}

/** `value` with every match replaced. A plain search inserts the
 *  replacement as typed; a regular expression may use `$1`. */
export function applyFindReplace(
  value: string,
  pattern: RegExp,
  spec: FindReplace,
): string {
  pattern.lastIndex = 0;
  if (spec.regex) return value.replace(pattern, spec.replace);
  return value.replace(pattern, () => spec.replace);
}

/** The text fields a search runs over when it is asked to run over all
 *  of them: never the numbers or the identifiers, which a stray
 *  replacement would make invalid. */
export const TEXT_FIELDS: readonly WorkshopField[] = [
  "title",
  "artist",
  "album",
  "album_artist",
  "genre",
  "composer",
];
