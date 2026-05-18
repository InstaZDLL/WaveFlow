import {
  useCallback,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { ThemeContext } from "../hooks/useTheme";

const THEME_STORAGE_KEY = "waveflow.theme.is_dark";

// Read the persisted preference synchronously so the very first render already
// matches the user's last choice. Survives crashes during the View Transitions
// animation (issue #34): even if the webview dies mid-toggle, the new value
// has already been written to localStorage before startViewTransition runs.
const readStoredTheme = (): boolean => {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(THEME_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
};

const writeStoredTheme = (isDark: boolean) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(THEME_STORAGE_KEY, isDark ? "true" : "false");
  } catch {
    // localStorage unavailable (private mode, quota) — preference simply
    // won't survive the next launch. Not worth surfacing to the user.
  }
};

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [isDark, setIsDark] = useState<boolean>(readStoredTheme);

  const toggleTheme = useCallback(
    (event?: ReactMouseEvent) => {
      let nextValue = false;
      const flipTheme = () =>
        setIsDark((prev) => {
          nextValue = !prev;
          return nextValue;
        });

      // Persist BEFORE triggering any animation. Some Linux WebKitGTK builds
      // crash the webview during startViewTransition on certain GPU/Wayland
      // stacks (issue #34) — writing first guarantees the next launch picks
      // up the new theme even if this transition kills the process.
      const persistNext = (value: boolean) => writeStoredTheme(value);

      // Fallback: no View Transitions API support → instant swap
      if (typeof document === "undefined" || !document.startViewTransition) {
        flipTheme();
        persistNext(nextValue);
        return;
      }

      // Compute & persist the future value before the animation starts so the
      // write lands even if the compositor crashes mid-transition.
      persistNext(!isDark);

      const transition = document.startViewTransition(flipTheme);

      // Radial reveal from the click point
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
            // Animation failed — the theme has still toggled via flipTheme()
          });
      }
    },
    [isDark],
  );

  return (
    <ThemeContext.Provider value={{ isDark, toggleTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}
