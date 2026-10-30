import type { LyricsLocalizationMode } from "../hooks/useLyricsLocalization";
import {
  isUntimedLrc,
  parseLyrics,
  saysTheSameThing,
  usableRomanization,
  type AssociatedLyrics,
  type AssociatedLyricsKind,
  type LyricsLine,
} from "./tauri/lyrics";

/**
 * How far apart an associated line's stamp and its original's may sit
 * and still be the same line. Providers stamp a translation with the
 * original's time; the slack absorbs only a rounding to another
 * precision (`[00:12.34]` against `[00:12.345]`).
 */
const ASSOCIATED_STAMP_TOLERANCE_MS = 100;

/**
 * Lay the translation and pronunciation that came with the lyrics
 * (`LyricsPayload.associated`) under the lines they belong to, as
 * `translation` and `romanization` — the fields the TTML
 * `<translations>` / `<transliterations>` fill, so the localization
 * toggle offers them the same way.
 *
 * An associated document is a document in its own right, not a list
 * running in step with the original: it may skip a line or stamp an
 * extra one. So lines are paired by time, never by position: each line
 * takes the associated line stamped nearest to it, within
 * {@link ASSOCIATED_STAMP_TOLERANCE_MS}. A line nothing is stamped near
 * gets nothing. An associated document with no timing (plain text, or
 * LRC stamped all on one time) has nothing to pair on and is left out.
 *
 * What the primary document says for itself wins: a line that already
 * carries a translation or a romanization keeps it. An entry that only
 * repeats its line is dropped, as for TTML. A romanization keeps its
 * word split only when it matches the line's word for word, and then
 * takes the line's word bounds, as {@link usableRomanization} rules.
 *
 * One document per kind: the one tagged `language` exactly, else one in
 * its language whatever the region or script, else the first by language
 * tag, the order the cache reads them back in.
 */
export function attachAssociatedLyrics(
  lines: LyricsLine[],
  associated: readonly AssociatedLyrics[] | undefined,
  language?: string,
): LyricsLine[] {
  if (lines.length === 0 || !associated?.length) return lines;
  const translated = timedLinesOf(
    pickAssociated(associated, "translation", language),
  );
  const romanized = timedLinesOf(
    pickAssociated(associated, "pronunciation", language),
  );
  if (translated.length === 0 && romanized.length === 0) return lines;

  return lines.map((line) => {
    let next = line;
    const translation = nearestByTime(translated, line.timeMs)?.text.trim();
    if (
      !line.translation &&
      translation &&
      !saysTheSameThing(translation, line.text)
    ) {
      next = { ...next, translation };
    }
    const reading = nearestByTime(romanized, line.timeMs);
    const text = reading?.text.trim();
    if (!line.romanization && reading && text) {
      const usable = usableRomanization(
        { text, words: reading.words },
        line.words,
        line.text,
      );
      if (usable) {
        next = {
          ...next,
          romanization: usable.words
            ? {
                text: usable.text,
                words: usable.words.map((word, i) => ({
                  ...word,
                  timeMs: line.words![i].timeMs,
                  endMs: line.words![i].endMs,
                })),
              }
            : usable,
        };
      }
    }
    return next;
  });
}

/** The associated document of one kind to show, or `undefined`. */
function pickAssociated(
  associated: readonly AssociatedLyrics[],
  kind: AssociatedLyricsKind,
  language: string | undefined,
): AssociatedLyrics | undefined {
  const docs = associated
    .filter((doc) => doc.kind === kind)
    .sort((a, b) => {
      const one = a.language ?? "";
      const other = b.language ?? "";
      return one < other ? -1 : one > other ? 1 : 0;
    });
  // The whole tag first, so a `zh-TW` interface is not handed `zh-CN`
  // because it sorts first; then the language alone.
  const exact = normalizedTag(language);
  const wanted = primarySubtag(language);
  return (
    (exact !== ""
      ? docs.find((doc) => normalizedTag(doc.language) === exact)
      : undefined) ??
    (wanted !== ""
      ? docs.find((doc) => primarySubtag(doc.language) === wanted)
      : undefined) ??
    docs[0]
  );
}

