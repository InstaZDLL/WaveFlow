import { useEffect } from "react";

import { useTheme } from "./useTheme";
import { themeAmbient } from "../lib/themes";
import { setWindowFrameColors } from "../lib/tauri/preferences";

/**
 * Keep the `adwaita` window frame in the theme's colours.
 *
 * That frame is the desktop's own, drawn by GTK and only restyled, so it
 * cannot read our CSS: the backend builds its stylesheet from what this
 * sends. The page background (`--ambient-bg`) rather than a fixed GNOME
 * grey, so the bar reads as the top of the window and not as a strip laid
 * over it — the way a libadwaita header bar shares its window's colour.
 *
 * Mounted once, by the main window's layout, and only sends while that
 * frame is the one in use: the backend keeps the last colours, and turning
 * the frame on later re-runs this with the current theme anyway.
 */
export function useWindowFrameColors(chrome: string) {
  const { theme, isDark } = useTheme();
  const background = themeAmbient(theme);

  useEffect(() => {
    if (chrome !== "adwaita") return;
    setWindowFrameColors(background, isDark).catch((err) =>
      console.error("[useWindowFrameColors] failed", err),
    );
  }, [chrome, background, isDark]);
}
