import {
  useCallback,
  useEffect,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { ThemeContext } from "../hooks/useTheme";
import {
  applyTheme,
  DEFAULT_THEME_ID,
  findTheme,
  THEME_PRESETS,
  type ThemePreset,
} from "../lib/themes";

const THEME_STORAGE_KEY = "waveflow.theme.id";
// Legacy key used by the previous binary light/dark toggle. We migrate
// it once at boot then keep writing to the new key only.
const LEGACY_DARK_KEY = "waveflow.theme.is_dark";

// Read the persisted preset synchronously so the very first render
// already matches the user's last choice. Falls back to legacy dark
// boolean if no new-format value exists.
const readStoredTheme = (): ThemePreset => {
  if (typeof window === "undefined") return findTheme(DEFAULT_THEME_ID);
  try {
    const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
    if (stored) return findTheme(stored);
    const legacyDark = window.localStorage.getItem(LEGACY_DARK_KEY);
    if (legacyDark === "true") return findTheme("default-dark");
    if (legacyDark === "false") return findTheme("default");
    return findTheme(DEFAULT_THEME_ID);
  } catch {
    return findTheme(DEFAULT_THEME_ID);
  }
};

const writeStoredTheme = (id: string) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, id);
  } catch {
    // localStorage unavailable (private mode, quota) — preference simply
    // won't survive the next launch. Not worth surfacing to the user.
  }
};

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useState<ThemePreset>(readStoredTheme);

  // Apply on every theme change so CSS vars + dark class stay in sync.
  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  const setThemeId = useCallback((id: string, event?: ReactMouseEvent) => {
    const next = findTheme(id);

    // Persist BEFORE triggering any animation. Some Linux WebKitGTK
    // builds crash the webview during startViewTransition on certain
    // GPU/Wayland stacks (issue #34) — writing first guarantees the
    // next launch picks up the new theme even if this transition kills
    // the process.
    writeStoredTheme(next.id);

    // Fallback: no View Transitions API support → instant swap.
    if (typeof document === "undefined" || !document.startViewTransition) {
      setTheme(next);
      return;
    }

    const transition = document.startViewTransition(() => setTheme(next));

    // Radial reveal from the click point if a mouse event was provided
    // (e.g. clicking a theme card in Settings). Falls back to the
    // default cross-fade if no event is available.
    if (event) {
      const x = event.clientX;
      const y = event.clientY;
      const endRadius = Math.hypot(
        Math.max(x, window.innerWidth - x),
        Math.max(y, window.innerHeight - y),
      );
      transition.ready
        .then(() => {
          document.documentElement.animate(
            {
              clipPath: [
                `circle(0px at ${x}px ${y}px)`,
                `circle(${endRadius}px at ${x}px ${y}px)`,
              ],
            },
            {
              duration: 600,
              easing: "ease-in-out",
              pseudoElement: "::view-transition-new(root)",
            },
          );
        })
        .catch(() => {
          // Animation failed — theme has still swapped via setTheme()
        });
    }
  }, []);

  const toggleTheme = useCallback(
    (event?: ReactMouseEvent) => {
      // Binary toggle for the existing topbar button: flip between
      // the two default presets. Custom themes are picked from the
      // Settings appearance panel, not from the topbar.
      const nextId =
        theme.mode === "dark" ? "default" : "default-dark";
      setThemeId(nextId, event);
    },
    [theme.mode, setThemeId],
  );

  const isDark = theme.mode === "dark";

  return (
    <ThemeContext.Provider
      value={{ theme, setThemeId, isDark, toggleTheme }}
    >
      {children}
    </ThemeContext.Provider>
  );
}

// Re-export the presets so callers can iterate the available themes
// without importing from the lib path directly.
export { THEME_PRESETS };
