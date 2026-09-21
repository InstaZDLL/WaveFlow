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
    dominantColor(artworkUrl)
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

export function playlistGradient({ r, g, b }: Color): string {
  return `linear-gradient(180deg, rgba(${r},${g},${b},0.34) 0%, rgba(${r},${g},${b},0.20) 42%, rgba(${r},${g},${b},0.07) 75%, transparent 100%)`;
}
