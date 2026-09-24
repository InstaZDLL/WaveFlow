import { useCallback } from "react";
import { useProfileSetting } from "./useProfileSetting";

const KEY = "ui.lyrics_highlight_color";

/** Window event `useProfileSetting` re-reads on, within one webview. The
 *  other windows — the mini-player above all — hear about a write through
 *  the cross-window bridge (#741). */
const EVENT = "waveflow:lyrics-highlight-color";

/**
 * The pastels on offer, in hue order — which is also the order `rainbow`
 * walks them in. A fixed palette rather than a free colour picker: both
 * surfaces are dark, and a free pick let a dark blue or a saturated red
 * land on them, unreadable. Every entry here is light enough to read on
 * either, and none of them clashes with its neighbours in the rainbow.
 */
export const LYRICS_PASTELS = {
  rose: "#f9a8d4",
  peach: "#fdba74",
  vanilla: "#fde68a",
  mint: "#86efac",
  aqua: "#67e8f9",
  sky: "#93c5fd",
  lavender: "#c4b5fd",
  lilac: "#f0abfc",
} as const;

export type LyricsPastelId = keyof typeof LYRICS_PASTELS;

/** A pastel, or `rainbow`: each sung line takes the next pastel. */
export type LyricsHighlightId = LyricsPastelId | "rainbow";

const PASTEL_ORDER = Object.keys(LYRICS_PASTELS) as LyricsPastelId[];

/** Every choice, in the order the Settings row shows them. */
export const LYRICS_HIGHLIGHT_ORDER: LyricsHighlightId[] = [
  ...PASTEL_ORDER,
  "rainbow",
];

/** `null` is "no colour of my own": each view keeps its own white. An
 *  unknown value — a `#rrggbb` from the free picker this replaced, which
 *  only ever lived in a pre-release build — reads as that same `null`. */
function parseId(raw: string | null): LyricsHighlightId | null {
  return raw != null && (LYRICS_HIGHLIGHT_ORDER as string[]).includes(raw)
    ? (raw as LyricsHighlightId)
    : null;
}

/**
 * The colour of the line at `lineIndex`, or `null` for the view's own.
 * `rainbow` keys on the line's index rather than on time, so the immersive
 * view and the mini-player — separate webviews reading the same lyrics —
 * paint the same line the same colour.
 */
export function lyricsHighlightColor(
  id: LyricsHighlightId | null,
  lineIndex: number,
): string | null {
  if (id == null) return null;
  if (id !== "rainbow") return LYRICS_PASTELS[id];
  const n = PASTEL_ORDER.length;
  return LYRICS_PASTELS[PASTEL_ORDER[((lineIndex % n) + n) % n]];
}

export interface LyricsHighlightColor {
  /** The chosen pastel or `rainbow`, or `null` when the user has not
   *  picked one. Resolve it per line with {@link lyricsHighlightColor}. */
  id: LyricsHighlightId | null;
  /** `false` until the active profile's stored value has been read. */
  ready: boolean;
  /** Pick a colour, or `null` to go back to each view's default. */
  setId: (next: LyricsHighlightId | null) => Promise<void>;
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
 * CSS (`.wf-lyrics-highlight` in `app.css`): even a pastel is a colour
 * picked for looks, and that mode is about legibility alone.
 */
export function useLyricsHighlightColor(): LyricsHighlightColor {
  const { value, ready, setValue } =
    useProfileSetting<LyricsHighlightId | null>({
      key: KEY,
      defaultValue: null,
      parse: parseId,
      serialize: (value) => value ?? "",
      valueType: "string",
      event: EVENT,
      label: "useLyricsHighlightColor",
    });

  const setId = useCallback(
    (next: LyricsHighlightId | null) => setValue(next),
    [setValue],
  );

  return { id: value, ready, setId };
}
