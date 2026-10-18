import type { ThemePreset } from "./themes";

/**
 * Custom accent colour, laid over whichever of the fourteen themes is
 * active (2.0).
 *
 * The theme keeps its surfaces and its light or dark mode; only the
 * accent scale — the eleven `--accent-*` stops every `emerald-*` utility
 * reads — is replaced. Three sources:
 *
 * - `theme`: the theme's own scale, as before;
 * - `custom`: a colour the user picked;
 * - `cover`: the dominant hue of the playing track's artwork, falling
 *   back to the theme's scale when the cover has no colour to give.
 *
 * **Contrast is inherited, not re-checked.** A generated scale keeps the
 * default palette's *lightness* at every stop and borrows only the hue
 * and the colourfulness of the chosen colour. Every skin was tuned
 * against those lightnesses — white on a 500 button, a 400 ring on a
 * dark surface, 600 text on a light one — so any hue reads as the
 * default does. Picking a yellow gets a deeper, mustard-leaning yellow
 * for that reason, and that is the trade.
 *
 * Pure apart from the cache helpers at the bottom: OKLCH maths only.
 */

export const ACCENT_SHADES = [
  50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950,
] as const;
export type AccentShade = (typeof ACCENT_SHADES)[number];
export type AccentScale = ThemePreset["accent"];

export type AccentChoice =
  { mode: "theme" } | { mode: "custom"; color: string } | { mode: "cover" };

export const ACCENT_SETTING_KEY = "appearance.accent";
export const DEFAULT_ACCENT_CHOICE: AccentChoice = { mode: "theme" };

/** A starting palette for the picker: one swatch per hue family. */
export const ACCENT_SWATCHES = [
  "#ef4444",
  "#f97316",
  "#f59e0b",
  "#84cc16",
  "#22c55e",
  "#14b8a6",
  "#0ea5e9",
  "#3b82f6",
  "#8b5cf6",
  "#ec4899",
] as const;

const HEX = /^#[0-9a-f]{6}$/i;

/** Read a stored choice; anything unexpected is the theme's accent. */
export function parseAccentChoice(raw: string | null): AccentChoice {
  if (!raw) return DEFAULT_ACCENT_CHOICE;
  try {
    const value = JSON.parse(raw) as { mode?: unknown; color?: unknown };
    if (value.mode === "cover") return { mode: "cover" };
    if (
      value.mode === "custom" &&
      typeof value.color === "string" &&
      HEX.test(value.color)
    ) {
      return { mode: "custom", color: value.color.toLowerCase() };
    }
  } catch {
    // A malformed row reads as the default, like a missing one.
  }
  return DEFAULT_ACCENT_CHOICE;
}

export function serializeAccentChoice(choice: AccentChoice): string {
  return JSON.stringify(choice);
}

// ── OKLCH ───────────────────────────────────────────────────────────

/** Lightness and chroma of the default (Émeraude) scale, stop by stop. */
const TEMPLATE: Record<AccentShade, { l: number; c: number }> = {
  50: { l: 0.979, c: 0.021 },
  100: { l: 0.95, c: 0.052 },
  200: { l: 0.905, c: 0.093 },
  300: { l: 0.845, c: 0.143 },
  400: { l: 0.765, c: 0.177 },
  500: { l: 0.696, c: 0.17 },
  600: { l: 0.596, c: 0.145 },
  700: { l: 0.508, c: 0.118 },
  800: { l: 0.432, c: 0.095 },
  900: { l: 0.378, c: 0.077 },
  950: { l: 0.262, c: 0.051 },
};
const TEMPLATE_PEAK = TEMPLATE[500].c;

/** Past this, no sRGB colour has more chroma at any lightness. */
const MAX_CHROMA = 0.37;

export interface Oklch {
  l: number;
  c: number;
  /** Degrees, 0–360. Meaningless when `c` is 0. */
  h: number;
}

function srgbToLinear(v: number): number {
  return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
}

/** `#rrggbb` or 0–255 channels to OKLCH. */
export function rgbToOklch(r: number, g: number, b: number): Oklch {
  const lr = srgbToLinear(r / 255);
  const lg = srgbToLinear(g / 255);
  const lb = srgbToLinear(b / 255);
  const l_ = Math.cbrt(
    0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb,
  );
  const m_ = Math.cbrt(
    0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb,
  );
  const s_ = Math.cbrt(
    0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb,
  );
  const l = 0.2104542553 * l_ + 0.793617785 * m_ - 0.0040720468 * s_;
  const a = 1.9779984951 * l_ - 2.428592205 * m_ + 0.4505937099 * s_;
  const bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.808675766 * s_;
  const c = Math.hypot(a, bb);
  const h = ((Math.atan2(bb, a) * 180) / Math.PI + 360) % 360;
  return { l, c, h };
}

