import { useCallback, useEffect } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

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

/**
 * The same notice across webviews. `useProfileSetting` broadcasts with a
 * window event, which never leaves the document that fired it — and the
 * point of this setting is to be edited in the main window while it is
 * read in the overlay. A write here re-emits it as a Tauri event, and
 * every other window turns that back into the window event.
 *
 * The payload names the window that wrote. Tauri delivers an event to its
 * sender too, and the writer must not re-read on it: while a slider is
 * dragged, the next write is already queued behind this one, and a read
 * landing in between would put the older stored value back on screen.
 */
const TAURI_EVENT = "desktop-lyrics:style-changed";

interface StyleChanged {
  source: string;
}

function ownLabel(): string {
  try {
    return getCurrentWindow().label;
  } catch {
    return "";
  }
}

function notifyOtherWindows(): void {
  const payload: StyleChanged = { source: ownLabel() };
  emit(TAURI_EVENT, payload).catch(() => {});
}

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
 * Concurrency, rollback and profile isolation come from
 * [`useProfileSetting`](./useProfileSetting.ts); this adds the
 * cross-window notice described at {@link TAURI_EVENT}.
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

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    const self = ownLabel();
    listen<StyleChanged>(TAURI_EVENT, (event) => {
      if (event.payload?.source === self) return;
      window.dispatchEvent(new CustomEvent(WINDOW_EVENT));
    })
      .then((off) => {
        if (cancelled) off();
        else unlisten = off;
      })
      .catch((err) => {
        console.warn("[useDesktopLyricsStyle] cross-window listen failed", err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const update = useCallback(
    (patch: Partial<DesktopLyricsStyle>) => {
      // Emitted after the write settles, whichever way: the other windows
      // re-read the database, so they land on what was actually stored
      // even when this write rolled back.
      void setValue((previous) => ({ ...previous, ...patch })).then(
        notifyOtherWindows,
      );
    },
    [setValue],
  );

  const reset = useCallback(() => {
    void setValue(DEFAULT_DESKTOP_LYRICS_STYLE).then(notifyOtherWindows);
  }, [setValue]);

  return { style: value, ready, update, reset };
}
