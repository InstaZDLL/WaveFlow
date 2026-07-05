import {
  useCallback,
  useEffect,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { ThemeContext } from "../hooks/useTheme";
import { useProfile } from "../hooks/useProfile";
import {
  applyTheme,
  DEFAULT_THEME_ID,
  findTheme,
  THEME_PRESETS,
  type ThemePreset,
} from "../lib/themes";
import { getProfileSetting, setProfileSetting } from "../lib/tauri/profile";

/// Profile-setting key holding the user's selected preset id. Read on
/// every profile mount + switch; written on every `setThemeId`.
const PROFILE_SETTING_KEY = "appearance.theme.id";

/// localStorage key used as a first-paint cache. The DB row is the
/// source of truth — this key just lets the initial React render paint
/// the right theme synchronously, avoiding the ~100 ms flash that an
/// async DB read at mount would otherwise produce.
const THEME_CACHE_KEY = "waveflow.theme.id";
// Legacy key used by the previous binary light/dark toggle. We migrate
// it once at boot then keep writing to the new key only.
const LEGACY_DARK_KEY = "waveflow.theme.is_dark";

// Read the cached preset synchronously so the very first render
// already matches the user's last choice. Falls back to the legacy
// dark boolean if no new-format value exists — and migrates it in the
// same pass so a downgrade-then-upgrade cycle can't silently overwrite
// a custom preset with the stale boolean.
const readCachedTheme = (): ThemePreset => {
  if (typeof window === "undefined") return findTheme(DEFAULT_THEME_ID);
  try {
    const stored = window.localStorage.getItem(THEME_CACHE_KEY);
    if (stored) return findTheme(stored);
    const legacyDark = window.localStorage.getItem(LEGACY_DARK_KEY);
    if (legacyDark === "true" || legacyDark === "false") {
      const migrated = findTheme(
        legacyDark === "true" ? "default-dark" : "default",
      );
      window.localStorage.setItem(THEME_CACHE_KEY, migrated.id);
      window.localStorage.removeItem(LEGACY_DARK_KEY);
      return migrated;
    }
    return findTheme(DEFAULT_THEME_ID);
  } catch {
    return findTheme(DEFAULT_THEME_ID);
  }
};

const writeCachedTheme = (id: string) => {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(THEME_CACHE_KEY, id);
  } catch {
    // localStorage unavailable (private mode, quota) — DB value still
    // wins on next launch, so the cache miss isn't fatal.
  }
};

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useState<ThemePreset>(readCachedTheme);
  const { activeProfile } = useProfile();

  // Apply on every theme change so CSS vars + dark class stay in sync.
  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  // Source-of-truth read: pull the active profile's saved theme on
  // mount AND on every profile switch. The cache may be stale (last
  // profile's choice) — the DB row wins. Also seeds the DB from
  // localStorage on the very first launch so a pre-existing user
  // doesn't lose their choice on the legacy → per-profile migration.
  useEffect(() => {
    if (!activeProfile) return;
    let cancelled = false;
    (async () => {
      try {
        const stored = await getProfileSetting(PROFILE_SETTING_KEY);
        if (cancelled) return;
        if (stored) {
          const fromDb = findTheme(stored);
          if (fromDb.id !== theme.id) {
            setTheme(fromDb);
            writeCachedTheme(fromDb.id);
          }
          return;
        }
        // No row yet for this profile. Seed it with whatever's
        // currently applied (cache hit or default) so subsequent
        // switches have something to read back.
        await setProfileSetting(PROFILE_SETTING_KEY, theme.id, "string");
      } catch (err) {
        console.warn("[ThemeContext] profile-scoped theme load failed", err);
      }
    })();
    return () => {
      cancelled = true;
    };
    // Intentionally only depends on activeProfile.id — theme is read
    // once per profile, not on every theme change (otherwise the
    // setProfileSetting in setThemeId would race this read).
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeProfile?.id]);

  const setThemeId = useCallback((id: string, event?: ReactMouseEvent) => {
    const next = findTheme(id);

    // Persist BEFORE triggering any animation. Some Linux WebKitGTK
    // builds crash the webview during startViewTransition on certain
    // GPU/Wayland stacks (issue #34) — writing first guarantees the
    // next launch picks up the new theme even if this transition kills
    // the process.
    writeCachedTheme(next.id);
    // DB write is best-effort: a failure logs but doesn't block the
    // visual swap. localStorage still carries the choice forward.
    void setProfileSetting(PROFILE_SETTING_KEY, next.id, "string").catch(
      (err) => console.warn("[ThemeContext] profile-setting write failed", err),
    );

    // Fallback: no View Transitions API support → instant swap.
    if (typeof document === "undefined" || !document.startViewTransition) {
      setTheme(next);
      return;
    }

    // `startViewTransition` itself can throw synchronously on some
    // WebKitGTK builds (same family of bugs as the comment above). If
    // it does, swap the theme without animation so we don't leave the
    // app desynced (theme id persisted but `setTheme` never called).
    let transition: ViewTransition;
    try {
      transition = document.startViewTransition(() => setTheme(next));
    } catch {
      setTheme(next);
      return;
    }

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
      // Binary topbar toggle: flip to the current preset's opposite-mode
      // counterpart (the `pair` field on `ThemePreset`) so picking
      // "Lavender" then clicking the sun toggle lands on "Lavender Light"
      // instead of resetting to the global Émeraude default — the v1.3.0
      // behaviour that lost the user's theme family on every click.
      // One-off presets without a mirrored variant (OLED black, Neon)
      // fall back to the global `default` / `default-dark` swap so the
      // toggle still does something sensible.
      const nextId =
        theme.pair ?? (theme.mode === "dark" ? "default" : "default-dark");
      setThemeId(nextId, event);
    },
    [theme.pair, theme.mode, setThemeId],
  );

  const isDark = theme.mode === "dark";

  return (
    <ThemeContext.Provider value={{ theme, setThemeId, isDark, toggleTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}

// Re-export the presets so callers can iterate the available themes
// without importing from the lib path directly.
export { THEME_PRESETS };
