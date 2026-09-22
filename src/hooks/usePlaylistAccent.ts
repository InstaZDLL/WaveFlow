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

/**
 * The two stops of the header, in the artwork's hue: `top`, the colour at
 * its full strength, and `deep`, darker, behind the title — dark enough
 * for the white label over it (4.5:1).
 */
function headerColors(accent: Color): { top: Color; deep: Color } {
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
  let deepLightness = 0.27;
  let deep = makeColor(saturation, deepLightness);
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
  // The lightest shade at or under `ceiling` that still carries the white
  // label at 4.5:1. Contrast falls as lightness rises, so a bisection
  // finds it.
  const lightestReadable = (ceiling: number): number => {
    if (labelContrast(makeColor(saturation, ceiling)) >= 4.5) return ceiling;
    let low = 0;
    let high = ceiling;
    for (let i = 0; i < 12; i += 1) {
      const middle = (low + high) / 2;
      if (labelContrast(makeColor(saturation, middle)) >= 4.5) low = middle;
      else high = middle;
    }
    return low;
  };
  if (labelContrast(deep) < 4.5) {
    deepLightness = lightestReadable(0.27);
    deep = makeColor(saturation, deepLightness);
  }
  // Brighter than `deep` by a fixed step, so the header darkens by about
  // the same amount whatever the artwork — but held to the same contrast:
  // on a narrow window the header stacks and the title sits in the upper
  // half. A light hue (yellow) can then barely brighten at all, and its
  // header is close to flat; the fade below still carries the colour.
  const top = makeColor(
    saturation,
    lightestReadable(Math.min(0.5, deepLightness + 0.2)),
  );
  return { top, deep };
}

function rgb({ r, g, b }: Color, alpha = 1): string {
  return `rgba(${r},${g},${b},${alpha})`;
}

/** CSS variable holding the header's height, set per breakpoint by the
 *  view (`[--playlist-header:…]` in its classes). */
const PLAYLIST_HEADER_VAR = "--playlist-header";

/**
 * The page backdrop, the way Spotify paints a playlist: the colour at full
 * strength at the top, darkening down to the end of the header, then
 * fading out over `fadePx` below it.
 *
 * The stops are placed against the header's real height (the CSS variable
 * above) rather than a share of the backdrop, so the darkest point lands
 * under the title at every breakpoint. The fade eases out over several
 * stops — two would read as a band — and runs to the same hue at zero
 * alpha rather than to a page colour: fading to a colour mixes through
 * grey, while transparent lets the page's own ground show through.
 */
export function playlistGradient(accent: Color, fadePx: number): string {
  const { top, deep } = headerColors(accent);
  const header = `var(${PLAYLIST_HEADER_VAR})`;
  const at = (share: number) =>
    `calc(${header} + ${Math.round(fadePx * share)}px)`;
  return `linear-gradient(to bottom, ${rgb(top)} 0, ${rgb(deep)} ${header}, ${rgb(deep, 0.72)} ${at(0.25)}, ${rgb(deep, 0.4)} ${at(0.5)}, ${rgb(deep, 0.14)} ${at(0.75)}, ${rgb(deep, 0)} ${at(1)})`;
}

/** The header alone, for the small preview in the editor: no fade. */
export function playlistPreviewGradient(accent: Color): string {
  const { top, deep } = headerColors(accent);
  return `linear-gradient(to bottom, ${rgb(top)} 0%, ${rgb(deep)} 100%)`;
}
