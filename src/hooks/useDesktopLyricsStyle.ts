import { useCallback } from "react";

import { useProfileSetting } from "./useProfileSetting";

/**
 * How the floating desktop lyrics window draws its text (issue #582).
 * Edited in Settings → Appearance, read by the overlay window.
 */
export interface DesktopLyricsStyle {
  /** Font size of the current line, in px. The next line is smaller. */
  fontSize: number;
  /** Colour of text not yet sung, and of the next line. `#rrggbb`. */
  textColor: string;
  /** Colour of the sung part of the current line. `#rrggbb`. */
  highlightColor: string;
  /** A dark outline around the glyphs, so light text stays readable on
   *  a light desktop. */
  outline: boolean;
  /** Opacity of the panel behind the text, 0–80 %. 0 floats the text on
   *  the desktop with nothing behind it. */
  backgroundOpacity: number;
  /** Show a line's translation under it, when the lyrics carry one. */
  showTranslation: boolean;
  /** Show the next line under the current one, when there is no
   *  translation to show there. Off, the window is a single line (#735). */
  showNextLine: boolean;
}

export const FONT_SIZE_MIN = 20;
export const FONT_SIZE_MAX = 72;
export const BACKGROUND_OPACITY_MAX = 80;

export const DEFAULT_DESKTOP_LYRICS_STYLE: DesktopLyricsStyle = {
  fontSize: 36,
  textColor: "#ffffff",
  highlightColor: "#34d399",
  outline: true,
  backgroundOpacity: 0,
  showTranslation: true,
  showNextLine: true,
};

const KEY = "ui.desktop_lyrics_style";

/** Window event `useProfileSetting` re-reads on, within one webview. */
const WINDOW_EVENT = "waveflow:desktop-lyrics-style-changed";

const HEX = /^#[0-9a-f]{6}$/i;

function clamp(n: unknown, min: number, max: number, fallback: number) {
  return typeof n === "number" && Number.isFinite(n)
    ? Math.min(max, Math.max(min, Math.round(n)))
    : fallback;
}

/** Field by field, so one bad value (a hand-edited row, a future build's
 *  shape) costs that field its default rather than the whole style. */
function parseStyle(raw: string | null): DesktopLyricsStyle {
  const d = DEFAULT_DESKTOP_LYRICS_STYLE;
  if (raw == null) return d;
  let parsed: Record<string, unknown>;
  try {
    const value: unknown = JSON.parse(raw);
    if (typeof value !== "object" || value === null) return d;
    parsed = value as Record<string, unknown>;
  } catch {
    return d;
  }
  const color = (v: unknown, fallback: string) =>
    typeof v === "string" && HEX.test(v) ? v : fallback;
  return {
    fontSize: clamp(parsed.fontSize, FONT_SIZE_MIN, FONT_SIZE_MAX, d.fontSize),
    textColor: color(parsed.textColor, d.textColor),
    highlightColor: color(parsed.highlightColor, d.highlightColor),
    outline: typeof parsed.outline === "boolean" ? parsed.outline : d.outline,
    backgroundOpacity: clamp(
      parsed.backgroundOpacity,
      0,
      BACKGROUND_OPACITY_MAX,
      d.backgroundOpacity,
    ),
    showTranslation:
      typeof parsed.showTranslation === "boolean"
        ? parsed.showTranslation
        : d.showTranslation,
    showNextLine:
      typeof parsed.showNextLine === "boolean"
        ? parsed.showNextLine
        : d.showNextLine,
  };
}

/**
 * Per-profile desktop lyrics style, `profile_setting['ui.desktop_lyrics_style']`.
 * Concurrency, rollback, profile isolation and the cross-window notice
 * all come from [`useProfileSetting`](./useProfileSetting.ts). This hook
 * carried its own copy of that notice until #741 made it general (#743).
 */
export function useDesktopLyricsStyle() {
  const { value, ready, setValue } = useProfileSetting<DesktopLyricsStyle>({
    key: KEY,
    defaultValue: DEFAULT_DESKTOP_LYRICS_STYLE,
    parse: parseStyle,
    serialize: (style) => JSON.stringify(style),
    valueType: "json",
    event: WINDOW_EVENT,
    label: "useDesktopLyricsStyle",
  });

  const update = useCallback(
    (patch: Partial<DesktopLyricsStyle>) => {
      void setValue((previous) => ({ ...previous, ...patch }));
    },
    [setValue],
  );

  const reset = useCallback(() => {
    void setValue(DEFAULT_DESKTOP_LYRICS_STYLE);
  }, [setValue]);

  return { style: value, ready, update, reset };
}
