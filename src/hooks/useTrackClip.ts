import { useEffect, useState } from "react";

import {
  getRemoteTrackClip,
  getTrackClip,
  type ClipSource,
} from "../lib/tauri/clip";
import { isNetworkPath, useNetworkSourcesVersion } from "../lib/networkTracks";
import type { Track } from "../lib/tauri/track";
import { useProfile } from "./useProfile";

/**
 * `track`'s music video (#1073), or `null`. A library track has one beside
 * its file, on this machine or on the share it lies on (#1133); a server
 * track (RFC-029) has the one beside the local file it was reconciled with,
 * or the server's. A stream and a file opened without importing it have
 * none.
 *
 * Asked at each track change, not cached: the local lookup is a directory
 * listing, and a clip the user just dropped beside a song should be offered
 * the next time the song comes up. The answer is tagged with the track and
 * profile it is about, so a render never shows the previous track's clip
 * during the lookup.
 *
 * `probe`: a server track whose row the mirror has not read since clips
 * existed is asked of the server. The now-playing track does, a menu does
 * not (`remote::clip::on_server`).
 */
export function useTrackClip(
  track: Track | null,
  probe = false,
): ClipSource | null {
  const remoteId = track?.remote_id ?? null;
  const local = track !== null && remoteId === null && track.id >= 0;
  const sourcesVersion = useNetworkSourcesVersion();
  return useClipLookup(
    local ? track.id : null,
    // The path too: a relinked or replaced file keeps its id, and its clip
    // is looked for beside the new one.
    local ? track.file_path : null,
    remoteId,
    probe,
    // A track on a share asks again when the sources change: the address
    // of its clip is served only by the source instance that found it, and
    // a share that was down when it was asked may answer now.
    local && isNetworkPath(track.file_path) ? sourcesVersion : 0,
  );
}

/** A server row's music video, for its menu: read from the mirror, never
 *  asked of the server. */
export function useServerTrackClip(
  remoteTrackId: string | null,
): ClipSource | null {
  return useClipLookup(null, null, remoteTrackId, false);
}

function useClipLookup(
  trackId: number | null,
  filePath: string | null,
  remoteTrackId: string | null,
  probe: boolean,
  sourcesVersion = 0,
): ClipSource | null {
  const { activeProfile } = useProfile();
  const profileId = activeProfile?.id ?? null;
  const [found, setFound] = useState<{
    key: string;
    track: string;
    clip: ClipSource | null;
  } | null>(null);
  // Which track the answer is about, and, with the sources' version, which
  // asking it answers: a new version asks again but keeps showing the last
  // answer about the same track until the new one lands, so the button does
  // not blink at each change of a share's state.
  const track =
    trackId !== null
      ? `${profileId}:${trackId}:${filePath}`
      : remoteTrackId !== null
        ? `${profileId}:server:${remoteTrackId}:${probe}`
        : null;
  const key = track === null ? null : `${track}:${sourcesVersion}`;

  useEffect(() => {
    if (key === null || track === null) return;
    let cancelled = false;
    const lookup: Promise<ClipSource | null> =
      remoteTrackId !== null
        ? getRemoteTrackClip(remoteTrackId, probe).then((clip) =>
            clip === null
              ? null
              : clip.kind === "local"
                ? { kind: "local", localPath: clip.localPath }
                : { kind: "server", remoteTrackId },
          )
        : getTrackClip(trackId as number).then((clip) =>
            clip === null
              ? null
              : clip.kind === "share"
                ? { kind: "share", url: clip.url }
                : { kind: "local", localPath: clip.localPath },
          );
    lookup
      .then((clip) => {
        if (!cancelled) setFound({ key, track, clip });
      })
      .catch((err) => {
        console.error("[useTrackClip] lookup failed", err);
        if (!cancelled) setFound({ key, track, clip: null });
      });
    return () => {
      cancelled = true;
    };
  }, [key, track, trackId, remoteTrackId, probe]);

  return found !== null && found.track === track ? found.clip : null;
}
