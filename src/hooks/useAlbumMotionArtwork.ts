import { useEffect, useState } from "react";

import {
  fetchAlbumMotionArtwork,
  type MotionArtwork,
} from "../lib/tauri/plugins";

/**
 * Resolve animated album artwork for `(artist, album)` via enabled
 * metadata plugins (Phase 3). Returns `null` when the inputs are missing
 * (e.g. a radio stream with no album), when offline, when no metadata
 * plugin is installed, or when none has motion artwork for the album —
 * callers render the static cover in that case.
 *
 * setState only fires inside the promise callbacks (never synchronously in
 * the effect body — `react-hooks/set-state-in-effect`), and a `cancelled`
 * guard drops a stale in-flight result when the track changes fast.
 */
export function useAlbumMotionArtwork(
  artist: string | null | undefined,
  album: string | null | undefined,
): MotionArtwork | null {
  const [motion, setMotion] = useState<MotionArtwork | null>(null);

  useEffect(() => {
    let cancelled = false;
    const apply = (m: MotionArtwork | null) => {
      if (!cancelled) setMotion(m);
    };
    // Clear the previous track's artwork right away so a stale overlay
    // never lingers over the new cover. Goes through a resolved promise
    // (not a synchronous setState in the effect body) to satisfy
    // `react-hooks/set-state-in-effect`, and runs before any fetch
    // resolves, so the new artwork only ever replaces `null`.
    Promise.resolve<MotionArtwork | null>(null).then(apply);
    if (artist && album) {
      fetchAlbumMotionArtwork(artist, album).then(apply, () => apply(null));
    }
    return () => {
      cancelled = true;
    };
  }, [artist, album]);

  return motion;
}