export function hexToOklch(hex: string): Oklch {
  const n = Number.parseInt(hex.slice(1), 16);
  return rgbToOklch((n >> 16) & 255, (n >> 8) & 255, n & 255);
}

/** Whether `oklch(l c h)` has an sRGB form, i.e. displays unclipped. */
function inGamut(l: number, c: number, h: number): boolean {
  const rad = (h * Math.PI) / 180;
  const a = c * Math.cos(rad);
  const b = c * Math.sin(rad);
  const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const r = 4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_;
  const g = -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_;
  const bl = -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_;
  const eps = 1e-4;
  return [r, g, bl].every((v) => v >= -eps && v <= 1 + eps);
}

/** The most chroma `h` can carry at lightness `l` and stay in sRGB. */
function maxChroma(l: number, h: number): number {
  let lo = 0;
  let hi = MAX_CHROMA;
  for (let i = 0; i < 24; i++) {
    const mid = (lo + hi) / 2;
    if (inGamut(l, mid, h)) lo = mid;
    else hi = mid;
  }
  return lo;
}

const fmt = (v: number, digits: number) => v.toFixed(digits);

/**
 * The eleven stops for a colour: the default lightness at each stop, the
 * colour's hue, and its chroma spread along the default's curve — so a
 * muted pick gives a muted scale, a grey one a grey scale. Each stop is
 * held inside sRGB rather than left for the browser to clip, which would
 * shift the hue on the way.
 */
export function scaleFromOklch({ c, h }: Oklch): AccentScale {
  const peak = Math.min(Math.max(c, 0), MAX_CHROMA);
  const scale = {} as AccentScale;
  for (const shade of ACCENT_SHADES) {
    const { l, c: tc } = TEMPLATE[shade];
    const wanted = (tc / TEMPLATE_PEAK) * peak;
    const chroma = Math.min(wanted, maxChroma(l, h));
    scale[shade] = `oklch(${fmt(l, 3)} ${fmt(chroma, 3)} ${fmt(h, 2)})`;
  }
  return scale;
}

export function scaleFromHex(hex: string): AccentScale {
  return scaleFromOklch(hexToOklch(hex));
}

/**
 * Below this a cover reads as grey: its accent would be a neutral scale
 * that looks like a bug rather than a choice, so the theme's accent
 * stays instead.
 */
export const COVER_MIN_CHROMA = 0.05;
/** A cover's hue is lifted to at least this much colour, so the accent
 *  reads as one even from a muted sleeve. */
const COVER_FLOOR_CHROMA = 0.12;

/** The scale for a cover's dominant colour, or `null` when it has none. */
export function scaleFromCover(rgb: {
  r: number;
  g: number;
  b: number;
}): AccentScale | null {
  const color = rgbToOklch(rgb.r, rgb.g, rgb.b);
  if (color.c < COVER_MIN_CHROMA) return null;
  return scaleFromOklch({ ...color, c: Math.max(color.c, COVER_FLOOR_CHROMA) });
}

// ── First-paint cache ───────────────────────────────────────────────

/**
 * The scale last applied, whatever its source. The `index.html`
 * bootstrap stamps it before React mounts, so a custom or cover accent —
 * and any theme other than Émeraude — paints right on the first frame
 * instead of flashing the stylesheet's green.
 */
export const ACCENT_CACHE_KEY = "waveflow.accent.scale";

const CACHED_STOP = /^oklch\([0-9. ]+\)$/;

/** The scale the bootstrap painted, or `null` when there is none worth
 *  trusting. Same filter as the bootstrap's. */
export function readCachedAccent(): AccentScale | null {
  try {
    const stops: unknown = JSON.parse(
      window.localStorage.getItem(ACCENT_CACHE_KEY) ?? "null",
    );
    if (
      !Array.isArray(stops) ||
      stops.length !== ACCENT_SHADES.length ||
      !stops.every((s) => typeof s === "string" && CACHED_STOP.test(s))
    ) {
      return null;
    }
    const scale = {} as AccentScale;
    ACCENT_SHADES.forEach((shade, i) => {
      scale[shade] = stops[i] as string;
    });
    return scale;
  } catch {
    return null;
  }
}

export function writeCachedAccent(scale: AccentScale): void {
  try {
    window.localStorage.setItem(
      ACCENT_CACHE_KEY,
      JSON.stringify(ACCENT_SHADES.map((shade) => scale[shade])),
    );
  } catch {
    // Private mode or storage off: the next launch flashes, nothing worse.
  }
}
