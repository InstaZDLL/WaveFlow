import type { LyricsLocalizationMode } from "../hooks/useLyricsLocalization";
import type { LyricsLine } from "./tauri/lyrics";

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
