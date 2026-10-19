import { useEffect, useRef, useState, type CSSProperties } from "react";
import { dominantColor } from "../../../../lib/dominantColor";

/**
 * The cover's light, as two CSS variables for the Fluid skin's
 * atmosphere: `--fe-glow` (the cover's average tone, deepened so it can
 * sit behind text) and `--fe-glow-deep` (the same, darker, for the
 * lower pool). Sampled through the app's own extractor, the one the
 * cover accent mode uses.
 *
 * The accent itself is not touched here: `AccentSync` owns it, and the
 * Fluid stylesheet reads `--accent-400`, so the user's accent choice
 * (theme, custom or cover) decides what the page is tinted with.
 */
export function useCoverGlow(url: string | null): CSSProperties {
  const [glow, setGlow] = useState<{ glow: string; deep: string } | null>(null);
  const seq = useRef(0);

  useEffect(() => {
    const mine = ++seq.current;
    if (!url) return;
    dominantColor(url, "average")
      .then(({ r, g, b }) => {
        if (mine !== seq.current) return;
        setGlow({ glow: shade(r, g, b, 0.62), deep: shade(r, g, b, 0.3) });
      })
      .catch(() => {
        // A cover that cannot be sampled keeps the previous light.
      });
  }, [url]);

  if (!glow) return {};
  return {
    ["--fe-glow" as string]: glow.glow,
    ["--fe-glow-deep" as string]: glow.deep,
  } as CSSProperties;
}

/** Scale a colour towards black and clamp its lightness so a white
 *  cover does not wash the page and a black one still shows. */
function shade(r: number, g: number, b: number, k: number): string {
  const lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
  // Pull bright covers down harder, lift very dark ones a little.
  const factor = lum > 0.6 ? k * 0.55 : lum < 0.12 ? k * 1.8 : k;
  const c = (v: number) =>
    Math.max(0, Math.min(255, Math.round(v * factor)))
      .toString(16)
      .padStart(2, "0");
  return `#${c(r)}${c(g)}${c(b)}`;
}
