import { useEffect, useState } from "react";

import { enrichArtistDeezer } from "../lib/tauri/detail";
import { resolveArtwork } from "../lib/tauri/artwork";

/**
 * Resolve the current artist's photo (local `artist.jpg` sidecar or the
 * Deezer-enriched picture), ready as an `<img src>` — a webview-relative
 * asset URL for a local file, or the remote CDN URL. Returns `null` while it
 * loads or when the artist has no picture.
 *
 * Backed by the same `enrichArtistDeezer` + `resolveArtwork` path
 * `NowPlayingPanel` uses inline; extracted so `ImmersiveNowPlaying` (which
 * doesn't otherwise load artist enrichment) can drive the cover slideshow
 * (issue #466). The backend caches enrichment for 30 days, so a second
 * caller for the same artist is a cache hit.
 *
 * The resolved value is tagged with the artist id and only surfaced on a
 * match, so a fast artist change never flashes the previous artist's photo
 * (and no synchronous reset is needed in the effect body).
 */
export function useArtistImage(
  artistId: number | null | undefined,
): string | null {
  const [resolved, setResolved] = useState<{
    id: number;
    src: string | null;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    if (artistId == null) return;
    enrichArtistDeezer(artistId)
      .then((e) => {
        if (cancelled) return;
        // "full" (largest available) — the slideshow paints this into the
        // big cover slot, so a small 1x thumbnail would upscale blurry.
        const src = resolveArtwork(
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
  }, [artistId]);

  return resolved && resolved.id === artistId ? resolved.src : null;
}
