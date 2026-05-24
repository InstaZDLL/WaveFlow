import {
  createContext,
  useContext,
  type MouseEvent as ReactMouseEvent,
} from "react";
import type { ThemePreset } from "../lib/themes";

interface ThemeContextValue {
  /** Currently active preset (one of `THEME_PRESETS`). */
  theme: ThemePreset;
  /** Switch to a preset by id. Persists across launches. */
  setThemeId: (id: string, event?: ReactMouseEvent) => void;
  /**
   * Backward-compat: derived from `theme.mode === "dark"`. New code
   * should prefer reading `theme` directly so it can adapt to the
   * full preset (accent, ambient bg, etc.) instead of binary light/dark.
   */
  isDark: boolean;
  /**
   * Toggle between the default light and default dark presets.
   * Preserved for the existing theme-toggle button in the topbar.
   */
  toggleTheme: (event?: ReactMouseEvent) => void;
}

export const ThemeContext = createContext<ThemeContextValue | null>(null);

export function useTheme() {
  const context = useContext(ThemeContext);
  if (!context) throw new Error("useTheme must be used within ThemeProvider");
  return context;
}
