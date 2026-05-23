/**
 * Theme presets. Each theme remaps Tailwind's emerald palette (used
 * across WaveFlow as the accent color) to a different color family
 * via CSS custom properties wired up in `app.css` through Tailwind v4's
 * `@theme inline` directive. Switching themes therefore re-skins every
 * existing `bg-emerald-*` / `text-emerald-*` / `border-emerald-*`
 * utility without touching components.
 *
 * Themes also choose whether they're light or dark — the root `dark`
 * class is toggled accordingly so the existing `dark:` Tailwind
 * variants stay coherent.
 *
 * Adding a new theme is a one-stop process: define the palette here
 * and the Settings picker picks it up automatically.
 */

export type ThemeMode = "light" | "dark";

export interface ThemePreset {
  /** Stable id used as the persisted value and the `data-theme` attribute. */
  id: string;
  /** i18n key suffix, e.g. `appearance.themes.midnight.label`. */
  labelKey: string;
  /** Whether the root should carry the `dark` class. */
  mode: ThemeMode;
  /**
   * Accent palette — full 50→950 scale. Maps to `--accent-XX` CSS
   * variables. The existing `emerald-*` Tailwind utilities are
   * remapped to these vars in `app.css`, so changing a theme re-tints
   * the entire app without touching any component.
   */
  accent: {
    50: string;
    100: string;
    200: string;
    300: string;
    400: string;
    500: string;
    600: string;
    700: string;
    800: string;
    900: string;
    950: string;
  };
  /**
   * Optional ambient tint applied to the page background. `null` keeps
   * the Tailwind default (`bg-white` / `dark:bg-surface-dark`).
   */
  ambient?: string | null;
  /**
   * Optional override for the `--color-surface-dark` token used by the
   * Sidebar, right panels (Queue/NowPlaying/Lyrics) and other base
   * panels via `dark:bg-surface-dark`. Default `#121212`. Set this to
   * the theme's ambient family so sidebar + panels carry the theme tint
   * instead of staying flat charcoal — otherwise a "Lavender" theme
   * leaves a black sidebar against a violet body, which is the bug the
   * theme-aware surfaces were introduced to fix.
   */
  surfaceDark?: string | null;
  /**
   * Optional override for `--color-surface-dark-elevated` used by the
   * PlayerBar, AudioQualityFooter and modal cards. Default `#181818`.
   * Should sit one notch lighter than `surfaceDark` so elevated panels
   * read above the body without breaking the theme palette.
   */
  surfaceDarkElevated?: string | null;
}

const EMERALD = {
  50: "oklch(0.979 0.021 166.113)",
  100: "oklch(0.95 0.052 163.051)",
  200: "oklch(0.905 0.093 164.15)",
  300: "oklch(0.845 0.143 164.978)",
  400: "oklch(0.765 0.177 163.223)",
  500: "oklch(0.696 0.17 162.48)",
  600: "oklch(0.596 0.145 163.225)",
  700: "oklch(0.508 0.118 165.612)",
  800: "oklch(0.432 0.095 166.913)",
  900: "oklch(0.378 0.077 168.94)",
  950: "oklch(0.262 0.051 172.552)",
};

const INDIGO = {
  50: "oklch(0.962 0.018 272.314)",
  100: "oklch(0.93 0.034 272.788)",
  200: "oklch(0.87 0.065 274.039)",
  300: "oklch(0.785 0.115 274.713)",
  400: "oklch(0.673 0.182 276.935)",
  500: "oklch(0.585 0.233 277.117)",
  600: "oklch(0.511 0.262 276.966)",
  700: "oklch(0.457 0.24 277.023)",
  800: "oklch(0.398 0.195 277.366)",
  900: "oklch(0.359 0.144 278.697)",
  950: "oklch(0.257 0.09 281.288)",
};

