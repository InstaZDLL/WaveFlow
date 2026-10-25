import { useEffect } from "react";

import { useSkin } from "./useSkin";
import { useTheme } from "./useTheme";
import { themeAmbient } from "../lib/themes";
import {
  setWindowFrameColors,
  type WindowChrome,
} from "../lib/tauri/preferences";

/**
 * The colour the page is painted with, as `#rrggbb`: the body's own
 * background where the layout gives it one (the 2.0 skins paint their
 * `--fe-bg` there and make the ambient wrapper transparent), else the
 * theme's ambient, which the classic layout paints on its wrapper.
 */
function pageBackground(fallback: string): string {
  const match = getComputedStyle(document.body).backgroundColor.match(
    /^rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)$/,
  );
  if (!match || (match[4] !== undefined && Number(match[4]) === 0)) {
    return fallback;
  }
  return `#${match
    .slice(1, 4)
    .map((channel) => Number(channel).toString(16).padStart(2, "0"))
    .join("")}`;
}

/**
 * Keep the `adwaita` window frame in the page's colour.
 *
 * That frame is the desktop's own, drawn by GTK and only restyled, so it
 * cannot read our CSS: the backend builds its stylesheet from what this
 * sends, and reads dark or light from the colour itself. The page's colour
 * rather than a fixed GNOME grey, so the bar reads as the top of the window
 * and not as a strip laid over it — the way a libadwaita header bar shares
 * its window's colour.
 *
 * Mounted once, by the main window's layout, and only sends while that
 * frame is the one in use: the backend keeps the last colour, and turning
 * the frame on later re-runs this with the current page anyway.
 */
export function useWindowFrameColors(chrome: WindowChrome) {
  const { theme } = useTheme();
  const { skin } = useSkin();

  useEffect(() => {
    if (chrome !== "adwaita") return;
    // A frame later: the theme and skin providers paint in their own
    // effects, which run after this one -- their parent's effects come
    // after their children's.
    const frame = requestAnimationFrame(() => {
      setWindowFrameColors(pageBackground(themeAmbient(theme))).catch((err) =>
        console.error("[useWindowFrameColors] failed", err),
      );
    });
    return () => cancelAnimationFrame(frame);
  }, [chrome, theme, skin.id]);
}
