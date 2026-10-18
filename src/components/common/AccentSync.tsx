import { useEffect, useMemo, useState } from "react";

import { useAccentChoice } from "../../hooks/useAccentChoice";
import { usePlayer } from "../../hooks/usePlayer";
import { useTheme } from "../../hooks/useTheme";
import {
  scaleFromCover,
  scaleFromHex,
  type AccentScale,
} from "../../lib/accent";
import { dominantColor } from "../../lib/dominantColor";
import { resolveArtwork } from "../../lib/tauri/artwork";

/**
 * Works out the accent the theme should paint, and hands it to
 * `ThemeProvider`, which stays the only writer of the `--accent-*`
 * variables. Renders nothing.
 *
 * Mounted under `PlayerProvider` in every window that has a theme — the
 * main one and the mini-player — because the cover mode follows the
 * playing track.
 *
 * - `custom`: the picked colour's scale.
 * - `cover`: the cover's dominant hue, sampled at the smallest size
 *   (64 px is all the sampler reads). The current accent stays up while
 *   the next cover is sampled, and while no track is loaded, rather than
 *   flashing the theme's colour in between; a cover with no colour to
 *   give falls back to the theme.
 * - `theme`: no override.
 * - Not read yet: nothing is sent, so the scale the bootstrap restored
 *   stays.
 */
export function AccentSync() {
  const { value: choice, ready } = useAccentChoice();
  const { setAccentOverride } = useTheme();
  const { currentTrack } = usePlayer();

  const coverUrl = useMemo(() => {
    if (!currentTrack) return null;
    return resolveArtwork(
      {
        full: currentTrack.artwork_path,
        x1: currentTrack.artwork_path_1x,
        x2: currentTrack.artwork_path_2x,
      },
      "1x",
    );
  }, [currentTrack]);

  const [coverScale, setCoverScale] = useState<{
    url: string;
    scale: AccentScale | null;
  } | null>(null);

  const wantsCover = ready && choice.mode === "cover";
  useEffect(() => {
    if (!wantsCover || !coverUrl) return;
    let active = true;
    dominantColor(coverUrl, "vibrant")
      .then((rgb) => {
        if (active)
          setCoverScale({ url: coverUrl, scale: scaleFromCover(rgb) });
      })
      .catch(() => {
        // No colourful pixel, or an image that would not load: the
        // theme's accent, until a cover with colour comes along.
        if (active) setCoverScale({ url: coverUrl, scale: null });
      });
    return () => {
      active = false;
    };
  }, [wantsCover, coverUrl]);

  // `undefined` is "no answer yet": what is painted stays, be it the
  // scale the bootstrap restored or the previous cover's.
  const override = useMemo((): AccentScale | null | undefined => {
    if (!ready) return undefined;
    switch (choice.mode) {
      case "custom":
        return scaleFromHex(choice.color);
      case "cover":
        // No track yet (the queue is still being restored at startup),
        // or this cover not sampled yet: keep the current accent rather
        // than flash the theme's in between.
        if (!coverUrl || coverScale?.url !== coverUrl) return undefined;
        return coverScale.scale;
      default:
        return null;
    }
  }, [ready, choice, coverUrl, coverScale]);

  useEffect(() => {
    if (override !== undefined) setAccentOverride(override);
  }, [override, setAccentOverride]);

  return null;
}
