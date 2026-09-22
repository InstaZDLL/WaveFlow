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

/** A light upper-right corner and a deeper lower-left corner share the artwork's hue. */
function headerColors(accent: Color): { pastel: Color; deep: Color } {
  const channels = [accent.r, accent.g, accent.b].map(
    (channel) => channel / 255,
  );
  const max = Math.max(...channels);
  const min = Math.min(...channels);
  const delta = max - min;
  let hue = 0;
  if (delta > 0) {
    if (max === channels[0]) hue = ((channels[1] - channels[2]) / delta) % 6;
    else if (max === channels[1]) hue = (channels[2] - channels[0]) / delta + 2;
    else hue = (channels[0] - channels[1]) / delta + 4;
    hue = (hue * 60 + 360) % 360;
  }

  const makeColor = (saturation: number, lightness: number): Color => {
    const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
    const sector = hue / 60;
    const secondary = chroma * (1 - Math.abs((sector % 2) - 1));
    const [red, green, blue] =
      sector < 1
        ? [chroma, secondary, 0]
        : sector < 2
          ? [secondary, chroma, 0]
          : sector < 3
            ? [0, chroma, secondary]
            : sector < 4
              ? [0, secondary, chroma]
              : sector < 5
                ? [secondary, 0, chroma]
                : [chroma, 0, secondary];
    const offset = lightness - chroma / 2;
    return {
      r: Math.round((red + offset) * 255),
      g: Math.round((green + offset) * 255),
      b: Math.round((blue + offset) * 255),
    };
  };

  const saturation = delta < 0.02 ? 0 : 0.72;
  const pastel = makeColor(saturation === 0 ? 0 : 0.6, 0.66);
  let deep = makeColor(saturation, 0.27);
  const luminance = ({ r, g, b }: Color) =>
    [r, g, b]
      .map((channel) => {
        const srgb = channel / 255;
        return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
      })
      .reduce(
        (sum, channel, index) =>
          sum + channel * [0.2126, 0.7152, 0.0722][index],
        0,
      );
  const labelContrast = (background: Color) => {
    const label = {
      r: background.r * 0.15 + 255 * 0.85,
      g: background.g * 0.15 + 255 * 0.85,
      b: background.b * 0.15 + 255 * 0.85,
    };
    return (luminance(label) + 0.05) / (luminance(background) + 0.05);
  };
  if (labelContrast(deep) < 4.5) {
    let low = 0;
    let high = 0.27;
    for (let i = 0; i < 12; i += 1) {
      const middle = (low + high) / 2;
      const candidate = makeColor(saturation, middle);
      if (labelContrast(candidate) >= 4.5) low = middle;
      else high = middle;
    }
    deep = makeColor(saturation, low);
  }
  return { pastel, deep };
}

function rgb({ r, g, b }: Color): string {
  return `rgb(${r},${g},${b})`;
}

export function playlistGradient(accent: Color): string {
  const { pastel, deep } = headerColors(accent);
  return `linear-gradient(to top right, ${rgb(deep)} 0%, ${rgb(deep)} 62%, ${rgb(pastel)} 100%)`;
}

export function playlistPreviewGradient(accent: Color): string {
  const { pastel, deep } = headerColors(accent);
  return `linear-gradient(to top right, ${rgb(deep)} 0%, ${rgb(deep)} 55%, ${rgb(pastel)} 100%)`;
}
