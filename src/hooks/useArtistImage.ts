import { useCallback, useEffect, useState } from "react";

import { enrichArtistDeezer } from "../lib/tauri/detail";
import { resolveArtwork } from "../lib/tauri/artwork";
import { useArtistUpdated } from "./useArtistUpdated";

/**
 * Resolve the current artist's photo, ready as an `<img src>` — a
 * webview-relative asset URL for a local file, or the remote CDN URL.
 * Returns `null` while it loads or when the artist has no picture.
 *
 * **The artist's own image wins.** An `artist.jpg` sidecar the scanner
 * linked, or a picture the user set by hand, is what the artist page and
 * the library grid show; this hook used to read the Deezer enrichment
 * alone, so the slideshow showed Deezer's photo instead of the curated
 * one — and showed nothing at all for an artist Deezer has no photo for,
 * next to a page displaying the local one (#701).
 *
 * Backed by the same `enrichArtistDeezer` + `resolveArtwork` path
 * `NowPlayingPanel` uses inline; extracted so `ImmersiveNowPlaying` (which
 * doesn't otherwise load artist enrichment) can drive the cover slideshow
 * (issue #466). The backend caches enrichment for 30 days, so a second
 * caller for the same artist is a cache hit.
 *
 * The resolved value is tagged with the artist id and only surfaced on a
 * match, so a fast artist change never flashes the previous artist's photo
 * (and no synchronous reset is needed in the effect body). It re-resolves
 * on `artist:updated`, which the picker's three commands emit — the
 * enrichment is cached per artist id, so nothing else would make this
 * hook look again while the same track plays (#692).
 */
export function useArtistImage(
  artistId: number | null | undefined,
): string | null {
  const [resolved, setResolved] = useState<{
    id: number;
    src: string | null;
  } | null>(null);

  // Bumped by `artist:updated` so a picture chosen while this artist is
  // playing reaches the slideshow without a track change.
  const [refresh, setRefresh] = useState(0);
  useArtistUpdated(
    useCallback(
      (updatedId: number) => {
        if (updatedId === artistId) setRefresh((n) => n + 1);
      },
      [artistId],
    ),
  );

  useEffect(() => {
    let cancelled = false;
    if (artistId == null) return;
    enrichArtistDeezer(artistId)
      .then((e) => {
        if (cancelled) return;
        // "full" (largest available) — the slideshow paints this into the
        // big cover slot, so a small 1x thumbnail would upscale blurry.
        // The artist's own image first, the Deezer one as the fallback.
        const src =
          resolveArtwork(
            {
              full: e.artwork_path,
              x1: e.artwork_path_1x,
              x2: e.artwork_path_2x,
            },
            "full",
          ) ??
          resolveArtwork(
            {
              full: e.picture_path,
              x1: e.picture_path_1x,
              x2: e.picture_path_2x,
              remoteUrl: e.picture_url,
            },
            "full",
          );
        setResolved({ id: artistId, src: src ?? null });
      })
      .catch(() => {
        if (!cancelled) setResolved({ id: artistId, src: null });
      });
    return () => {
      cancelled = true;
    };
  }, [artistId, refresh]);

  return resolved && resolved.id === artistId ? resolved.src : null;
}
