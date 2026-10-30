import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const KEY = "lyrics.editor_reaction_ms";
const EVENT = "waveflow:lyrics-editor-reaction-changed";
/** No correction until the user measures their own. */
const DEFAULT = 0;
/** A reaction slower than half a second is not one a key can help. */
export const MAX_REACTION_MS = 500;

function clamp(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT;
  return Math.max(0, Math.min(MAX_REACTION_MS, Math.round(value)));
}

function parse(raw: string | null): number {
  return raw === null ? DEFAULT : clamp(Number(raw));
}

function serialize(ms: number): string {
  return String(ms);
}

/**
 * How many milliseconds the lyrics editor moves every tap earlier, to
 * make up for the time between hearing a word and pressing the key — per
 * profile, as it is the user's own reflex rather than the song's.
 *
 * Kept by the editor, not shown in Settings, so it has no entry in the
 * settings registry (like the lyrics localization mode).
 */
export function useLyricsEditorReaction() {
  const { value, setValue } = useProfileSetting<number>({
    key: KEY,
    defaultValue: DEFAULT,
    parse,
    serialize,
    valueType: "int",
    event: EVENT,
    label: "useLyricsEditorReaction",
  });
  const setReactionMs = useCallback(
    (ms: number) => {
      void setValue(clamp(ms));
    },
    [setValue],
  );
  return { reactionMs: value, setReactionMs };
}
