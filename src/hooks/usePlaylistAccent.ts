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

/** Which way the header is painted: the two themes are not each other's
 *  inverse, they are two designs. */
export type HeaderScheme = "dark" | "light";

/** The ink each theme writes over the header, as the view writes it:
 *  `text-white` in the dark one, `text-neutral-900` in the light one.
 *  The guard below measures the palette against these exact values. */
const INK: Record<HeaderScheme, Color> = {
  dark: { r: 255, g: 255, b: 255 },
  light: { r: 23, g: 23, b: 23 },
};

/**
 * The two stops of the header, in the artwork's hue.
 *
 * Dark theme: `top` is about as light as the artwork and `deep` is well
 * darker at the header's foot, so the header visibly darkens downward.
 * Light theme: the reverse — a pale tint at the top fading paler still,
 * arriving at the page rather than stopping on an edge above it.
 */
function headerColors(
  accent: Color,
  scheme: HeaderScheme,
): { top: Color; deep: Color } {
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
  // The ink is lighter than its ground in the dark theme and darker in the
  // light one, so the ratio is taken between the brighter and the dimmer of
  // the two rather than in a fixed order.
  const labelContrast = (background: Color, ink: Color, opacity: number) => {
    const ground = 1 - opacity;
    const label = {
      r: background.r * ground + ink.r * opacity,
      g: background.g * ground + ink.g * opacity,
      b: background.b * ground + ink.b * opacity,
    };
    const written = luminance(label);
    const behind = luminance(background);
    return (
      (Math.max(written, behind) + 0.05) / (Math.min(written, behind) + 0.05)
    );
  };
  const mix = (a: Color, b: Color, t: number): Color => ({
    r: Math.round(a.r + (b.r - a.r) * t),
    g: Math.round(a.g + (b.g - a.g) * t),
    b: Math.round(a.b + (b.b - a.b) * t),
  });

  // The contrast is checked where the text starts (the label sits a little
  // under half way down the header, higher when it stacks on a narrow
  // window), not at the top, where there is nothing to read: requiring it
  // there is what once flattened the header into one dark tone.
  //
  // The check also has to stand for the *faintest* ink on the header, or it
  // clears a palette some of the text never passes on. Nothing over the
  // gradient may go below 85 % opacity — PlaylistView's label, summary and
  // counts, and the modal's preview lines, all sit exactly there.
  const TEXT_AT = 0.4;
  const LABEL_OPACITY = 0.85;

  if (scheme === "light") {
    // A pale wash of the artwork's hue, with dark text over it — a light
    // theme that is actually light, rather than a dark header dropped onto
    // a white page. That collision is what made the old light header
    // unusable: a saturated block, then a 120 px scramble down to white,
    // which read as a dirty band and let the Liquid skin's aurora bleed
    // through the tail in a hue unrelated to the cover.
    //
    // The lightness comes from the theme, not the cover: a near-black
    // sleeve must still give a light header, so only the hue and a lifted
    // saturation carry the artwork's identity. Pale colours need the lift
    // or they collapse to grey.
    //
    // The colour also runs the other way — strongest at the top, palest at
    // the foot — so it arrives at the page's own ground instead of leaving
    // an edge above it.
    const tint =
      saturation === 0 ? 0 : Math.min(0.85, Math.max(0.5, saturation));
    const FOOT = 0.93;
    let topLightness = 0.72;
    const stops = () => ({
      top: makeColor(tint, topLightness),
      deep: makeColor(tint, Math.max(topLightness + 0.05, FOOT)),
    });
    let { top, deep } = stops();
    // Dark ink on a pale ground reaches the full 4.5:1 without being
    // dragged anywhere near black, so the light theme holds the stricter
    // level the dark one cannot — and it is measured against `top`, the
    // palette's darkest point, rather than where the text happens to
    // land. The dark theme cannot afford that (requiring it at the top is
    // what once flattened it), but here the worst hue still clears 5:1,
    // so the guard may as well cover the whole header.
    for (
      let i = 0;
      i < 40 && labelContrast(top, INK.light, LABEL_OPACITY) < 4.5;
      i += 1
    ) {
      topLightness += 0.01;
      ({ top, deep } = stops());
    }
    return { top, deep };
  }

  // Dark theme. A real span between the stops — the header darkens by 28
  // points of lightness, which is what makes the gradient show at all.
  //
  // 3:1, the large-text level: at 4.5:1 the whole palette was pulled down
  // to near black. The title is large and bold; the small label and counts
  // over it carry a soft shadow in the view to make up for it.
  const SPAN = 0.28;
  const MIN_CONTRAST = 3;
  let topLightness = Math.min(0.72, Math.max(0.55, artLightness));
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
    i < 40 &&
    labelContrast(mix(top, deep, TEXT_AT), INK.dark, LABEL_OPACITY) <
      MIN_CONTRAST;
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
 * The page backdrop: the colour at full strength at the top, travelling to
 * its second stop at the end of the header — darker in the dark theme,
 * paler in the light one — then fading out over `fadePx` below it.
 *
 * The stops are placed against the header's real height (the CSS variable
 * above) rather than a share of the backdrop, so that second point lands
 * under the title at every breakpoint. The fade eases out over several
 * stops — two would read as a band — and runs to the same hue at zero
 * alpha rather than to a page colour: fading to a colour mixes through
 * grey, while transparent lets the page's own ground show through.
 */
export function playlistGradient(
  accent: Color,
  fadePx: number,
  scheme: HeaderScheme,
): string {
  const { top, deep } = headerColors(accent, scheme);
  const header = `var(${PLAYLIST_HEADER_VAR})`;
  const at = (share: number) =>
    `calc(${header} + ${Math.round(fadePx * share)}px)`;
  return `linear-gradient(to bottom, ${rgb(top)} 0, ${rgb(deep)} ${header}, ${rgb(deep, 0.72)} ${at(0.25)}, ${rgb(deep, 0.4)} ${at(0.5)}, ${rgb(deep, 0.14)} ${at(0.75)}, ${rgb(deep, 0)} ${at(1)})`;
}

/** The header alone, for the small preview in the editor: no fade. */
export function playlistPreviewGradient(
  accent: Color,
  scheme: HeaderScheme,
): string {
  const { top, deep } = headerColors(accent, scheme);
  return `linear-gradient(to bottom, ${rgb(top)} 0%, ${rgb(deep)} 100%)`;
}
