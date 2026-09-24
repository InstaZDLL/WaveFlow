import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const KEY = "ui.lyrics_highlight_color";

/** Window event `useProfileSetting` re-reads on, within one webview. The
 *  other windows — the mini-player above all — hear about a write through
 *  the cross-window bridge (#741). */
const EVENT = "waveflow:lyrics-highlight-color";

const HEX = /^#[0-9a-f]{6}$/i;

/** `null` is "no colour of my own": each view keeps its own white. */
function parseColor(raw: string | null): string | null {
  return raw != null && HEX.test(raw) ? raw.toLowerCase() : null;
}

export interface LyricsHighlightColor {
  /** `#rrggbb`, or `null` when the user has not picked one. */
  color: string | null;
  /** `false` until the active profile's stored value has been read. */
  ready: boolean;
  /** Pick a colour, or `null` to go back to each view's default. */
  setColor: (next: string | null) => Promise<void>;
}

/**
 * Per-profile colour of the line being sung in the immersive lyrics — both
 * the merged view and the lyrics-only one — and in the mini-player (#751).
 * The desktop lyrics window keeps its own, richer style
 * ([`useDesktopLyricsStyle`](./useDesktopLyricsStyle.ts)).
 *
 * Unset by default, and unset is not white written down: it leaves each
 * view on its own colour, so a later change to a view's default reaches
 * everyone who never chose. High contrast overrides a chosen colour, in
 * CSS (`.wf-lyrics-highlight` in `app.css`), because a colour picked for
 * looks is not one picked for legibility.
 */
export function useLyricsHighlightColor(): LyricsHighlightColor {
  const { value, ready, setValue } = useProfileSetting<string | null>({
    key: KEY,
    defaultValue: null,
    parse: parseColor,
    serialize: (value) => value ?? "",
    valueType: "string",
    event: EVENT,
    label: "useLyricsHighlightColor",
  });

  const setColor = useCallback(
    (next: string | null) => setValue(next == null ? null : parseColor(next)),
    [setValue],
  );

  return { color: value, ready, setColor };
}
