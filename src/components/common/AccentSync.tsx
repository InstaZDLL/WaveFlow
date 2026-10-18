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
 *   (64 px is all the sampler reads). The last cover's scale stays up
 *   while the next one is sampled, rather than flashing the theme's
 *   colour between tracks; a cover with no colour to give, or none at
 *   all, falls back to the theme.
 * - `theme`, or the choice not read yet: no override.
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

  const override = useMemo((): AccentScale | null => {
    if (!ready) return null;
    switch (choice.mode) {
      case "custom":
        return scaleFromHex(choice.color);
      case "cover":
        if (!coverUrl) return null;
        // Keep showing the previous cover's accent until this one is
        // sampled; only a sampled answer for this cover replaces it.
        return coverScale?.scale ?? null;
      default:
        return null;
    }
  }, [ready, choice, coverUrl, coverScale]);

  useEffect(() => {
    setAccentOverride(override);
  }, [override, setAccentOverride]);

  return null;
}