function normalizedTag(tag: string | undefined): string {
  return (tag ?? "").replace(/_/g, "-").toLowerCase();
}

function primarySubtag(tag: string | undefined): string {
  return normalizedTag(tag).split("-")[0];
}

/** A document's lines, when it carries timing to pair them on. */
function timedLinesOf(doc: AssociatedLyrics | undefined): LyricsLine[] {
  if (!doc) return [];
  const lines = parseLyrics(doc.content, doc.format);
  if (
    (doc.format === "lrc" || doc.format === "enhanced_lrc") &&
    isUntimedLrc(lines)
  ) {
    return [];
  }
  return lines;
}

/**
 * The line of `sorted` (ascending `timeMs`, as every parser returns
 * them) stamped nearest to `timeMs`, within the tolerance. Blank lines
 * count: one stamped on the original says there is nothing to show
 * under it, and must not let a neighbour's text in.
 */
function nearestByTime(
  sorted: readonly LyricsLine[],
  timeMs: number,
): LyricsLine | undefined {
  let lo = 0;
  let hi = sorted.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (sorted[mid].timeMs < timeMs) lo = mid + 1;
    else hi = mid;
  }
  let best: LyricsLine | undefined;
  for (const candidate of [sorted[lo - 1], sorted[lo]]) {
    if (!candidate) continue;
    const gap = Math.abs(candidate.timeMs - timeMs);
    if (gap > ASSOCIATED_STAMP_TOLERANCE_MS) continue;
    if (!best || gap < Math.abs(best.timeMs - timeMs)) best = candidate;
  }
  return best;
}

/** What the current document can actually show under its lines. */
export interface AvailableLocalizations {
  romanization: boolean;
  translation: boolean;
  /** True when at least one of the two is present. */
  any: boolean;
}

/**
 * What the lines in hand carry.
 *
 * Asked of the parsed lines rather than of the raw document because
 * that is what the renderer will actually draw: a document can declare
 * a `<transliteration>` whose every entry was dropped for not lining up
 * word for word, and offering a toggle that then shows nothing is worse
 * than not offering it.
 */
export function availableLocalizations(
  lines: readonly LyricsLine[],
): AvailableLocalizations {
  let romanization = false;
  let translation = false;
  for (const line of lines) {
    if (!romanization && (line.romanization?.text.length ?? 0) > 0)
      romanization = true;
    if (!translation && (line.translation ?? "").length > 0) translation = true;
    if (romanization && translation) break;
  }
  return { romanization, translation, any: romanization || translation };
}

/**
 * The next mode for one press of the toggle, skipping what this
 * document does not have.
 *
 * Cycles off → romanization → translation → off, minus the missing
 * ones, so a document with only a romanization is a plain on/off
 * switch. Returns `"off"` when nothing is available, which also covers
 * a press that raced a track change.
 */
export function cycleLocalizationMode(
  current: LyricsLocalizationMode,
  available: AvailableLocalizations,
): LyricsLocalizationMode {
  const order: LyricsLocalizationMode[] = ["off"];
  if (available.romanization) order.push("romanization");
  if (available.translation) order.push("translation");
  if (order.length === 1) return "off";
  // A mode the current document lacks is not in `order`; `indexOf`
  // returns -1 and the next step lands on `order[0]` — "off" — which is
  // the honest answer for a preference carried over from a track that
  // had it.
  return order[(order.indexOf(current) + 1) % order.length];
}

/**
 * The mode to actually render with, which is the stored preference only
 * while this document can honour it.
 *
 * The preference is per profile and outlives the track: someone who
 * turned romanization on for one song keeps it on for the next, where
 * the document may have none. Resolving here rather than writing the
 * preference back means the setting survives the gap — the next song
 * that does carry one shows it again without being asked twice.
 */
export function effectiveLocalizationMode(
  stored: LyricsLocalizationMode,
  available: AvailableLocalizations,
): LyricsLocalizationMode {
  if (stored === "romanization" && available.romanization)
    return "romanization";
  if (stored === "translation" && available.translation) return "translation";
  return "off";
}
