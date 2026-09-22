import { useEffect, useState } from "react";
import { dominantColor } from "../lib/dominantColor";
import { resolvePlaylistColor } from "../lib/playlistVisuals";

type Color = { r: number; g: number; b: number };

/** The palette remains the immediate fallback while artwork is loading. */
export function usePlaylistAccent(
  artworkUrl: string | null,
  colorId: string,
  mode: "auto" | "manual",
): Color {
  const fallback = resolvePlaylistColor(colorId).rgb;
  const [sample, setSample] = useState<{ url: string; color: Color } | null>(
    null,
  );

  useEffect(() => {
    if (mode !== "auto" || !artworkUrl) return;
    let active = true;
    dominantColor(artworkUrl, "vibrant")
      .then((color) => {
        if (active) setSample({ url: artworkUrl, color });
      })
      .catch(() => {
        // Unavailable artwork keeps the stable palette fallback.
      });
    return () => {
      active = false;
    };
  }, [artworkUrl, mode]);

  if (mode === "auto" && artworkUrl && sample?.url === artworkUrl) {
    return sample.color;
  }
  return { r: fallback[0], g: fallback[1], b: fallback[2] };
}

/** Keep white header text readable even when the cover has a pale accent. */
function headerColor(accent: Color): Color {
  const luminance = (factor: number) => {
    const channel = (value: number) => {
      const srgb = (value * factor) / 255;
      return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
    };
    return (
      0.2126 * channel(accent.r) +
      0.7152 * channel(accent.g) +
      0.0722 * channel(accent.b)
    );
  };

  if (luminance(1) <= 0.13) return accent;
  let low = 0;
  let high = 1;
  for (let i = 0; i < 12; i += 1) {
    const middle = (low + high) / 2;
    if (luminance(middle) <= 0.13) low = middle;
    else high = middle;
  }
  return {
    r: Math.round(accent.r * low),
    g: Math.round(accent.g * low),
    b: Math.round(accent.b * low),
  };
}

export function playlistGradient(accent: Color): string {
  const { r, g, b } = headerColor(accent);
  return `linear-gradient(180deg, rgb(${r},${g},${b}) 0%, rgb(${r},${g},${b}) 56%, rgba(${r},${g},${b},0.78) 62%, rgba(${r},${g},${b},0.24) 80%, rgba(${r},${g},${b},0) 100%)`;
}

export function playlistPreviewGradient(accent: Color): string {
  const { r, g, b } = headerColor(accent);
  return `linear-gradient(135deg, rgb(${r},${g},${b}), rgb(${Math.round(r * 0.75)},${Math.round(g * 0.75)},${Math.round(b * 0.75)}))`;
}
