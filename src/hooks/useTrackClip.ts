import { useEffect, useState } from "react";

import { getTrackClip, type TrackClip } from "../lib/tauri/clip";
import { isNetworkPath } from "../lib/networkTracks";
import type { Track } from "../lib/tauri/track";
import { useProfile } from "./useProfile";

/**
 * `track`'s music video (#1073), or `null`. Only a library track on this
 * machine can have one: a stream, a file opened without importing it and a
 * track on a share have no row or no local file to look beside.
 *
 * Asked of the disk at each track change, not cached: the lookup is a
 * directory listing, and a clip the user just dropped beside a song should
 * be offered the next time the song comes up. The answer is tagged with the
 * track and profile it is about, so a render never shows the previous
 * track's clip during the lookup.
 */
export function useTrackClip(track: Track | null): TrackClip | null {
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  const eligible =
    track !== null && track.id >= 0 && !isNetworkPath(track.file_path);
  const trackId = eligible ? track.id : null;
  const [found, setFound] = useState<{
    key: string;
    clip: TrackClip | null;
  } | null>(null);
  const key = trackId === null ? null : `${profileId}:${trackId}`;

  useEffect(() => {
    if (trackId === null || key === null) return;
    let cancelled = false;
    getTrackClip(trackId)
      .then((clip) => {
        if (!cancelled) setFound({ key, clip });
      })
      .catch((err) => {
        console.error("[useTrackClip] lookup failed", err);
        if (!cancelled) setFound({ key, clip: null });
      });
    return () => {
      cancelled = true;
    };
  }, [trackId, key]);

  return found !== null && found.key === key ? found.clip : null;
}
