import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const KEY = "lyrics.localization_mode";

/**
 * Window event broadcast after a successful write, so the docked panel
 * and the immersive column agree the moment either one toggles — they
 * render the same lines side by side when the immersive view is open
 * over the panel.
 */
export const LYRICS_LOCALIZATION_EVENT = "waveflow:lyrics-localization";

/**
 * Which extra reading of a line to show under it, when the document
 * carries one.
 *
 * Only one at a time, which is how Apple Music presents it: a third
 * row under every line turns a lyric column into a table, and the two
 * readings answer different questions — "how do I say this" and "what
 * does it mean" — so a reader wants one or the other, not both.
 */
export type LyricsLocalizationMode = "off" | "romanization" | "translation";

const DEFAULT_MODE: LyricsLocalizationMode = "off";

function parse(raw: string | null): LyricsLocalizationMode {
  return raw === "romanization" || raw === "translation" ? raw : DEFAULT_MODE;
}

function serialize(value: LyricsLocalizationMode): string {
  return value;
}

export interface LyricsLocalization {
  mode: LyricsLocalizationMode;
  /** Fire-and-forget: optimistic, serialized, rolled back on failure. */
  setMode: (next: LyricsLocalizationMode) => void;
}

/**
 * Per-profile preference for the romanization / translation row under
 * each lyric line (issue #584).
 *
 * Default OFF, and deliberately so even though a document carrying a
 * romanization is one the user asked for: the extra row doubles the
 * height of every line, and a reader who can read the original script
 * wants it gone. The control only appears when the current document
 * actually has something to show, so the default costs nothing to
 * anyone whose lyrics are plain.
 *
 * Per profile rather than per track: the choice follows a person's
 * reading ability, which does not change from one song to the next.
 */
export function useLyricsLocalization(): LyricsLocalization {
  const { value, setValue } = useProfileSetting<LyricsLocalizationMode>({
    key: KEY,
    defaultValue: DEFAULT_MODE,
    parse,
    serialize,
    valueType: "string",
    event: LYRICS_LOCALIZATION_EVENT,
    label: "useLyricsLocalization",
  });

  // Fire-and-forget by contract: the shared hook never rejects (it logs
  // and rolls back internally), so dropping the promise is safe.
  const setMode = useCallback(
    (next: LyricsLocalizationMode) => {
      void setValue(next);
    },
    [setValue],
  );

  return { mode: value, setMode };
}