const VIOLET = {
  50: "oklch(0.969 0.016 293.756)",
  100: "oklch(0.943 0.029 294.588)",
  200: "oklch(0.894 0.057 293.283)",
  300: "oklch(0.811 0.111 293.571)",
  400: "oklch(0.702 0.183 293.541)",
  500: "oklch(0.606 0.25 292.717)",
  600: "oklch(0.541 0.281 293.009)",
  700: "oklch(0.491 0.27 292.581)",
  800: "oklch(0.432 0.232 292.759)",
  900: "oklch(0.38 0.189 293.745)",
  950: "oklch(0.283 0.141 291.089)",
};

const ROSE = {
  50: "oklch(0.969 0.015 12.422)",
  100: "oklch(0.941 0.03 12.58)",
  200: "oklch(0.892 0.058 10.001)",
  300: "oklch(0.81 0.117 11.638)",
  400: "oklch(0.712 0.194 13.428)",
  500: "oklch(0.645 0.246 16.439)",
  600: "oklch(0.586 0.253 17.585)",
  700: "oklch(0.514 0.222 16.935)",
  800: "oklch(0.455 0.188 13.697)",
  900: "oklch(0.41 0.159 10.272)",
  950: "oklch(0.271 0.105 12.094)",
};

const AMBER = {
  50: "oklch(0.987 0.022 95.277)",
  100: "oklch(0.962 0.059 95.617)",
  200: "oklch(0.924 0.12 95.746)",
  300: "oklch(0.879 0.169 91.605)",
  400: "oklch(0.828 0.189 84.429)",
  500: "oklch(0.769 0.188 70.08)",
  600: "oklch(0.666 0.179 58.318)",
  700: "oklch(0.555 0.163 48.998)",
  800: "oklch(0.473 0.137 46.201)",
  900: "oklch(0.414 0.112 45.904)",
  950: "oklch(0.279 0.077 45.635)",
};

const SKY = {
  50: "oklch(0.977 0.013 236.62)",
  100: "oklch(0.951 0.026 236.824)",
  200: "oklch(0.901 0.058 230.902)",
  300: "oklch(0.828 0.111 230.318)",
  400: "oklch(0.746 0.16 232.661)",
  500: "oklch(0.685 0.169 237.323)",
  600: "oklch(0.588 0.158 241.966)",
  700: "oklch(0.5 0.134 242.749)",
  800: "oklch(0.443 0.11 240.79)",
  900: "oklch(0.391 0.09 240.876)",
  950: "oklch(0.293 0.066 243.157)",
};

const FUCHSIA = {
  50: "oklch(0.977 0.017 320.058)",
  100: "oklch(0.952 0.037 318.852)",
  200: "oklch(0.903 0.076 319.62)",
  300: "oklch(0.833 0.145 321.434)",
  400: "oklch(0.74 0.238 322.16)",
  500: "oklch(0.667 0.295 322.15)",
  600: "oklch(0.591 0.293 322.896)",
  700: "oklch(0.518 0.253 323.949)",
  800: "oklch(0.452 0.211 324.591)",
  900: "oklch(0.401 0.17 325.612)",
  950: "oklch(0.293 0.136 325.661)",
};

