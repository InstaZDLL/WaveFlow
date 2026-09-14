/**
 * High-contrast mode (#596).
 *
 * Our secondary text is grey on grey by design. For someone with low
 * vision that design *is* the problem, and there was no way to ask the
 * app to be legible instead of elegant.
 *
 * ## Why this is a palette and not a fork
 *
 * The issue assumed colour already lived in WaveFlow's own theme
 * tokens. It does not: only the *accent* is exposed as `--accent-*`
 * variables, while the greys are ~1 900 raw `text-zinc-*` /
 * `bg-zinc-*` Tailwind utilities spread across the components.
 *
 * The conclusion survives anyway, for a different reason. Tailwind v4
 * defines its own palette as `--color-zinc-*` custom properties, and
 * `@theme inline` in `app.css` re-points that whole scale at
 * `--wf-zinc-*` variables we own — exactly the trick that already lets
 * a theme re-tint every `bg-emerald-*` in the app without touching a
 * component. Overriding `--wf-zinc-*` under `[data-contrast="high"]`
 * therefore moves every grey in the interface at once.
 *
 * ## Why the attribute, not a class
 *
 * `applyTheme` owns the `dark` class and `applySkin` owns `data-skin`.
 * Contrast has to *compose* with both rather than replace either, so it
 * gets its own attribute and its own storage key, and no code path
 * writes two of the three.
 */

/** What the user asked for, which is not the same as what is applied. */
export type ContrastMode = "auto" | "normal" | "high";

export const DEFAULT_CONTRAST_MODE: ContrastMode = "auto";

/** `profile_setting` key holding the user's choice. */
export const CONTRAST_SETTING_KEY = "appearance.contrast";

/**
 * localStorage key read by the bootstrap script in `index.html`.
 *
 * The DB row is the source of truth; this is a first-paint cache, like
 * `waveflow.theme.id`. It matters more here than it does for the theme:
 * a flash of low-contrast chrome is precisely the thing the person who
 * turned this on cannot read.
 */
export const CONTRAST_CACHE_KEY = "waveflow.contrast";

export function parseContrastMode(raw: string | null): ContrastMode {
  return raw === "high" || raw === "normal" || raw === "auto"
    ? raw
    : DEFAULT_CONTRAST_MODE;
}

/**
 * Whether the OS asks for more contrast.
 *
 * `prefers-contrast: more` is the standard signal; some engines still
 * only ship the older `-ms-high-contrast`-era `forced-colors`, which is
 * a different feature (it replaces colours outright) and is deliberately
 * not consulted here — forced-colors already overrides our palette, so
 * layering our own on top would fight it.
 */
export function systemPrefersMoreContrast(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  try {
    return window.matchMedia("(prefers-contrast: more)").matches;
  } catch {
    return false;
  }
}

/** Turn the stored choice into the state actually painted. */
export function resolveContrast(mode: ContrastMode): "normal" | "high" {
  if (mode === "high") return "high";
  if (mode === "normal") return "normal";
  return systemPrefersMoreContrast() ? "high" : "normal";
}

/**
 * Write the resolved state onto the document root.
 *
 * Both states are stamped explicitly rather than leaving "normal"
 * implicit: the CSS has to tell "the user chose normal" from "nothing
 * has been decided yet", so that an OS-level `prefers-contrast: more`
 * can raise contrast in the undecided case without overriding someone
 * who deliberately turned the mode off.
 */
export function applyContrast(mode: ContrastMode) {
  if (typeof document === "undefined") return;
  document.documentElement.setAttribute("data-contrast", resolveContrast(mode));
}

export function writeCachedContrast(mode: ContrastMode) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(CONTRAST_CACHE_KEY, mode);
  } catch {
    // localStorage unavailable (private mode, quota). The DB row still
    // wins on the next launch; only the first paint is degraded.
  }
}

export function readCachedContrast(): ContrastMode {
  if (typeof window === "undefined") return DEFAULT_CONTRAST_MODE;
  try {
    return parseContrastMode(window.localStorage.getItem(CONTRAST_CACHE_KEY));
  } catch {
    return DEFAULT_CONTRAST_MODE;
  }
}
