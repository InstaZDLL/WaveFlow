/**
 * Palette → the two colours the page reads.
 *
 * `glow` is for atmosphere: the average tone of the cover, deepened so
 * it sits behind text without competing. `accent` is for ink and
 * controls: the cover's most vivid hue, with its saturation pulled
 * down and its lightness pinned into a band that stays legible on the
 * near-black foundation (and takes dark ink on a filled button).
 */

interface Rgb {
  r: number;
  g: number;
  b: number;
}

export function hexToRgb(hex: string): Rgb {
  const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) return { r: 63, g: 63, b: 70 };
  return {
    r: parseInt(m[1], 16),
    g: parseInt(m[2], 16),
    b: parseInt(m[3], 16),
  };
}

function rgbToHsl({ r, g, b }: Rgb): [number, number, number] {
  const rn = r / 255,
    gn = g / 255,
    bn = b / 255;
  const max = Math.max(rn, gn, bn),
    min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === rn) h = (gn - bn) / d + (gn < bn ? 6 : 0);
  else if (max === gn) h = (bn - rn) / d + 2;
  else h = (rn - gn) / d + 4;
  return [h / 6, s, l];
}

function hslToHex(h: number, s: number, l: number): string {
  const f = (n: number) => {
    const k = (n + h * 12) % 12;
    const a = s * Math.min(l, 1 - l);
    const c = l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(c * 255)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}

export interface Atmosphere {
  accent: string;
  /** Ink that sits on a filled `accent` surface. */
  accentInk: string;
  glow: string;
  glowDeep: string;
}

const BRAND: Atmosphere = {
  accent: "#5ed3a5",
  accentInk: "#0b0b0e",
  glow: "#1f5a48",
  glowDeep: "#0f2b22",
};

export function atmosphereFromPalette(
  palette: string[] | null | undefined,
): Atmosphere {
  if (!palette || palette.length < 3) return BRAND;
  const [avg, , vivid] = palette;
  const [h, s] = rgbToHsl(hexToRgb(vivid));
  const [gh, gs, gl] = rgbToHsl(hexToRgb(avg));
  // A grey cover gives no hue worth painting with: stay on brand.
  if (s < 0.08 && gs < 0.08) return BRAND;
  const accent = hslToHex(h, Math.min(0.62, s * 0.85 + 0.12), 0.66);
  return {
    accent,
    accentInk: "#0b0b0e",
    glow: hslToHex(
      gh,
      Math.min(0.5, gs * 0.9 + 0.08),
      Math.min(0.34, Math.max(0.2, gl * 0.7)),
    ),
    glowDeep: hslToHex(gh, Math.min(0.45, gs * 0.9), 0.14),
  };
}

/** Same shaping, from a live `dominantColor()` sample. */
export function atmosphereFromRgb(vivid: Rgb, base: Atmosphere): Atmosphere {
  const [h, s] = rgbToHsl(vivid);
  if (s < 0.1) return base;
  return {
    ...base,
    accent: hslToHex(h, Math.min(0.62, s * 0.85 + 0.12), 0.66),
  };
}

/** A soft two-stop gradient used wherever a cover is missing. */
export function fallbackGradient(palette: string[] | null | undefined): string {
  const [a, b] =
    palette && palette.length >= 2 ? palette : ["#2a2a31", "#141418"];
  return `linear-gradient(135deg, ${a} 0%, ${b} 100%)`;
}