export const THEME_PRESETS: ThemePreset[] = [
  {
    id: "default",
    labelKey: "settings.appearance.themes.default",
    mode: "light",
    accent: EMERALD,
    ambient: null,
  },
  {
    id: "default-dark",
    labelKey: "settings.appearance.themes.defaultDark",
    mode: "dark",
    accent: EMERALD,
    ambient: null,
  },
  {
    id: "oled",
    labelKey: "settings.appearance.themes.oled",
    mode: "dark",
    accent: EMERALD,
    ambient: "#000000",
    // True black canvas — keep surface pitch black, elevate by the
    // smallest perceivable step so cards still read above the body.
    surfaceDark: "#000000",
    surfaceDarkElevated: "#0a0a0a",
  },
  {
    id: "midnight",
    labelKey: "settings.appearance.themes.midnight",
    mode: "dark",
    accent: INDIGO,
    ambient: "#0b1020",
    surfaceDark: "#0b1020",
    surfaceDarkElevated: "#141a2e",
  },
  {
    id: "forest",
    labelKey: "settings.appearance.themes.forest",
    mode: "dark",
    accent: EMERALD,
    ambient: "#0c1612",
    surfaceDark: "#0c1612",
    surfaceDarkElevated: "#13201b",
  },
  {
    id: "sunset",
    labelKey: "settings.appearance.themes.sunset",
    mode: "dark",
    accent: AMBER,
    ambient: "#1a0e08",
    surfaceDark: "#1a0e08",
    surfaceDarkElevated: "#241612",
  },
  {
    id: "lavender",
    labelKey: "settings.appearance.themes.lavender",
    mode: "dark",
    accent: VIOLET,
    ambient: "#15101e",
    surfaceDark: "#15101e",
    surfaceDarkElevated: "#1f1828",
  },
  {
    id: "crimson",
    labelKey: "settings.appearance.themes.crimson",
    mode: "dark",
    accent: ROSE,
    ambient: "#19090c",
    surfaceDark: "#19090c",
    surfaceDarkElevated: "#241016",
  },
  {
    id: "ocean",
    labelKey: "settings.appearance.themes.ocean",
    mode: "dark",
    accent: SKY,
    ambient: "#081420",
    surfaceDark: "#081420",
    surfaceDarkElevated: "#0f1c30",
  },
  {
    id: "neon",
    labelKey: "settings.appearance.themes.neon",
    mode: "dark",
    accent: FUCHSIA,
    ambient: "#1a0a18",
    surfaceDark: "#1a0a18",
    surfaceDarkElevated: "#231022",
  },
];

export const DEFAULT_THEME_ID = "default-dark";

export function findTheme(id: string | null | undefined): ThemePreset {
  if (!id) return THEME_PRESETS.find((t) => t.id === DEFAULT_THEME_ID)!;
  return (
    THEME_PRESETS.find((t) => t.id === id) ??
    THEME_PRESETS.find((t) => t.id === DEFAULT_THEME_ID)!
  );
}

/**
 * Apply the theme's CSS variables + dark class to the document root.
 * Idempotent — calling repeatedly with the same theme is a no-op.
 */
// Tailwind v4 `@theme { --color-surface-dark }` defaults — re-applied
// when a theme doesn't override them so a swap from "Lavender" back to
// "Émeraude" doesn't leave the previous violet surface lingering.
const DEFAULT_SURFACE_DARK = "#121212";
const DEFAULT_SURFACE_DARK_ELEVATED = "#181818";

export function applyTheme(theme: ThemePreset) {
  const root = document.documentElement;
  // Accent palette overrides — picked up by emerald-* utilities in
  // app.css, so every existing `bg-emerald-500` automatically retints.
  for (const [shade, value] of Object.entries(theme.accent)) {
    root.style.setProperty(`--accent-${shade}`, value);
  }
  root.style.setProperty(
    "--ambient-bg",
    theme.ambient ?? (theme.mode === "dark" ? "#121212" : "#ffffff"),
  );
  // Surface tokens that drive `bg-surface-dark` / `bg-surface-dark-elevated`
  // (Tailwind v4 generates the utilities from these custom-property
  // names). Themed dark palettes override both so sidebar / right panels
  // / player bar carry the theme tint instead of staying flat charcoal.
  // Always re-set: switching from a custom dark theme back to the
  // default needs to clear the lingering override.
  root.style.setProperty(
    "--color-surface-dark",
    theme.surfaceDark ?? DEFAULT_SURFACE_DARK,
  );
  root.style.setProperty(
    "--color-surface-dark-elevated",
    theme.surfaceDarkElevated ?? DEFAULT_SURFACE_DARK_ELEVATED,
  );
  root.setAttribute("data-theme", theme.id);
  // Keep the legacy `dark` class wired to mode so the existing
  // `dark:` Tailwind variants stay coherent with the new theme system.
  if (theme.mode === "dark") {
    root.classList.add("dark");
  } else {
    root.classList.remove("dark");
  }
}
