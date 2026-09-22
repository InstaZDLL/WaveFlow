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
 * The two stops of the header, in the artwork's hue: `top`, the colour
 * about as light as the artwork, and `deep`, well darker at the header's
 * foot, so the header visibly darkens from one to the other.
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

  // The artwork's own saturation and lightness, held to a range that
  // stays a colour: a washed-out cover still gets some tint, a garish one
  // is calmed, and a grey one stays grey.
  const artLightness = (max + min) / 2;
  const artSaturation =
    delta === 0 ? 0 : delta / (1 - Math.abs(2 * artLightness - 1));
  const saturation =
    delta < 0.02 ? 0 : Math.min(0.8, Math.max(0.35, artSaturation));

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
  const mix = (a: Color, b: Color, t: number): Color => ({
    r: Math.round(a.r + (b.r - a.r) * t),
    g: Math.round(a.g + (b.g - a.g) * t),
    b: Math.round(a.b + (b.b - a.b) * t),
  });

  // A real span between the stops — the header darkens by 30 points of
  // lightness, which is what makes the gradient show at all. The contrast
  // is checked where the text starts (the label sits a little under half
  // way down the header, higher when it stacks on a narrow window), not at
  // the top, where there is nothing to read: requiring it there is what
  // flattened the header into one dark tone.
  const SPAN = 0.3;
  const TEXT_AT = 0.4;
  let topLightness = Math.min(0.6, Math.max(0.42, artLightness));
  const stops = () => {
    const deepLightness = Math.max(0.08, topLightness - SPAN);
    return {
      top: makeColor(saturation, topLightness),
      deep: makeColor(saturation, deepLightness),
    };
  };
  let { top, deep } = stops();
  for (
    let i = 0;
    i < 40 && labelContrast(mix(top, deep, TEXT_AT)) < 4.5;
    i += 1
  ) {
    topLightness -= 0.01;
    ({ top, deep } = stops());
  }
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
